//! `graphersal-server start|stop|status --dir DIR` (round 3 B2): a server in the background
//! without hunting for its pid.
//!
//! - [`start`] checks the directory ([`crate::security::check`]), refuses when a server already
//!   holds it (`server.lock`), creates the first admin when there is no `access.toml` (here, so
//!   that its token is printed once on the terminal and never lands in the log), then spawns
//!   `serve --dir DIR` with the given `serve` options as a detached process: on Unix in a new
//!   session (`setsid`, no controlling terminal, so closing the terminal does not stop it), on
//!   Windows with `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` (no fork anywhere; this
//!   process's own console handles are made non-inheritable first, so the server does not
//!   hold `start`'s stdout open); stdin is
//!   null, stdout and stderr are appended to `DIR/logs/server.log` (`logs/` mode `0700`, the
//!   log `0600` on unix). It waits until the new process wrote its `server.run`, answers
//!   `GET /readyz` with `200` and printed its address line, then hands back what the server
//!   printed since (the usual start block). A process that exits before, or does not get ready
//!   within the timeout, is [`ServerError::Service`] with the log's last lines (a server still
//!   starting after the timeout keeps running; the error says so).
//! - [`stop`] asks the running server to stop (`POST /api/admin/shutdown` with the operator
//!   token of `server.run`, `docs/admin-api.md`); the server stops as on Ctrl+C. When the
//!   server cannot be reached (or is too old to know the route) the fallback on Unix is
//!   SIGTERM to the pid of `server.run` (the same graceful path); on Windows the error names
//!   `taskkill`. It waits until `server.lock` is free. No server: [`Stopped::NotRunning`]
//!   (a stale `server.run` left by a server that did not stop cleanly is removed, under the
//!   lock).
//! - [`status`] tells whether a server holds the directory, and what it answers on
//!   `GET /api/admin/server` (pid, address, uptime, version, databases, memory). A stale
//!   `server.run` (no lock held) is reported, not removed.
//!
//! [`stop_with_token`] and [`status_with_token`] do the same through the server's address
//! with a user's admin token (`--token`), touching no file of the directory: stop waits until
//! the address stops answering.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::access::{FirstAdmin, OPERATOR, ensure_first_admin};
use crate::audit::Audit;
use crate::error::ServerError;
use crate::layout::ServerDir;
use crate::lock::ServerLock;
use crate::run_file::RunInfo;

/// How long [`start`] waits for the server to get ready by default.
pub const START_TIMEOUT: Duration = Duration::from_secs(60);
/// How long [`stop`] waits for the server to stop by default (running requests get 5 s, then
/// every database is unloaded).
pub const STOP_TIMEOUT: Duration = Duration::from_secs(60);

/// How many lines of the log an error shows.
const TAIL_LINES: usize = 30;

/// The line `serve` prints on stdout once it serves (`console::address_line`).
pub(crate) const ADDRESS_PREFIX: &str = "Graphersal server: ";

/// What [`start`] started.
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug)]
#[non_exhaustive]
pub struct Started {
    /// The server's process id.
    pub pid: u32,
    /// Its run file (address, URL, version; the operator token removed).
    pub run: RunInfo,
    /// Its log file.
    pub log: PathBuf,
    /// What the server printed since it was started (its start block and address line).
    pub output: String,
    /// The first admin created for a directory without `access.toml`: its token, to be shown
    /// once.
    pub first_admin: Option<FirstAdmin>,
}

/// What [`stop`] did.
///
/// `#[non_exhaustive]`: further outcomes may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Stopped {
    /// The server stopped.
    #[non_exhaustive]
    Stopped {
        /// Its process id, when known.
        pid: Option<u32>,
        /// Its URL, when known.
        url: Option<String>,
        /// Whether it was stopped by SIGTERM because its admin API could not be used.
        signalled: bool,
    },
    /// No server ran.
    #[non_exhaustive]
    NotRunning {
        /// The stale `server.run` found (and removed) without a server, if any.
        stale: Option<RunInfo>,
        /// The address that did not answer (`--token` without a lock to look at).
        address: Option<SocketAddr>,
    },
}

/// What [`status`] found.
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Status {
    /// Whether a server runs (holds the lock; with a token: answers).
    pub running: bool,
    /// The run file (the operator token removed): the running server's, or a stale one.
    pub run: Option<RunInfo>,
    /// Whether [`Self::run`] is stale: no server holds the lock.
    pub stale: bool,
    /// The server's answer to `GET /api/admin/server`.
    pub server: Option<Value>,
    /// Why the server did not answer it, when it runs but did not.
    pub unanswered: Option<String>,
    /// The log of `start`, when there is one.
    pub log: Option<PathBuf>,
}

