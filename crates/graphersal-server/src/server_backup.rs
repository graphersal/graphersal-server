//! The backup of a whole server directory (round 3 B-CONF): its configuration, and with
//! `--with-databases` every database's data, in ONE ZIP archive; and its restore into a NEW
//! server directory (`graphersal-server backup` / `restore`, `GET /api/admin/server-backup`).
//!
//! The archive (a ZIP of stored entries with zip64 records, written and read by the
//! crate's own minimal ZIP code) holds, in this order:
//!
//! - `server.toml`, `aliases.toml`, `access.toml` (users with their token HASHES and grants: the
//!   archive is a secret), `mcp.toml`, each when the directory has it;
//! - `databases/<name>/database.toml` of every database (its settings: flags, description,
//!   limits, role and base; not its data);
//! - `models.json`: the shared models (repository, identity, licence, blob hash and size, the
//!   databases using them), always;
//! - with databases: `databases/<name>/backup.zip` (a fresh VERIFIED backup of the database
//!   through the per-database backup path, [`crate::ops::Admin::backup`]'s first half: its Store,
//!   with the catalog that holds its saved queries and backup job definitions, and its model
//!   blobs) + `databases/<name>/backup.toml` (the backup's sidecar), and
//!   `models/blobs/<2 hex>/<sha256>` (every shared model blob);
//! - `manifest.json` LAST ([`Manifest`]): the format and its version, the server version, the
//!   time, the source directory's NAME (never its path), every other entry with its length and
//!   SHA-256, the databases (included or not, and why) and what was left out and why.
//!
//! Left out by design: `sessions/` (restoring them would revive logins), `logs/`,
//! `server.lock`, `server.run` (a run's operator token), `server.open` and `recovery.json` (the
//! state of a run, not configuration), `tmp/`, `trash/`, `imports/`, `backups/` (the
//! per-database backups; the job backup root too), `models/models.toml` (derived, rebuilt).
//!
//! A running server copies the configuration files under their writers' locks (the registry's
//! read lock for `aliases.toml` and every `database.toml`, then the access file's lock, then the
//! MCP settings' lock: the documented lock order), so the archive is one consistent state; the
//! console on a stopped directory holds the server lock.
//!
//! [`restore`] builds a NEW server directory from an archive: a missing or empty directory only
//! (never over a live one); every entry's CRC-32 and SHA-256 against the manifest, the format
//! version and the server version checked first (a newer one refused unless
//! [`RestoreOptions::with_force_version`]), nothing written before; then the layout with
//! owner-only modes (every directory of the layout `0700`, every file the restore writes
//! `0600`), the databases from their embedded backups (bases and plain
//! ones before the layers over them, each through the ordinary restore of a backup, so a
//! database is registered whole or not at all), their `database.toml`, and last `aliases.toml`,
//! `mcp.toml` and `access.toml`. A database the archive holds no backup of (a configuration-only
//! archive, or one whose backup failed) is SKIPPED and reported: no half-registered database.
//! Any failure removes everything the restore wrote.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::archive::ZipReader;
use crate::database::{DATABASE_FILE, DatabaseConfig, DatabaseKind};
use crate::error::ServerError;
use crate::layout::{
    ACCESS_FILE, ALIASES_FILE, CONFIG_FILE, MCP_SETTINGS_FILE, ServerDir, write_atomic,
    write_atomic_private,
};
use crate::names::{fold, validate_name};

/// The manifest's `format`.
pub const FORMAT: &str = "graphersal-server-backup";
/// The archive format version this server writes (and the newest it restores without
/// [`RestoreOptions::with_force_version`]).
pub const FORMAT_VERSION: u32 = 1;
/// The manifest's entry name (the archive's last entry).
pub const MANIFEST_FILE: &str = "manifest.json";
/// The models list's entry name.
pub const MODELS_FILE: &str = "models.json";
/// A database's embedded backup: `databases/<name>/backup.zip`.
pub(crate) const DB_BACKUP: &str = "backup.zip";
/// Its sidecar: `databases/<name>/backup.toml`.
pub(crate) const DB_SIDECAR: &str = "backup.toml";
/// The name the restore gives an embedded backup in the new directory's `backups/<name>/`
/// while it restores it (removed afterwards).
const RESTORE_STAMP: &str = "from-server-backup";
/// The largest entry the restore keeps in memory (configuration files, the manifest).
const MAX_SMALL_ENTRY: u64 = 64 << 20;

