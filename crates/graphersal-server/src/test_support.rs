//! Helpers of the unit tests.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

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
        Self(path)
    }

    /// The directory.
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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
