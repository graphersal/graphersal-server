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
use crate::layout::{ServerDir, write_atomic_private_volatile};

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
    /// When the server started, in Unix milliseconds (`graphersal-server status` shows the
    /// uptime even when the server does not answer); `None` in a run file of an older server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
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
            started_at_ms: None,
        }
    }

    /// The run information with the operator token `token`.
    pub fn with_operator_token(mut self, token: Secret) -> Self {
        self.operator_token = Some(token);
        self
    }

    /// The run information with the start time `ms` (Unix milliseconds).
    pub fn with_started_at_ms(mut self, ms: u64) -> Self {
        self.started_at_ms = Some(ms);
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
        write_atomic_private_volatile(&path, text.as_bytes())
    }

    /// Reads the run file of `dir`: `None` when there is none.
    pub fn read(dir: &ServerDir) -> Result<Option<Self>, ServerError> {
        let path = dir.run_path();
        let text = match crate::layout::read_replaced(&path) {
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

    /// The run file of `dir` when a user token may be sent to the address it names (SEC L2):
    /// `None` without one; [`ServerError::Connect`] when no server holds `server.lock` (the
    /// file is stale: the server is gone and another program may hold its port now) or when
    /// the file is not owned by the directory's owner (unix). The token is then never sent.
    pub fn read_live(dir: &ServerDir) -> Result<Option<Self>, ServerError> {
        let Some(info) = Self::read(dir)? else {
            return Ok(None);
        };
        let refused = |message: String| ServerError::Connect {
            address: info.address.to_string(),
            message,
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let uid = |path: &std::path::Path| {
                fs::metadata(path)
                    .map(|meta| meta.uid())
                    .map_err(|err| ServerError::io("read", path, err))
            };
            check_run_owner(uid(&dir.run_path())?, uid(dir.root())?).map_err(refused)?;
        }
        if !crate::lock::ServerLock::is_held(dir)? {
            return Err(refused(format!(
                "{} names {}, but no server holds server.lock: that server is gone (the file \
                 is stale) and another program may listen there now, so the token is not \
                 sent; start the server again, or name its address with --server ADDRESS",
                dir.run_path().display(),
                info.address
            )));
        }
        Ok(Some(info))
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

/// `Err` (the refusal's text) when the run file's owner `run_uid` is not the directory's
/// owner `dir_uid`: another OS user wrote it, so its address is not trusted with a token.
#[cfg(unix)]
fn check_run_owner(run_uid: u32, dir_uid: u32) -> Result<(), String> {
    if run_uid == dir_uid {
        return Ok(());
    }
    Err(format!(
        "server.run belongs to the OS user {run_uid}, not to the directory's owner \
         {dir_uid}: its address is not trusted with a token; name the server's address with \
         --server ADDRESS"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    /// SEC L2: a token goes to the address of `server.run` only while a server holds the
    /// lock and the file is the directory owner's.
    #[test]
    fn a_stale_or_foreign_run_file_is_not_trusted_with_a_token() {
        let temp = TempDir::new("run-file-live");
        let dir = ServerDir::init(temp.path()).unwrap().dir;
        assert_eq!(RunInfo::read_live(&dir).unwrap(), None);
        let info = RunInfo::new(
            "127.0.0.1:4242".parse().unwrap(),
            "http://127.0.0.1:4242/".to_string(),
        );
        info.write(&dir).unwrap();
        let err = RunInfo::read_live(&dir).unwrap_err().to_string();
        assert!(err.contains("no server holds server.lock"), "{err}");
        assert!(err.contains("--server ADDRESS"), "{err}");
        let lock = crate::lock::ServerLock::acquire(&dir).unwrap();
        assert_eq!(RunInfo::read_live(&dir).unwrap(), Some(info));
        drop(lock);
        #[cfg(unix)]
        {
            assert!(check_run_owner(501, 501).is_ok());
            let err = check_run_owner(1000, 501).unwrap_err();
            assert!(err.contains("not trusted"), "{err}");
        }
    }

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