/// The configuration files of the server directory's root, in archive order.
pub(crate) const ROOT_FILES: &[&str] = &[CONFIG_FILE, ALIASES_FILE, ACCESS_FILE, MCP_SETTINGS_FILE];

/// What the archive leaves out, with the reason (only the paths the directory has are listed).
pub(crate) const EXCLUDED: &[(&str, &str)] = &[
    (
        "sessions/",
        "browser and MCP sessions: restoring them would revive logins (users log in again with \
         their tokens)",
    ),
    ("logs/", "the server's log, not configuration"),
    ("server.lock", "the lock of a running server"),
    (
        "server.run",
        "the address and operator token of one run (a new one at every start)",
    ),
    (
        "server.open",
        "the databases a run had open (state of a run, not configuration)",
    ),
    (
        "recovery.json",
        "the last unclean shutdown's report (state of a run, not configuration)",
    ),
    ("tmp/", "builds and uploads in progress"),
    ("trash/", "deleted and replaced databases"),
    ("imports/", "the import inbox: files waiting to be imported"),
    (
        "backups/",
        "the per-database backups and the backup jobs' root (back them up on their own)",
    ),
    (
        "models/models.toml",
        "a derived index, rebuilt by the server from models/blobs/",
    ),
];

/// The manifest of a server backup (`manifest.json`, the archive's last entry; module docs).
///
/// `#[non_exhaustive]`: further fields may be added (readers ignore unknown ones).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Manifest {
    /// Always [`FORMAT`].
    pub format: String,
    /// The archive format's version ([`FORMAT_VERSION`] when written by this server).
    pub format_version: u32,
    /// The version of the server that wrote it.
    pub server_version: String,
    /// When it was written (RFC 3339 UTC).
    pub created_at: String,
    /// The source server directory's NAME (its last path component; never the path).
    pub source_dir: String,
    /// Whether the databases' data is included.
    pub with_databases: bool,
    /// Every other entry of the archive with its length and SHA-256.
    pub files: Vec<ManifestFile>,
    /// Every database of the source directory.
    #[serde(default)]
    pub databases: Vec<ManifestDatabase>,
    /// What was left out and why.
    #[serde(default)]
    pub excluded: Vec<Excluded>,
}

/// One entry of the archive in the [`Manifest`].
///
/// `#[non_exhaustive]`: further fields may be added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ManifestFile {
    /// The entry's name (`/` separators, relative to the server directory's root).
    pub path: String,
    /// Its length in bytes.
    pub bytes: u64,
    /// Its SHA-256, 64 lower-case hex digits.
    pub sha256: String,
}

/// One database of the source directory in the [`Manifest`].
///
/// `#[non_exhaustive]`: further fields may be added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ManifestDatabase {
    /// Its name.
    pub name: String,
    /// `plain` or `layer` (empty for a database without a readable `database.toml`).
    #[serde(default)]
    pub kind: String,
    /// `normal` or `base`.
    #[serde(default)]
    pub role: String,
    /// A layer's base database.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// Its state when the backup was taken (`loaded`, `unloaded`, `failed`, ...).
    #[serde(default)]
    pub state: String,
    /// Whether its data (`databases/<name>/backup.zip`) is in the archive.
    pub included: bool,
    /// Why not, when it is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Something the archive leaves out ([`Manifest::excluded`]).
///
/// `#[non_exhaustive]`: further fields may be added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Excluded {
    /// The path, relative to the server directory (a directory ends in `/`).
    pub path: String,
    /// Why it is left out.
    pub reason: String,
}

impl Excluded {
    pub(crate) fn new(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            reason: reason.into(),
        }
    }
}

