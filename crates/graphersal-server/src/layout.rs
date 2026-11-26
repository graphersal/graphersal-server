//! The server directory: its layout and `init`.
//!
//! ```text
//! <server-dir>/
//!   server.toml      configuration (crate::config)
//!   server.lock      one running server per directory (crate::lock)
//!   server.run       the address of the running server (crate::run_file), removed at stop
//!   aliases.toml     alias -> database name (crate::registry)
//!   access.toml      users (token hashes), roles, grants (crate::access); mode 0600
//!   databases/       one directory per database: database.toml + store/ (crate::registry)
//!   models/          the shared embedding models: blobs/<2 hex>/<sha256>, models.toml (crate::models)
//!   sessions/        browser.json, mcp.json: the sessions that survive a restart (crate::sessions)
//!   imports/         the import inbox: files an admin imports by name (crate::ops::InboxFile)
//!   backups/         the admin's backups (backups/<name>/) and, by default, the backup root
//!                    of backup jobs (backups/jobs/, `server.toml [jobs] backup_root`)
//!   trash/  tmp/
//! ```
//!
//! There is no log directory: the server logs to stderr (redirect it to keep a log file).
//!
//! A directory is a server directory when it holds `server.toml`. Every path is built with
//! [`Path::join`] one component at a time (Windows).

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::access::FirstAdmin;
use crate::audit::Audit;
use crate::config::{ServerConfig, default_config_text};
use crate::error::ServerError;

/// The configuration file's name.
pub const CONFIG_FILE: &str = "server.toml";
/// The lock file's name.
pub const LOCK_FILE: &str = "server.lock";
/// The run file's name.
pub const RUN_FILE: &str = "server.run";
/// The open-databases file's name (round 3 B-CRASH: the databases the running server has
/// loaded or is loading, rewritten on every load and unload, removed at a clean stop; one left
/// behind marks an unclean shutdown: [`crate::recovery`]).
pub const OPEN_FILE: &str = "server.open";
/// The file of the last unclean shutdown's check (round 3 B-CRASH: [`crate::recovery`]).
pub const RECOVERY_FILE: &str = "recovery.json";
/// The alias file's name.
pub const ALIASES_FILE: &str = "aliases.toml";
/// The access file's name (users, token hashes, grants: [`crate::access`]).
pub const ACCESS_FILE: &str = "access.toml";
/// The MCP settings file's name (the runtime switches of the MCP endpoints, written by the
/// server: the MCP tab, `PUT /api/admin/mcp...`, `graphersal-server mcp ...`).
pub const MCP_SETTINGS_FILE: &str = "mcp.toml";

/// The subdirectories `init` creates, each as its path components.
const SUBDIRS: &[&[&str]] = &[
    &["databases"],
    &["models", "blobs"],
    &["backups"],
    &["trash"],
    &["tmp"],
    &["sessions"],
    &[IMPORTS_DIR],
];

/// The import inbox's directory name (`imports/`).
pub const IMPORTS_DIR: &str = "imports";

/// The log directory's name (`logs/`: `graphersal-server start`, created on its first use).
pub const LOGS_DIR: &str = "logs";

/// The log file's name in `logs/` (`server.log`).
pub const LOG_FILE: &str = "server.log";

/// A server directory (its root path; nothing is checked until it is opened).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerDir {
    root: PathBuf,
}

/// What [`ServerDir::init`] did.
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct InitReport {
    /// The server directory.
    pub dir: ServerDir,
    /// Whether the directory was a server directory before (`server.toml` kept unchanged).
    pub existed: bool,
    /// The paths `init` created (the root, `server.toml`, subdirectories, `access.toml`), in
    /// creation order.
    pub created: Vec<PathBuf>,
    /// The first admin, when the directory had no `access.toml`: its token, to be shown ONCE.
    pub first_admin: Option<FirstAdmin>,
    /// The paths that were there before whose mode `init` tightened to owner only (unix: the
    /// directory `0700`, `sessions/` `0700`, the secret files `0600`: [`crate::security`]).
    pub secured: Vec<PathBuf>,
}