/// Starts a server on `dir` in the background (module docs): `program` is the
/// `graphersal-server` binary, `serve_args` the `serve` options after `--dir DIR`.
///
/// `Err`: [`ServerError::InsecureDir`], the errors of opening the directory,
/// [`ServerError::Service`] when a server already runs or the new one failed.
pub fn start(
    dir: &Path,
    program: &Path,
    serve_args: &[String],
    timeout: Duration,
) -> Result<Started, ServerError> {
    crate::security::check(&ServerDir::new(dir))?;
    let root = std::path::absolute(dir).map_err(|err| ServerError::io("resolve", dir, err))?;
    let dir = ServerDir::open(&root)?;
    let lock = match ServerLock::acquire(&dir) {
        Ok(lock) => lock,
        Err(ServerError::Locked { .. }) => return Err(already_running(&dir)),
        Err(err) => return Err(err),
    };
    let audit = Audit::default()
        .with_actor(Some(OPERATOR.to_string()))
        .with_os_user(crate::security::os_user());
    let first_admin = ensure_first_admin(&dir, &audit)?;
    let started = spawn(&dir, program, serve_args, timeout, lock);
    if started.is_err() {
        // A first admin nobody saw must not close the directory for good (as `serve`).
        if let Some(first) = &first_admin {
            let _ = fs::remove_file(&first.path);
        }
    }
    let mut started = started?;
    started.first_admin = first_admin;
    Ok(started)
}

/// The refusal of [`start`] on a directory a server holds.
fn already_running(dir: &ServerDir) -> ServerError {
    let run = RunInfo::read(dir).ok().flatten();
    let what = match &run {
        Some(run) => format!(" (pid {}, {})", run.pid, run.url),
        None => String::new(),
    };
    let arg = dir_arg(dir);
    ServerError::Service {
        message: format!(
            "a graphersal-server already runs on {}{what}",
            dir.root().display()
        ),
        help: Some(format!(
            "see it with `graphersal-server status --dir {arg}`; stop it with \
             `graphersal-server stop --dir {arg}`"
        )),
        log_tail: Vec::new(),
    }
}

/// The directory as a shell word for the help texts.
fn dir_arg(dir: &ServerDir) -> String {
    crate::console::shell_word(dir.root())
}

/// Spawns `serve` (the lock is released just before) and waits for it (module docs).
fn spawn(
    dir: &ServerDir,
    program: &Path,
    serve_args: &[String],
    timeout: Duration,
    lock: ServerLock,
) -> Result<Started, ServerError> {
    let log_path = dir.log_path();
    crate::security::create_private_dir(&dir.logs())?;
    let mut log = open_log(&log_path)?;
    let mut line = format!(
        "\n== {} graphersal-server start: serve --dir {}",
        crate::database::utc_now(),
        dir.root().display()
    );
    for arg in serve_args {
        line.push(' ');
        line.push_str(arg);
    }
    if let Some(user) = crate::security::os_user() {
        line.push_str(&format!(" (OS user {user})"));
    }
    line.push_str(" ==\n");
    log.write_all(line.as_bytes())
        .and_then(|()| log.flush())
        .map_err(|err| ServerError::io("write", &log_path, err))?;
    let offset = log
        .metadata()
        .map_err(|err| ServerError::io("read", &log_path, err))?
        .len();
    let out = log
        .try_clone()
        .map_err(|err| ServerError::io("open", &log_path, err))?;
    let mut command = Command::new(program);
    command
        .arg("serve")
        .arg("--dir")
        .arg(dir.root())
        .args(serve_args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(log));
    detach(&mut command);
    // The new server takes the lock itself.
    drop(lock);
    let mut child = command.spawn().map_err(|err| ServerError::Service {
        message: format!("cannot start {}: {err}", program.display()),
        help: None,
        log_tail: Vec::new(),
    })?;
    let pid = child.id();
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(ServerError::Service {
                    message: format!("the server exited before it was ready ({status})"),
                    help: Some(format!("the whole log: {}", log_path.display())),
                    log_tail: log_tail(&log_path, offset),
                });
            }
            Ok(None) => {}
            Err(err) => return Err(ServerError::io("wait for", program, err)),
        }
        if let Ok(Some(mut run)) = RunInfo::read(dir)
            && run.pid == pid
            && is_ready(run.address)
        {
            let output = log_since(&log_path, offset);
            if output.lines().any(|line| line.starts_with(ADDRESS_PREFIX)) {
                run.operator_token = None;
                return Ok(Started {
                    pid,
                    run,
                    log: log_path,
                    output,
                    first_admin: None,
                });
            }
        }
        if Instant::now() >= deadline {
            let arg = dir_arg(dir);
            return Err(ServerError::Service {
                message: format!(
                    "the server (pid {pid}) did not get ready within {} s; it keeps running \
                     (a large registry may still be scanning)",
                    timeout.as_secs()
                ),
                help: Some(format!(
                    "watch it with `graphersal-server status --dir {arg}` and its log {}; \
                     stop it with `graphersal-server stop --dir {arg}`",
                    log_path.display()
                )),
                log_tail: log_tail(&log_path, offset),
            });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Opens the log for appending (created owner only).
fn open_log(path: &Path) -> Result<File, ServerError> {
    let mut options = OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|err| ServerError::io("open", path, err))
}