/// What a server backup wrote ([`crate::ops::Admin::server_backup_to_file`], the admin API's
/// download).
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ServerBackedUp {
    /// Its manifest.
    pub manifest: Manifest,
    /// The archive's size in bytes.
    pub bytes: u64,
    /// The file name a download suggests: `<dir name>-server-backup-<UTC time>.zip`.
    pub file_name: String,
}

/// How [`restore`] runs.
///
/// `#[non_exhaustive]`: build it with [`RestoreOptions::new`] and the `with_*` setters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RestoreOptions {
    /// Restore an archive of a newer format version or server version anyway (its files must
    /// still read: an unknown key is refused as at a start; unknown entries are left out with a
    /// note).
    pub force_version: bool,
}

impl RestoreOptions {
    /// The defaults: a newer archive is refused.
    pub fn new() -> Self {
        Self::default()
    }

    /// The options with `force_version`.
    pub fn with_force_version(mut self, force: bool) -> Self {
        self.force_version = force;
        self
    }
}

/// What [`restore`] did.
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RestoreReport {
    /// The new server directory.
    pub dir: PathBuf,
    /// The archive's manifest.
    pub manifest: Manifest,
    /// The configuration files written (relative paths).
    pub files: Vec<String>,
    /// The databases restored from their embedded backups.
    pub databases: Vec<String>,
    /// The databases not restored, with the reason.
    pub skipped: Vec<(String, String)>,
    /// How many shared model blobs were written into `models/blobs/`.
    pub model_blobs: usize,
    /// Things to know (one sentence each).
    pub notes: Vec<String>,
}

/// The name of a server directory for the manifest and the file name: its last component.
pub(crate) fn dir_name(dir: &ServerDir) -> String {
    let root = fs::canonicalize(dir.root()).unwrap_or_else(|_| dir.root().to_path_buf());
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "server".to_string())
}

/// The file name of a backup of the directory named `name` at the RFC 3339 time `created`.
pub(crate) fn file_name(name: &str, created: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{safe}-server-backup-{}.zip", created.replace(':', "-"))
}

/// What an entry name of an archive is ([`restore`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum EntryKind {
    Root,
    Manifest,
    Models,
    DatabaseConfig(String),
    DatabaseBackup(String),
    DatabaseSidecar(String),
    Blob(String),
}

impl EntryKind {
    fn of(name: &str) -> Option<EntryKind> {
        if ROOT_FILES.contains(&name) {
            return Some(EntryKind::Root);
        }
        if name == MANIFEST_FILE {
            return Some(EntryKind::Manifest);
        }
        if name == MODELS_FILE {
            return Some(EntryKind::Models);
        }
        let parts: Vec<&str> = name.split('/').collect();
        match parts.as_slice() {
            ["databases", db, file] if validate_name(db).is_ok() && fold(db) == *db => {
                let db = db.to_string();
                match *file {
                    DATABASE_FILE => Some(EntryKind::DatabaseConfig(db)),
                    DB_BACKUP => Some(EntryKind::DatabaseBackup(db)),
                    DB_SIDECAR => Some(EntryKind::DatabaseSidecar(db)),
                    _ => None,
                }
            }
            ["models", "blobs", prefix, hash]
                if hash.len() == 64
                    && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                    && hash.starts_with(prefix)
                    && prefix.len() == 2 =>
            {
                Some(EntryKind::Blob(hash.to_string()))
            }
            _ => None,
        }
    }

    /// Whether the restore keeps the entry's bytes in memory (everything but the data).
    fn small(&self) -> bool {
        !matches!(self, EntryKind::DatabaseBackup(_) | EntryKind::Blob(_))
    }
}