impl ServerDir {
    /// The server directory at `root` (not checked).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The server directory at `root`: `Err` when it holds no `server.toml`.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, ServerError> {
        let dir = Self::new(root);
        match fs::metadata(dir.config_path()) {
            Ok(meta) if meta.is_file() => Ok(dir),
            Ok(_) => Err(ServerError::NotServerDir { path: dir.root }),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                Err(ServerError::NotServerDir { path: dir.root })
            }
            Err(err) => Err(ServerError::io("read", dir.config_path(), err)),
        }
    }

    /// Creates the server directory layout at `root`:
    ///
    /// - a missing or empty directory: `server.toml` with every default, then the
    ///   subdirectories;
    /// - a server directory (it holds `server.toml`): its `server.toml` must parse and is kept
    ///   unchanged; missing subdirectories are created (so `init` completes an interrupted
    ///   `init` and is safe to repeat);
    /// - anything else (a file, a non-empty directory without `server.toml`): refused.
    ///
    /// A directory without `access.toml` then gets its first admin
    /// ([`crate::access::ensure_first_admin`]): [`InitReport::first_admin`] holds its token,
    /// for the caller to show once.
    ///
    /// The directory is made private to its owner (unix: mode `0700`, `sessions/` `0700`, the
    /// secret files `0600`; [`crate::security`]); an existing directory must be owned by the
    /// current OS user ([`ServerError::InsecureDir`] otherwise), and the modes it finds too
    /// open are tightened ([`InitReport::secured`]).
    pub fn init(root: impl Into<PathBuf>) -> Result<InitReport, ServerError> {
        Self::init_with(root.into(), true)
    }

    /// [`ServerDir::init`]; without `first_admin` a directory without `access.toml` stays
    /// without one (the restore of a server backup, [`crate::server_backup::restore`], writes
    /// the archive's own `access.toml` afterwards).
    pub(crate) fn init_with(root: PathBuf, first_admin: bool) -> Result<InitReport, ServerError> {
        let dir = Self::new(root);
        let mut created = Vec::new();
        let mut config = None;
        let existed = match fs::metadata(&dir.root) {
            Ok(meta) if !meta.is_dir() => {
                return Err(ServerError::NotEmpty { path: dir.root });
            }
            Ok(_) => {
                // Only the owner makes (or completes) a server directory of it.
                crate::security::check_owner(&dir)?;
                if dir.config_path().is_file() {
                    config = Some(dir.load_config()?);
                    true
                } else {
                    let mut entries = fs::read_dir(&dir.root)
                        .map_err(|err| ServerError::io("read directory", &dir.root, err))?;
                    if entries.next().is_some() {
                        return Err(ServerError::NotEmpty { path: dir.root });
                    }
                    false
                }
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                crate::security::create_private_dir(&dir.root)?;
                created.push(dir.root.clone());
                false
            }
            Err(err) => return Err(ServerError::io("read", &dir.root, err)),
        };
        if !existed {
            // First the configuration: it marks the directory as a server directory, so an
            // interrupted `init` is completed by the next one.
            write_atomic(&dir.config_path(), default_config_text().as_bytes())?;
            created.push(dir.config_path());
        }
        for components in SUBDIRS {
            let mut path = dir.root.clone();
            for component in *components {
                path = path.join(component);
                match fs::create_dir(&path) {
                    Ok(()) => created.push(path.clone()),
                    Err(err) if err.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => {}
                    Err(err) => return Err(ServerError::io("create directory", &path, err)),
                }
            }
        }
        // The backup root of backup jobs (`[jobs] backup_root`, by default `backups/jobs/`).
        let root = dir.backup_root(&config.unwrap_or_default());
        if !root.is_dir() {
            dir.ensure_backup_root(&root)?;
            created.push(root);
        }
        // Last: a directory without `access.toml` is closed; its first admin is made here (by
        // the operator: the OS user running `init`).
        let audit = Audit::default()
            .with_actor(Some(crate::access::OPERATOR.to_string()))
            .with_os_user(crate::security::os_user());
        let first_admin = if first_admin {
            crate::access::ensure_first_admin(&dir, &audit)?
        } else {
            None
        };
        if let Some(first) = &first_admin {
            created.push(first.path.clone());
        }
        // Owner-only modes on everything that holds secrets (new and old directories alike).
        let secured: Vec<PathBuf> = crate::security::tighten(&dir)?
            .into_iter()
            .filter(|path| !created.contains(path))
            .collect();
        tracing::info!(
            target: "graphersal_server",
            "init {}: {} created",
            dir.root.display(),
            created.len()
        );
        Ok(InitReport {
            dir,
            existed,
            created,
            first_admin,
            secured,
        })
    }

    /// The root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `server.toml`.
    pub fn config_path(&self) -> PathBuf {
        self.root.join(CONFIG_FILE)
    }

    /// `server.lock`.
    pub fn lock_path(&self) -> PathBuf {
        self.root.join(LOCK_FILE)
    }

    /// `server.run`.
    pub fn run_path(&self) -> PathBuf {
        self.root.join(RUN_FILE)
    }

    /// `server.open` ([`OPEN_FILE`]).
    pub fn open_path(&self) -> PathBuf {
        self.root.join(OPEN_FILE)
    }

    /// `recovery.json` ([`RECOVERY_FILE`]).
    pub fn recovery_path(&self) -> PathBuf {
        self.root.join(RECOVERY_FILE)
    }

    /// `aliases.toml`.
    pub fn aliases_path(&self) -> PathBuf {
        self.root.join(ALIASES_FILE)
    }

    /// `access.toml`.
    pub fn access_path(&self) -> PathBuf {
        self.root.join(ACCESS_FILE)
    }

    /// `mcp.toml` (the MCP settings; missing = the defaults).
    pub fn mcp_settings_path(&self) -> PathBuf {
        self.root.join(MCP_SETTINGS_FILE)
    }

    /// `databases/`.
    pub fn databases(&self) -> PathBuf {
        self.root.join("databases")
    }

    /// `models/`.
    pub fn models(&self) -> PathBuf {
        self.root.join("models")
    }

    /// `models/blobs/`.
    pub fn model_blobs(&self) -> PathBuf {
        self.models().join("blobs")
    }

    /// `backups/`.
    pub fn backups(&self) -> PathBuf {
        self.root.join("backups")
    }

    /// `trash/`.
    pub fn trash(&self) -> PathBuf {
        self.root.join("trash")
    }

    /// `tmp/`.
    pub fn tmp(&self) -> PathBuf {
        self.root.join("tmp")
    }

    /// `sessions/` (the browser and MCP sessions: [`crate::sessions`]).
    pub fn sessions(&self) -> PathBuf {
        self.root.join(crate::sessions::SESSIONS_DIR)
    }

    /// The log directory of `graphersal-server start`, `logs/` (owner only).
    pub fn logs(&self) -> PathBuf {
        self.root.join(LOGS_DIR)
    }

    /// The log of a server `graphersal-server start` runs in the background,
    /// `logs/server.log` (appended to; owner only).
    pub fn log_path(&self) -> PathBuf {
        self.logs().join(LOG_FILE)
    }

    /// `imports/` (the import inbox: [`crate::ops::Admin::inbox`]).
    pub fn imports(&self) -> PathBuf {
        self.root.join(IMPORTS_DIR)
    }

    /// The backup root of backup jobs for `config` (`[jobs] backup_root`, relative to this
    /// directory; by default `backups/jobs/`), as configured (not created, not resolved).
    pub fn backup_root(&self, config: &ServerConfig) -> PathBuf {
        config.jobs.backup_root(&self.root)
    }

    /// Creates the backup root `root` (with its parents) when it is missing and answers it
    /// resolved (symbolic links followed): the path every database context confines its job
    /// paths and fork/backup directories to.
    pub(crate) fn ensure_backup_root(&self, root: &Path) -> Result<PathBuf, ServerError> {
        fs::create_dir_all(root).map_err(|err| ServerError::io("create directory", root, err))?;
        fs::canonicalize(root).map_err(|err| ServerError::io("resolve", root, err))
    }

    /// Reads and checks `server.toml`.
    pub fn load_config(&self) -> Result<ServerConfig, ServerError> {
        let path = self.config_path();
        let text = fs::read_to_string(&path).map_err(|err| ServerError::io("read", &path, err))?;
        ServerConfig::parse(&text, &path)
    }
}

