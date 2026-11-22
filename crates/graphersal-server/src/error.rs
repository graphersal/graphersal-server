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