fn damaged(path: &Path, message: impl Into<String>) -> ServerError {
    ServerError::BackupArchive {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

/// `a.b.c` (a pre-release suffix ignored) as numbers.
fn version_tuple(text: &str) -> Option<(u64, u64, u64)> {
    let core = text.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((
        parts.next()??,
        parts.next()??,
        parts.next().flatten().unwrap_or(0),
    ))
}

/// One entry seen by the check pass.
struct Seen {
    kind: Option<EntryKind>,
    bytes: u64,
    sha256: String,
    data: Option<Vec<u8>>,
}

/// The check pass: every entry read, its CRC checked, its SHA-256 taken; the small ones kept.
fn read_entries(archive: &Path) -> Result<BTreeMap<String, Seen>, ServerError> {
    let file = File::open(archive).map_err(|err| ServerError::io("read", archive, err))?;
    let mut reader = ZipReader::new(BufReader::new(file));
    let mut seen = BTreeMap::new();
    loop {
        let entry = match reader.next_entry() {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(err) => return Err(damaged(archive, err.to_string())),
        };
        let name = entry.name.clone();
        let bytes = entry.size;
        let kind = EntryKind::of(&name);
        let mut data = None;
        let mut entry = entry;
        if kind.as_ref().is_some_and(EntryKind::small) {
            if bytes > MAX_SMALL_ENTRY {
                return Err(damaged(
                    archive,
                    format!("the entry {name:?} is too large ({bytes} bytes)"),
                ));
            }
            let mut buf = Vec::with_capacity(usize::try_from(bytes).unwrap_or(0));
            entry
                .by_ref()
                .read_to_end(&mut buf)
                .map_err(|err| damaged(archive, err.to_string()))?;
            data = Some(buf);
        }
        let sha256 = entry
            .finish()
            .map_err(|err| damaged(archive, err.to_string()))?;
        seen.insert(
            name,
            Seen {
                kind,
                bytes,
                sha256,
                data,
            },
        );
    }
    Ok(seen)
}

/// The manifest, checked against the entries (module docs). Answers it and the notes about
/// entries left out (only with `force`).
fn check_manifest(
    archive: &Path,
    seen: &mut BTreeMap<String, Seen>,
    options: RestoreOptions,
) -> Result<(Manifest, Vec<String>), ServerError> {
    let text = seen
        .get(MANIFEST_FILE)
        .and_then(|s| s.data.clone())
        .ok_or_else(|| {
            damaged(
                archive,
                "no manifest.json: not a graphersal-server backup (a database's own backup is \
                 restored with `db restore` or imported with `import`)",
            )
        })?;
    let raw: serde_json::Value = serde_json::from_slice(&text)
        .map_err(|err| damaged(archive, format!("manifest.json is not JSON: {err}")))?;
    if raw.get("format").and_then(|v| v.as_str()) != Some(FORMAT) {
        return Err(damaged(
            archive,
            format!("manifest.json: the format is not {FORMAT:?}"),
        ));
    }
    let version = raw
        .get("formatVersion")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| damaged(archive, "manifest.json: no formatVersion"))?;
    if version > u64::from(FORMAT_VERSION) && !options.force_version {
        return Err(ServerError::Refused {
            name: archive.display().to_string(),
            message: format!(
                "the archive has format version {version}, this server reads up to \
                 {FORMAT_VERSION}: restore it with a newer graphersal-server, or try anyway \
                 with --force-version"
            ),
        });
    }
    let manifest: Manifest = serde_json::from_value(raw)
        .map_err(|err| damaged(archive, format!("manifest.json: {err}")))?;
    let ours = env!("CARGO_PKG_VERSION");
    if let (Some(theirs), Some(mine)) =
        (version_tuple(&manifest.server_version), version_tuple(ours))
        && theirs > mine
        && !options.force_version
    {
        return Err(ServerError::Refused {
            name: archive.display().to_string(),
            message: format!(
                "the archive was written by graphersal-server {}, this is {ours}: its files may \
                 hold settings this version does not know; restore it with that version or \
                 newer, or try anyway with --force-version",
                manifest.server_version
            ),
        });
    }
    // Every listed file present with its length and hash; nothing unlisted.
    let mut listed = std::collections::BTreeSet::new();
    for file in &manifest.files {
        if !listed.insert(file.path.as_str()) {
            return Err(damaged(
                archive,
                format!("manifest.json lists {:?} twice", file.path),
            ));
        }
        let Some(entry) = seen.get(&file.path) else {
            return Err(damaged(
                archive,
                format!("{:?} is in the manifest but not in the archive", file.path),
            ));
        };
        if entry.bytes != file.bytes || entry.sha256 != file.sha256 {
            return Err(damaged(
                archive,
                format!(
                    "{:?}: checksum mismatch (SHA-256 {} in the archive, {} in the manifest): \
                     the archive was changed or damaged",
                    file.path, entry.sha256, file.sha256
                ),
            ));
        }
    }
    let mut notes = Vec::new();
    let unlisted: Vec<String> = seen
        .keys()
        .filter(|name| name.as_str() != MANIFEST_FILE && !listed.contains(name.as_str()))
        .cloned()
        .collect();
    if let Some(name) = unlisted.first() {
        return Err(damaged(
            archive,
            format!("{name:?} is in the archive but not in the manifest"),
        ));
    }
    let unknown: Vec<String> = seen
        .iter()
        .filter(|(_, s)| s.kind.is_none())
        .map(|(name, _)| name.clone())
        .collect();
    if !unknown.is_empty() {
        if !options.force_version {
            return Err(damaged(
                archive,
                format!(
                    "unexpected entries {unknown:?} (written by a newer graphersal-server? try \
                     --force-version)"
                ),
            ));
        }
        for name in unknown {
            seen.remove(&name);
            notes.push(format!(
                "left out {name:?}: this version does not know the entry"
            ));
        }
    }
    if !seen.contains_key(CONFIG_FILE) {
        return Err(damaged(archive, "the archive holds no server.toml"));
    }
    Ok((manifest, notes))
}

/// Refuses `dir` unless it is missing or an empty directory of this OS user; answers whether
/// it exists.
fn check_target(dir: &ServerDir) -> Result<bool, ServerError> {
    match fs::metadata(dir.root()) {
        Ok(meta) if meta.is_dir() => {
            let mut entries = fs::read_dir(dir.root())
                .map_err(|err| ServerError::io("read directory", dir.root(), err))?;
            if entries.next().is_some() {
                return Err(ServerError::Refused {
                    name: dir.root().display().to_string(),
                    message: "a restore builds a NEW server directory and never writes over an \
                              existing one (or a running server's): give a new or empty \
                              directory"
                        .to_string(),
                });
            }
            crate::security::check_owner(dir)?;
            Ok(true)
        }
        Ok(_) => Err(ServerError::NotEmpty {
            path: dir.root().to_path_buf(),
        }),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(ServerError::io("read", dir.root(), err)),
    }
}

/// Removes what a failed restore wrote (the directory was missing or empty before).
struct Cleanup {
    root: PathBuf,
    existed: bool,
    armed: bool,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let result = if self.existed {
            fs::read_dir(&self.root).and_then(|entries| {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if entry.file_type().is_ok_and(|t| t.is_dir()) {
                        fs::remove_dir_all(&path)?;
                    } else {
                        fs::remove_file(&path)?;
                    }
                }
                Ok(())
            })
        } else {
            fs::remove_dir_all(&self.root)
        };
        if let Err(err) = result {
            tracing::warn!(
                target: "graphersal_server",
                "the failed restore left files in {}: {err}",
                self.root.display()
            );
        }
    }
}

