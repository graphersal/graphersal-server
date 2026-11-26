//! The server directory's OS permissions: the boundary of the operator's power.
//!
//! Whoever can write the server directory can edit `access.toml` (make themselves an admin),
//! read `server.run` (the operator token of the running server) or copy the Stores, whatever
//! the server checks over HTTP. So the OS user owning the directory IS the operator (the
//! PostgreSQL peer-authentication model): the console acts on the directory without a token
//! because only that user can, and the server refuses to start on a directory others could
//! change. Like ssh with `~/.ssh`:
//!
//! - `init` creates the directory with mode `0700`, `sessions/` `0700`, and every secret file
//!   (`access.toml`, `server.run`, `sessions/*.json`) `0600`; every later write keeps these
//!   modes ([`crate::layout`]'s private writes). `init` on an existing server directory owned
//!   by the current user tightens the modes it finds too open (the upgrade path).
//! - [`check`] runs before `serve` and every console command (offline and through a running
//!   server with the operator token): the directory, `server.toml`, `aliases.toml` and `mcp.toml` must be
//!   owned by the process's effective user and not writable by group or others; the secrets
//!   (`access.toml`, `server.run`, `sessions/` and its files) must be owned by it and neither
//!   readable nor writable by group or others. A missing file is fine. A refusal is
//!   [`ServerError::InsecureDir`], naming the exact `chmod`/`chown` commands. `root` on a
//!   directory owned by another user is refused too (run as the owner: `sudo -u OWNER ...`).
//! - The console's `--token` path sends a command to a running server with a user's admin
//!   token and touches no file of the directory but reading `server.run`'s address, so no
//!   check runs there: the token is the credential.
//!
//! Windows: no check (the directory's ACL is the boundary: create it where only the service
//! account may write; the files are written with the directory's inherited ACL).

use std::fs;
#[cfg(unix)]
use std::io;
use std::path::{Path, PathBuf};

use crate::error::ServerError;
use crate::layout::ServerDir;

/// The OS user name of this process (its effective user on unix: `getpwuid_r`, else
/// `uid <n>`; Windows: `USERNAME`), for audit records and refusals. `None` when it is unknown.
pub fn os_user() -> Option<String> {
    #[cfg(unix)]
    {
        let uid = euid();
        Some(user_name(uid).unwrap_or_else(|| format!("uid {uid}")))
    }
    #[cfg(not(unix))]
    {
        std::env::var("USERNAME")
            .ok()
            .filter(|name| !name.is_empty())
    }
}

