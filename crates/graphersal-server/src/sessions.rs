//! Sessions that survive a restart: the browser sessions and the MCP sessions, kept in the
//! server directory's `sessions/` (on by default; there is no setting to turn it off).
//!
//! - `sessions/browser.json` ([`FileSessions`], a [`SessionTable`]): one row per browser
//!   session: the SHA-256 of its id, its user, the SHA-256 of the token it logged in with,
//!   created and last seen (seconds since the Unix epoch; `last_seen` is updated coarsely, see
//!   [`crate::access::SessionLimits`]).
//! - `sessions/mcp.json` ([`McpSessionFile`], a `graphersal_mcp::SessionStore`): one row per
//!   MCP session: the SHA-256 of its id, the identity of its `initialize`, the selected name,
//!   a pending rename note, the protocol revision, the client and the creation time.
//!
//! Only hashes of the ids are kept (a file never holds a usable id or token). Every change is
//! written through at once, the whole file each time, atomically (a temporary file and a
//! rename; mode `0600` on unix). A missing file is an empty table; a file that cannot be read
//! or parsed is a warning and an empty table (the server starts; the clients log in or
//! initialize again), and the next change overwrites it. A missing `sessions/` (a directory
//! from before) is created by the first write.
//!
//! Ending sessions: logout and token/user changes go through [`SessionTable::remove`] /
//! [`SessionTable::retain`] (browser) and `Mcp::forget_sessions_of` (MCP), both written through;
//! with no server running, the console's user commands edit the same files
//! ([`McpSessionFile::forget_identity`]).

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use graphersal_mcp::{McpError, SessionStore, StoredSession};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::access::{BrowserSession, SessionTable, TokenHash};
use crate::error::ServerError;
use crate::layout::{ServerDir, write_atomic_private};

/// The directory of the session files.
pub const SESSIONS_DIR: &str = "sessions";
/// The browser sessions' file in [`SESSIONS_DIR`].
pub const BROWSER_FILE: &str = "browser.json";
/// The MCP sessions' file in [`SESSIONS_DIR`].
pub const MCP_FILE: &str = "mcp.json";
/// The format version of both files.
const FORMAT_VERSION: u64 = 1;

/// Seconds since the Unix epoch (`0` before it).
fn unix_s(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|age| age.as_secs())
        .unwrap_or(0)
}

/// The text of `path`: `None` when it is missing or cannot be read (a warning).
fn read_text(path: &Path, what: &str) -> Option<String> {
    match fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => {
            tracing::warn!(
                target: "graphersal_server",
                "{what}: cannot read {}: {err}; starting with no sessions",
                path.display()
            );
            None
        }
    }
}

/// Parses the text of a session file (`path` and `what` name it in the warning): its rows, or
/// none (a warning) for a broken file or another format version.
fn parse_rows<R: for<'de> Deserialize<'de>>(text: &str, path: &Path, what: &str) -> Vec<R> {
    #[derive(Deserialize)]
    struct File<R> {
        version: u64,
        sessions: Vec<R>,
    }
    let broken = |why: String| {
        tracing::warn!(
            target: "graphersal_server",
            "{what}: {} is not readable ({why}); starting with no sessions",
            path.display()
        );
        Vec::new()
    };
    match serde_json::from_str::<File<R>>(text) {
        Ok(file) if file.version == FORMAT_VERSION => file.sessions,
        Ok(file) => broken(format!("format version {}", file.version)),
        Err(err) => broken(err.to_string()),
    }
}

/// Writes `rows` as the session file `path` (atomically, private; `sessions/` created when
/// missing, owner only: mode `0700` on unix).
fn write_rows<R: Serialize>(path: &Path, rows: &[R]) -> Result<(), ServerError> {
    #[derive(Serialize)]
    struct File<'a, R> {
        version: u64,
        sessions: &'a [R],
    }
    if let Some(parent) = path.parent()
        && !parent.is_dir()
    {
        crate::security::create_private_dir(parent)?;
    }
    let mut text = serde_json::to_string_pretty(&File {
        version: FORMAT_VERSION,
        sessions: rows,
    })
    .map_err(|err| ServerError::io("write", path, io::Error::other(err)))?;
    text.push('\n');
    write_atomic_private(path, text.as_bytes())
}

/// One row of `browser.json`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserRow {
    id_sha256: String,
    user: String,
    token_sha256: String,
    created_unix_s: u64,
    last_seen_unix_s: u64,
}

/// The browser sessions in `sessions/browser.json` (module docs): the table in memory, every
/// change written through.
#[derive(Debug)]
pub struct FileSessions {
    path: PathBuf,
    sessions: Mutex<HashMap<TokenHash, BrowserSession>>,
}

