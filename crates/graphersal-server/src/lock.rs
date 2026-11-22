//! `server.lock`: at most one running server per server directory.
//!
//! The lock is the OS's advisory file lock (`fd-lock`: `flock` on Unix, `LockFileEx` on
//! Windows) on `server.lock`, held while the [`ServerLock`] lives and released by the OS when
//! the process ends, also on a crash. The file itself stays (an unlocked file means nothing).

use std::fs::{File, OpenOptions};
use std::io;

use crate::error::ServerError;
use crate::layout::ServerDir;

/// The held lock of a server directory; dropping it releases the lock.
#[derive(Debug)]
pub struct ServerLock {
    /// The locked file: the lock lives as long as it stays open (the guard was forgotten).
    _file: fd_lock::RwLock<File>,
}

impl ServerLock {
    /// Takes the lock of `dir`: [`ServerError::Locked`] when another server (in this or another
    /// process) holds it.
    pub fn acquire(dir: &ServerDir) -> Result<Self, ServerError> {
        let path = dir.lock_path();
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|err| ServerError::io("open", &path, err))?;
        let mut lock = fd_lock::RwLock::new(file);
        match lock.try_write() {
            // The lock lives as long as the file stays open.
            Ok(guard) => std::mem::forget(guard),
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                return Err(ServerError::Locked {
                    path: dir.root().to_path_buf(),
                });
            }
            Err(err) => return Err(ServerError::io("lock", &path, err)),
        }
        Ok(Self { _file: lock })
    }

    /// Whether a server holds the lock of `dir` now (the lock is taken and released at once
    /// when it is free). A directory without `server.lock` is not locked.
    pub fn is_held(dir: &ServerDir) -> Result<bool, ServerError> {
        if !dir.lock_path().exists() {
            return Ok(false);
        }
        match Self::acquire(dir) {
            Ok(_) => Ok(false),
            Err(ServerError::Locked { .. }) => Ok(true),
            Err(err) => Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn one_holder_at_a_time() {
        let temp = TempDir::new("lock");
        let dir = ServerDir::init(temp.path()).unwrap().dir;
        assert!(!ServerLock::is_held(&dir).unwrap());
        let held = ServerLock::acquire(&dir).unwrap();
        let err = ServerLock::acquire(&dir).unwrap_err();
        assert!(matches!(err, ServerError::Locked { .. }), "{err}");
        assert!(err.to_string().contains("already serves"), "{err}");
        assert!(ServerLock::is_held(&dir).unwrap());
        drop(held);
        assert!(!ServerLock::is_held(&dir).unwrap());
        let _again = ServerLock::acquire(&dir).unwrap();
    }
}