/// Refuses a server directory that is not private to the current OS user (module docs).
///
/// `Err`: [`ServerError::InsecureDir`] with every problem found and the commands fixing them;
/// [`ServerError::Io`] when a file's status cannot be read. A missing directory passes (the
/// caller's open refuses it for what it is). Windows: always `Ok`.
pub fn check(dir: &ServerDir) -> Result<(), ServerError> {
    #[cfg(unix)]
    {
        check_as(dir, euid())
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Refuses a server directory owned by another OS user than the current one (the first part
/// of [`check`]; `init` on an existing directory). Windows: always `Ok`.
pub(crate) fn check_owner(dir: &ServerDir) -> Result<(), ServerError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let euid = euid();
        match fs::metadata(dir.root()) {
            Ok(meta) if meta.uid() != euid => Err(foreign_owner(dir.root(), meta.uid(), euid)),
            Ok(_) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(ServerError::io("read", dir.root(), err)),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// What a checked path is.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// The server directory, `server.toml`, `aliases.toml`, `mcp.toml`: owner only may write.
    Shared,
    /// A secret file (`access.toml`, `server.run`, session files): owner only, `0600`.
    SecretFile,
    /// A directory of secrets (`sessions/`): owner only, `0700`.
    SecretDir,
}

#[cfg(unix)]
impl Kind {
    /// The mode bits that must be clear.
    fn forbidden(self) -> u32 {
        match self {
            Kind::Shared => 0o022,
            Kind::SecretFile | Kind::SecretDir => 0o077,
        }
    }

    /// The mode `init` and the `chmod` help set.
    fn mode(self, is_dir: bool) -> u32 {
        match self {
            Kind::Shared if is_dir => 0o700,
            Kind::Shared => 0o644,
            Kind::SecretFile => 0o600,
            Kind::SecretDir => 0o700,
        }
    }
}

/// The paths [`check`] looks at, with their kind (the session files are listed by
/// [`checked_paths`]).
#[cfg(unix)]
fn checked_paths(dir: &ServerDir) -> Result<Vec<(PathBuf, Kind)>, ServerError> {
    let mut paths = vec![
        (dir.root().to_path_buf(), Kind::Shared),
        (dir.config_path(), Kind::Shared),
        (dir.aliases_path(), Kind::Shared),
        (dir.mcp_settings_path(), Kind::Shared),
        (dir.access_path(), Kind::SecretFile),
        (dir.run_path(), Kind::SecretFile),
        (dir.sessions(), Kind::SecretDir),
    ];
    let sessions = dir.sessions();
    match fs::read_dir(&sessions) {
        Ok(entries) => {
            let mut files: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
            files.sort();
            paths.extend(files.into_iter().map(|path| (path, Kind::SecretFile)));
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(ServerError::io("read directory", &sessions, err)),
    }
    Ok(paths)
}

/// [`check`] as the effective user `euid` (tests pretend to be another user).
#[cfg(unix)]
pub(crate) fn check_as(dir: &ServerDir, euid: u32) -> Result<(), ServerError> {
    use std::os::unix::fs::MetadataExt;
    // The directory first: when it is someone else's, nothing below it is ours to look at.
    let root = dir.root();
    match fs::metadata(root) {
        Ok(meta) if meta.uid() != euid => {
            return Err(foreign_owner(root, meta.uid(), euid));
        }
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(ServerError::io("read", root, err)),
    }
    let mut problems = Vec::new();
    let mut fixes = Vec::new();
    for (path, kind) in checked_paths(dir)? {
        let meta = match fs::metadata(&path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(ServerError::io("read", &path, err)),
        };
        if meta.uid() != euid {
            problems.push(format!(
                "{} is owned by {}, not by {}",
                path.display(),
                user_text(meta.uid()),
                user_text(euid)
            ));
            fixes.push((
                "make it yours (the directory's owner)".to_string(),
                format!("sudo chown {} {}", user_word(euid), word(&path)),
            ));
            continue;
        }
        let mode = meta.mode() & 0o7777;
        if mode & kind.forbidden() != 0 {
            let what = match kind {
                Kind::Shared => "writable by group or others",
                Kind::SecretFile | Kind::SecretDir => "open to group or others",
            };
            let holds = match kind {
                Kind::Shared if meta.is_dir() => "",
                Kind::Shared => " (it configures the server)",
                Kind::SecretFile | Kind::SecretDir => " (it holds token hashes or tokens)",
            };
            problems.push(format!(
                "{} is {what} (mode {mode:04o}){holds}",
                path.display()
            ));
            fixes.push((
                format!("owner only: mode {:04o}", kind.mode(meta.is_dir())),
                format!("chmod {:o} {}", kind.mode(meta.is_dir()), word(&path)),
            ));
        }
    }
    if problems.is_empty() {
        return Ok(());
    }
    fixes.push((
        "or let init set every mode at once".to_string(),
        format!("graphersal-server init {}", word(root)),
    ));
    Err(ServerError::InsecureDir {
        dir: root.to_path_buf(),
        problems,
        fixes,
    })
}

/// The refusal of a directory owned by another user than `euid` (root included).
#[cfg(unix)]
fn foreign_owner(root: &Path, owner: u32, euid: u32) -> ServerError {
    let problem = if euid == 0 {
        format!(
            "{} is owned by {}; root does not act on another user's server directory",
            root.display(),
            user_text(owner)
        )
    } else {
        format!(
            "{} is owned by {}, not by {}: only the directory's owner is its operator",
            root.display(),
            user_text(owner),
            user_text(euid)
        )
    };
    ServerError::InsecureDir {
        dir: root.to_path_buf(),
        problems: vec![problem],
        fixes: vec![
            (
                "run the command as the directory's owner".to_string(),
                format!("sudo -u {} graphersal-server ...", user_word(owner)),
            ),
            (
                "or, with an admin token, through its running server".to_string(),
                "graphersal-server ... --token TOKEN --server ADDRESS".to_string(),
            ),
            (
                "or make the directory yours".to_string(),
                format!("sudo chown -R {} {}", user_word(euid), word(root)),
            ),
        ],
    }
}

/// Sets the owner-only modes on what `dir` holds (module docs): the directory and `sessions/`
/// `0700`, the secret files `0600`, `server.toml`/`aliases.toml`/`mcp.toml` without group/other write.
/// Only paths owned by `euid` whose mode is too open are changed; answers them. Windows:
/// nothing.
pub(crate) fn tighten(dir: &ServerDir) -> Result<Vec<PathBuf>, ServerError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let euid = euid();
        let mut changed = Vec::new();
        for (path, kind) in checked_paths(dir)? {
            let meta = match fs::metadata(&path) {
                Ok(meta) => meta,
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                Err(err) => return Err(ServerError::io("read", &path, err)),
            };
            if meta.uid() != euid {
                continue;
            }
            let mode = meta.mode() & 0o7777;
            // The root always becomes 0700 (`init`'s mode); the rest only loses what is too open.
            let target = if path == dir.root() {
                0o700
            } else if mode & kind.forbidden() != 0 {
                kind.mode(meta.is_dir())
            } else {
                continue;
            };
            if mode == target {
                continue;
            }
            fs::set_permissions(&path, fs::Permissions::from_mode(target))
                .map_err(|err| ServerError::io("set the permissions of", &path, err))?;
            changed.push(path);
        }
        Ok(changed)
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(Vec::new())
    }
}

/// Creates the directory `path` (and its parents) for its owner only: mode `0700` on unix,
/// also when it was there with a wider mode. Windows: a plain `create_dir_all`.
pub(crate) fn create_private_dir(path: &Path) -> Result<(), ServerError> {
    fs::create_dir_all(path).map_err(|err| ServerError::io("create directory", path, err))?;
    set_private_dir(path)
}

/// Sets mode `0700` on the directory `path` (unix; Windows: nothing).
pub(crate) fn set_private_dir(path: &Path) -> Result<(), ServerError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|err| ServerError::io("set the permissions of", path, err))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// The process's effective user id.
#[cfg(unix)]
pub(crate) fn euid() -> u32 {
    // SAFETY: `geteuid` takes no arguments, cannot fail and touches no memory of ours.
    unsafe { libc::geteuid() }
}

/// The name of the user `uid` (`getpwuid_r`), `None` when the user database has none.
#[cfg(unix)]
fn user_name(uid: u32) -> Option<String> {
    let mut size = 1024usize;
    loop {
        let mut buf: Vec<libc::c_char> = vec![0; size];
        // SAFETY: `passwd` is a plain C struct; all-zero is a valid value to be overwritten.
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: every pointer is valid for the call: `pwd` and `result` live on this stack
        // frame, `buf` holds `buf.len()` bytes; the strings `pwd` points to live in `buf`,
        // which outlives their use below.
        let rc =
            unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result) };
        if rc == libc::ERANGE && size < 1 << 20 {
            size *= 4;
            continue;
        }
        if rc != 0 || result.is_null() || pwd.pw_name.is_null() {
            return None;
        }
        // SAFETY: `pw_name` is a NUL-terminated string inside `buf` (checked non-null above).
        let name = unsafe { std::ffi::CStr::from_ptr(pwd.pw_name) };
        return Some(name.to_string_lossy().into_owned());
    }
}