/// Writes `data` of `entry` to `path` through a temporary name, checking its SHA-256
/// (`expected`) before the rename; mode `0600` on unix.
fn write_entry(
    entry: crate::archive::Entry<'_, impl Read>,
    path: &Path,
    expected: &str,
    archive: &Path,
) -> Result<(), ServerError> {
    let name = entry.name.clone();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| ServerError::io("create directory", parent, err))?;
    }
    let temp = crate::layout::temp_path(path);
    let mut out = crate::layout::create_new_file(&temp, true)
        .map_err(|err| ServerError::io("create", &temp, err))?;
    let guard = crate::ops::TmpFile::at(temp.clone());
    let mut entry = entry;
    io::copy(&mut entry, &mut out).map_err(|err| damaged(archive, err.to_string()))?;
    let sha = entry
        .finish()
        .map_err(|err| damaged(archive, err.to_string()))?;
    if sha != expected {
        return Err(damaged(
            archive,
            format!("{name:?}: checksum mismatch (the archive changed while it was restored)"),
        ));
    }
    out.flush()
        .and_then(|()| out.sync_all())
        .map_err(|err| ServerError::io("write", &temp, err))?;
    drop(out);
    fs::rename(&temp, path).map_err(|err| ServerError::io("rename", &temp, err))?;
    drop(guard);
    Ok(())
}

