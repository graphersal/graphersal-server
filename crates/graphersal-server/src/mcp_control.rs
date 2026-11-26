//! MCP administration (round 3 B6): the runtime switches of the MCP endpoints, kept in
//! `mcp.toml` ([`McpSettings`], [`McpControl`]), and their usage since the start
//! ([`McpUsage`]).
//!
//! **Switches.** `mcp.toml` (written by the server only: the web UI's MCP tab, `PUT
//! /api/admin/mcp`, `PUT /api/admin/mcp/databases/<name>`, `graphersal-server mcp ...`; a
//! missing file = the defaults):
//!
//! ```toml
//! enabled = true          # false: /mcp and every /db/<name>/mcp answer 503 at once
//! expose_new = false      # whether a NEW database or alias is offered to MCP (default false)
//!
//! [databases.acme]        # a database or an alias, by name
//! exposed = false         # MCP does not offer it (list_graphs, use_graph, /db/acme/mcp)
//! read_only = true        # MCP serves it read-only, whatever the caller's grant
//! ```
//!
//! **New servers and new names (round 4 B-MCPDEF).** `init` writes this file with `enabled =
//! false` and `expose_new = false`: a new server offers nothing to MCP until an admin switches
//! MCP on and exposes names. Every name the server creates (a database made by create, import,
//! clone, a restore into a new name, repair; a new alias) gets its entry at birth
//! ([`McpSettings::new_name`]): `exposed = false` unless `expose_new` is true or the operator
//! asked for it (`--mcp`, the API's `"mcp": true`, the UI's checkbox). A name WITHOUT an entry
//! is exposed: the rule of every earlier version, kept so that an upgraded server directory
//! offers exactly what it offered before (its names have no entries); a missing file (a server
//! directory made before round 3) is `enabled = true`, `expose_new = false`. A renamed alias
//! carries its entry; a promoted database or a swap keeps its name and so its entry.
//!
//! A setting belongs to a NAME. A request naming `requested` that resolves to the database
//! `real` is offered when both names are exposed, and read-only when either is read-only (an
//! alias may narrow its database, never widen it). The effective level of an MCP request is
//! the least of the caller's grant, the database's mode (read-only flag, base) and this
//! read-only switch. `server.toml [mcp] enabled = false` stays the hard switch: the endpoint is
//! not built at all, and this file's switch cannot turn it on (restart after changing it).
//!
//! The global switch off: `/mcp` and `/db/<name>/mcp` answer `503` with help; the open MCP
//! sessions are kept (not ended): their requests are refused while it is off and go on when
//! it is on again.
//!
//! Concurrent changes: [`McpControl`] holds its write lock across read, check, write and the
//! update of the memory (a leaf lock: it takes no other); every change is based on the file's
//! revision ([`crate::revision`], of the canonical text) when one is given. A deleted database
//! or removed alias loses its settings, a renamed alias carries them (`Admin::drop_grants_on`,
//! `Admin::move_alias_grants`).
//!
//! **Usage.** [`McpUsage`] counts in memory since the server started (lost on a restart): the
//! open MCP sessions (from the endpoint's session table, which it saves through
//! [`ObservedSessions`], plus the per-database endpoint each session was opened on), and every
//! `tools/call` seen by the router ([`McpUsage::observe`]): per tool, per user, per database,
//! errors with the last [`RECENT_ERRORS`] of them.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use graphersal_mcp::{McpError, SessionStore, StoredSession};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::ServerError;
use crate::layout::{ServerDir, write_atomic};
use crate::sessions::McpSessionFile;

/// How many recent tool errors [`McpUsage`] keeps.
pub(crate) const RECENT_ERRORS: usize = 50;

/// The most distinct names (tools, users, databases) counted each; further names are counted
/// under `(other)` (a client may send any tool name or graph name).
const MAX_NAMES: usize = 512;

/// The entry of the names beyond [`MAX_NAMES`].
const OTHER: &str = "(other)";

/// The longest error message kept, in characters.
const MAX_MESSAGE_CHARS: usize = 300;

/// The help of a request refused while MCP is switched off.
pub(crate) const SWITCHED_OFF_HELP: &str = "an admin switches it on again in the web UI's MCP \
     tab, with PUT /api/admin/mcp {\"enabled\": true}, or with graphersal-server mcp on --dir DIR";

/// The MCP settings of one database or alias name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NameSettings {
    /// Whether MCP offers the name (default `true`).
    #[serde(default = "default_true")]
    pub(crate) exposed: bool,
    /// Whether MCP serves it read-only (default `false`).
    #[serde(default)]
    pub(crate) read_only: bool,
}