/// The bytes of every file below `dir` (each directory entry once: a file hard-linked from
/// `models/` counts in full here; links to the same file inside `dir` are not deduplicated
/// either, a Store has none). Unreadable entries count `0`; symbolic links are not followed.
pub(crate) fn dir_bytes(dir: &Path) -> u64 {
    let Ok(listing) = fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0u64;
    for item in listing.flatten() {
        match item.file_type() {
            Ok(kind) if kind.is_dir() => total = total.saturating_add(dir_bytes(&item.path())),
            Ok(kind) if kind.is_file() => {
                total = total.saturating_add(item.metadata().map(|m| m.len()).unwrap_or(0));
            }
            _ => {}
        }
    }
    total
}

/// Writes `bytes` to `path` through a temporary file beside it and a rename, so a reader sees
/// either the old file or the whole new one.
///
/// Safe for concurrent writers of the same file (several requests, several threads): every
/// write has its OWN temporary name ([`temp_path`]: `<file>.tmp-<pid>-<n>`), created new
/// (`create_new`), so two writers never share a temporary file; the last rename wins with a
/// whole file. The temporary file is flushed to the disk (`fsync`) before the rename, and on
/// unix the directory afterwards, so the new name survives a power loss too. A failure before
/// the rename removes the temporary file and leaves `path` unchanged (`Err`); a failing
/// directory `fsync` after the rename is only a warning (the new file is in place, which the
/// caller's memory must follow). The callers serialize their read-modify-write cycles with
/// their own lock (the inventory in `book/src/reference/server_directory.md`): this function only guarantees that
/// the file is always one writer's whole content.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ServerError> {
    write_atomic_with(path, bytes, false)
}