/// Builds the NEW server directory `dir` from the server backup `archive` (module docs).
///
/// `Err`: [`ServerError::Refused`] for an existing non-empty `dir` or a newer archive (without
/// [`RestoreOptions::with_force_version`]); [`ServerError::BackupArchive`] for an archive that
/// is damaged, changed (a checksum mismatch) or not a server backup; [`ServerError::Config`]
/// for a configuration file this version cannot read; [`ServerError::Io`]. Nothing is left in
/// `dir` after an error.
pub fn restore(
    archive: &Path,
    dir: &Path,
    options: RestoreOptions,
) -> Result<RestoreReport, ServerError> {
    let target = ServerDir::new(dir);
    let existed = check_target(&target)?;
    // 1. Check everything before writing anything.
    let mut seen = read_entries(archive)?;
    let (manifest, mut notes) = check_manifest(archive, &mut seen, options)?;
    let small = |name: &str| seen.get(name).and_then(|s| s.data.clone());
    let config_text = small(CONFIG_FILE).unwrap_or_default();
    let config_text =
        String::from_utf8(config_text).map_err(|_| damaged(archive, "server.toml is not UTF-8"))?;
    crate::config::ServerConfig::parse(&config_text, &target.config_path())?;
    if let Some(bytes) = small(ACCESS_FILE) {
        let text =
            String::from_utf8(bytes).map_err(|_| damaged(archive, "access.toml is not UTF-8"))?;
        crate::access::AccessFile::parse(&text, &target.access_path())?;
    }
    if let Some(bytes) = small(MCP_SETTINGS_FILE) {
        let text =
            String::from_utf8(bytes).map_err(|_| damaged(archive, "mcp.toml is not UTF-8"))?;
        crate::mcp_control::McpSettings::parse(&text, &target.mcp_settings_path())?;
    }
    let aliases: BTreeMap<String, String> = match small(ALIASES_FILE) {
        Some(bytes) => {
            let text = String::from_utf8(bytes)
                .map_err(|_| damaged(archive, "aliases.toml is not UTF-8"))?;
            toml::from_str(&text).map_err(|err| ServerError::Config {
                path: target.aliases_path(),
                message: err.message().to_string(),
            })?
        }
        None => BTreeMap::new(),
    };
    // The databases: their settings, and whether the archive holds their data.
    let mut configs: BTreeMap<String, (Vec<u8>, DatabaseConfig)> = BTreeMap::new();
    for (name, entry) in &seen {
        if let Some(EntryKind::DatabaseConfig(db)) = &entry.kind {
            let bytes = entry.data.clone().unwrap_or_default();
            let text = String::from_utf8(bytes.clone())
                .map_err(|_| damaged(archive, format!("{name} is not UTF-8")))?;
            let path = target.databases().join(db).join(DATABASE_FILE);
            let config = DatabaseConfig::parse(&text, &path)?;
            configs.insert(db.clone(), (bytes, config));
        }
    }
    let mut skipped: Vec<(String, String)> = Vec::new();
    let mut with_data: Vec<String> = Vec::new();
    for db in configs.keys() {
        let has_backup = seen.contains_key(&format!("databases/{db}/{DB_BACKUP}"));
        if has_backup {
            with_data.push(db.clone());
        } else {
            let reason = manifest
                .databases
                .iter()
                .find(|d| &d.name == db)
                .and_then(|d| d.reason.clone())
                .unwrap_or_else(|| {
                    if manifest.with_databases {
                        "the archive holds no backup of it".to_string()
                    } else {
                        "a configuration-only archive (back up with --with-databases to \
                         include the data)"
                            .to_string()
                    }
                });
            skipped.push((db.clone(), reason));
        }
    }
    for d in &manifest.databases {
        if !configs.contains_key(&d.name) {
            skipped.push((
                d.name.clone(),
                d.reason
                    .clone()
                    .unwrap_or_else(|| "no readable database.toml in the archive".to_string()),
            ));
        }
    }
    // 2. The layout: the root (0700), server.toml, the subdirectories, no first admin yet.
    let mut cleanup = Cleanup {
        root: target.root().to_path_buf(),
        existed,
        armed: true,
    };
    crate::security::create_private_dir(target.root())?;
    write_atomic_private(&target.config_path(), config_text.as_bytes())?;
    ServerDir::init_with(target.root().to_path_buf(), false, None)?;
    // 3. The data entries: model blobs into models/blobs/, the databases' backups into
    //    backups/<name>/ for the ordinary restore of a backup.
    let mut model_blobs = 0;
    {
        let file = File::open(archive).map_err(|err| ServerError::io("read", archive, err))?;
        let mut reader = ZipReader::new(BufReader::new(file));
        while let Some(entry) = reader
            .next_entry()
            .map_err(|err| damaged(archive, err.to_string()))?
        {
            let Some(found) = seen.get(&entry.name) else {
                entry
                    .finish()
                    .map_err(|err| damaged(archive, err.to_string()))?;
                continue;
            };
            let expected = found.sha256.clone();
            match &found.kind {
                Some(EntryKind::Blob(hash)) => {
                    let path = crate::models::ModelsDir::new(&target).blob_path(hash);
                    write_entry(entry, &path, &expected, archive)?;
                    model_blobs += 1;
                }
                Some(EntryKind::DatabaseBackup(db)) if configs.contains_key(db) => {
                    let path = target
                        .backups()
                        .join(db)
                        .join(format!("{RESTORE_STAMP}.zip"));
                    write_entry(entry, &path, &expected, archive)?;
                }
                _ => {
                    entry
                        .finish()
                        .map_err(|err| damaged(archive, err.to_string()))?;
                }
            }
        }
    }
    // 4. The databases: bases and plain ones first, then the layers over them.
    let (admin, lock) = crate::ops::Admin::open_offline(&target)?;
    // A restored database is no NEW name: it gets no MCP entry here (none = offered); the
    // archive's `mcp.toml`, written below, brings back what the server had (round 4 B-MCPDEF).
    let admin = admin.exposing(Some(true)).with_audit(
        crate::audit::Audit::default()
            .with_actor(Some(crate::access::OPERATOR.to_string()))
            .with_os_user(crate::security::os_user()),
    );
    with_data.sort_by_key(|db| {
        let layer = configs
            .get(db)
            .is_some_and(|(_, c)| c.kind == DatabaseKind::Layer);
        (layer, db.clone())
    });
    let mut restored = Vec::new();
    for db in &with_data {
        let backups = target.backups().join(db);
        if let Some(sidecar) = small(&format!("databases/{db}/{DB_SIDECAR}")) {
            write_atomic(&backups.join(format!("{RESTORE_STAMP}.toml")), &sidecar)?;
        }
        let outcome = admin.restore(db, RESTORE_STAMP, None);
        // The copy of the backup is not kept: the archive is the backup.
        let _ = fs::remove_dir_all(&backups);
        match outcome {
            Ok(done) => {
                let (bytes, wanted) = &configs[db];
                let have = done
                    .entry
                    .config
                    .clone()
                    .unwrap_or_else(DatabaseConfig::plain);
                if have.kind == wanted.kind && have.base == wanted.base && have.role == wanted.role
                {
                    write_atomic_private(&done.entry.dir.join(DATABASE_FILE), bytes)?;
                } else {
                    notes.push(format!(
                        "database '{db}': its backup restored as kind {} role {}, not as its \
                         database.toml says; its flags and limits were not restored",
                        have.kind.as_str(),
                        have.role.as_str()
                    ));
                }
                restored.push(db.clone());
            }
            Err(err) => {
                tracing::warn!(target: "graphersal_server", "restore of '{db}' skipped: {err}");
                skipped.push((db.clone(), format!("its backup did not restore: {err}")));
            }
        }
    }
    // 5. The configuration that names databases and users, last.
    let mut files = vec![CONFIG_FILE.to_string()];
    if let Some(bytes) = small(ALIASES_FILE) {
        write_atomic_private(&target.aliases_path(), &bytes)?;
        files.push(ALIASES_FILE.to_string());
    }
    if let Some(bytes) = small(MCP_SETTINGS_FILE) {
        write_atomic_private(&target.mcp_settings_path(), &bytes)?;
        files.push(MCP_SETTINGS_FILE.to_string());
    }
    if let Some(bytes) = small(ACCESS_FILE) {
        write_atomic_private(&target.access_path(), &bytes)?;
        files.push(ACCESS_FILE.to_string());
    } else {
        notes.push(
            "the archive has no access.toml: the first serve makes a first admin and prints its \
             token"
                .to_string(),
        );
    }
    for db in &restored {
        files.push(format!("databases/{db}/{DATABASE_FILE}"));
    }
    // Owner only: every directory of the layout 0700 (the files written here are 0600).
    if let Ok(entries) = fs::read_dir(target.root()) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                crate::security::set_private_dir(&entry.path())?;
            }
        }
    }
    crate::security::tighten(&target)?;
    drop(admin);
    drop(lock);
    cleanup.armed = false;
    // Notes about what the skipped databases leave behind.
    skipped.sort();
    skipped.dedup_by(|a, b| a.0 == b.0);
    for (db, _) in &skipped {
        let names: Vec<&String> = aliases
            .iter()
            .filter(|(_, target)| fold(target) == *db)
            .map(|(alias, _)| alias)
            .collect();
        if !names.is_empty() {
            notes.push(format!(
                "aliases of the skipped database '{db}' stay in aliases.toml but are dropped at \
                 start until it exists: {}",
                names
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    if !skipped.is_empty() {
        notes.push(
            "grants and MCP settings naming a skipped database stay in access.toml and mcp.toml; \
             copy the database's directory into databases/ while no server runs to bring it back \
             with them (a database created or imported under that name starts without them)"
                .to_string(),
        );
    }
    notes.push(
        "browser and MCP sessions were not restored: users log in again with their tokens"
            .to_string(),
    );
    tracing::info!(
        target: "graphersal_server",
        "server backup {} restored into {}: {} database(s), {} skipped",
        archive.display(),
        target.root().display(),
        restored.len(),
        skipped.len()
    );
    Ok(RestoreReport {
        dir: target.root().to_path_buf(),
        manifest,
        files,
        databases: restored,
        skipped,
        model_blobs,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_names_are_recognised_and_nothing_else() {
        assert_eq!(EntryKind::of("server.toml"), Some(EntryKind::Root));
        assert_eq!(
            EntryKind::of("databases/acme/database.toml"),
            Some(EntryKind::DatabaseConfig("acme".into()))
        );
        assert_eq!(
            EntryKind::of("databases/acme/backup.zip"),
            Some(EntryKind::DatabaseBackup("acme".into()))
        );
        let hash = "ab".repeat(32);
        assert_eq!(
            EntryKind::of(&format!("models/blobs/ab/{hash}")),
            Some(EntryKind::Blob(hash.clone()))
        );
        assert_eq!(EntryKind::of(&format!("models/blobs/cd/{hash}")), None);
        assert_eq!(EntryKind::of("databases/../server.toml"), None);
        assert_eq!(EntryKind::of("databases/Acme/database.toml"), None);
        assert_eq!(EntryKind::of("sessions/browser.json"), None);
        assert_eq!(EntryKind::of("../server.toml"), None);
    }

    #[test]
    fn versions_compare_by_number() {
        assert!(version_tuple("0.10.0") > version_tuple("0.9.9"));
        assert_eq!(version_tuple("0.3.0-dev"), Some((0, 3, 0)));
        assert_eq!(version_tuple("x"), None);
    }
}