impl Default for NameSettings {
    fn default() -> Self {
        Self {
            exposed: true,
            read_only: false,
        }
    }
}

fn default_true() -> bool {
    true
}

/// The contents of `mcp.toml` (module docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpSettings {
    /// Whether the MCP endpoints answer (default `true`: a missing file or key, a server
    /// directory older than this file; `init` writes `false`).
    #[serde(default = "default_true")]
    pub(crate) enabled: bool,
    /// Whether a new database or alias is offered to MCP when its creator does not say
    /// (default `false`; [`McpSettings::new_name`]).
    #[serde(default)]
    pub(crate) expose_new: bool,
    /// The settings per (folded) database or alias name; a name with the defaults is not kept.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) databases: BTreeMap<String, NameSettings>,
}

impl Default for McpSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            expose_new: false,
            databases: BTreeMap::new(),
        }
    }
}

impl McpSettings {
    /// The settings `init` writes into a new server directory: MCP off, new names not exposed.
    pub(crate) fn initial() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    /// The settings of a name the server just created: exposed when `expose` says so, else
    /// when [`McpSettings::expose_new`] is on; never read-only.
    pub(crate) fn new_name(&self, expose: Option<bool>) -> NameSettings {
        NameSettings {
            exposed: expose.unwrap_or(self.expose_new),
            read_only: false,
        }
    }

    /// The settings of `name` (folded), the defaults when it has none.
    pub(crate) fn of(&self, name: &str) -> NameSettings {
        self.databases
            .get(&crate::names::fold(name))
            .copied()
            .unwrap_or_default()
    }

    /// Whether a request naming `requested` that resolves to the database `real` is offered.
    pub(crate) fn exposed(&self, requested: &str, real: &str) -> bool {
        self.of(requested).exposed && self.of(real).exposed
    }

    /// Whether such a request is served read-only by this switch.
    pub(crate) fn read_only(&self, requested: &str, real: &str) -> bool {
        self.of(requested).read_only || self.of(real).read_only
    }

    /// The canonical text of the file.
    pub(crate) fn text(&self) -> Result<String, String> {
        let mut text = String::from(
            "# MCP settings (written by graphersal-server: the web UI's MCP tab, the admin API,\n\
             # `graphersal-server mcp ...`). enabled = false: the MCP endpoints answer 503.\n\
             # expose_new: whether a new database or alias is offered to MCP (each new name\n\
             # gets its own entry below; a name without an entry is offered).\n\
             # [databases.<name>] exposed = false hides a database or alias from MCP;\n\
             # read_only = true serves it read-only to MCP whatever the caller's grant.\n",
        );
        text.push_str(&toml::to_string(self).map_err(|err| err.to_string())?);
        Ok(text)
    }

    /// The revision of these settings ([`crate::revision`], of the canonical text).
    pub(crate) fn revision(&self) -> String {
        crate::revision::of(self.text().unwrap_or_default().as_bytes())
    }

    /// Parses `mcp.toml`'s text (`path` names the file in errors); folds the names.
    pub(crate) fn parse(text: &str, path: &std::path::Path) -> Result<Self, ServerError> {
        let mut settings: McpSettings =
            toml::from_str(text).map_err(|err| ServerError::Config {
                path: path.to_path_buf(),
                message: crate::config::toml_error_message(&err, text),
            })?;
        settings.databases = std::mem::take(&mut settings.databases)
            .into_iter()
            .map(|(name, value)| (crate::names::fold(&name), value))
            .filter(|(_, value)| *value != NameSettings::default())
            .collect();
        Ok(settings)
    }
}

/// The MCP switches of a server directory ([`McpSettings`] in `mcp.toml`, module docs) and the
/// usage statistics. Online ONE per server (`Loader::mcp_control`); offline the console opens
/// one per command ([`McpControl::open`]).
#[derive(Debug)]
pub(crate) struct McpControl {
    path: PathBuf,
    settings: RwLock<McpSettings>,
    usage: Arc<McpUsage>,
}

impl McpControl {
    /// The switches of `dir` as `mcp.toml` holds them now (missing = the defaults).
    ///
    /// `Err`: [`ServerError::Config`] for a file that does not parse, [`ServerError::Io`].
    pub(crate) fn open(dir: &ServerDir) -> Result<Self, ServerError> {
        let path = dir.mcp_settings_path();
        let settings = match std::fs::read_to_string(&path) {
            Ok(text) => McpSettings::parse(&text, &path)?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => McpSettings::default(),
            Err(err) => return Err(ServerError::io("read", &path, err)),
        };
        Ok(Self {
            path,
            settings: RwLock::new(settings),
            usage: Arc::new(McpUsage::new()),
        })
    }

