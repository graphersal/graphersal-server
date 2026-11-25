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
//!   backups/  trash/  tmp/
//! ```
//!
//! There is no log directory: the server logs to stderr (redirect it to keep a log file).
//!
//! A directory is a server directory when it holds `server.toml`. Every path is built with
//! [`Path::join`] one component at a time (Windows).

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

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
/// The alias file's name.
pub const ALIASES_FILE: &str = "aliases.toml";
/// The access file's name (users, token hashes, grants: [`crate::access`]).
pub const ACCESS_FILE: &str = "access.toml";

/// The subdirectories `init` creates, each as its path components.
const SUBDIRS: &[&[&str]] = &[
    &["databases"],
    &["models", "blobs"],
    &["backups"],
    &["trash"],
    &["tmp"],
    &["sessions"],
];

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
    pub fn init(root: impl Into<PathBuf>) -> Result<InitReport, ServerError> {
        let dir = Self::new(root);
        let mut created = Vec::new();
        let existed = match fs::metadata(&dir.root) {
            Ok(meta) if !meta.is_dir() => {
                return Err(ServerError::NotEmpty { path: dir.root });
            }
            Ok(_) => {
                if dir.config_path().is_file() {
                    dir.load_config()?;
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
                fs::create_dir_all(&dir.root)
                    .map_err(|err| ServerError::io("create directory", &dir.root, err))?;
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
        // Last: a directory without `access.toml` is closed; its first admin is made here.
        let first_admin = crate::access::ensure_first_admin(&dir, &Audit::default())?;
        if let Some(first) = &first_admin {
            created.push(first.path.clone());
        }
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

    /// `aliases.toml`.
    pub fn aliases_path(&self) -> PathBuf {
        self.root.join(ALIASES_FILE)
    }

    /// `access.toml`.
    pub fn access_path(&self) -> PathBuf {
        self.root.join(ACCESS_FILE)
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
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ServerError> {
    write_atomic_with(path, bytes, false)
}

/// [`write_atomic`] for a file only its owner may read (mode `0600` on unix; Windows: the
/// directory's permissions): the run file with the operator token, `access.toml`.
pub(crate) fn write_atomic_private(path: &Path, bytes: &[u8]) -> Result<(), ServerError> {
    write_atomic_with(path, bytes, true)
}

/// Creates (or truncates) `path` for writing; `private`: mode `0600` on unix, also when the
/// file was there before.
pub(crate) fn create_file(path: &Path, private: bool) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
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

fn write_atomic_with(path: &Path, bytes: &[u8], private: bool) -> Result<(), ServerError> {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let tmp = path.with_file_name(name);
    let written = create_file(&tmp, private).and_then(|mut file| {
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
