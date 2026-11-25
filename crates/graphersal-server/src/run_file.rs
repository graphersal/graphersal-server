//! `server.run`: where the running server listens, for the consoles on this machine.
//!
//! The server writes it (TOML) once its listener is bound, with the address it actually bound
//! (`--listen` and port `0` included), and removes it when it stops. A run file is only
//! trustworthy while `server.lock` is held ([`crate::ServerLock::is_held`]): after a crash it
//! stays behind, and the next server overwrites it.
//!
//! It also carries the operator token of this run (a new one per start): the console on this
//! machine authenticates with it as the operator. So the file is written for its owner only
//! (mode `0600` on unix).

use std::fs;
use std::io;
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

use crate::access::Secret;
use crate::error::ServerError;
use crate::layout::{ServerDir, write_atomic_private};

/// The contents of `server.run`.
///
/// `#[non_exhaustive]`: further fields may be added (readers ignore unknown keys).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RunInfo {
    /// The bound address (`127.0.0.1:8484`, `[::1]:8484`).
    pub address: SocketAddr,
    /// The URL a browser on this machine opens (`http://127.0.0.1:8484/`).
    pub url: String,
    /// The server's process id.
    pub pid: u32,
    /// The server's version.
    pub version: String,
    /// The operator token of this run (`Authorization: Bearer <token>`: full rights), if the
    /// server has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_token: Option<Secret>,
}

impl RunInfo {
    /// The run information of this process listening on `address` with `url`.
    pub fn new(address: SocketAddr, url: String) -> Self {
        Self {
            address,
            url,
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            operator_token: None,
        }
    }

    /// The run information with the operator token `token`.
    pub fn with_operator_token(mut self, token: Secret) -> Self {
        self.operator_token = Some(token);
        self
    }

    /// Writes the run file of `dir` (atomically, for its owner only: mode `0600` on unix).
    pub fn write(&self, dir: &ServerDir) -> Result<(), ServerError> {
        let path = dir.run_path();
        let body = toml::to_string(self).map_err(|err| ServerError::Config {
            path: path.clone(),
            message: err.to_string(),
        })?;
        let text = format!(
            "# Written by the running graphersal-server and removed when it stops; valid only \
             while server.lock is held.\n{body}"
        );
        write_atomic_private(&path, text.as_bytes())
    }

    /// Reads the run file of `dir`: `None` when there is none.
    pub fn read(dir: &ServerDir) -> Result<Option<Self>, ServerError> {
        let path = dir.run_path();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(ServerError::io("read", &path, err)),
        };
        toml::from_str(&text)
            .map(Some)
            .map_err(|err| ServerError::Config {
                path,
                message: err.message().to_string(),
            })
    }

    /// Removes the run file of `dir` (no error when there is none).
    pub fn remove(dir: &ServerDir) -> Result<(), ServerError> {
        let path = dir.run_path();
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(ServerError::io("remove", &path, err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn write_read_remove() {
        let temp = TempDir::new("run-file");
        let dir = ServerDir::init(temp.path()).unwrap().dir;
        assert_eq!(RunInfo::read(&dir).unwrap(), None);
        let info = RunInfo::new(
            "[::1]:4242".parse().unwrap(),
            "http://[::1]:4242/".to_string(),
        );
        info.write(&dir).unwrap();
        assert_eq!(RunInfo::read(&dir).unwrap(), Some(info));
        RunInfo::remove(&dir).unwrap();
        RunInfo::remove(&dir).unwrap();
        assert_eq!(RunInfo::read(&dir).unwrap(), None);
    }

    #[test]
    fn the_operator_token_is_kept_for_the_owner_only() {
        let temp = TempDir::new("run-file-token");
        let dir = ServerDir::init(temp.path()).unwrap().dir;
        let token = Secret::generate().unwrap();
        let info = RunInfo::new(
            "127.0.0.1:4242".parse().unwrap(),
            "http://127.0.0.1:4242/".to_string(),
        )
        .with_operator_token(token.clone());
        info.write(&dir).unwrap();
        let read = RunInfo::read(&dir).unwrap().unwrap();
        assert_eq!(
            read.operator_token.as_ref().map(Secret::expose),
            Some(token.expose())
        );
        assert!(!format!("{read:?}").contains(token.expose()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.run_path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