impl FileSessions {
    /// The browser sessions of `dir`, read from its `sessions/browser.json` (missing: none;
    /// unreadable or broken: none, with a warning; a row with a bad hash is skipped).
    pub fn open(dir: &ServerDir) -> Self {
        let path = dir.sessions().join(BROWSER_FILE);
        let what = "browser sessions";
        let rows: Vec<BrowserRow> = read_text(&path, what)
            .map(|text| parse_rows(&text, &path, what))
            .unwrap_or_default();
        let mut sessions = HashMap::new();
        for row in rows {
            let (Some(id), Some(token)) = (
                TokenHash::parse_hex(&row.id_sha256),
                TokenHash::parse_hex(&row.token_sha256),
            ) else {
                tracing::warn!(
                    target: "graphersal_server",
                    "{what}: {}: a row without valid hashes skipped",
                    path.display()
                );
                continue;
            };
            let at = |secs: u64| UNIX_EPOCH + Duration::from_secs(secs);
            let mut session = BrowserSession::new(row.user, token, at(row.created_unix_s));
            session.last_seen = at(row.last_seen_unix_s);
            sessions.insert(id, session);
        }
        Self {
            path,
            sessions: Mutex::new(sessions),
        }
    }

    /// The file (`sessions/browser.json`).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How many sessions the table holds.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether the table holds no session.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<TokenHash, BrowserSession>> {
        self.sessions.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Writes `sessions` (oldest first) as the file.
    fn save(&self, sessions: &HashMap<TokenHash, BrowserSession>) -> Result<(), ServerError> {
        let mut rows: Vec<BrowserRow> = sessions
            .iter()
            .map(|(id, session)| BrowserRow {
                id_sha256: id.to_hex(),
                user: session.user.clone(),
                token_sha256: session.token.to_hex(),
                created_unix_s: unix_s(session.created),
                last_seen_unix_s: unix_s(session.last_seen),
            })
            .collect();
        rows.sort_by(|a, b| {
            (a.created_unix_s, &a.id_sha256).cmp(&(b.created_unix_s, &b.id_sha256))
        });
        write_rows(&self.path, &rows)
    }
}

impl SessionTable for FileSessions {
    /// Adds the session and writes the file; a failed write takes it out again (`Err`).
    fn insert(&self, id: TokenHash, session: BrowserSession) -> Result<(), ServerError> {
        let mut sessions = self.lock();
        let before = sessions.insert(id, session);
        if let Err(err) = self.save(&sessions) {
            match before {
                Some(old) => sessions.insert(id, old),
                None => sessions.remove(&id),
            };
            return Err(err);
        }
        Ok(())
    }

    fn get(&self, id: &TokenHash) -> Option<BrowserSession> {
        self.lock().get(id).cloned()
    }

    fn touch(&self, id: &TokenHash, now: SystemTime) -> Result<(), ServerError> {
        let mut sessions = self.lock();
        let Some(session) = sessions.get_mut(id) else {
            return Ok(());
        };
        session.last_seen = now;
        self.save(&sessions)
    }

    /// Removes the session (from memory even when the write fails: `Err` then).
    fn remove(&self, id: &TokenHash) -> Result<Option<BrowserSession>, ServerError> {
        let mut sessions = self.lock();
        let removed = sessions.remove(id);
        if removed.is_some() {
            self.save(&sessions)?;
        }
        Ok(removed)
    }

    /// Keeps what `keep` accepts (removed from memory even when the write fails: `Err` then).
    fn retain(&self, keep: &dyn Fn(&BrowserSession) -> bool) -> Result<usize, ServerError> {
        let mut sessions = self.lock();
        let before = sessions.len();
        sessions.retain(|_, session| keep(session));
        let removed = before - sessions.len();
        if removed > 0 {
            self.save(&sessions)?;
        }
        Ok(removed)
    }
}

/// One row of `mcp.json`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpRow {
    id_sha256: String,
    #[serde(default)]
    identity: Option<String>,
    #[serde(default)]
    selected: Option<String>,
    #[serde(default)]
    renamed_from: Option<String>,
    protocol_version: String,
    #[serde(default)]
    client: Value,
    created_unix_s: u64,
}

impl McpRow {
    fn of(session: &StoredSession) -> Self {
        Self {
            id_sha256: session.id_sha256().to_string(),
            identity: session.identity().map(str::to_string),
            selected: session.selected().map(str::to_string),
            renamed_from: session.renamed_from().map(str::to_string),
            protocol_version: session.protocol_version().to_string(),
            client: session.client().clone(),
            created_unix_s: session.created_unix_s(),
        }
    }

    fn into_stored(self) -> StoredSession {
        StoredSession::new(
            self.id_sha256,
            self.identity,
            self.selected,
            self.renamed_from,
            self.protocol_version,
            self.client,
            self.created_unix_s,
        )
    }
}