/// Makes `command` start a process that outlives this one and its terminal (module docs).
fn detach(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: the closure runs in the child between fork and exec and calls only `setsid`,
        // which is async-signal-safe, takes no arguments and touches no memory of ours.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        keep_std_handles_from_children();
    }
    #[cfg(not(any(unix, windows)))]
    let _ = command;
}

/// Makes this process's own stdin, stdout and stderr handles non-inheritable (Windows).
///
/// A process inherits every inheritable handle of its parent, not only the three it is given:
/// the server would keep this console's stdout pipe open, and whoever reads `start`'s output
/// to its end (a script, `Command::output`) would wait until the server exits. `start` ends
/// right after the spawn, so nothing else needs these flags. Unix needs nothing: the child's
/// descriptors 0-2 are replaced and std opens every other one close-on-exec.
#[cfg(windows)]
fn keep_std_handles_from_children() {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn SetHandleInformation(handle: *mut c_void, mask: u32, flags: u32) -> i32;
    }
    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: plain Win32 calls with integer arguments and a handle the OS gave us; an
        // invalid or null handle is skipped, a failure only leaves the flag as it was.
        unsafe {
            let handle = GetStdHandle(which);
            if !handle.is_null() && handle as isize != -1 {
                let _ = SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// What the log holds after `offset` (lossy UTF-8; empty when it cannot be read).
fn log_since(path: &Path, offset: u64) -> String {
    let mut text = Vec::new();
    let read = File::open(path).and_then(|mut file| {
        file.seek(SeekFrom::Start(offset))?;
        file.take(16 << 20).read_to_end(&mut text)
    });
    match read {
        Ok(_) => String::from_utf8_lossy(&text).into_owned(),
        Err(_) => String::new(),
    }
}

/// The last lines of the log after `offset`.
fn log_tail(path: &Path, offset: u64) -> Vec<String> {
    let text = log_since(path, offset);
    let lines: Vec<&str> = text.lines().collect();
    let from = lines.len().saturating_sub(TAIL_LINES);
    lines[from..].iter().map(|line| line.to_string()).collect()
}

/// `address` as this machine connects to it (a wildcard bind through the loopback).
fn connectable(address: SocketAddr) -> SocketAddr {
    match address.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), address.port())
        }
        IpAddr::V6(ip) if ip.is_unspecified() => {
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), address.port())
        }
        _ => address,
    }
}

/// Whether the server at `address` answers `GET /readyz` with `200`.
fn is_ready(address: SocketAddr) -> bool {
    matches!(
        crate::client::request(connectable(address), None, "GET", "/readyz", None),
        Ok((200, _))
    )
}

