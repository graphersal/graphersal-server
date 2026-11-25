//! The error type of the server.

use std::fmt;
use std::io;
use std::path::PathBuf;

/// Why a server operation failed.
///
/// `#[non_exhaustive]` (as are its named-field variants): further reasons may be added.
#[derive(Debug)]
#[non_exhaustive]
pub enum ServerError {
    /// A file system operation failed.
    #[non_exhaustive]
    Io {
        /// What was being done (`"create directory"`, `"write"`, ...).
        action: &'static str,
        /// The path it was done on.
        path: PathBuf,
        /// The OS error.
        source: io::Error,
    },
    /// `init` was given a directory that is neither empty nor a server directory.
    #[non_exhaustive]
    NotEmpty {
        /// The directory.
        path: PathBuf,
    },
    /// The path is not a server directory (no `server.toml`).
    #[non_exhaustive]
    NotServerDir {
        /// The directory.
        path: PathBuf,
    },
    /// `server.toml` (or another server file) cannot be read as configured.
    #[non_exhaustive]
    Config {
        /// The file.
        path: PathBuf,
        /// What is wrong with it.
        message: String,
    },
    /// A listen address is not `IP:PORT` (or `localhost:PORT`).
    #[non_exhaustive]
    InvalidListen {
        /// The value given.
        value: String,
        /// Why it was refused.
        message: String,
    },
    /// Another server runs on this server directory (its `server.lock` is held).
    #[non_exhaustive]
    Locked {
        /// The server directory.
        path: PathBuf,
    },
    /// The listener cannot bind its address.
    #[non_exhaustive]
    Bind {
        /// The address.
        address: String,
        /// The OS's reason.
        message: String,
    },
    /// A thread of the server cannot be started.
    #[non_exhaustive]
    Thread {
        /// The OS's reason.
        message: String,
    },
    /// A database or alias name is not `[a-z0-9][a-z0-9._-]{0,62}` (or is reserved).
    #[non_exhaustive]
    InvalidName {
        /// The name given.
        name: String,
        /// Why it was refused.
        message: String,
    },
    /// A name is taken: databases and aliases share one namespace (case-insensitive).
    #[non_exhaustive]
    NameInUse {
        /// The name given.
        name: String,
        /// What holds it (`"database"`, `"alias"`, `"database being built"`, ...).
        holder: &'static str,
    },
    /// No database (or alias) has this name.
    #[non_exhaustive]
    NotFound {
        /// The name given.
        name: String,
    },
    /// The database is failed (its `database.toml` cannot be read): it cannot be changed.
    #[non_exhaustive]
    DatabaseFailed {
        /// The database.
        name: String,
        /// Its state with the reason.
        reason: String,
    },
    /// A registry rule refuses the operation (a base's read-only flag, a layer's base, an
    /// alias naming an alias, ...).
    #[non_exhaustive]
    Refused {
        /// The database, alias or file concerned.
        name: String,
        /// The rule, as a sentence.
        message: String,
    },
    /// The serving runtime of a loaded database failed an operation (its read-only toggle, the
    /// close of its Store, ...).
    #[non_exhaustive]
    Serve {
        /// The database.
        name: String,
        /// What failed, with the runtime's reason.
        message: String,
    },
    /// A request's input does not fit (a schema that does not parse or does not fit the data,
    /// a bad value in a request body).
    #[non_exhaustive]
    InvalidInput {
        /// The database (or the request) concerned.
        name: String,
        /// What is wrong.
        message: String,
    },
    /// A Store operation of an admin action failed (create, fork, reading a base).
    #[non_exhaustive]
    Store {
        /// The database.
        name: String,
        /// What failed, with the Store's reason.
        message: String,
    },
    /// A database name has no backup of this name in `backups/<database>/`.
    #[non_exhaustive]
    BackupNotFound {
        /// The database name.
        database: String,
        /// The backup asked for.
        backup: String,
    },
    /// An import's source file or directory does not exist.
    #[non_exhaustive]
    SourceNotFound {
        /// The path given.
        path: PathBuf,
    },
    /// An import's source is none of the formats an import reads (a packed snapshot, a Store
    /// directory or `.gstore` file, GraphSON, GraphML).
    #[non_exhaustive]
    UnsupportedSource {
        /// The path given (for an upload: its file in `tmp/`).
        path: PathBuf,
        /// Why it was not recognized.
        message: String,
    },
    /// The running server cannot be reached (the console's admin client).
    #[non_exhaustive]
    Connect {
        /// The server's address.
        address: String,
        /// The reason.
        message: String,
    },
    /// The shared models (`models/`) hold no model of this repository (or blob hash).
    #[non_exhaustive]
    ModelNotFound {
        /// The repository (or blob hash) asked for.
        repo: String,
    },
    /// A Store import needs a model blob the source Store lacks, and the shared models
    /// (`models/`) do not hold it either: the Store cannot be copied without it. Since
    /// graphersal 0.7.1 the import leaves such a blob out (the database becomes `needs-model`),
    /// so this answers only a Store failure of that kind the copy could not skip.
    #[non_exhaustive]
    ModelUnavailable {
        /// The database being imported.
        name: String,
        /// The repositories of the source's models whose blob is missing (empty when the
        /// source's catalog cannot be read).
        repos: Vec<String>,
        /// The Store's reason (names the missing blob `blobs/<2 hex>/<hash>`).
        message: String,
    },
    /// A Store import needs a model blob the source Store lacks, and the shared models hold it
    /// DAMAGED (its bytes do not match its name).
    #[non_exhaustive]
    ModelDamaged {
        /// The database being imported.
        name: String,
        /// The damaged file in the shared models (`models/blobs/<2 hex>/<hash>`), when the
        /// Store's reason names the blob.
        blob: Option<PathBuf>,
        /// The Store's reason.
        message: String,
    },
    /// No user of `access.toml` has this name.
    #[non_exhaustive]
    UserNotFound {
        /// The name given (folded to lower case).
        name: String,
    },
    /// The change would leave no user who can act as an admin (the `admin` role and a token):
    /// the last admin is not removed, demoted or locked out.
    #[non_exhaustive]
    LastAdmin {
        /// The user the change was for (folded to lower case).
        name: String,
    },
    /// The operating system's random generator failed (a token or session id cannot be
    /// made).
    #[non_exhaustive]
    Random {
        /// The reason.
        message: String,
    },
    /// The server directory is not private to the current OS user (`crate::security`): it, a
    /// secret file in it or `sessions/` is owned by another user, or open to group or others.
    /// `serve` and the console refuse to act on it.
    #[non_exhaustive]
    InsecureDir {
        /// The server directory.
        dir: PathBuf,
        /// Every problem found, one sentence each.
        problems: Vec<String>,
        /// The commands fixing them: (what it does, the command).
        fixes: Vec<(String, String)>,
    },
    /// `graphersal-server start` or `stop` could not do its part: the server is already
    /// running, exited before it was ready, did not get ready or stop in time
    /// ([`crate::daemon`]).
    #[non_exhaustive]
    Service {
        /// What went wrong, as a sentence.
        message: String,
        /// What to do about it, if anything.
        help: Option<String>,
        /// The last lines the server wrote into its log since this command started it (empty
        /// when there are none).
        log_tail: Vec<String>,
    },
    /// The running server refused an admin request (the console's admin client): its answer.
    #[non_exhaustive]
    Remote {
        /// The HTTP status.
        status: u16,
        /// The answer's `error`.
        message: String,
        /// The answer's `help`, if any.
        help: Option<String>,
    },
}