/// Which identities' MCP sessions a load keeps.
type KeepIdentity = Box<dyn Fn(Option<&str>) -> bool + Send + Sync>;

/// The MCP sessions in `sessions/mcp.json` (module docs): the endpoint's
/// `graphersal_mcp::SessionStore` (`Mcp::with_session_store`), which hands it the whole table
/// after every change.
pub struct McpSessionFile {
    path: PathBuf,
    keep: Option<KeepIdentity>,
    write: Mutex<()>,
}

impl std::fmt::Debug for McpSessionFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpSessionFile")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl McpSessionFile {
    /// The MCP session file of `dir` (`sessions/mcp.json`; nothing is read yet).
    pub fn new(dir: &ServerDir) -> Self {
        Self {
            path: dir.sessions().join(MCP_FILE),
            keep: None,
            write: Mutex::new(()),
        }
    }

    /// Loads keep only the sessions whose identity `keep` accepts (the others are dropped and
    /// the file rewritten without them): at start, the sessions of users that were removed or
    /// lost their token while the server was down.
    pub fn keeping(mut self, keep: impl Fn(Option<&str>) -> bool + Send + Sync + 'static) -> Self {
        self.keep = Some(Box::new(keep));
        self
    }

    /// The file (`sessions/mcp.json`).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The stored sessions (missing file: none; unreadable or broken: none, with a warning).
    pub fn read(&self) -> Vec<StoredSession> {
        let what = "MCP sessions";
        read_text(&self.path, what)
            .map(|text| parse_rows::<McpRow>(&text, &self.path, what))
            .unwrap_or_default()
            .into_iter()
            .map(McpRow::into_stored)
            .collect()
    }

    /// Removes the stored sessions of `identity` (exact; `None` = anonymous) from the file:
    /// how many. For a server directory no server runs on (the console holds its lock) or whose
    /// server has MCP off; a running endpoint ends them with `Mcp::forget_sessions_of`.
    pub fn forget_identity(&self, identity: Option<&str>) -> Result<usize, ServerError> {
        let _write = self.write.lock().unwrap_or_else(|p| p.into_inner());
        let mut sessions = self.read();
        let before = sessions.len();
        sessions.retain(|session| session.identity() != identity);
        let removed = before - sessions.len();
        if removed > 0 {
            self.write_sessions(&sessions)?;
        }
        Ok(removed)
    }

    fn write_sessions(&self, sessions: &[StoredSession]) -> Result<(), ServerError> {
        let rows: Vec<McpRow> = sessions.iter().map(McpRow::of).collect();
        write_rows(&self.path, &rows)
    }
}

impl SessionStore for McpSessionFile {
    /// The stored sessions; never `Err` (a broken file is a warning and no sessions, so the
    /// server starts).
    fn load(&self) -> Result<Vec<StoredSession>, McpError> {
        let _write = self.write.lock().unwrap_or_else(|p| p.into_inner());
        let mut sessions = self.read();
        if let Some(keep) = &self.keep {
            let before = sessions.len();
            sessions.retain(|session| keep(session.identity()));
            if sessions.len() < before
                && let Err(err) = self.write_sessions(&sessions)
            {
                tracing::warn!(target: "graphersal_server", "MCP sessions: {err}");
            }
        }
        Ok(sessions)
    }