/// [`write_atomic`] for a file only its owner may read (mode `0600` on unix; Windows: the
/// directory's permissions): `access.toml`, the session files.
pub(crate) fn write_atomic_private(path: &Path, bytes: &[u8]) -> Result<(), ServerError> {
    write_atomic_with(path, bytes, true)
}

/// [`write_atomic_private`] without the directory's `fsync` after the rename: for the run file,
/// which a power loss makes stale anyway (it describes a process that is gone), so its name
/// need not be flushed, and the server answers its probes without waiting for the disk.
pub(crate) fn write_atomic_private_volatile(path: &Path, bytes: &[u8]) -> Result<(), ServerError> {
    write_atomic_file(path, bytes, true)
}

/// Creates the NEW file `path` (`AlreadyExists` when it is there); `private`: mode `0600` on
/// unix.
pub(crate) fn create_new_file(path: &Path, private: bool) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    open_with(options, path, private)
}

fn open_with(
    #[cfg_attr(not(unix), allow(unused_mut))] mut options: fs::OpenOptions,
    path: &Path,
    private: bool,
) -> io::Result<fs::File> {
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.mode(0o600);
        let file = options.open(path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        return Ok(file);
    }
    #[cfg(not(unix))]
    let _ = private;
    options.open(path)
}

/// The marker of the temporary names [`temp_path`] makes: `<file>.tmp-<pid>-<n>`.
pub(crate) const TEMP_MARK: &str = ".tmp-";

