//! Helpers of the unit tests.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The live test directories ([`TempDir`]), where [`operator_token`] looks for a server's
/// run file.
static DIRS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// A fresh directory under the system's temporary directory, removed on drop.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    /// A new empty directory named after `name`, this process and a counter.
    pub(crate) fn new(name: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "graphersal-server-unit-{name}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create a test directory");
        DIRS.lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(path.clone());
        Self(path)
    }

    /// The directory.
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        DIRS.lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|dir| dir != &self.0);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The operator token of the server listening on `address` that a test started in one of the
/// live [`TempDir`]s (its `server.run`, in the directory or one level below): the tests that
/// talk to the admin API and the databases send it as the admin's credentials. `None` when no
/// such server runs.
pub(crate) fn operator_token(address: SocketAddr) -> Option<String> {
    let dirs = DIRS.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let mut candidates = Vec::new();
    for dir in dirs {
        if let Ok(children) = std::fs::read_dir(&dir) {
            candidates.extend(children.flatten().map(|child| child.path()));
        }
        candidates.push(dir);
    }
    candidates.into_iter().find_map(|root| {
        let info = crate::RunInfo::read(&crate::ServerDir::new(root)).ok()??;
        (info.address == address)
            .then(|| info.operator_token.map(|token| token.expose().to_string()))
            .flatten()
    })
}

/// The `Authorization` header line of the admin (the operator token of the server at
/// `address`, [`operator_token`]); empty when no such server runs.
pub(crate) fn admin_header(address: SocketAddr) -> String {
    operator_token(address)
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default()
}

/// Whether this run is on a CI runner (`CI` set and not empty, "false" or "0"), as in
/// graphersal's `tests/all/ci_support.rs`. A test whose result depends on how much CPU time or
/// memory the machine gives it (an upper bound on wall-clock time) returns early there.
pub(crate) fn skip_on_ci(what: &str) -> bool {
    let on_ci = std::env::var("CI").is_ok_and(|v| !v.is_empty() && v != "false" && v != "0");
    if on_ci {
        eprintln!("skipped on CI ({what}): depends on the runner's CPU or memory");
    }
    on_ci
}