    fn save(&self, sessions: &[StoredSession]) -> Result<(), McpError> {
        let _write = self.write.lock().unwrap_or_else(|p| p.into_inner());
        self.write_sessions(sessions)
            .map_err(|err| McpError::Failed(err.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    fn server_dir(temp: &TempDir) -> ServerDir {
        ServerDir::new(temp.path())
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn browser_sessions_are_written_through_and_read_back() {
        let temp = TempDir::new("sessions-browser");
        let dir = server_dir(&temp);
        let table = FileSessions::open(&dir);
        assert!(table.is_empty());
        assert!(!table.path().exists(), "nothing written before a change");
        let (a, b) = (TokenHash::of("id-a"), TokenHash::of("id-b"));
        let mut alice = BrowserSession::new("alice", TokenHash::of("ta"), at(1_000));
        table.insert(a, alice.clone()).unwrap();
        table
            .insert(
                b,
                BrowserSession::new("bob", TokenHash::of("tb"), at(2_000)),
            )
            .unwrap();
        table.touch(&a, at(1_500)).unwrap();
        alice.last_seen = at(1_500);

        let text = fs::read_to_string(table.path()).unwrap();
        assert!(!text.contains("id-a") && !text.contains("\"ta\""), "{text}");
        assert!(text.contains(&TokenHash::of("id-a").to_hex()), "{text}");
        assert!(text.contains("\"version\": 1"), "{text}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(table.path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let again = FileSessions::open(&dir);
        assert_eq!(again.len(), 2);
        assert_eq!(again.get(&a), Some(alice));
        assert_eq!(again.retain(&|s| s.user != "bob").unwrap(), 1);
        assert_eq!(again.remove(&a).unwrap().unwrap().user, "alice");
        assert!(FileSessions::open(&dir).is_empty());
    }

    #[test]
    fn a_broken_or_unknown_file_starts_empty_and_is_overwritten() {
        let temp = TempDir::new("sessions-broken");
        let dir = server_dir(&temp);
        fs::create_dir_all(dir.sessions()).unwrap();
        let path = dir.sessions().join(BROWSER_FILE);
        for text in [
            "{ not json",
            "{\"version\": 99, \"sessions\": []}",
            "{\"version\": 1, \"sessions\": [{\"id_sha256\": \"x\"}]}",
        ] {
            fs::write(&path, text).unwrap();
            assert!(FileSessions::open(&dir).is_empty(), "{text}");
        }
        // A row with a bad hash is skipped, the others are kept.
        let good = TokenHash::of("good").to_hex();
        fs::write(
            &path,
            format!(
                "{{\"version\": 1, \"sessions\": [\
                 {{\"id_sha256\": \"zz\", \"user\": \"a\", \"token_sha256\": \"{good}\", \
                 \"created_unix_s\": 1, \"last_seen_unix_s\": 1}},\
                 {{\"id_sha256\": \"{good}\", \"user\": \"b\", \"token_sha256\": \"{good}\", \
                 \"created_unix_s\": 1, \"last_seen_unix_s\": 1}}]}}"
            ),
        )
        .unwrap();
        let table = FileSessions::open(&dir);
        assert_eq!(table.len(), 1);
        assert_eq!(table.get(&TokenHash::of("good")).unwrap().user, "b");

        // Broken: the next change writes a good file.
        fs::write(&path, "garbage").unwrap();
        let table = FileSessions::open(&dir);
        table
            .insert(
                TokenHash::of("n"),
                BrowserSession::new("c", TokenHash::of("t"), at(5)),
            )
            .unwrap();
        assert_eq!(FileSessions::open(&dir).len(), 1);
        // The MCP file likewise.
        fs::write(dir.sessions().join(MCP_FILE), "[1, 2").unwrap();
        assert_eq!(McpSessionFile::new(&dir).load().unwrap(), Vec::new());
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_insert_leaves_no_session() {
        use std::os::unix::fs::PermissionsExt;
        let temp = TempDir::new("sessions-readonly");
        let dir = server_dir(&temp);
        fs::create_dir_all(dir.sessions()).unwrap();
        fs::set_permissions(dir.sessions(), fs::Permissions::from_mode(0o500)).unwrap();
        let table = FileSessions::open(&dir);
        let id = TokenHash::of("id");
        let result = table.insert(id, BrowserSession::new("a", TokenHash::of("t"), at(1)));
        fs::set_permissions(dir.sessions(), fs::Permissions::from_mode(0o700)).unwrap();
        if result.is_ok() {
            // Running as root: permissions do not stop the write.
            return;
        }
        assert_eq!(table.get(&id), None);
    }

    fn stored(id: &str, identity: Option<&str>, created: u64) -> StoredSession {
        StoredSession::new(
            TokenHash::of(id).to_hex(),
            identity.map(str::to_string),
            Some("acme".to_string()),
            None,
            "2025-06-18",
            serde_json::json!({"name": "it", "version": "1"}),
            created,
        )
    }

    #[test]
    fn mcp_sessions_round_trip_and_are_forgotten_per_identity() {
        let temp = TempDir::new("sessions-mcp");
        let dir = server_dir(&temp);
        let file = McpSessionFile::new(&dir);
        assert_eq!(file.load().unwrap(), Vec::new());
        let sessions = vec![
            stored("a", Some("user:alice"), 1),
            stored("b", Some("user:bob"), 2),
            stored("c", None, 3),
        ];
        file.save(&sessions).unwrap();
        let text = fs::read_to_string(file.path()).unwrap();
        assert!(text.contains("\"selected\": \"acme\""), "{text}");
        assert_eq!(McpSessionFile::new(&dir).load().unwrap(), sessions);

        assert_eq!(file.forget_identity(Some("user:bob")).unwrap(), 1);
        assert_eq!(file.forget_identity(Some("user:bob")).unwrap(), 0);
        assert_eq!(file.forget_identity(None).unwrap(), 1);
        assert_eq!(file.read(), vec![stored("a", Some("user:alice"), 1)]);

        // A load that keeps only some identities rewrites the file without the others.
        let kept = McpSessionFile::new(&dir)
            .keeping(|identity| identity != Some("user:alice"))
            .load()
            .unwrap();
        assert_eq!(kept, Vec::new());
        assert_eq!(McpSessionFile::new(&dir).read(), Vec::new());
    }
}