/// Counts the temporary names of this process (unique with the process id).
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temporary name beside `path` that no other write uses: `<file>.tmp-<pid>-<n>` (`n`
/// counts the temporary names of this process).
pub(crate) fn temp_path(path: &Path) -> PathBuf {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!("{TEMP_MARK}{}-{n}", std::process::id()));
    path.with_file_name(name)
}

/// Flushes the directory `dir` itself to the disk (a rename or a new name in it survives a
/// power loss). Unix only; elsewhere nothing to do (Windows cannot open a directory as a file;
/// NTFS journals its metadata). A plain `fsync(2)`: on macOS `File::sync_all` is
/// `F_FULLFSYNC`, a flush of the whole drive cache, far too slow for every small write (the
/// file itself still gets `sync_all` before the rename).
pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let file = fs::File::open(dir)?;
        // SAFETY: `file` is open for the whole call; fsync only reads the descriptor.
        if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// [`sync_dir`] of `path`'s directory after a rename into it: a failure is a warning (the file
/// is in place).
pub(crate) fn sync_parent(path: &Path) {
    let Some(dir) = path.parent() else {
        return;
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    if let Err(err) = sync_dir(dir) {
        tracing::warn!(
            target: "graphersal_server",
            "cannot flush the directory {} to the disk after writing {}: {err}",
            dir.display(),
            path.display()
        );
    }
}

fn write_atomic_with(path: &Path, bytes: &[u8], private: bool) -> Result<(), ServerError> {
    write_atomic_file(path, bytes, private)?;
    sync_parent(path);
    Ok(())
}

/// The temporary file, its `fsync` and the rename of [`write_atomic`] (no directory `fsync`).
fn write_atomic_file(path: &Path, bytes: &[u8], private: bool) -> Result<(), ServerError> {
    let tmp = temp_path(path);
    let written = create_new_file(&tmp, private).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    if let Err(err) = written {
        let _ = fs::remove_file(&tmp);
        return Err(ServerError::io("write", &tmp, err));
    }
    fs::rename(&tmp, path).map_err(|err| {
        let _ = fs::remove_file(&tmp);
        ServerError::io("write", path, err)
    })
}

/// The file names this server writes atomically (the legacy temporary names of earlier
/// versions, `<name>.tmp`, are recognised for these only).
const ATOMIC_FILES: &[&str] = &[
    CONFIG_FILE,
    RUN_FILE,
    OPEN_FILE,
    RECOVERY_FILE,
    ALIASES_FILE,
    ACCESS_FILE,
    crate::database::DATABASE_FILE,
    crate::sessions::BROWSER_FILE,
    crate::sessions::MCP_FILE,
    crate::models::MODELS_FILE,
];

/// Whether `name` is a temporary name this server makes (and nobody else): `<file>.tmp-<pid>-<n>`
/// ([`temp_path`]), the fixed `<file>.tmp` of versions up to 0.2 for the files it writes
/// atomically, and `access.toml.new-<pid>` (the first admin's file before it is linked).
pub(crate) fn is_temp_name(name: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if let Some(at) = name.rfind(TEMP_MARK) {
        let (base, rest) = (&name[..at], &name[at + TEMP_MARK.len()..]);
        if !base.is_empty()
            && let Some((pid, n)) = rest.split_once('-')
            && digits(pid)
            && digits(n)
        {
            return true;
        }
    }
    if let Some(base) = name.strip_suffix(".tmp")
        && ATOMIC_FILES.contains(&base)
    {
        return true;
    }
    name.strip_prefix(ACCESS_FILE)
        .and_then(|rest| rest.strip_prefix(".new-"))
        .is_some_and(digits)
}

/// Removes the temporary files of interrupted atomic writes ([`is_temp_name`]: only names this
/// server makes) directly in `dir` (not below it): the paths removed. Call it while nothing
/// writes there (the server lock held, before the server serves).
pub(crate) fn remove_temps_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(listing) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    for item in listing.flatten() {
        let is_file = item.file_type().is_ok_and(|kind| kind.is_file());
        let name = item.file_name();
        if !is_file || !name.to_str().is_some_and(is_temp_name) {
            continue;
        }
        let path = item.path();
        match fs::remove_file(&path) {
            Ok(()) => removed.push(path),
            Err(err) => tracing::warn!(
                target: "graphersal_server",
                "cannot remove the temporary file {}: {err}",
                path.display()
            ),
        }
    }
    removed
}