/// `alice (uid 501)`, or `uid 501` for a user without a name.
#[cfg(unix)]
fn user_text(uid: u32) -> String {
    match user_name(uid) {
        Some(name) => format!("{name} (uid {uid})"),
        None => format!("uid {uid}"),
    }
}

/// The user `uid` as a command word: its name, else the number.
#[cfg(unix)]
fn user_word(uid: u32) -> String {
    user_name(uid).unwrap_or_else(|| uid.to_string())
}

/// `path` as one shell word.
#[cfg(unix)]
fn word(path: &Path) -> String {
    crate::console::shell_word(path)
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::test_support::TempDir;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn set(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn insecure(err: ServerError) -> (Vec<String>, Vec<(String, String)>) {
        match err {
            ServerError::InsecureDir {
                problems, fixes, ..
            } => (problems, fixes),
            other => panic!("not an InsecureDir error: {other}"),
        }
    }

    #[test]
    fn init_makes_the_directory_and_its_secrets_owner_only() {
        let temp = TempDir::new("security-init");
        let root = temp.path().join("srv");
        let dir = ServerDir::init(&root).unwrap().dir;
        assert_eq!(mode(&root), 0o700);
        assert_eq!(mode(&dir.sessions()), 0o700);
        assert_eq!(mode(&dir.access_path()), 0o600);
        check(&dir).unwrap();
        // An existing empty directory becomes 0700 too.
        let empty = temp.path().join("empty");
        fs::create_dir(&empty).unwrap();
        set(&empty, 0o755);
        ServerDir::init(&empty).unwrap();
        assert_eq!(mode(&empty), 0o700);
    }

    #[test]
    fn every_too_open_mode_is_refused_with_its_chmod() {
        let temp = TempDir::new("security-modes");
        let dir = ServerDir::init(temp.path().join("srv")).unwrap().dir;
        crate::RunInfo::new("127.0.0.1:1".parse().unwrap(), "http://127.0.0.1:1/".into())
            .write(&dir)
            .unwrap();
        fs::write(dir.sessions().join("browser.json"), "{}").unwrap();
        set(&dir.sessions().join("browser.json"), 0o600);
        check(&dir).unwrap();
        let cases: Vec<(PathBuf, u32, &str, &str)> = vec![
            (
                dir.root().to_path_buf(),
                0o770,
                "writable by group or others",
                "chmod 700",
            ),
            (
                dir.root().to_path_buf(),
                0o707,
                "writable by group or others",
                "chmod 700",
            ),
            (
                dir.config_path(),
                0o664,
                "writable by group or others",
                "chmod 644",
            ),
            (
                dir.access_path(),
                0o640,
                "open to group or others",
                "chmod 600",
            ),
            (
                dir.access_path(),
                0o604,
                "open to group or others",
                "chmod 600",
            ),
            (
                dir.run_path(),
                0o644,
                "open to group or others",
                "chmod 600",
            ),
            (
                dir.sessions(),
                0o750,
                "open to group or others",
                "chmod 700",
            ),
            (
                dir.sessions().join("browser.json"),
                0o660,
                "open to group or others",
                "chmod 600",
            ),
        ];
        for (path, bad, words, fix) in cases {
            let good = mode(&path);
            set(&path, bad);
            let err = check(&dir).unwrap_err();
            let text = crate::console::error_text(&err);
            let (problems, fixes) = insecure(err);
            assert_eq!(problems.len(), 1, "{problems:?}");
            assert!(problems[0].contains(words), "{problems:?}");
            assert!(problems[0].contains(&format!("{bad:04o}")), "{problems:?}");
            assert_eq!(fixes[0].1, format!("{fix} {}", path.display()), "{fixes:?}");
            assert!(
                text.contains(&format!("\n  {fix} {}\n", path.display())),
                "{text}"
            );
            assert!(text.contains("graphersal-server init"), "{text}");
            set(&path, good);
            check(&dir).unwrap();
        }
        // Several at once: every one named.
        set(&dir.access_path(), 0o644);
        set(&dir.run_path(), 0o644);
        let (problems, fixes) = insecure(check(&dir).unwrap_err());
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert_eq!(fixes.len(), 3, "{fixes:?}");
        // init tightens them (the upgrade path) and the check passes again.
        set(dir.root(), 0o755);
        let report = ServerDir::init(dir.root()).unwrap();
        assert_eq!(
            report.secured,
            vec![dir.root().to_path_buf(), dir.access_path(), dir.run_path()]
        );
        assert_eq!(mode(dir.root()), 0o700);
        check(&dir).unwrap();
    }

    #[test]
    fn a_group_readable_directory_passes_but_init_closes_it() {
        let temp = TempDir::new("security-readable");
        let dir = ServerDir::init(temp.path().join("srv")).unwrap().dir;
        set(dir.root(), 0o755);
        check(&dir).unwrap();
        assert_eq!(tighten(&dir).unwrap(), vec![dir.root().to_path_buf()]);
        assert_eq!(mode(dir.root()), 0o700);
        assert!(tighten(&dir).unwrap().is_empty());
    }

    #[test]
    fn another_users_directory_is_refused_root_included() {
        let temp = TempDir::new("security-owner");
        let dir = ServerDir::init(temp.path().join("srv")).unwrap().dir;
        let me = euid();
        // Pretend to be another user.
        let other = if me == 4242 { 4243 } else { 4242 };
        let err = check_as(&dir, other).unwrap_err();
        let text = crate::console::error_text(&err);
        let (problems, fixes) = insecure(err);
        assert!(
            problems[0].contains("only the directory's owner is its operator"),
            "{problems:?}"
        );
        assert!(problems[0].contains(&format!("uid {me}")), "{problems:?}");
        assert!(fixes[0].1.starts_with("sudo -u "), "{fixes:?}");
        assert!(fixes[1].1.contains("--token TOKEN"), "{fixes:?}");
        assert!(fixes[2].1.starts_with("sudo chown -R "), "{fixes:?}");
        assert!(text.contains("Help: "), "{text}");
        // Root on a directory owned by another user: refused too.
        if me != 0 {
            let (problems, _) = insecure(check_as(&dir, 0).unwrap_err());
            assert!(
                problems[0].contains("root does not act on another user's server directory"),
                "{problems:?}"
            );
        }
        // A missing directory passes (its open refuses it).
        check(&ServerDir::new(temp.path().join("missing"))).unwrap();
    }

    #[test]
    fn the_os_user_has_a_name() {
        let user = os_user().unwrap();
        assert!(!user.is_empty());
        assert_eq!(
            Some(user),
            user_name(euid()).or(Some(format!("uid {}", euid())))
        );
    }
}