    /// The usage statistics.
    pub(crate) fn usage(&self) -> &Arc<McpUsage> {
        &self.usage
    }

    /// The settings now.
    pub(crate) fn settings(&self) -> McpSettings {
        self.read().clone()
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, McpSettings> {
        self.settings
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The settings' read lock (a leaf): no change writes `mcp.toml` while it is held (the
    /// server backup copies the file under it).
    pub(crate) fn hold(&self) -> std::sync::RwLockReadGuard<'_, McpSettings> {
        self.read()
    }

    /// Whether the MCP endpoints answer.
    pub(crate) fn enabled(&self) -> bool {
        self.read().enabled
    }

    /// [`McpSettings::exposed`] now.
    pub(crate) fn exposed(&self, requested: &str, real: &str) -> bool {
        self.read().exposed(requested, real)
    }

    /// [`McpSettings::read_only`] now.
    pub(crate) fn read_only(&self, requested: &str, real: &str) -> bool {
        self.read().read_only(requested, real)
    }

    /// Whether `name` has settings of its own.
    pub(crate) fn has_own(&self, name: &str) -> bool {
        self.read()
            .databases
            .contains_key(&crate::names::fold(name))
    }

    /// Changes the settings with `change` under the write lock, based on `expected` (a stale
    /// revision is [`ServerError::Conflict`], nothing changed): the file is written first, the
    /// memory follows. Answers the new settings.
    pub(crate) fn change(
        &self,
        expected: Option<&str>,
        change: impl FnOnce(&mut McpSettings) -> Result<(), ServerError>,
    ) -> Result<McpSettings, ServerError> {
        let mut current = self
            .settings
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        crate::revision::check(expected, &current.revision(), "the MCP settings")?;
        let mut next = current.clone();
        change(&mut next)?;
        next.databases
            .retain(|_, value| *value != NameSettings::default());
        if next != *current {
            let text = next.text().map_err(|message| ServerError::Config {
                path: self.path.clone(),
                message,
            })?;
            write_atomic(&self.path, text.as_bytes())?;
            *current = next.clone();
        }
        Ok(next)
    }

    /// Removes the settings of `name` (a deleted database, a removed alias): whether it had
    /// any. A failure only warns (the settings stay, listed as `missing`).
    pub(crate) fn forget_name(&self, name: &str) -> bool {
        let name = crate::names::fold(name);
        if !self.has_own(&name) {
            return false;
        }
        match self.change(None, |settings| {
            settings.databases.remove(&name);
            Ok(())
        }) {
            Ok(_) => true,
            Err(err) => {
                tracing::warn!(target: "graphersal_server", "MCP settings of '{name}': {err}");
                false
            }
        }
    }

    /// Gives the name `name`, which the server is creating, its settings
    /// ([`McpSettings::new_name`]; replaces any left on it): the settings given.
    ///
    /// `Err`: [`ServerError::Io`] when `mcp.toml` cannot be written (the creation is refused:
    /// the name would be offered to MCP otherwise).
    pub(crate) fn claim_new(
        &self,
        name: &str,
        expose: Option<bool>,
    ) -> Result<NameSettings, ServerError> {
        let key = crate::names::fold(name);
        let mut given = NameSettings::default();
        self.change(None, |settings| {
            given = settings.new_name(expose);
            settings.databases.insert(key.clone(), given);
            Ok(())
        })?;
        Ok(given)
    }

    /// Moves the settings of `old` to `new` (a renamed alias). A failure only warns.
    pub(crate) fn rename(&self, old: &str, new: &str) {
        let (old, new) = (crate::names::fold(old), crate::names::fold(new));
        if !self.has_own(&old) {
            return;
        }
        if let Err(err) = self.change(None, |settings| {
            if let Some(value) = settings.databases.remove(&old) {
                settings.databases.insert(new.clone(), value);
            }
            Ok(())
        }) {
            tracing::warn!(target: "graphersal_server", "MCP settings of '{old}': {err}");
        }
    }
}

/// One counter pair.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Counter {
    calls: u64,
    errors: u64,
}

impl Counter {
    fn add(&mut self, error: bool) {
        self.calls += 1;
        if error {
            self.errors += 1;
        }
    }
}

/// Counts `error` for `name` in `map`, under [`OTHER`] beyond [`MAX_NAMES`] names.
fn count(map: &mut BTreeMap<String, Counter>, name: &str, error: bool) {
    if let Some(counter) = map.get_mut(name) {
        counter.add(error);
        return;
    }
    let key = if map.len() >= MAX_NAMES { OTHER } else { name };
    map.entry(key.to_string()).or_default().add(error);
}