impl ServerError {
    /// An [`ServerError::Io`] error.
    pub(crate) fn io(action: &'static str, path: impl Into<PathBuf>, source: io::Error) -> Self {
        ServerError::Io {
            action,
            path: path.into(),
            source,
        }
    }
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServerError::Io {
                action,
                path,
                source,
            } => write!(f, "cannot {action} {}: {source}", path.display()),
            ServerError::NotEmpty { path } => write!(
                f,
                "{} is not empty and not a server directory (no server.toml); give an empty or \
                 new directory",
                path.display()
            ),
            ServerError::NotServerDir { path } => write!(
                f,
                "{} is not a server directory (no server.toml); create one with \
                 `graphersal-server init <dir>`",
                path.display()
            ),
            ServerError::Config { path, message } => {
                write!(f, "{}: {message}", path.display())
            }
            ServerError::InvalidListen { value, message } => write!(
                f,
                "invalid listen address {value:?}: {message}; expected IP:PORT such as \
                 127.0.0.1:8484 (port 0 picks a free port)"
            ),
            ServerError::Locked { path } => write!(
                f,
                "another graphersal-server already serves {} (its server.lock is held); stop it \
                 first, or use another server directory",
                path.display()
            ),
            ServerError::Bind { address, message } => {
                write!(f, "cannot listen on {address}: {message}")
            }
            ServerError::Thread { message } => write!(f, "cannot start a thread: {message}"),
            ServerError::InvalidName { name, message } => {
                write!(f, "invalid name {name:?}: {message}")
            }
            ServerError::NameInUse { name, holder } => write!(
                f,
                "the name '{name}' is taken by a {holder} (databases and aliases share one \
                 namespace, case-insensitive)"
            ),
            ServerError::NotFound { name } => {
                write!(f, "no database or alias '{name}' on this server")
            }
            ServerError::DatabaseFailed { name, reason } => write!(
                f,
                "database '{name}' is {reason}; fix or remove its directory and restart the server"
            ),
            ServerError::Refused { name, message } => write!(f, "'{name}': {message}"),
            ServerError::Serve { name, message } => write!(f, "database '{name}': {message}"),
            ServerError::InvalidInput { name, message } => write!(f, "'{name}': {message}"),
            ServerError::Store { name, message } => write!(f, "database '{name}': {message}"),
            ServerError::BackupNotFound { database, backup } => write!(
                f,
                "database '{database}' has no backup '{backup}' (backups/{database}/{backup}.zip)"
            ),
            ServerError::SourceNotFound { path } => {
                write!(f, "the import source {} does not exist", path.display())
            }
            ServerError::UnsupportedSource { path, message } => write!(
                f,
                "cannot import {}: {message}; an import reads a packed snapshot (.gsnap), a \
                 Store directory or .gstore file, GraphSON (.json, .jsonl, .graphson) or GraphML \
                 (.xml, .graphml)",
                path.display()
            ),
            ServerError::Connect { address, message } => write!(
                f,
                "cannot reach the running graphersal-server at {address}: {message}"
            ),
            ServerError::ModelNotFound { repo } => {
                write!(f, "the server's shared models hold no model '{repo}'")
            }
            ServerError::ModelUnavailable {
                name,
                repos,
                message,
            } => {
                let what = match repos.as_slice() {
                    [] => "an embedding model".to_string(),
                    [repo] => format!("the embedding model '{repo}'"),
                    many => format!("the embedding models '{}'", many.join("', '")),
                };
                write!(
                    f,
                    "'{name}': the source Store lacks the blob of {what} and the server's shared \
                     models do not hold it, so the Store cannot be copied: {message}"
                )
            }
            ServerError::ModelDamaged {
                name,
                blob,
                message,
            } => match blob {
                Some(blob) => write!(
                    f,
                    "'{name}': the source Store lacks a model blob and the server's shared copy \
                     {} is damaged (its bytes do not match its name): {message}",
                    blob.display()
                ),
                None => write!(
                    f,
                    "'{name}': the source Store lacks a model blob and the server's shared copy \
                     is damaged (its bytes do not match its name): {message}"
                ),
            },
            ServerError::UserNotFound { name } => {
                write!(f, "no user '{name}' in access.toml")
            }
            ServerError::LastAdmin { name } => write!(
                f,
                "'{name}' is the last admin: no other user with the admin role and a token would \
                 be left"
            ),
            ServerError::Random { message } => {
                write!(f, "cannot make a secret token: {message}")
            }
            ServerError::InsecureDir { dir, problems, .. } => write!(
                f,
                "refused: the server directory {} is not private to its owner: {}",
                dir.display(),
                problems.join("; ")
            ),
            ServerError::Service { message, .. } => f.write_str(message),
            ServerError::Remote { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for ServerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ServerError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