impl ServerDir {
    /// Removes the temporary files that interrupted atomic writes left (a crash between the
    /// write and the rename) in every directory where this server writes files atomically:
    /// the root, `sessions/`, `models/`, every `databases/<name>/` and `backups/<name>/`.
    /// Only names this server makes are touched (`<file>.tmp-<pid>-<n>` and the legacy names of
    /// `book/src/reference/server_directory.md` "Concurrent changes"). Run by the registry scan
    /// at start, with the server lock held. Answers the paths removed (each logged).
    pub fn remove_stale_temps(&self) -> Vec<PathBuf> {
        let mut dirs = vec![self.root.clone(), self.sessions(), self.models()];
        for parent in [self.databases(), self.backups()] {
            if let Ok(listing) = fs::read_dir(&parent) {
                dirs.extend(
                    listing
                        .flatten()
                        .filter(|item| item.file_type().is_ok_and(|kind| kind.is_dir()))
                        .map(|item| item.path()),
                );
            }
        }
        let mut removed = Vec::new();
        for dir in dirs {
            removed.extend(remove_temps_in(&dir));
        }
        for path in &removed {
            tracing::info!(
                target: "graphersal_server",
                "removed the temporary file {} of an interrupted write",
                path.display()
            );
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn init_creates_the_layout_and_is_safe_to_repeat() {
        let temp = TempDir::new("layout-init");
        let root = temp.path().join("server");
        let report = ServerDir::init(&root).unwrap();
        assert!(!report.existed);
        assert_eq!(report.created.first(), Some(&root));
        let dir = ServerDir::open(&root).unwrap();
        for path in [
            dir.databases(),
            dir.model_blobs(),
            dir.backups(),
            dir.trash(),
            dir.tmp(),
        ] {
            assert!(path.is_dir(), "{}", path.display());
        }
        assert_eq!(dir.load_config().unwrap(), ServerConfig::default());
        assert!(!dir.lock_path().exists());
        assert!(!dir.aliases_path().exists());

        // Again: nothing to do, the configuration kept byte for byte.
        fs::write(dir.config_path(), "listen = \"127.0.0.1:0\"\n").unwrap();
        let again = ServerDir::init(&root).unwrap();
        assert!(again.existed);
        assert!(again.created.is_empty());
        assert_eq!(
            fs::read_to_string(dir.config_path()).unwrap(),
            "listen = \"127.0.0.1:0\"\n"
        );
        // An interrupted init (a subdirectory missing) is completed.
        fs::remove_dir(dir.tmp()).unwrap();
        let completed = ServerDir::init(&root).unwrap();
        assert_eq!(completed.created, vec![dir.tmp()]);
        // No log directory: the server logs to stderr.
        assert!(!root.join("logs").exists());
    }

    #[test]
    fn init_creates_the_sessions_directory_also_in_an_older_server_directory() {
        let temp = TempDir::new("layout-sessions");
        let dir = ServerDir::init(temp.path()).unwrap().dir;
        assert!(dir.sessions().is_dir());
        // A directory from before the sessions were kept: `init` adds `sessions/`.
        fs::remove_dir(dir.sessions()).unwrap();
        let report = ServerDir::init(temp.path()).unwrap();
        assert_eq!(report.created, vec![dir.sessions()]);
    }

    #[test]
    fn init_creates_the_imports_inbox_also_in_an_older_server_directory() {
        let temp = TempDir::new("layout-imports");
        let dir = ServerDir::init(temp.path()).unwrap().dir;
        assert!(dir.imports().is_dir());
        // A directory from before the inbox: `init` adds `imports/`.
        fs::remove_dir(dir.imports()).unwrap();
        let report = ServerDir::init(temp.path()).unwrap();
        assert_eq!(report.created, vec![dir.imports()]);
    }

    #[test]
    fn init_takes_an_existing_empty_directory() {
        let temp = TempDir::new("layout-empty");
        let report = ServerDir::init(temp.path()).unwrap();
        assert!(!report.existed);
        assert_eq!(report.created.first(), Some(&temp.path().join(CONFIG_FILE)));
        assert!(ServerDir::open(temp.path()).is_ok());
    }

    #[test]
    fn init_refuses_what_is_not_empty_or_a_server_directory() {
        let temp = TempDir::new("layout-refuse");
        fs::write(temp.path().join("notes.txt"), "mine").unwrap();
        let err = ServerDir::init(temp.path()).unwrap_err();
        assert!(matches!(err, ServerError::NotEmpty { .. }), "{err}");
        assert!(err.to_string().contains("not empty"), "{err}");
        // Nothing was written.
        assert!(!temp.path().join(CONFIG_FILE).exists());

        let file = temp.path().join("notes.txt");
        assert!(matches!(
            ServerDir::init(&file).unwrap_err(),
            ServerError::NotEmpty { .. }
        ));

        // A server directory with a broken configuration is not overwritten.
        let broken = temp.path().join("broken");
        fs::create_dir(&broken).unwrap();
        fs::write(broken.join(CONFIG_FILE), "listen = 5").unwrap();
        let err = ServerDir::init(&broken).unwrap_err();
        assert!(matches!(err, ServerError::Config { .. }), "{err}");
        assert_eq!(
            fs::read_to_string(broken.join(CONFIG_FILE)).unwrap(),
            "listen = 5"
        );
    }

    #[test]
    fn open_needs_a_configuration() {
        let temp = TempDir::new("layout-open");
        let err = ServerDir::open(temp.path()).unwrap_err();
        assert!(matches!(err, ServerError::NotServerDir { .. }), "{err}");
        assert!(err.to_string().contains("graphersal-server init"), "{err}");
    }

    #[test]
    fn atomic_writes_leave_no_temporary_file() {
        let temp = TempDir::new("layout-atomic");
        let path = temp.path().join("x.toml");
        write_atomic(&path, b"a = 1\n").unwrap();
        write_atomic(&path, b"a = 2\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "a = 2\n");
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    /// The content one writer writes in one round: whole and recognisable (a torn or mixed
    /// file does not parse back to the same writer and round).
    fn payload(writer: usize, round: usize) -> String {
        let pad = "x".repeat((writer * 37 + round * 11) % 3000);
        format!("writer = {writer}\nround = {round}\npad = \"{pad}\"\nend = {writer}\n")
    }

    fn parse_payload(text: &str) -> Option<(usize, usize)> {
        let value: toml::Value = toml::from_str(text).ok()?;
        let writer = usize::try_from(value.get("writer")?.as_integer()?).ok()?;
        let round = usize::try_from(value.get("round")?.as_integer()?).ok()?;
        (text == payload(writer, round)).then_some((writer, round))
    }

    #[test]
    fn concurrent_atomic_writes_of_one_file_never_mix_and_leave_no_temporary_file() {
        let temp = TempDir::new("layout-atomic-concurrent");
        let path = temp.path().join("shared.toml");
        write_atomic(&path, payload(0, 0).as_bytes()).unwrap();
        const WRITERS: usize = 8;
        const ROUNDS: usize = 60;
        let done = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|scope| {
            let reader = scope.spawn(|| {
                let mut reads = 0usize;
                while !done.load(std::sync::atomic::Ordering::Relaxed) {
                    let text = fs::read_to_string(&path).unwrap();
                    assert!(parse_payload(&text).is_some(), "a torn file: {text:?}");
                    reads += 1;
                }
                reads
            });
            let writers: Vec<_> = (0..WRITERS)
                .map(|writer| {
                    let path = &path;
                    scope.spawn(move || {
                        for round in 0..ROUNDS {
                            let private = round % 2 == 0;
                            let bytes = payload(writer, round);
                            if private {
                                write_atomic_private(path, bytes.as_bytes()).unwrap();
                            } else {
                                write_atomic(path, bytes.as_bytes()).unwrap();
                            }
                        }
                    })
                })
                .collect();
            for writer in writers {
                writer.join().unwrap();
            }
            done.store(true, std::sync::atomic::Ordering::Relaxed);
            assert!(reader.join().unwrap() > 0);
        });
        let last = parse_payload(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            last.1,
            ROUNDS - 1,
            "the last file is some writer's last round"
        );
        let names: Vec<String> = fs::read_dir(temp.path())
            .unwrap()
            .map(|item| item.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["shared.toml".to_string()]);
    }

    #[test]
    fn temporary_names_are_unique_and_recognised() {
        let path = Path::new("dir").join("aliases.toml");
        let a = temp_path(&path);
        let b = temp_path(&path);
        assert_ne!(a, b);
        for name in [a, b] {
            let name = name.file_name().unwrap().to_str().unwrap().to_string();
            assert!(name.starts_with("aliases.toml.tmp-"), "{name}");
            assert!(is_temp_name(&name), "{name}");
        }
        for ours in [
            "access.toml.tmp-12-0",
            "2026-11-26T10-00-00Z.toml.tmp-4-17",
            "aliases.toml.tmp",
            "database.toml.tmp",
            "browser.json.tmp",
            "access.toml.new-4711",
        ] {
            assert!(is_temp_name(ours), "{ours}");
        }
        for theirs in [
            "notes.tmp",
            "aliases.toml",
            ".tmp-1-2",
            "x.tmp-1",
            "x.tmp-a-2",
            "x.tmp-1-",
            "access.toml.new-",
            "access.toml.new-x",
            "report.toml.tmp",
        ] {
            assert!(!is_temp_name(theirs), "{theirs}");
        }
    }

    #[test]
    fn stale_temporary_files_are_removed_and_nothing_else() {
        let temp = TempDir::new("layout-stale-temps");
        let root = temp.path().join("server");
        ServerDir::init(&root).unwrap();
        let dir = ServerDir::open(&root).unwrap();
        let db = dir.databases().join("acme");
        let backups = dir.backups().join("acme");
        fs::create_dir_all(&db).unwrap();
        fs::create_dir_all(&backups).unwrap();
        let stale = [
            root.join("aliases.toml.tmp-99-3"),
            root.join("access.toml.tmp"),
            root.join("access.toml.new-123"),
            dir.sessions().join("browser.json.tmp-1-1"),
            dir.models().join("models.toml.tmp"),
            db.join("database.toml.tmp-7-0"),
            backups.join("2026-11-26T10-00-00Z.toml.tmp-7-1"),
        ];
        let kept = [
            root.join("notes.tmp"),
            db.join("database.toml"),
            db.join("mine.tmp-x"),
            dir.imports().join("upload.toml.tmp-1-1"),
        ];
        for path in stale.iter().chain(kept.iter()) {
            fs::write(path, "x").unwrap();
        }
        let mut removed = dir.remove_stale_temps();
        removed.sort();
        let mut expected = stale.to_vec();
        expected.sort();
        assert_eq!(removed, expected);
        for path in &stale {
            assert!(!path.exists(), "{}", path.display());
        }
        for path in &kept {
            assert!(path.exists(), "{}", path.display());
        }
    }

    #[test]
    fn private_atomic_writes_are_for_the_owner_only() {
        let temp = TempDir::new("layout-private");
        let path = temp.path().join("secret.toml");
        fs::write(&path, "old").unwrap();
        write_atomic_private(&path, b"a = 1\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "a = 1\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