/// One open MCP session as the statistics see it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenSession {
    id_sha256: String,
    identity: Option<String>,
    selected: Option<String>,
}

/// One recent tool error.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RecentError {
    at_ms: u64,
    tool: String,
    user: String,
    database: Option<String>,
    message: String,
}

/// The counters behind [`McpUsage`].
#[derive(Debug, Default)]
struct UsageState {
    sessions: Vec<OpenSession>,
    /// The session (SHA-256 of its id) opened on a per-database endpoint -> that name.
    mounts: HashMap<String, String>,
    calls: Counter,
    refused: u64,
    tools: BTreeMap<String, Counter>,
    users: BTreeMap<String, Counter>,
    databases: BTreeMap<String, Counter>,
    recent: VecDeque<RecentError>,
    /// The session ids of the sessions opened since the start (SHA-256 -> id): what the
    /// per-identity quota ends a session by ([`crate::mcp::make_room`]).
    plain: PlainIds,
}

/// Session ids in clear text, kept in memory only and never shown (`Debug` names the count).
#[derive(Default)]
struct PlainIds(HashMap<String, String>);

impl std::fmt::Debug for PlainIds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PlainIds({} ids)", self.0.len())
    }
}

/// The usage of the MCP endpoints since the server started (module docs), in memory only.
#[derive(Debug)]
pub(crate) struct McpUsage {
    started_at_ms: u64,
    state: Mutex<UsageState>,
}

/// One `tools/call` seen by the router.
pub(crate) struct ToolCall<'a> {
    /// The caller's identity (`None`: anonymous).
    pub(crate) identity: Option<&'a str>,
    /// The database or alias the call worked on, as named, if known.
    pub(crate) database: Option<String>,
    /// The tool's name.
    pub(crate) tool: &'a str,
    /// The error, `None` when the call succeeded.
    pub(crate) error: Option<String>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The user word of an identity: `user:bob` -> `bob`, none -> `anonymous`, else itself.
fn user_word(identity: Option<&str>) -> String {
    match identity {
        None => "anonymous".to_string(),
        Some(id) => id.strip_prefix("user:").unwrap_or(id).to_string(),
    }
}

/// `text` cut to `max` characters (with an ellipsis when cut).
fn cut(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max).collect();
    cut.push('…');
    cut
}