/// Stops the server running on `dir` (module docs), waiting at most `timeout`.
///
/// `Err`: [`ServerError::InsecureDir`], the errors of opening the directory, a refusal of
/// the server ([`ServerError::Remote`]), [`ServerError::Service`] when it did not stop in time.
pub fn stop(dir: &Path, timeout: Duration) -> Result<Stopped, ServerError> {
    crate::security::check(&ServerDir::new(dir))?;
    let dir = ServerDir::open(dir)?;
    match ServerLock::acquire(&dir) {
        Ok(lock) => {
            // No server: a run file left behind is stale; removed while the lock keeps a new
            // server from writing its own meanwhile.
            let stale = RunInfo::read(&dir).ok().flatten().map(without_token);
            if stale.is_some() {
                RunInfo::remove(&dir)?;
            }
            drop(lock);
            return Ok(Stopped::NotRunning {
                stale,
                address: None,
            });
        }
        Err(ServerError::Locked { .. }) => {}
        Err(err) => return Err(err),
    }
    let run = crate::console::server_info(&dir)?;
    let answer = crate::client::request(
        connectable(run.address),
        crate::console::bearer(&run),
        "POST",
        "/api/admin/shutdown",
        None,
    );
    let signalled = match answer {
        Ok((status, _)) if status < 400 => false,
        // A server without the route (an older version): the signal.
        Ok((404, json)) => fallback(&run, crate::console::answer_of(404, json).err())?,
        Ok((status, json)) => {
            return Err(crate::console::answer_of(status, json).err().unwrap_or(
                ServerError::Remote {
                    status,
                    message: format!("the server answered {status}"),
                    help: None,
                },
            ));
        }
        Err(err) => fallback(&run, Some(err))?,
    };
    let deadline = Instant::now() + timeout;
    while ServerLock::is_held(&dir)? {
        if Instant::now() >= deadline {
            return Err(stop_timeout(run.pid, timeout, &dir));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(Stopped::Stopped {
        pid: Some(run.pid),
        url: Some(run.url),
        signalled,
    })
}

/// The run file without its operator token.
fn without_token(mut run: RunInfo) -> RunInfo {
    run.operator_token = None;
    run
}

/// The fallback when the admin API cannot stop the server `run` (`why`): SIGTERM, the same
/// clean stop as Ctrl+C (`Ok(true)`).
#[cfg(unix)]
fn fallback(run: &RunInfo, why: Option<ServerError>) -> Result<bool, ServerError> {
    let why = why.map(|err| err.to_string()).unwrap_or_default();
    tracing::debug!(target: "graphersal_server", "stop: the admin API failed ({why}); SIGTERM");
    let pid = i32::try_from(run.pid).map_err(|_| ServerError::Service {
        message: format!("the pid {} of server.run is not a process id", run.pid),
        help: None,
        log_tail: Vec::new(),
    })?;
    // SAFETY: `kill` takes two integers and touches no memory of ours.
    if unsafe { libc::kill(pid, libc::SIGTERM) } == 0 {
        return Ok(true);
    }
    Err(ServerError::Service {
        message: format!(
            "the server (pid {}) could not be asked to stop: {why}; SIGTERM failed too: {}",
            run.pid,
            std::io::Error::last_os_error()
        ),
        help: None,
        log_tail: Vec::new(),
    })
}

/// The fallback when the admin API cannot stop the server `run` (`why`): no signal to send
/// outside Unix, the error names `taskkill`.
#[cfg(not(unix))]
fn fallback(run: &RunInfo, why: Option<ServerError>) -> Result<bool, ServerError> {
    let why = why.map(|err| err.to_string()).unwrap_or_default();
    Err(ServerError::Service {
        message: format!(
            "the server (pid {}) could not be asked to stop: {why}",
            run.pid
        ),
        help: Some(format!(
            "end it with `taskkill /PID {} /F` (a hard stop, without the clean unload of its \
             databases)",
            run.pid
        )),
        log_tail: Vec::new(),
    })
}

/// The error of a server `pid` that did not stop within `timeout`.
fn stop_timeout(pid: u32, timeout: Duration, dir: &ServerDir) -> ServerError {
    let kill = if cfg!(windows) {
        format!("taskkill /PID {pid} /F")
    } else {
        format!("kill {pid} (or kill -9 {pid}: no clean unload)")
    };
    ServerError::Service {
        message: format!(
            "the server (pid {pid}) did not stop within {} s; it may still be unloading its \
             databases",
            timeout.as_secs()
        ),
        help: Some(format!(
            "wait and run `graphersal-server status --dir {}`, look at {}, or end it: {kill}",
            dir_arg(dir),
            dir.log_path().display()
        )),
        log_tail: Vec::new(),
    }
}

/// The server to reach with a token: `address`, else the one in `dir`'s `server.run`
/// (`None`: no run file).
fn token_target(
    dir: &ServerDir,
    address: Option<SocketAddr>,
) -> Result<Option<RunInfo>, ServerError> {
    match address {
        Some(address) => Ok(Some(RunInfo::new(address, format!("http://{address}/")))),
        None => RunInfo::read(dir).map_err(|err| ServerError::Connect {
            address: dir.run_path().display().to_string(),
            message: format!("{err}; name the running server's address with --server ADDRESS"),
        }),
    }
}

/// [`stop`] through the server at `address` (else `dir`'s `server.run`) with the admin token
/// `token`; waits until the address stops answering.
pub fn stop_with_token(
    dir: &Path,
    token: &str,
    address: Option<SocketAddr>,
    timeout: Duration,
) -> Result<Stopped, ServerError> {
    let dir = ServerDir::new(dir);
    let Some(run) = token_target(&dir, address)? else {
        return Ok(Stopped::NotRunning {
            stale: None,
            address: None,
        });
    };
    let target = connectable(run.address);
    let pid = match crate::client::request(target, Some(token), "POST", "/api/admin/shutdown", None)
    {
        Ok((status, json)) if status < 400 => json
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok()),
        Ok((status, json)) => {
            return Err(crate::console::answer_of(status, json).err().unwrap_or(
                ServerError::Remote {
                    status,
                    message: format!("the server answered {status}"),
                    help: None,
                },
            ));
        }
        Err(ServerError::Connect { .. }) => {
            return Ok(Stopped::NotRunning {
                stale: None,
                address: Some(run.address),
            });
        }
        Err(err) => return Err(err),
    };
    let deadline = Instant::now() + timeout;
    while crate::client::request(target, None, "GET", "/healthz", None).is_ok() {
        if Instant::now() >= deadline {
            return Err(stop_timeout(pid.unwrap_or_default(), timeout, &dir));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(Stopped::Stopped {
        pid,
        url: Some(run.url),
        signalled: false,
    })
}

/// The status of `dir` (module docs).
///
/// `Err`: [`ServerError::InsecureDir`], the errors of opening the directory, a lock that cannot
/// be tested, a held lock without `server.run` for 10 s.
pub fn status(dir: &Path) -> Result<Status, ServerError> {
    crate::security::check(&ServerDir::new(dir))?;
    let dir = ServerDir::open(dir)?;
    let log = Some(dir.log_path()).filter(|path| path.is_file());
    if !ServerLock::is_held(&dir)? {
        let stale = RunInfo::read(&dir).ok().flatten().map(without_token);
        return Ok(Status {
            running: false,
            stale: stale.is_some(),
            run: stale,
            server: None,
            unanswered: None,
            log,
        });
    }
    let run = crate::console::server_info(&dir)?;
    let (server, unanswered) = server_answer(&run, crate::console::bearer(&run));
    Ok(Status {
        running: true,
        run: Some(without_token(run)),
        stale: false,
        server,
        unanswered,
        log,
    })
}

/// `GET /api/admin/server` of `run`'s server with `token`: the answer, or why there is none.
fn server_answer(run: &RunInfo, token: Option<&str>) -> (Option<Value>, Option<String>) {
    match crate::client::request(
        connectable(run.address),
        token,
        "GET",
        "/api/admin/server",
        None,
    ) {
        Ok((200, json)) => (Some(json), None),
        Ok((status, json)) => (
            None,
            Some(crate::console::answer_of(status, json).err().map_or_else(
                || format!("the server answered {status}"),
                |e| e.to_string(),
            )),
        ),
        Err(err) => (None, Some(err.to_string())),
    }
}

/// [`status`] through the server at `address` (else `dir`'s `server.run`) with the admin token
/// `token`: running when it answers.
pub fn status_with_token(
    dir: &Path,
    token: &str,
    address: Option<SocketAddr>,
) -> Result<Status, ServerError> {
    let dir = ServerDir::new(dir);
    let stopped = |run: Option<RunInfo>, why: Option<String>| Status {
        running: false,
        run,
        stale: false,
        server: None,
        unanswered: why,
        log: None,
    };
    let Some(run) = token_target(&dir, address)? else {
        return Ok(stopped(None, None));
    };
    let run = without_token(run);
    match crate::client::request(
        connectable(run.address),
        Some(token),
        "GET",
        "/api/admin/server",
        None,
    ) {
        Ok((200, json)) => Ok(Status {
            running: true,
            run: Some(run),
            stale: false,
            server: Some(json),
            unanswered: None,
            log: None,
        }),
        Ok((status, json)) => {
            Err(crate::console::answer_of(status, json)
                .err()
                .unwrap_or(ServerError::Remote {
                    status,
                    message: format!("the server answered {status}"),
                    help: None,
                }))
        }
        Err(err @ ServerError::Connect { .. }) => Ok(stopped(Some(run), Some(err.to_string()))),
        Err(err) => Err(err),
    }
}