/// The SHA-256 of `text` as 64 lowercase hex digits (an MCP session id as the table keeps it).
pub(crate) fn sha256_hex(text: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, text.as_bytes())
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl McpUsage {
    pub(crate) fn new() -> Self {
        Self {
            started_at_ms: now_ms(),
            state: Mutex::new(UsageState::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, UsageState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The endpoint saved its session table (every change): the open sessions now.
    pub(crate) fn sessions_saved(&self, sessions: &[StoredSession]) {
        let open: Vec<OpenSession> = sessions
            .iter()
            .map(|s| OpenSession {
                id_sha256: s.id_sha256().to_string(),
                identity: s.identity().map(str::to_string),
                selected: s.selected().map(crate::names::fold),
            })
            .collect();
        let mut state = self.lock();
        let keep: std::collections::HashSet<&str> =
            open.iter().map(|s| s.id_sha256.as_str()).collect();
        state.mounts.retain(|id, _| keep.contains(id.as_str()));
        state.plain.0.retain(|id, _| keep.contains(id.as_str()));
        state.sessions = open;
    }

    /// The session `session_id` was just opened (its id is kept in memory, so the per-identity
    /// quota can end it later: [`crate::mcp::make_room`]).
    pub(crate) fn session_opened(&self, session_id: &str) {
        let id = sha256_hex(session_id);
        let mut state = self.lock();
        if state.sessions.iter().any(|s| s.id_sha256 == id) {
            state.plain.0.insert(id, session_id.to_string());
        }
    }

    /// The open sessions of `identity` (`None`: anonymous), oldest first: each one's id when
    /// it was opened since the start ([`Self::session_opened`]), `None` for one restored from
    /// `sessions/mcp.json`.
    pub(crate) fn sessions_of(&self, identity: Option<&str>) -> Vec<Option<String>> {
        let state = self.lock();
        state
            .sessions
            .iter()
            .filter(|s| s.identity.as_deref() == identity)
            .map(|s| state.plain.0.get(&s.id_sha256).cloned())
            .collect()
    }

    /// A session `session_id` was opened on the per-database endpoint of `name`.
    pub(crate) fn mount_session(&self, session_id: &str, name: &str) {
        let id = sha256_hex(session_id);
        let mut state = self.lock();
        if state.sessions.iter().any(|s| s.id_sha256 == id) {
            state.mounts.insert(id, crate::names::fold(name));
        }
    }

    /// The name the session `session_id` selected (`/mcp`, `use_graph`), if any.
    pub(crate) fn selected(&self, session_id: &str) -> Option<String> {
        let id = sha256_hex(session_id);
        let state = self.lock();
        state
            .sessions
            .iter()
            .find(|s| s.id_sha256 == id)
            .and_then(|s| s.selected.clone())
    }

    /// A request refused because MCP is off (globally or for its database).
    pub(crate) fn refused(&self) {
        self.lock().refused += 1;
    }

    /// Counts one tool call.
    pub(crate) fn record(&self, call: ToolCall<'_>) {
        let user = user_word(call.identity);
        let tool = cut(call.tool, 64);
        let error = call.error.is_some();
        let mut state = self.lock();
        state.calls.add(error);
        count(&mut state.tools, &tool, error);
        count(&mut state.users, &user, error);
        if let Some(database) = &call.database {
            count(&mut state.databases, database, error);
        }
        if let Some(message) = call.error {
            if state.recent.len() >= RECENT_ERRORS {
                state.recent.pop_front();
            }
            state.recent.push_back(RecentError {
                at_ms: now_ms(),
                tool,
                user,
                database: call.database,
                message: cut(&message, MAX_MESSAGE_CHARS),
            });
        }
    }

    /// The open sessions per name (the selection on `/mcp`, the name of a per-database
    /// endpoint) and the calls per name: `(sessions, calls, errors)` of `name`.
    pub(crate) fn of_name(&self, name: &str) -> (u64, u64, u64) {
        let name = crate::names::fold(name);
        let state = self.lock();
        let sessions = state
            .sessions
            .iter()
            .filter(|s| session_name(&state, s).as_deref() == Some(name.as_str()))
            .count() as u64;
        let counter = state.databases.get(&name).copied().unwrap_or_default();
        (sessions, counter.calls, counter.errors)
    }

    /// The statistics as JSON (`GET /api/admin/mcp` `usage`; book/src/reference/admin_api.md).
    pub(crate) fn to_json(&self) -> Value {
        let state = self.lock();
        let mut identities: BTreeMap<String, u64> = BTreeMap::new();
        let mut names: BTreeMap<String, u64> = BTreeMap::new();
        let mut unselected = 0u64;
        for session in &state.sessions {
            *identities
                .entry(user_word(session.identity.as_deref()))
                .or_default() += 1;
            match session_name(&state, session) {
                Some(name) => *names.entry(name).or_default() += 1,
                None => unselected += 1,
            }
        }
        let counters = |map: &BTreeMap<String, Counter>, key: &str| -> Vec<Value> {
            let mut rows: Vec<(&String, &Counter)> = map.iter().collect();
            rows.sort_by(|a, b| b.1.calls.cmp(&a.1.calls).then(a.0.cmp(b.0)));
            rows.into_iter()
                .map(|(name, c)| json!({ key: name, "calls": c.calls, "errors": c.errors }))
                .collect()
        };
        let sessions_of = |map: BTreeMap<String, u64>, key: &str| -> Vec<Value> {
            map.into_iter()
                .map(|(name, n)| json!({ key: name, "sessions": n }))
                .collect()
        };
        json!({
            "startedAtMs": self.started_at_ms,
            "note": "since the server started; kept in memory only, reset by a restart",
            "sessions": {
                "open": state.sessions.len(),
                "byUser": sessions_of(identities, "user"),
                "byDatabase": sessions_of(names, "database"),
                "withoutDatabase": unselected,
            },
            "calls": {
                "total": state.calls.calls,
                "errors": state.calls.errors,
                "refused": state.refused,
            },
            "tools": counters(&state.tools, "tool"),
            "users": counters(&state.users, "user"),
            "databases": counters(&state.databases, "database"),
            "recentErrors": state.recent.iter().rev().map(|e| json!({
                "atMs": e.at_ms,
                "tool": e.tool,
                "user": e.user,
                "database": e.database,
                "message": e.message,
            })).collect::<Vec<_>>(),
            "recentErrorsMax": RECENT_ERRORS,
        })
    }

    /// Counts the tool calls of one MCP request: `body` the JSON-RPC message(s) the client
    /// sent, `status`/`answer` the endpoint's answer; `mount` the database of a per-database
    /// endpoint (`None`: `/mcp`, where a call works on its `graph` argument, the name
    /// `use_graph` selects, or the session's selection); `session` the request's
    /// `Mcp-Session-Id`. An `initialize` on a per-database endpoint records the new session's
    /// database.
    pub(crate) fn observe(&self, request: &ObservedRequest<'_>, status: u16, answer: &[u8]) {
        let Ok(sent) = serde_json::from_slice::<Value>(request.body) else {
            return;
        };
        let messages: Vec<&Value> = match &sent {
            Value::Array(items) => items.iter().collect(),
            other => vec![other],
        };
        let answered: Vec<Value> = match serde_json::from_slice::<Value>(answer) {
            Ok(Value::Array(items)) => items,
            Ok(other) => vec![other],
            Err(_) => Vec::new(),
        };
        for message in messages {
            let method = message.get("method").and_then(Value::as_str);
            if method == Some("initialize") {
                if let (Some(mount), Some(id)) = (request.mount, request.new_session) {
                    self.mount_session(id, mount);
                }
                continue;
            }
            if method != Some("tools/call") {
                continue;
            }
            let params = message.get("params");
            let tool = params
                .and_then(|p| p.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("(unknown)");
            let arguments = params.and_then(|p| p.get("arguments"));
            let named = |key: &str| {
                arguments
                    .and_then(|a| a.get(key))
                    .and_then(Value::as_str)
                    .map(crate::names::fold)
            };
            let database = match request.mount {
                Some(mount) => Some(crate::names::fold(mount)),
                None => named("graph")
                    .or_else(|| (tool == "use_graph").then(|| named("name")).flatten())
                    .or_else(|| request.session.and_then(|id| self.selected(id))),
            };
            let id = message.get("id");
            let reply = answered
                .iter()
                .find(|a| id.is_some() && a.get("id") == id)
                .or_else(|| (answered.len() == 1).then(|| &answered[0]));
            let error = call_error(status, reply);
            self.record(ToolCall {
                identity: request.identity,
                database,
                tool,
                error,
            });
        }
    }
}

/// What [`McpUsage::observe`] needs of a request.
pub(crate) struct ObservedRequest<'a> {
    /// The caller's identity.
    pub(crate) identity: Option<&'a str>,
    /// The database of a per-database endpoint (`None`: `/mcp`).
    pub(crate) mount: Option<&'a str>,
    /// The request's `Mcp-Session-Id`.
    pub(crate) session: Option<&'a str>,
    /// The answer's `Mcp-Session-Id` (a new session).
    pub(crate) new_session: Option<&'a str>,
    /// The body sent.
    pub(crate) body: &'a [u8],
}

/// The error of one tool call's answer, `None` when it succeeded: a JSON-RPC `error`, a
/// result with `isError` (its first text), or an HTTP status other than 200.
fn call_error(status: u16, reply: Option<&Value>) -> Option<String> {
    if let Some(reply) = reply {
        if let Some(error) = reply.get("error") {
            return Some(
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("JSON-RPC error")
                    .to_string(),
            );
        }
        if let Some(result) = reply.get("result") {
            if result.get("isError").and_then(Value::as_bool) == Some(true) {
                let text = result
                    .get("content")
                    .and_then(Value::as_array)
                    .and_then(|items| items.iter().find_map(|i| i.get("text")?.as_str()))
                    .unwrap_or("tool error");
                return Some(text.to_string());
            }
            return None;
        }
        if status != 200 {
            let message = reply
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("refused");
            return Some(format!("HTTP {status}: {message}"));
        }
        return None;
    }
    (status != 200).then(|| format!("HTTP {status}"))
}

/// The name a session works on: its selection, else the per-database endpoint it was opened on.
fn session_name(state: &UsageState, session: &OpenSession) -> Option<String> {
    session
        .selected
        .clone()
        .or_else(|| state.mounts.get(&session.id_sha256).cloned())
}

/// The endpoint's session store: `sessions/mcp.json` (or nothing, when it cannot be read: the
/// sessions then live in memory only), each saved table also handed to [`McpUsage`] (the open
/// sessions of the statistics).
pub(crate) struct ObservedSessions {
    file: Option<McpSessionFile>,
    usage: Arc<McpUsage>,
}

impl ObservedSessions {
    /// The store of `file` (`None`: in memory only) reporting to `usage`.
    pub(crate) fn new(file: Option<McpSessionFile>, usage: Arc<McpUsage>) -> Self {
        Self { file, usage }
    }
}

impl SessionStore for ObservedSessions {
    fn load(&self) -> Result<Vec<StoredSession>, McpError> {
        let sessions = match &self.file {
            Some(file) => file.load()?,
            None => Vec::new(),
        };
        self.usage.sessions_saved(&sessions);
        Ok(sessions)
    }

    fn save(&self, sessions: &[StoredSession]) -> Result<(), McpError> {
        self.usage.sessions_saved(sessions);
        match &self.file {
            Some(file) => file.save(sessions),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    fn dir(temp: &TempDir) -> ServerDir {
        crate::registry::tests::server(temp)
    }

    #[test]
    fn settings_default_round_trip_and_fold_names() {
        let temp = TempDir::new("mcp-settings");
        let dir = dir(&temp);
        let control = McpControl::open(&dir).unwrap();
        // `init` wrote MCP off (round 4 B-MCPDEF).
        assert!(!control.enabled());
        assert!(control.exposed("a", "a"));
        assert!(!control.read_only("a", "a"));
        let revision = control.settings().revision();
        let next = control
            .change(Some(&revision), |s| {
                s.enabled = false;
                s.databases.insert(
                    "acme".into(),
                    NameSettings {
                        exposed: false,
                        read_only: true,
                    },
                );
                s.databases.insert("plain".into(), NameSettings::default());
                Ok(())
            })
            .unwrap();
        assert!(
            !next.databases.contains_key("plain"),
            "defaults are not kept"
        );
        // A stale revision changes nothing.
        let err = control.change(Some(&revision), |s| {
            s.enabled = true;
            Ok(())
        });
        assert!(matches!(err, Err(ServerError::Conflict { .. })), "{err:?}");
        assert!(!control.enabled());
        let reopened = McpControl::open(&dir).unwrap();
        assert_eq!(reopened.settings(), next);
        assert!(!reopened.exposed("ACME", "acme"));
        // An alias narrows its database, never widens it.
        assert!(!reopened.exposed("prod", "acme"));
        assert!(reopened.read_only("prod", "acme"));
        assert!(reopened.exposed("prod", "other"));
        let text = std::fs::read_to_string(dir.mcp_settings_path()).unwrap();
        assert!(text.contains("[databases.acme]"), "{text}");
        assert!(text.starts_with("# MCP settings"), "{text}");
        // Hand-written names are folded; a typo is refused.
        std::fs::write(
            dir.mcp_settings_path(),
            "[databases.Big]\nread_only = true\n",
        )
        .unwrap();
        assert!(McpControl::open(&dir).unwrap().read_only("big", "x"));
        std::fs::write(dir.mcp_settings_path(), "enable = true\n").unwrap();
        let err = McpControl::open(&dir).err().unwrap().to_string();
        assert!(err.contains("enable"), "{err}");
    }

    #[test]
    fn a_new_server_starts_off_and_new_names_are_hidden_unless_asked() {
        let temp = TempDir::new("mcp-settings-new");
        let dir = dir(&temp);
        let text = std::fs::read_to_string(dir.mcp_settings_path()).unwrap();
        assert!(
            text.contains("enabled = false\nexpose_new = false\n"),
            "{text}"
        );
        let control = McpControl::open(&dir).unwrap();
        assert_eq!(control.settings(), McpSettings::initial());
        // A name without an entry is offered (older directories); a new one is not.
        assert!(control.exposed("old", "old"));
        assert!(!control.claim_new("New", None).unwrap().exposed);
        assert!(!control.exposed("new", "new"));
        assert!(control.claim_new("shown", Some(true)).unwrap().exposed);
        assert!(!control.has_own("shown"), "an offered name needs no entry");
        control
            .change(None, |s| {
                s.expose_new = true;
                Ok(())
            })
            .unwrap();
        assert!(control.claim_new("later", None).unwrap().exposed);
        assert!(!control.claim_new("hidden", Some(false)).unwrap().exposed);
        // A claim replaces what a gone name left behind.
        control
            .change(None, |s| {
                s.databases.insert(
                    "reused".into(),
                    NameSettings {
                        exposed: false,
                        read_only: true,
                    },
                );
                Ok(())
            })
            .unwrap();
        assert!(!control.claim_new("reused", None).unwrap().read_only);
        assert!(control.exposed("reused", "reused"));
        // A file of round 3 (no expose_new) reads as expose_new = false.
        std::fs::write(dir.mcp_settings_path(), "enabled = true\n").unwrap();
        let settings = McpControl::open(&dir).unwrap().settings();
        assert!(settings.enabled && !settings.expose_new);
    }

    #[test]
    fn names_are_forgotten_and_renamed() {
        let temp = TempDir::new("mcp-settings-names");
        let control = McpControl::open(&dir(&temp)).unwrap();
        control
            .change(None, |s| {
                s.databases.insert(
                    "prod".into(),
                    NameSettings {
                        exposed: true,
                        read_only: true,
                    },
                );
                Ok(())
            })
            .unwrap();
        control.rename("prod", "live");
        assert!(control.read_only("live", "x"));
        assert!(!control.has_own("prod"));
        assert!(control.forget_name("LIVE"));
        assert!(!control.forget_name("live"));
        assert!(control.settings().databases.is_empty());
    }

    fn stored(id: &str, identity: Option<&str>, selected: Option<&str>) -> StoredSession {
        StoredSession::new(
            sha256_hex(id),
            identity.map(str::to_string),
            selected.map(str::to_string),
            None,
            "2025-06-18",
            json!({"name": "t", "version": "1"}),
            1,
        )
    }

    #[test]
    fn usage_counts_sessions_calls_and_errors() {
        let usage = Arc::new(McpUsage::new());
        let store = ObservedSessions::new(None, usage.clone());
        store
            .save(&[
                stored("s1", Some("user:bob"), Some("acme")),
                stored("s2", Some("user:bob"), None),
                stored("s3", None, None),
            ])
            .unwrap();
        usage.mount_session("s2", "Other");
        usage.mount_session("unknown", "x");
        let call = |session: Option<&str>, mount: Option<&str>, body: Value, answer: Value| {
            let body = body.to_string();
            let request = ObservedRequest {
                identity: Some("user:bob"),
                mount,
                session,
                new_session: None,
                body: body.as_bytes(),
            };
            usage.observe(&request, 200, answer.to_string().as_bytes());
        };
        let tool = |id: u64, name: &str, args: Value| {
            json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
                   "params": {"name": name, "arguments": args}})
        };
        let ok = |id: u64| json!({"jsonrpc": "2.0", "id": id, "result": {"content": [], "isError": false}});
        call(Some("s1"), None, tool(1, "gql", json!({})), ok(1));
        call(
            Some("s1"),
            None,
            tool(2, "gql", json!({"graph": "Beta"})),
            json!({
                "jsonrpc": "2.0", "id": 2,
                "result": {"isError": true, "content": [{"type": "text", "text": "boom"}]},
            }),
        );
        call(
            Some("s2"),
            Some("other"),
            tool(3, "query", json!({})),
            json!({
                "jsonrpc": "2.0", "id": 3, "error": {"code": -32602, "message": "bad"},
            }),
        );
        call(
            None,
            None,
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!(null),
        );
        usage.refused();
        assert_eq!(usage.of_name("acme"), (1, 1, 0));
        assert_eq!(usage.of_name("beta"), (0, 1, 1));
        assert_eq!(usage.of_name("other"), (1, 1, 1));
        let json = usage.to_json();
        assert_eq!(json["sessions"]["open"], 3);
        assert_eq!(json["sessions"]["withoutDatabase"], 1);
        assert_eq!(
            json["sessions"]["byUser"],
            json!([
                {"user": "anonymous", "sessions": 1}, {"user": "bob", "sessions": 2},
            ])
        );
        assert_eq!(
            json["calls"],
            json!({"total": 3, "errors": 2, "refused": 1})
        );
        assert_eq!(
            json["tools"][0],
            json!({"tool": "gql", "calls": 2, "errors": 1})
        );
        assert_eq!(
            json["users"],
            json!([{"user": "bob", "calls": 3, "errors": 2}])
        );
        let recent = json["recentErrors"].as_array().unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0]["message"], "bad");
        assert_eq!(recent[0]["database"], "other");
        assert_eq!(recent[1]["message"], "boom");
        assert_eq!(recent[1]["tool"], "gql");
        // A session ended: its count and its endpoint go.
        store
            .save(&[stored("s1", Some("user:bob"), Some("acme"))])
            .unwrap();
        assert_eq!(usage.of_name("other").0, 0);
        assert_eq!(usage.to_json()["sessions"]["open"], 1);
    }

    #[test]
    fn recent_errors_and_names_are_bounded() {
        let usage = McpUsage::new();
        for i in 0..(RECENT_ERRORS + 5) {
            usage.record(ToolCall {
                identity: None,
                database: None,
                tool: &format!("t{i}"),
                error: Some("x".repeat(1000)),
            });
        }
        let json = usage.to_json();
        let recent = json["recentErrors"].as_array().unwrap();
        assert_eq!(recent.len(), RECENT_ERRORS);
        assert_eq!(recent[0]["tool"], format!("t{}", RECENT_ERRORS + 4));
        assert!(recent[0]["message"].as_str().unwrap().chars().count() <= MAX_MESSAGE_CHARS + 1);
        for i in 0..(MAX_NAMES + 10) {
            usage.record(ToolCall {
                identity: None,
                database: Some(format!("db{i}")),
                tool: "gql",
                error: None,
            });
        }
        let json = usage.to_json();
        assert!(json["databases"].as_array().unwrap().len() <= MAX_NAMES + 1);
        assert!(
            json["databases"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["database"] == OTHER)
        );
    }
}
