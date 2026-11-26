//! The command line and the HTTP listener end to end: `init`, `serve` on port 0, the probes,
//! `/api/info`, the server lock, the run file and a clean stop.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use graphersal_server::{
    DbState, RunInfo, ServeOptions, ServerDir, ServerError, ServerLock, serve,
};

const BIN: &str = env!("CARGO_BIN_EXE_graphersal-server");

/// The live test directories ([`TempDir`]), where [`admin_header`] looks for a server's run
/// file.
static DIRS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// A fresh directory under the system's temporary directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "graphersal-server-it-{name}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        // Owner only, whatever the umask: a server directory in it must be private to its owner
        // (`serve` and the console refuse a group-writable one).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .expect("make the test directory private");
        }
        DIRS.lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(path.clone());
        Self(path)
    }

    fn path(&self) -> &Path {
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

/// The operator token of the server listening on `address` that a test started (in process or
/// as a child) in one of the live [`TempDir`]s: its `server.run`, in the directory or one level
/// below. `None` when no such server runs.
fn operator_token(address: SocketAddr) -> Option<String> {
    let dirs = DIRS.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let mut candidates = Vec::new();
    for dir in dirs {
        if let Ok(children) = std::fs::read_dir(&dir) {
            candidates.extend(children.flatten().map(|child| child.path()));
        }
        candidates.push(dir);
    }
    candidates.into_iter().find_map(|root| {
        let info = RunInfo::read(&ServerDir::new(root)).ok()??;
        (info.address == address)
            .then(|| info.operator_token.map(|token| token.expose().to_string()))
            .flatten()
    })
}

/// The `Authorization` header line of the admin of the server at `address` (its operator
/// token, [`operator_token`]): the requests of these tests go as the admin unless they carry
/// their own `Authorization` (B3 closed every route but the probes, `/api/info` and the UI
/// files to anonymous). Empty when no such server runs.
fn admin_header(address: SocketAddr) -> String {
    operator_token(address)
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default()
}

/// `head` (a request head) with the admin's `Authorization` ([`admin_header`]) after the
/// request line, unless it carries one.
fn as_admin(address: SocketAddr, head: &str) -> String {
    if head.to_ascii_lowercase().contains("\r\nauthorization:") {
        return head.to_string();
    }
    match head.split_once("\r\n") {
        Some((line, rest)) => format!("{line}\r\n{}{rest}", admin_header(address)),
        None => head.to_string(),
    }
}

/// A server process, killed on drop if it still runs.
struct ServerProcess(Child);

impl ServerProcess {
    /// Spawns `command` (a `serve`). On Windows in its own process group, so that
    /// [`send_stop`] can send it Ctrl+Break without reaching the test process.
    fn spawn(command: &mut Command) -> Self {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            command.creation_flags(CREATE_NEW_PROCESS_GROUP);
        }
        Self(command.spawn().unwrap())
    }
}

/// Asks the server to stop cleanly: SIGTERM on Unix, Ctrl+Break to its process group on Windows
/// (the console event the `ctrlc` handler of `serve` takes for SIGTERM there).
fn send_stop(server: &ServerProcess) {
    #[cfg(unix)]
    {
        let status = Command::new("kill")
            .args(["-TERM", &server.0.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
    }
    #[cfg(windows)]
    {
        const CTRL_BREAK_EVENT: u32 = 1;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GenerateConsoleCtrlEvent(event: u32, process_group: u32) -> i32;
        }
        // SAFETY: a plain Win32 call with integer arguments; the group id is the child's pid
        // (it was spawned with CREATE_NEW_PROCESS_GROUP by `ServerProcess::spawn`).
        let sent = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, server.0.id()) };
        assert!(
            sent != 0,
            "GenerateConsoleCtrlEvent failed: {}",
            std::io::Error::last_os_error()
        );
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `path` (`/`-separated) with the platform's separator, as the console shows paths.
fn sep(path: &str) -> String {
    path.replace('/', std::path::MAIN_SEPARATOR_STR)
}

/// The run file of `dir` once a server wrote it (panics after 30 s).
fn wait_for_run_file(dir: &ServerDir) -> RunInfo {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(Some(info)) = RunInfo::read(dir) {
            return info;
        }
        assert!(Instant::now() < deadline, "no run file in {:?}", dir.root());
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The token of the framed first-admin block that `stderr` must consist of (plain ASCII frame,
/// no escape codes); fails the test otherwise.
fn first_admin_token(stderr: &str) -> String {
    assert!(!stderr.contains('\u{1b}'), "{stderr}");
    let lines: Vec<&str> = stderr.trim().lines().collect();
    assert!(lines.len() > 2, "no framed block: {stderr}");
    assert!(lines[0].starts_with("+--"), "{stderr}");
    assert_eq!(lines.last(), lines.first(), "{stderr}");
    assert!(
        lines[1..lines.len() - 1]
            .iter()
            .all(|l| l.starts_with("| ") && l.ends_with(" |")),
        "{stderr}"
    );
    assert!(stderr.contains("shown once"), "{stderr}");
    let tokens: Vec<String> = lines
        .iter()
        .filter_map(|l| l.strip_prefix("| token:"))
        .map(|rest| rest.trim_end_matches('|').trim().to_string())
        .collect();
    assert_eq!(tokens.len(), 1, "{stderr}");
    tokens[0].clone()
}

/// Sends `head` (and `body`) on a new connection, as the admin ([`as_admin`]), and reads the
/// answer: (status, body).
fn exchange(address: SocketAddr, head: &str, body: &[u8]) -> (u16, String) {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .write_all(as_admin(address, head).as_bytes())
        .unwrap();
    stream.write_all(body).unwrap();
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).unwrap();
    let answer = text(&answer);
    let status = answer
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("no status in {answer:?}"));
    let body = answer
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (status, body)
}

fn get(address: SocketAddr, path: &str) -> (u16, String) {
    exchange(
        address,
        &format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"),
        b"",
    )
}

#[test]
fn init_creates_a_server_directory_and_refuses_a_foreign_one() {
    let temp = TempDir::new("init");
    let root = temp.path().join("srv");
    let root_text = root.to_str().unwrap();
    let out = run(&["init", root_text]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    // The layout of the result (the graphersal CLI's note layout): a headline, aligned
    // details, every command on its own line under a `# comment`.
    let config_path = root.join("server.toml");
    assert_eq!(
        text(&out.stdout),
        format!(
            "Created the server directory {root_text}\n  config: {} (listen 127.0.0.1:8484: \
             edit it before serving)\n  MCP:    off, and new databases are not offered to it \
             (switch it on: graphersal-server mcp on --dir {root_text})\n  # start it\n  \
             graphersal-server serve --dir {root_text}\n  # or create a database first\n  \
             graphersal-server db create NAME --dir {root_text}\n",
            config_path.display()
        )
    );
    // stderr: only the first admin's token, framed (plain ASCII: not a terminal), once.
    let token = first_admin_token(&text(&out.stderr));
    assert_eq!(
        graphersal_server::access::AccessFile::read(&ServerDir::new(&root))
            .unwrap()
            .unwrap()
            .user_by_token(&graphersal_server::access::TokenHash::of(&token))
            .map(|(name, _)| name.to_string()),
        Some("admin".to_string())
    );
    for parts in [
        &["server.toml"][..],
        &["databases"],
        &["models", "blobs"],
        &["backups"],
        &["trash"],
        &["tmp"],
    ] {
        let path = parts.iter().fold(root.clone(), |p, c| p.join(c));
        assert!(path.exists(), "{}", path.display());
    }
    // No log directory: the server logs to stderr.
    assert!(!root.join("logs").exists());
    let config = std::fs::read_to_string(root.join("server.toml")).unwrap();
    assert!(config.contains("listen = \"127.0.0.1:8484\""), "{config}");
    assert!(
        config.contains("[mcp]") && config.contains("[defaults]"),
        "{config}"
    );

    let again = run(&["init", root_text]);
    assert!(again.status.success());
    assert!(
        text(&again.stdout).starts_with(&format!(
            "{root_text} is already a server directory: nothing to do\n  # start it\n"
        )),
        "{}",
        text(&again.stdout)
    );
    assert_eq!(
        std::fs::read_to_string(root.join("server.toml")).unwrap(),
        config
    );

    let foreign = temp.path().join("foreign");
    std::fs::create_dir(&foreign).unwrap();
    std::fs::write(foreign.join("data.bin"), b"x").unwrap();
    let refused = run(&["init", foreign.to_str().unwrap()]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        text(&refused.stderr).contains("not empty"),
        "{}",
        text(&refused.stderr)
    );
    assert!(!foreign.join("server.toml").exists());

    let usage = run(&["init"]);
    assert_eq!(usage.status.code(), Some(2));
    // A usage error names the problem and the command that shows the usage, not the usage.
    assert_eq!(
        text(&usage.stderr),
        "Error: init takes exactly one argument: the directory\n  # show the usage\n  \
         graphersal-server --help\n"
    );
    assert!(usage.stdout.is_empty());
    assert_eq!(run(&["serve"]).status.code(), Some(2));
    assert_eq!(run(&["serve", "--dir"]).status.code(), Some(2));
    assert_eq!(
        run(&["serve", "--dir", root_text, "--listen", "nowhere"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(run(&["bogus"]).status.code(), Some(2));
    let not_a_server = run(&["serve", "--dir", foreign.to_str().unwrap()]);
    assert_eq!(not_a_server.status.code(), Some(1));
    assert!(text(&not_a_server.stderr).contains("not a server directory"));
}

#[test]
fn serve_refuses_connection_limits_that_are_not_whole_numbers_of_at_least_1() {
    let temp = TempDir::new("serve-limit-flags");
    let dir = ServerDir::init(temp.path().join("srv")).unwrap().dir;
    let root = dir.root().to_str().unwrap();
    for (flag, value) in [
        ("--max-connections", "0"),
        ("--header-timeout", "x"),
        ("--idle-timeout", "-1"),
    ] {
        let out = Command::new(BIN)
            .args([
                "serve",
                "--dir",
                root,
                "--listen",
                "127.0.0.1:0",
                flag,
                value,
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{flag} {value}");
        let err = text(&out.stderr);
        assert!(err.contains(flag) && err.contains("at least 1"), "{err}");
    }
}

#[test]
fn serve_answers_refuses_a_second_server_and_stops_cleanly() {
    let temp = TempDir::new("serve");
    let dir = ServerDir::init(temp.path()).unwrap().dir;
    let root = temp.path().to_str().unwrap();
    let mut server = ServerProcess::spawn(
        Command::new(BIN)
            .args(["serve", "--dir", root, "--listen", "127.0.0.1:0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    );
    let info = wait_for_run_file(&dir);
    assert_eq!(info.pid, server.0.id());
    assert!(info.address.ip().is_loopback());
    assert_ne!(info.address.port(), 0);
    assert_eq!(info.url, format!("http://{}/", info.address));
    assert!(ServerLock::is_held(&dir).unwrap());

    let (status, body) = get(info.address, "/healthz");
    assert_eq!(status, 200, "{body}");
    let (status, body) = get(info.address, "/readyz");
    assert_eq!((status, body.contains("ready")), (200, true), "{body}");
    let (status, body) = get(info.address, "/api/info");
    assert_eq!(status, 200, "{body}");
    let info_json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(info_json["server"], "graphersal-server");
    assert_eq!(info_json["uiProfile"]["name"], "server");
    assert_eq!(info_json["experimental"], true);
    let (status, _) = get(info.address, "/");
    assert_eq!(status, 200);
    let (status, _) = get(info.address, "/db/none/api/info");
    assert_eq!(status, 404);

    // A second server on the same directory is refused; the first keeps serving.
    let second = run(&["serve", "--dir", root, "--listen", "127.0.0.1:0"]);
    assert_eq!(second.status.code(), Some(1));
    assert!(
        text(&second.stderr).contains("already serves"),
        "{}",
        text(&second.stderr)
    );
    assert_eq!(RunInfo::read(&dir).unwrap(), Some(info.clone()));
    assert_eq!(get(info.address, "/healthz").0, 200);

    let (stdout, stderr) = stop_and_wait(&mut server);
    assert_eq!(RunInfo::read(&dir).unwrap(), None);
    assert!(!ServerLock::is_held(&dir).unwrap());
    assert!(TcpStream::connect(info.address).is_err());

    // stdout: the address line only; stderr: the start block (the graphersal CLI's layout),
    // the log without colours (not a terminal), the stop lines.
    assert_eq!(stdout, format!("Graphersal server: {}\n", info.url));
    assert!(
        !stderr.contains('\u{1b}'),
        "ANSI codes in a piped log: {stderr}"
    );
    // The start block says where it serves; no log line repeats it before the block.
    assert!(!stderr.contains(" on http://"), "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    let banner = lines
        .iter()
        .position(|l| l.starts_with("====="))
        .unwrap_or_else(|| panic!("no banner: {stderr}"));
    assert_eq!(lines[banner].chars().count(), 78, "{stderr}");
    assert!(lines[banner + 2].contains("Loopback only"), "{stderr}");
    let headings: Vec<&str> = lines
        .iter()
        .filter(|l| l.starts_with("── "))
        .map(|l| l.trim_end_matches('─').trim())
        .collect();
    assert_eq!(
        headings,
        [
            "── Server",
            "── Databases (0)",
            "── Embedding models",
            "── MCP"
        ],
        "{stderr}"
    );
    assert!(
        lines
            .iter()
            .filter(|l| l.starts_with("── "))
            .all(|l| l.chars().count() == 78),
        "{stderr}"
    );
    let row = |key: &str| -> String {
        lines
            .iter()
            .find(|l| l.starts_with(&format!("  {key}:")))
            .unwrap_or_else(|| panic!("no row {key}: {stderr}"))
            .to_string()
    };
    // Values in one column across the sections.
    let column = row("directory").find(root).unwrap();
    assert_eq!(
        row("listening"),
        format!(
            "  {:w$}{} (loopback, this machine only)",
            "listening:",
            info.address,
            w = column - 2
        )
    );
    assert_eq!(row("UI")[column..], info.url[..]);
    assert_eq!(row("endpoint")[column..], format!("{}mcp", info.url));
    assert!(
        row("connect").ends_with(&format!("{}mcp", info.url)),
        "{stderr}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("graphersal-server db create NAME --dir")),
        "{stderr}"
    );
    if cfg!(unix) {
        assert!(stderr.contains("\nStopping: "), "{stderr}");
        assert!(stderr.trim_end().ends_with("lock released)."), "{stderr}");
    }
}

#[test]
fn a_network_bind_warns_exactly_once_in_the_start_block() {
    let temp = TempDir::new("network");
    let dir = ServerDir::init(temp.path()).unwrap().dir;
    let root = temp.path().to_str().unwrap();
    let mut server = ServerProcess::spawn(
        Command::new(BIN)
            .args(["serve", "--dir", root, "--listen", "0.0.0.0:0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    );
    let info = wait_for_run_file(&dir);
    let (_, stderr) = stop_and_wait(&mut server);
    assert_eq!(
        stderr.matches("reachable from").count(),
        1,
        "the warning once, no log line repeating it: {stderr}"
    );
    let lines: Vec<&str> = stderr.lines().collect();
    let banner = lines
        .iter()
        .rposition(|l| l.starts_with("====="))
        .unwrap_or_else(|| panic!("no banner: {stderr}"));
    assert!(
        lines[banner + 1].starts_with(&format!(
            "warning: graphersal-server listens on 0.0.0.0:{}",
            info.address.port()
        )),
        "{stderr}"
    );
    assert!(
        stderr.contains("REACHABLE FROM THE NETWORK"),
        "the banner and the Server section say so: {stderr}"
    );
}

/// Stops the server cleanly ([`send_stop`]) and checks a clean exit (`0`) within 30 s.
/// Returns what the server wrote: (stdout, stderr).
fn stop_and_wait(server: &mut ServerProcess) -> (String, String) {
    send_stop(server);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = server.0.try_wait().unwrap() {
            let mut stderr = String::new();
            if let Some(mut pipe) = server.0.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            let mut stdout = String::new();
            if let Some(mut pipe) = server.0.stdout.take() {
                let _ = pipe.read_to_string(&mut stdout);
            }
            assert!(status.success(), "{status}: {stderr}");
            assert!(stderr.contains("Stopping"), "{stderr}");
            return (stdout, stderr);
        }
        assert!(Instant::now() < deadline, "the server did not stop");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn start_and_stop_in_process() {
    let temp = TempDir::new("in-process");
    let dir = ServerDir::init(temp.path()).unwrap().dir;
    let listen: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let running = serve::start(temp.path(), ServeOptions::new().with_listen(listen)).unwrap();
    let address = running.address();
    assert_eq!(RunInfo::read(&dir).unwrap().unwrap().address, address);
    let err = serve::start(temp.path(), ServeOptions::new().with_listen(listen))
        .err()
        .unwrap();
    assert!(matches!(err, ServerError::Locked { .. }), "{err}");
    assert_eq!(get(address, "/healthz").0, 200);
    running.stop().unwrap();
    assert_eq!(RunInfo::read(&dir).unwrap(), None);
    assert!(!ServerLock::is_held(&dir).unwrap());

    // Restartable, and dropping a running server stops it too.
    let again = serve::start(temp.path(), ServeOptions::new().with_listen(listen)).unwrap();
    drop(again);
    assert_eq!(RunInfo::read(&dir).unwrap(), None);
    assert!(!ServerLock::is_held(&dir).unwrap());

    let empty = TempDir::new("not-a-server");
    let err = serve::start(empty.path(), ServeOptions::new())
        .err()
        .unwrap();
    assert!(matches!(err, ServerError::NotServerDir { .. }), "{err}");
}

#[test]
fn start_scans_the_registry_and_broken_databases_are_not_fatal() {
    let temp = TempDir::new("registry-scan");
    let dir = ServerDir::init(temp.path()).unwrap().dir;
    let good = dir.databases().join("good");
    std::fs::create_dir_all(good.join("store")).unwrap();
    // Not `auto_load`: its empty `store/` would be loaded at start and fail (V3).
    std::fs::write(good.join("database.toml"), "description = \"good\"\n").unwrap();
    let broken = dir.databases().join("broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join("database.toml"), "kind = ").unwrap();
    std::fs::write(dir.aliases_path(), "g = \"good\"\n").unwrap();
    let debris = dir.tmp().join("build-old-0");
    std::fs::create_dir_all(&debris).unwrap();

    let listen: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let running = serve::start(temp.path(), ServeOptions::new().with_listen(listen)).unwrap();
    assert_eq!(get(running.address(), "/readyz").0, 200);
    let registry = running.registry();
    assert_eq!(registry.list().len(), 2);
    assert_eq!(registry.resolve("G").unwrap().name, "good");
    assert_eq!(registry.get("good").unwrap().state, DbState::Unloaded);
    assert_eq!(registry.get("broken").unwrap().state.as_str(), "failed");
    assert!(!debris.exists());
    running.stop().unwrap();
}

/// Sends `GET path` (or `POST` with `body`) with `Host: host` and no credentials: (status,
/// head, body).
fn as_host(
    address: SocketAddr,
    host: &str,
    method: &str,
    path: &str,
    body: &str,
) -> (u16, String, String) {
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    stream.write_all(head.as_bytes()).unwrap();
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).unwrap();
    let answer = text(&answer);
    let (head, body) = answer.split_once("\r\n\r\n").unwrap_or((&answer, ""));
    let status = head.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap();
    (status, head.to_string(), body.to_string())
}

#[test]
fn server_toml_modes_access_off_https_and_allowed_hosts() {
    let temp = TempDir::new("modes");
    let report = ServerDir::init(temp.path()).unwrap();
    let dir = report.dir;
    let admin_token = report.first_admin.unwrap().token.expose().to_string();
    // The first admin exists already (init): access off ignores it.
    std::fs::write(
        dir.config_path(),
        "listen = \"127.0.0.1:0\"\n[access]\nenabled = false\n[sessions]\nlifetime_secs = 7200\n\
         [http]\npublic_scheme = \"https\"\nallowed_hosts = [\"db.example.com\"]\n",
    )
    .unwrap();
    let running = serve::start(temp.path(), ServeOptions::new()).unwrap();
    let address = running.address();
    assert!(!running.config().access.enabled);
    let block = graphersal_server::console::start_block(&running);
    assert!(block.contains("ACCESS CONTROL OFF"), "{block}");
    assert!(block.contains("warning: access control is off"), "{block}");
    assert!(block.contains("OFF: every request is an admin"), "{block}");
    assert!(block.contains("https, TLS ended by a proxy"), "{block}");
    assert!(block.contains("db.example.com (any port)"), "{block}");
    // Through the proxy name, without credentials: the admin `everyone`.
    let (status, _, body) = as_host(address, "db.example.com", "GET", "/api/info", "");
    assert_eq!(status, 200, "{body}");
    assert_eq!(json(&body)["principal"]["kind"], "everyone", "{body}");
    let (status, _, body) = as_host(
        address,
        "db.example.com:443",
        "GET",
        "/api/admin/databases",
        "",
    );
    assert_eq!(status, 200, "{body}");
    // Another name is refused, naming the setting.
    let (status, _, body) = as_host(address, "other.example.com", "GET", "/api/info", "");
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("allowed_hosts"), "{body}");
    // A login (unnecessary, but it works) sets a Secure cookie that outlives the browser.
    let login = serde_json::json!({ "token": admin_token }).to_string();
    let (status, head, body) = as_host(address, "db.example.com", "POST", "/api/login", &login);
    assert_eq!(status, 200, "{body}");
    let cookie = header_of(&head, "Set-Cookie").unwrap();
    assert!(cookie.contains("; Max-Age=7200;"), "{cookie}");
    assert!(cookie.ends_with("; Secure"), "{cookie}");
    running.stop().unwrap();
}

#[test]
fn the_listen_address_comes_from_server_toml() {
    let temp = TempDir::new("config-listen");
    let dir = ServerDir::init(temp.path()).unwrap().dir;
    std::fs::write(dir.config_path(), "listen = \"localhost:0\"\n").unwrap();
    let running = serve::start(temp.path(), ServeOptions::new()).unwrap();
    assert_eq!(running.address().ip().to_string(), "127.0.0.1");
    assert_ne!(running.address().port(), 0);
    assert_eq!(get(running.address(), "/healthz").0, 200);
    running.stop().unwrap();
}

#[test]
fn request_bodies_are_drained_or_ignored_without_harm() {
    let temp = TempDir::new("bodies");
    ServerDir::init(temp.path()).unwrap();
    let running = serve::start(
        temp.path(),
        ServeOptions::new().with_listen("127.0.0.1:0".parse().unwrap()),
    )
    .unwrap();
    let address = running.address();
    // An unread body (100 KiB) is drained; the answer arrives.
    let body = vec![b'x'; 100 * 1024];
    let (status, _) = exchange(
        address,
        &format!(
            "POST /api/info HTTP/1.1\r\nHost: {address}\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n",
            body.len()
        ),
        &body,
    );
    assert_eq!(status, 405);
    // A huge declared body is answered 413 at once (never read nor allocated) and its
    // connection closed; the server keeps serving.
    let mut huge = TcpStream::connect(address).unwrap();
    huge.write_all(
        format!(
            "POST /api/info HTTP/1.1\r\nHost: {address}\r\nContent-Length: {}\r\n\r\n",
            1u64 << 62
        )
        .as_bytes(),
    )
    .unwrap();
    let (status, head, body) = answer_then_eof(huge);
    assert_eq!(status, 413, "{head}");
    assert!(
        head.to_ascii_lowercase().contains("connection: close"),
        "{head}"
    );
    assert!(
        body["error"].as_str().unwrap().contains("67108864"),
        "{body}"
    );
    assert_eq!(get(address, "/healthz").0, 200);
    running.stop().unwrap();
}

/// Reads one answer and then EOF from `stream` (every read bounded by 10 s: a hanging server
/// fails the test): (status, head, JSON body or `Null`).
fn answer_then_eof(mut stream: TcpStream) -> (u16, String, serde_json::Value) {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut answer = Vec::new();
    stream
        .read_to_end(&mut answer)
        .expect("the server answers and closes the connection");
    let answer = text(&answer);
    let (head, body) = answer
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("no head in {answer:?}"));
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("no status in {head:?}"));
    let json = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    (status, head.to_string(), json)
}

/// A server directory whose upload limit is 1 MiB, served in this process.
fn small_upload_server(temp: &TempDir) -> serve::Running {
    let dir = ServerDir::init(temp.path()).unwrap().dir;
    let config = std::fs::read_to_string(dir.config_path()).unwrap();
    assert!(config.contains("upload_bytes = 4294967296"), "{config}");
    std::fs::write(
        dir.config_path(),
        config.replace("upload_bytes = 4294967296", "upload_bytes = 1048576"),
    )
    .unwrap();
    serve::start(
        temp.path(),
        ServeOptions::new().with_listen("127.0.0.1:0".parse().unwrap()),
    )
    .unwrap()
}

fn upload_head(address: SocketAddr, extra: &str) -> String {
    format!(
        "POST /api/admin/import?name=big HTTP/1.1\r\nHost: {address}\r\n{}Content-Type: \
         application/octet-stream\r\n{extra}\r\n",
        admin_header(address)
    )
}

fn assert_upload_refused(status: u16, head: &str, body: &serde_json::Value) {
    assert_eq!(status, 413, "{head}");
    assert!(
        head.to_ascii_lowercase().contains("connection: close"),
        "{head}"
    );
    assert!(
        body["error"].as_str().unwrap().contains("1048576"),
        "{body}"
    );
    assert!(
        body["help"]
            .as_str()
            .is_some_and(|h| h.contains("upload_bytes")),
        "{body}"
    );
}

#[test]
fn an_upload_declaring_more_than_the_limit_gets_413_and_eof_at_once() {
    let temp = TempDir::new("over-limit-declared");
    let running = small_upload_server(&temp);
    let address = running.address();
    // Only the head: the answer comes without the body being sent.
    let mut stream = TcpStream::connect(address).unwrap();
    let head = upload_head(address, &format!("Content-Length: {}\r\n", 1u64 << 50));
    stream.write_all(head.as_bytes()).unwrap();
    let (status, head, body) = answer_then_eof(stream);
    assert_upload_refused(status, &head, &body);

    // The client sends its whole body regardless: it still reads its 413 and EOF, no reset.
    let mut stream = TcpStream::connect(address).unwrap();
    let length = 8usize << 20;
    let head = upload_head(address, &format!("Content-Length: {length}\r\n"));
    stream.write_all(head.as_bytes()).unwrap();
    let writer = {
        let mut stream = stream.try_clone().unwrap();
        std::thread::spawn(move || {
            let chunk = vec![b' '; 64 * 1024];
            for _ in 0..length / chunk.len() {
                if stream.write_all(&chunk).is_err() {
                    break;
                }
            }
        })
    };
    let (status, head, body) = answer_then_eof(stream);
    assert_upload_refused(status, &head, &body);
    writer.join().unwrap();
    assert!(!running.dir().databases().join("big").exists());
    assert_eq!(get(address, "/healthz").0, 200);
    running.stop().unwrap();
}

#[test]
fn an_upload_expecting_100_continue_over_the_limit_gets_413_not_100() {
    let temp = TempDir::new("over-limit-continue");
    let running = small_upload_server(&temp);
    let address = running.address();
    let mut stream = TcpStream::connect(address).unwrap();
    let head = upload_head(
        address,
        &format!("Content-Length: {}\r\nExpect: 100-continue\r\n", 2u64 << 20),
    );
    stream.write_all(head.as_bytes()).unwrap();
    let (status, head, body) = answer_then_eof(stream);
    assert!(!head.starts_with("HTTP/1.1 100"), "{head}");
    assert_upload_refused(status, &head, &body);
    running.stop().unwrap();
}

#[test]
fn a_chunked_upload_growing_over_the_limit_gets_413_and_eof() {
    let temp = TempDir::new("over-limit-chunked");
    let running = small_upload_server(&temp);
    let address = running.address();
    let mut stream = TcpStream::connect(address).unwrap();
    let head = upload_head(address, "Transfer-Encoding: chunked\r\n");
    stream.write_all(head.as_bytes()).unwrap();
    // 8 MiB in 64 KiB chunks (the limit is 1 MiB), sent while the answer is read.
    let writer = {
        let mut stream = stream.try_clone().unwrap();
        std::thread::spawn(move || {
            let mut chunk = format!("{:x}\r\n", 64 * 1024).into_bytes();
            chunk.extend(std::iter::repeat_n(b' ', 64 * 1024));
            chunk.extend_from_slice(b"\r\n");
            for _ in 0..128 {
                if stream.write_all(&chunk).is_err() {
                    return;
                }
            }
            let _ = stream.write_all(b"0\r\n\r\n");
        })
    };
    let (status, head, body) = answer_then_eof(stream);
    assert_eq!(status, 413, "{head}");
    assert!(
        head.to_ascii_lowercase().contains("connection: close"),
        "{head}"
    );
    assert!(
        body["error"].as_str().unwrap().contains("1048576"),
        "{body}"
    );
    writer.join().unwrap();
    assert!(!running.dir().databases().join("big").exists());
    assert_eq!(get(address, "/healthz").0, 200);
    running.stop().unwrap();
    assert_eq!(
        std::fs::read_dir(temp.path().join("tmp")).unwrap().count(),
        0
    );
}

#[test]
fn keep_alive_connections_serve_several_requests_and_close_on_stop() {
    let temp = TempDir::new("keep-alive");
    ServerDir::init(temp.path()).unwrap();
    let running = serve::start(
        temp.path(),
        ServeOptions::new().with_listen("127.0.0.1:0".parse().unwrap()),
    )
    .unwrap();
    let address = running.address();
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    for _ in 0..2 {
        stream
            .write_all(format!("GET /healthz HTTP/1.1\r\nHost: {address}\r\n\r\n").as_bytes())
            .unwrap();
        let mut answer = Vec::new();
        let mut byte = [0u8; 1];
        while !answer.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            answer.push(byte[0]);
        }
        let head = text(&answer);
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        let length: usize = head
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .map(str::to_string)
            })
            .and_then(|v| v.trim().parse().ok())
            .unwrap();
        let mut body = vec![0u8; length];
        stream.read_exact(&mut body).unwrap();
    }
    // The idle connection does not hold up the stop, and sees EOF.
    let started = Instant::now();
    running.stop().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    let mut rest = Vec::new();
    let _ = stream.read_to_end(&mut rest);
    assert!(rest.is_empty(), "{}", text(&rest));
}

/// Sends one request (`headers` beside `Host` and `Connection: close`) as the admin (unless
/// `headers` carry an `Authorization`, [`as_admin`]) and reads the answer: (status, head,
/// body).
fn call(
    address: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String, String) {
    let own = headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("authorization"));
    let auth = if own {
        String::new()
    } else {
        admin_header(address)
    };
    send(address, method, path, headers, &auth, body)
}

/// Sends one request (`headers` beside `Host` and `Connection: close`, `auth` = an
/// `Authorization` line or nothing) and reads the answer: (status, head, body).
fn send(
    address: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    auth: &str,
    body: &str,
) -> (u16, String, String) {
    let mut head =
        format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n{auth}");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body.as_bytes()).unwrap();
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).unwrap();
    let answer = text(&answer);
    let (head, body) = answer.split_once("\r\n\r\n").unwrap_or((&answer, ""));
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("no status in {answer:?}"));
    (status, head.to_string(), body.to_string())
}

fn json(body: &str) -> serde_json::Value {
    serde_json::from_str(body).unwrap_or_else(|err| panic!("{err}: {body}"))
}

/// The value of the header `name` in an answer's head.
fn header_of<'h>(head: &'h str, name: &str) -> Option<&'h str> {
    head.lines().find_map(|line| {
        let (field, value) = line.split_once(':')?;
        field
            .trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim())
    })
}

/// Runs `script` on the database page `db` (`/db/<db>/api/execute`): its first result.
fn execute(address: SocketAddr, db: &str, script: &str) -> serde_json::Value {
    let body = serde_json::json!({ "script": script }).to_string();
    let (status, _, answer) = call(
        address,
        "POST",
        &format!("/db/{db}/api/execute"),
        &[("Content-Type", "application/json")],
        &body,
    );
    assert_eq!(status, 200, "{answer}");
    json(&answer)["execution"]["results"][0].clone()
}

/// A plain database `name` (a closed Store holding the modern sample).
/// Switches MCP on in `dir`'s `mcp.toml` (a new server starts with it off: round 4
/// B-MCPDEF); names without an entry stay offered.
fn mcp_on(dir: &ServerDir) {
    std::fs::write(dir.mcp_settings_path(), "enabled = true\n").unwrap();
}

fn modern_db(dir: &ServerDir, name: &str) {
    use graphersal::persist::{Store, StoreOptions};
    use graphersal::prelude::TraversalGraph;
    let db = dir.databases().join(name);
    std::fs::create_dir_all(&db).unwrap();
    std::fs::write(db.join("database.toml"), "description = \"modern\"\n").unwrap();
    Store::create_with(
        db.join("store"),
        TraversalGraph::tinkerpop_modern(),
        StoreOptions::new(),
    )
    .unwrap()
    .close()
    .unwrap();
}

#[test]
fn databases_are_served_under_their_names_and_aliases() {
    let temp = TempDir::new("routing");
    let dir = ServerDir::init(temp.path()).unwrap().dir;
    for name in ["a", "b", "c"] {
        modern_db(&dir, name);
    }
    let running = serve::start(
        temp.path(),
        ServeOptions::new().with_listen("127.0.0.1:0".parse().unwrap()),
    )
    .unwrap();
    let address = running.address();
    let loader = running.loader();
    for name in ["a", "b"] {
        loader.load(name).unwrap();
        assert_eq!(
            loader.wait(name, Duration::from_secs(120)).unwrap(),
            DbState::Loaded
        );
    }

    // Each page reaches its own graph: a write in a is not seen in b.
    assert_eq!(execute(address, "a", "g.v().count().next()"), 6);
    execute(address, "a", "g.add_v(\"x\").iterate()");
    assert_eq!(execute(address, "a", "g.v().count().next()"), 7);
    assert_eq!(execute(address, "b", "g.v().count().next()"), 6);
    // Names are case-insensitive; a client's own graph header is dropped.
    assert_eq!(execute(address, "A", "g.v().count().next()"), 7);
    let (_, _, steered) = call(
        address,
        "POST",
        "/db/b/api/execute",
        &[
            ("Content-Type", "application/json"),
            ("X-Graphersal-Graph", "a"),
        ],
        r#"{"script": "g.v().count().next()"}"#,
    );
    assert_eq!(json(&steered)["execution"]["results"][0], 6);

    // An alias is resolved on every request: a flip takes effect at once.
    running.registry().alias_set("current", "a").unwrap();
    assert_eq!(execute(address, "current", "g.v().count().next()"), 7);
    running.registry().alias_set("current", "b").unwrap();
    assert_eq!(execute(address, "current", "g.v().count().next()"), 6);

    // Per-database info: the mount, the real name, the link home.
    let (status, _, body) = call(address, "GET", "/db/a/api/info", &[], "");
    assert_eq!(status, 200, "{body}");
    let info = json(&body);
    assert_eq!(info["mountPath"], "/db/a");
    assert_eq!(info["database"], "a");
    assert!(info.get("admin").is_none());
    assert_eq!(info["home"], "/");
    // A mount names this server (not the CLI's dev server), carries the protocol marker the
    // playground recognises a Graphersal host by, and is no dev server (no DEV badge).
    assert_eq!(info["server"], "graphersal-server");
    assert_eq!(info["protocol"], "graphersal/1");
    assert_eq!(info["dev"], false);
    let info = json(&call(address, "GET", "/db/current/api/info", &[], "").2);
    assert_eq!(info["database"], "b", "an alias shows the real database");
    assert_eq!(info["mountPath"], "/db/b");
    // The root info delivers the server's own UI profile (no graph, no landing view).
    let root = json(&get(address, "/api/info").1);
    assert!(root.get("admin").is_none());
    assert_eq!(root["server"], "graphersal-server");
    assert_eq!(root["protocol"], "graphersal/1");
    assert_eq!(root["dev"], false);
    assert!(root.get("product").is_none());
    assert_eq!(root["uiProfile"]["name"], "server");
    assert_eq!(root["uiProfile"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(root.get("mountPath").is_none());

    // Not loaded: 409 with the exact text; unknown: 404.
    let (status, _, body) = call(address, "GET", "/db/c/api/info", &[], "");
    assert_eq!(status, 409, "{body}");
    let refused = json(&body);
    assert_eq!(refused["error"], "database 'c' is not loaded");
    assert_eq!(refused["help"], "load it: POST /api/admin/databases/c/load");
    assert_eq!(refused["state"], "unloaded");
    let (status, _, body) = call(address, "GET", "/db/none/api/info", &[], "");
    assert_eq!(status, 404, "{body}");
    assert_eq!(json(&body)["error"], "no database or alias 'none'");

    // `/db/a` redirects to `/db/a/`.
    let (status, head, _) = call(address, "GET", "/db/a?x=1", &[], "");
    assert_eq!(status, 308);
    assert_eq!(header_of(&head, "Location"), Some("/db/a/?x=1"));

    // The playground under each mount and at the root (the `server` profile).
    for prefix in ["/db/a/", "/"] {
        let (status, head, body) = call(address, "GET", prefix, &[], "");
        assert_eq!(status, 200, "{prefix}");
        assert_eq!(
            header_of(&head, "Content-Type"),
            Some("text/html; charset=utf-8")
        );
        assert!(body.contains("<html"), "{prefix}");
        let asset = format!("{prefix}js/engine/http-engine.js");
        let (status, head, _) = call(address, "GET", &asset, &[], "");
        assert_eq!(status, 200, "{asset}");
        assert_eq!(
            header_of(&head, "Content-Type"),
            Some("text/javascript; charset=utf-8")
        );
    }
    // The root page is the embedded index.html unchanged (the profile names its style sheet
    // in `styles`), the same page as a mount's; the profile's files are served under
    // /server-ui/.
    let root_page = call(address, "GET", "/", &[], "").2;
    assert!(!root_page.contains("server-ui/"), "{root_page}");
    assert_eq!(root_page, call(address, "GET", "/db/a/", &[], "").2);
    let (status, head, body) = call(address, "GET", "/server-ui/profile.json", &[], "");
    assert_eq!(status, 200);
    assert_eq!(
        header_of(&head, "Content-Type"),
        Some("application/json; charset=utf-8")
    );
    assert_eq!(json(&body)["name"], "server");
    assert_eq!(json(&body)["styles"][0], "server-ui.css");
    let (status, head, _) = call(address, "GET", "/server-ui/server-ui.css", &[], "");
    assert_eq!(status, 200);
    assert_eq!(
        header_of(&head, "Content-Type"),
        Some("text/css; charset=utf-8")
    );
    assert_eq!(call(address, "GET", "/server-ui/none.js", &[], "").0, 404);
    assert_eq!(
        call(address, "POST", "/server-ui/shared.js", &[], "").0,
        405
    );
    assert_eq!(call(address, "GET", "/no-such-file.js", &[], "").0, 404);
    assert_eq!(call(address, "GET", "/api/nothing", &[], "").0, 404);

    // The admin listing follows the registry.
    let (status, _, body) = call(address, "GET", "/api/admin/databases", &[], "");
    assert_eq!(status, 200, "{body}");
    let listing = json(&body);
    let state_of = |name: &str| {
        listing["databases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == name)
            .map(|e| e["state"].clone())
    };
    for entry in running.registry().list() {
        assert_eq!(
            state_of(&entry.name),
            Some(serde_json::Value::from(entry.state.as_str())),
            "{}",
            entry.name
        );
    }
    assert_eq!(state_of("a"), Some("loaded".into()));
    assert_eq!(state_of("c"), Some("unloaded".into()));
    assert_eq!(listing["aliases"]["current"], "b");
    let one = json(&call(address, "GET", "/api/admin/databases/current", &[], "").2);
    assert_eq!(one["aliasOf"], "b");
    assert_eq!(one["state"], "loaded");

    // A foreign Origin is refused on a database page and on the admin API.
    let evil = [("Origin", "http://evil.example")];
    for path in ["/db/a/api/info", "/api/admin/databases", "/db/c/api/info"] {
        let (status, _, body) = call(address, "GET", path, &evil, "");
        assert_eq!(status, 403, "{path}: {body}");
    }

    // An unloaded database answers 409 from then on.
    assert!(loader.unload("a").unwrap());
    let (status, _, body) = call(address, "GET", "/db/a/api/info", &[], "");
    assert_eq!(status, 409, "{body}");
    running.stop().unwrap();
}

/// Runs `graphersal-server db ARGS --dir DIR`: (exit code, stdout, stderr).
fn db(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(BIN)
        .arg("db")
        .args(args)
        .arg("--dir")
        .arg(dir)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        text(&out.stdout),
        text(&out.stderr),
    )
}

#[test]
fn db_commands_act_on_the_directory_when_no_server_runs() {
    let temp = TempDir::new("console-offline");
    let dir = ServerDir::init(temp.path()).unwrap().dir;
    let root = temp.path();
    let schema = temp.path().join("schema.json");
    std::fs::write(
        &schema,
        r#"{"vertices": {"person": {"schema": {"type": "object"}}}}"#,
    )
    .unwrap();
    let (code, out, err) = db(
        root,
        &[
            "create",
            "acme",
            "--description",
            "Acme graph",
            "--schema",
            schema.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "{err}");
    // A new database is not offered to MCP (round 4 B-MCPDEF).
    assert_eq!(
        out,
        format!(
            "Created database 'acme'\n  kind:        plain\n  description: Acme graph\n  \
             state:       unloaded\n  MCP:         not offered\n  # offer it to MCP\n  \
             graphersal-server mcp db acme on --dir {}\n",
            root.display()
        )
    );
    assert!(!err.contains("note:"), "offline: {err}");
    let (code, out, err) = db(root, &["create", "base", "--role", "base"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("  kind:  base\n  flags: read-only\n"), "{out}");
    let (code, out, err) = db(root, &["create", "l1", "--base", "base"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("  kind:  layer over 'base'\n"), "{out}");
    let (code, _, err) = db(root, &["create", "l2", "--base", "acme"]);
    assert_eq!(code, 1);
    assert!(err.contains("not a base database"), "{err}");

    let (code, out, _) = db(root, &["list"]);
    assert_eq!(code, 0);
    // The layout: a section heading as wide as the CLI's blocks, then the table in aligned
    // columns (the size depends on the file system, its column is checked by position).
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("── Databases (3) ───"), "{out}");
    assert_eq!(lines[0].chars().count(), 78, "{out}");
    let cells = |name: &str, state: &str, kind: &str, flags: &str| {
        format!("  {name:4}  {state:8}  {kind:17}  {flags:9}  ")
    };
    assert!(
        lines[1].starts_with(&format!(
            "{}ON DISK",
            cells("NAME", "STATE", "KIND", "FLAGS")
        )),
        "{out}"
    );
    let size_at = lines[1].find("ON DISK").unwrap();
    let memory_at = lines[1].find("MEMORY").unwrap();
    let description_at = lines[1].find("DESCRIPTION").unwrap();
    assert_eq!(lines[2][..size_at], cells("acme", "unloaded", "plain", ""));
    assert_eq!(&lines[2][description_at..], "Acme graph");
    assert!(lines[2][size_at..memory_at].contains("KiB"), "{out}");
    // Not loaded: no memory.
    assert_eq!(lines[2][memory_at..description_at].trim(), "", "{out}");
    assert_eq!(
        lines[3][..size_at],
        cells("base", "unloaded", "base", "read-only")
    );
    assert_eq!(
        lines[4][..size_at],
        cells("l1", "unloaded", "layer over 'base'", "")
    );
    assert_eq!(lines.len(), 5, "{out}");
    let (code, out, _) = db(root, &["list", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(json(&out)["databases"].as_array().unwrap().len(), 3);

    // Offline: no load, but flags, aliases, clone and delete.
    let (code, _, err) = db(root, &["load", "acme"]);
    assert_eq!(code, 1);
    assert!(err.contains("no server runs"), "{err}");
    let (code, out, _) = db(root, &["read-only", "acme", "on"]);
    assert_eq!(
        (code, out.trim()),
        (0, "Read-only is on for database 'acme'")
    );
    let (code, _, _) = db(root, &["auto-load", "acme", "on"]);
    assert_eq!(code, 0);
    let (code, out, err) = db(root, &["clone", "acme", "acme-2"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.starts_with("Cloned database 'acme' into 'acme-2'\n"),
        "{out}"
    );
    let (code, _, _) = db(root, &["alias", "set", "current", "acme"]);
    assert_eq!(code, 0);
    let (code, out, _) = db(root, &["list"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("── Aliases ───") && out.contains("\n  ALIAS    DATABASE\n  current  acme"),
        "{out}"
    );
    let (code, _, err) = db(root, &["delete", "acme"]);
    assert_eq!(code, 1);
    assert!(err.contains("current"), "{err}");
    assert!(err.contains("\nHelp: remove the alias first"), "{err}");
    let (code, _, _) = db(root, &["alias", "remove", "current"]);
    assert_eq!(code, 0);
    let (code, out, err) = db(root, &["delete", "acme"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.starts_with("Deleted database 'acme'\n  moved to: "),
        "{out}"
    );
    assert!(out.contains(&sep("trash/acme-")), "{out}");
    assert!(!dir.databases().join("acme").exists());
    assert!(
        !ServerLock::is_held(&dir).unwrap(),
        "the console released the lock"
    );

    // Invalid invocations.
    assert_eq!(db(root, &["create"]).0, 2);
    assert_eq!(db(root, &["frobnicate", "x"]).0, 2);
    assert_eq!(db(root, &["read-only", "acme-2", "maybe"]).0, 2);
    assert_eq!(db(root, &["delete", "acme-2", "--base", "x"]).0, 2);
    assert_eq!(run(&["db", "list"]).status.code(), Some(2));
}

#[test]
fn db_commands_go_through_a_running_server() {
    let temp = TempDir::new("console-online");
    let dir = ServerDir::init(temp.path()).unwrap().dir;
    let root = temp.path();
    let mut server = ServerProcess::spawn(
        Command::new(BIN)
            .args(["serve", "--dir"])
            .arg(root)
            .args(["--listen", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    let info = wait_for_run_file(&dir);
    let note = format!("sent to its admin API at {}", info.address);

    let (code, out, err) = db(root, &["create", "acme"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("Created database 'acme'\n"), "{out}");
    assert!(err.contains(&note), "through the server: {err}");
    let (code, out, err) = db(root, &["list"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("acme") && out.contains("unloaded"), "{out}");

    // Load waits for the end of the load; the database is served at once.
    let (code, out, err) = db(root, &["load", "acme"]);
    assert_eq!(code, 0, "{err}");
    // The note shows where the database is served now.
    assert_eq!(
        out,
        format!(
            "Database 'acme' is loaded\n  served at: {}db/acme/\n",
            info.url
        )
    );
    let (status, _) = get(info.address, "/db/acme/api/info");
    assert_eq!(status, 200);
    let (code, out, err) = db(root, &["load", "acme"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out,
        format!(
            "Database 'acme' is loaded already\n  served at: {}db/acme/\n",
            info.url
        )
    );
    // Delete of a loaded database: refused with the API's help.
    let (code, _, err) = db(root, &["delete", "acme"]);
    assert_eq!(code, 1);
    assert!(err.contains("unload it first"), "{err}");
    assert!(err.contains("\nHelp: "), "{err}");
    let (code, out, err) = db(root, &["clone", "acme", "acme-2"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("'acme-2'"), "{out}");
    let (code, out, _) = db(root, &["unload", "acme"]);
    assert_eq!((code, out.trim()), (0, "Unloaded database 'acme'"));
    let (code, out, err) = db(root, &["delete", "acme"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(&sep("trash/acme-")), "{out}");
    let (code, out, _) = db(root, &["list", "--json"]);
    assert_eq!(code, 0);
    let names: Vec<String> = json(&out)["databases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["acme-2"]);
    assert_eq!(get(info.address, "/db/acme/api/info").0, 404);

    send_stop(&server);
    let deadline = Instant::now() + Duration::from_secs(30);
    while server.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "the server did not stop");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn db_backup_restore_and_export_offline_and_through_a_server() {
    use graphersal::persist::read_snapshot_info;
    let temp = TempDir::new("console-transfer");
    let dir = ServerDir::init(temp.path().join("srv")).unwrap().dir;
    let root = dir.root().to_path_buf();
    modern_db(&dir, "acme");

    // Offline.
    let (code, out, err) = db(&root, &["backup", "acme"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.starts_with("Backed up database 'acme'\n  file: "),
        "{out}"
    );
    assert!(out.contains(&sep("backups/acme/")), "{out}");
    let (code, out, _) = db(&root, &["backups", "acme", "--json"]);
    assert_eq!(code, 0);
    let backup = json(&out)["backups"][0]["backup"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, out, _) = db(&root, &["backups", "acme"]);
    assert_eq!(code, 0);
    let lines: Vec<&str> = out.lines().collect();
    assert!(
        lines[0].starts_with("── Backups of 'acme' (1) ───"),
        "{out}"
    );
    assert!(lines[1].starts_with("  BACKUP  "), "{out}");
    assert!(lines[2].starts_with(&format!("  {backup}  ")), "{out}");
    let (code, out, err) = db(&root, &["restore", "acme", &backup, "--name", "acme-2"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("Restored database 'acme-2'\n"), "{out}");
    assert!(
        out.contains(&format!(" backup {backup} of 'acme'\n")),
        "{out}"
    );
    let file = temp.path().join("acme.gsnap");
    let file_arg = file.to_str().unwrap();
    let (code, out, err) = db(&root, &["export", "acme", "--out", file_arg]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("  snapshot: full\n  models:   included\n"),
        "{out}"
    );
    let info = read_snapshot_info(std::fs::File::open(&file).unwrap()).unwrap();
    assert_eq!(info.vertex_count, 6);
    let (code, _, err) = db(&root, &["export", "acme"]);
    assert_eq!(code, 2, "{err}");
    let (code, _, err) = db(&root, &["backup", "acme", "--out", file_arg]);
    assert_eq!(code, 2, "{err}");
    let (code, _, err) = db(&root, &["restore", "acme", "nope"]);
    assert_eq!(code, 1);
    assert!(
        err.contains("no backup 'nope'") && err.contains("\nHelp: "),
        "{err}"
    );

    // Through a running server.
    let mut server = ServerProcess::spawn(
        Command::new(BIN)
            .args(["serve", "--dir"])
            .arg(&root)
            .args(["--listen", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    let info = wait_for_run_file(&dir);
    let note = format!("sent to its admin API at {}", info.address);
    let (code, _, err) = db(&root, &["load", "acme"]);
    assert_eq!(code, 0, "{err}");
    let served = temp.path().join("served.gsnap");
    let (code, out, err) = db(
        &root,
        &[
            "export",
            "acme",
            "--out",
            served.to_str().unwrap(),
            "--no-models",
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert!(err.contains(&note), "{err}");
    assert!(out.contains("  models:   excluded\n"), "{out}");
    assert_eq!(
        read_snapshot_info(std::fs::File::open(&served).unwrap())
            .unwrap()
            .vertex_count,
        6
    );
    let (code, out, err) = db(&root, &["backup", "acme"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(&sep("backups/acme/")), "{out}");
    // Restore over the loaded database: refused with the API's help; unloaded: replaced.
    let (code, _, err) = db(&root, &["restore", "acme", &backup]);
    assert_eq!(code, 1);
    assert!(
        err.contains("unload it first") && err.contains("\nHelp: "),
        "{err}"
    );
    let (code, _, _) = db(&root, &["unload", "acme"]);
    assert_eq!(code, 0);
    let (code, out, err) = db(&root, &["restore", "acme", &backup]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("  replaced:    moved to "), "{out}");
    assert!(out.contains(&sep("trash/acme-")), "{out}");
    let (code, out, _) = db(&root, &["backups", "acme", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(json(&out)["backups"].as_array().unwrap().len(), 2);

    send_stop(&server);
    let deadline = Instant::now() + Duration::from_secs(30);
    while server.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "the server did not stop");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Runs `graphersal-server import ARGS`: (exit code, stdout, stderr).
fn import(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(BIN).arg("import").args(args).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        text(&out.stdout),
        text(&out.stderr),
    )
}

#[test]
fn import_offline_and_through_a_server() {
    use graphersal::prelude::{Graph, GraphExport, TraversalGraph};
    let temp = TempDir::new("console-import");
    let dir = ServerDir::init(temp.path().join("srv")).unwrap().dir;
    let root = dir.root().to_str().unwrap().to_string();
    let graph = Graph::new(TraversalGraph::tinkerpop_modern());
    let graphson = temp.path().join("modern.json");
    graph
        .export_graphson_writer(std::fs::File::create(&graphson).unwrap())
        .unwrap();
    let graphson_arg = graphson.to_str().unwrap();

    // Offline.
    let (code, out, err) = import(&[
        &root,
        graphson_arg,
        "--name",
        "gs",
        "--description",
        "From GraphSON",
    ]);
    assert_eq!(code, 0, "{err}");
    // The layout: a headline, then the details in one column.
    assert_eq!(
        out,
        format!(
            "Imported database 'gs'\n  source:      {graphson_arg}\n  format:      graphson\n  \
             kind:        plain\n  description: From GraphSON\n  state:       unloaded\n  \
             MCP:         not offered\n  # offer it to MCP\n  graphersal-server mcp db gs on \
             --dir {root}\n"
        )
    );
    assert!(!err.contains("sent to its admin API"), "{err}");
    let (code, out, _) = db(dir.root(), &["list", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(json(&out)["databases"][0]["description"], "From GraphSON");
    // Usage errors (2) and refusals (1).
    let (code, _, _) = import(&[&root, graphson_arg]);
    assert_eq!(code, 2, "no --name");
    let (code, _, _) = import(&[&root, "--name", "x"]);
    assert_eq!(code, 2, "no source");
    let (code, _, err) = import(&[&root, graphson_arg, "--name", "gs"]);
    assert_eq!(code, 1);
    assert!(err.contains("taken"), "{err}");
    // A refusal's help names the console command, not the admin route.
    let (code, _, err) = db(dir.root(), &["create", "gs"]);
    assert_eq!(code, 1);
    assert!(
        err.contains(&format!(
            "\nHelp: choose another name: graphersal-server db list --dir {root} lists the \
             taken names"
        )),
        "{err}"
    );
    assert!(!err.contains("/api/admin"), "{err}");
    // The source is shown (and sent) absolute, without `.`/`..` segments.
    let dotted = dir.root().join("..").join(".").join("modern.json");
    let (code, out, err) = import(&[&root, dotted.to_str().unwrap(), "--name", "gs-dots"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains(&format!("\n  source: {graphson_arg}\n")),
        "{out}"
    );
    let (code, _, err) = import(&[&root, "missing.json", "--name", "x"]);
    assert_eq!(code, 1);
    assert!(err.contains("does not exist"), "{err}");
    let file = temp.path().join("gs.gsnap");
    let (code, _, err) = db(
        dir.root(),
        &["export", "gs", "--out", file.to_str().unwrap()],
    );
    assert_eq!(code, 0, "{err}");

    // Through a running server: the server reads the file (path form).
    let mut server = ServerProcess::spawn(
        Command::new(BIN)
            .args(["serve", "--dir", &root, "--listen", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    );
    let info = wait_for_run_file(&dir);
    let (code, out, err) = import(&[&root, file.to_str().unwrap(), "--name", "snap", "--json"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        err.contains(&format!("sent to its admin API at {}", info.address)),
        "{err}"
    );
    let answer = json(&out);
    assert_eq!(answer["name"], "snap");
    assert_eq!(answer["import"]["format"], "snapshot");
    let (code, _, err) = import(&[&root, file.to_str().unwrap(), "--name", "snap"]);
    assert_eq!(code, 1);
    assert!(err.contains("taken"), "{err}");
    let (code, out, err) = db(dir.root(), &["load", "snap"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("Database 'snap' is loaded"), "{out}");
    stop_and_wait(&mut server);
}

#[test]
fn import_from_the_inbox_offline_and_through_a_server() {
    use graphersal::prelude::{Graph, GraphExport, TraversalGraph};
    let temp = TempDir::new("console-inbox");
    let dir = ServerDir::init(temp.path().join("srv")).unwrap().dir;
    let root = dir.root().to_str().unwrap().to_string();
    let (code, out, err) = import(&[&root, "--list"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("The import inbox "), "{out}");
    assert!(
        out.contains(&format!(
            "graphersal-server import {root} --inbox FILE --name NAME"
        )),
        "{out}"
    );
    Graph::new(TraversalGraph::tinkerpop_modern())
        .export_graphson_writer(std::fs::File::create(dir.imports().join("modern.json")).unwrap())
        .unwrap();
    let (code, out, err) = import(&[&root, "--list"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("Import inbox (1)"), "{out}");
    assert!(out.contains("modern.json"), "{out}");
    assert!(out.contains("graphson"), "{out}");
    let (code, out, _) = import(&[&root, "--list", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(json(&out)["imports"][0]["name"], "modern.json");

    // Offline.
    let (code, out, err) = import(&[&root, "--inbox", "modern.json", "--name", "gs"]);
    assert_eq!(code, 0, "{err}");
    let shown = dir.imports().join("modern.json");
    assert!(
        out.contains(&format!("  source: {}\n", shown.display())),
        "{out}"
    );
    // Usage errors (2): SOURCE and --inbox both, --list with options; a traversal (1).
    let (code, _, _) = import(&[&root, "/x.json", "--inbox", "modern.json", "--name", "x"]);
    assert_eq!(code, 2);
    let (code, _, _) = import(&[&root, "--list", "--name", "x"]);
    assert_eq!(code, 2);
    let (code, _, err) = import(&[&root, "--inbox", "../server.toml", "--name", "x"]);
    assert_eq!(code, 1);
    assert!(err.contains("inbox name"), "{err}");
    let (code, _, err) = import(&[&root, "--inbox", "nope.json", "--name", "x"]);
    assert_eq!(code, 1);
    assert!(
        err.contains(&format!("graphersal-server import {root} --list")),
        "the help names the console command: {err}"
    );

    // Through a running server.
    let mut server = ServerProcess::spawn(
        Command::new(BIN)
            .args(["serve", "--dir", &root, "--listen", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    );
    let info = wait_for_run_file(&dir);
    let (code, out, err) = import(&[&root, "--list", "--json"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("sent to its admin API"), "{err}");
    assert_eq!(json(&out)["imports"].as_array().unwrap().len(), 1);
    let (code, out, err) = import(&[&root, "--inbox", "modern.json", "--name", "gs2", "--json"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        err.contains(&format!("sent to its admin API at {}", info.address)),
        "{err}"
    );
    assert_eq!(json(&out)["import"]["format"], "graphson");
    stop_and_wait(&mut server);
}

#[test]
fn import_on_a_taken_name_shows_the_console_help_offline_and_through_a_server() {
    use graphersal::prelude::{Graph, GraphExport, TraversalGraph};
    let temp = TempDir::new("console-import-taken");
    let dir = ServerDir::init(temp.path().join("srv")).unwrap().dir;
    let root = dir.root().to_str().unwrap().to_string();
    let graphson = temp.path().join("modern.json");
    Graph::new(TraversalGraph::tinkerpop_modern())
        .export_graphson_writer(std::fs::File::create(&graphson).unwrap())
        .unwrap();
    let source = graphson.to_str().unwrap();
    let (code, _, err) = import(&[&root, source, "--name", "gs"]);
    assert_eq!(code, 0, "{err}");
    let help = format!(
        "\nHelp: choose another name: graphersal-server db list --dir {root} lists the taken \
         names (databases and aliases share one namespace, case-insensitive)"
    );

    // Offline.
    let (code, _, err) = import(&[&root, source, "--name", "gs"]);
    assert_eq!(code, 1);
    assert!(err.contains(&help), "{err}");
    assert!(!err.contains("/api/admin"), "{err}");

    // Through a running server (the path form of the route).
    let mut server = ServerProcess::spawn(
        Command::new(BIN)
            .args(["serve", "--dir", &root, "--listen", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    );
    wait_for_run_file(&dir);
    let (code, _, err) = import(&[&root, source, "--name", "gs"]);
    assert_eq!(code, 1);
    assert!(err.contains(&help), "{err}");
    assert!(!err.contains("/api/admin"), "{err}");
    stop_and_wait(&mut server);
}

/// Runs `graphersal-server models ARGS --dir DIR`: (exit code, stdout, stderr).
fn models(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(BIN)
        .arg("models")
        .args(args)
        .arg("--dir")
        .arg(dir)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        text(&out.stdout),
        text(&out.stderr),
    )
}

/// A tiny synthetic Model2Vec model directory (4 tokens x 3 dimensions) in `dir`, as in the
/// crate's `test_models.rs` (no network).
fn tiny_model_dir(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let vocab = ["[UNK]", "graph", "vector", "model"];
    let entries: Vec<String> = vocab
        .iter()
        .enumerate()
        .map(|(id, token)| format!("{token:?}: {id}"))
        .collect();
    let tokenizer = format!(
        r#"{{"version": "1.0", "truncation": null, "padding": null, "added_tokens": [],
            "normalizer": {{"type": "Lowercase"}}, "pre_tokenizer": {{"type": "Whitespace"}},
            "post_processor": null, "decoder": null,
            "model": {{"type": "WordLevel", "vocab": {{{}}}, "unk_token": "[UNK]"}}}}"#,
        entries.join(", ")
    );
    let data: Vec<u8> = (0..12)
        .flat_map(|i| (1.0f32 + i as f32).to_le_bytes())
        .collect();
    let mut header = format!(
        r#"{{"embeddings": {{"dtype": "F32", "shape": [4, 3], "data_offsets": [0, {}]}}}}"#,
        data.len()
    );
    while header.len() % 8 != 0 {
        header.push(' ');
    }
    let mut weights = (header.len() as u64).to_le_bytes().to_vec();
    weights.extend_from_slice(header.as_bytes());
    weights.extend_from_slice(&data);
    std::fs::write(dir.join("config.json"), r#"{"normalize": true}"#).unwrap();
    std::fs::write(dir.join("tokenizer.json"), tokenizer).unwrap();
    std::fs::write(dir.join("model.safetensors"), weights).unwrap();
    std::fs::write(
        dir.join("README.md"),
        "---\nlicense: mit\n---\n# A tiny model\n",
    )
    .unwrap();
}

#[test]
fn models_commands_offline_and_through_a_server() {
    let temp = TempDir::new("console-models");
    let dir = ServerDir::init(temp.path().join("srv")).unwrap().dir;
    let root = dir.root().to_str().unwrap().to_string();
    let model = temp.path().join("tiny");
    tiny_model_dir(&model);

    // Offline.
    let (code, out, err) = models(
        dir.root(),
        &[
            "add",
            model.to_str().unwrap(),
            "--repo",
            "acme/e2e",
            "--license",
            "mit",
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert!(
        out.starts_with("Model acme/e2e is in the shared models\n  model: "),
        "{out}"
    );
    assert!(!err.contains("sent to its admin API"), "{err}");
    let (code, out, _) = models(dir.root(), &["list"]);
    assert_eq!(code, 0);
    // The layout: a section, the models as a table in aligned columns.
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("── Shared models (1) ───"), "{out}");
    assert!(lines[1].starts_with("  MODEL     SIZE "), "{out}");
    let column = |name: &str| lines[1].find(name).unwrap();
    assert!(lines[2].starts_with("  acme/e2e  "), "{out}");
    assert_eq!(&lines[2][column("LICENSE")..column("BLOB")], "mit      ");
    assert_eq!(&lines[2][column("USED BY")..], "nobody");
    assert_eq!(lines.len(), 3, "{out}");
    assert!(dir.models().join("models.toml").is_file());
    let (code, _, _) = models(dir.root(), &[]);
    assert_eq!(code, 2, "no command");
    let (code, _, err) = models(dir.root(), &["add", "nobody/none"]);
    assert_eq!(code, 1);
    assert!(
        err.starts_with("Error: ") && err.contains("no model 'nobody/none'"),
        "{err}"
    );
    assert!(err.contains("\nHelp: "), "{err}");

    // Through a running server.
    let mut server = ServerProcess::spawn(
        Command::new(BIN)
            .args(["serve", "--dir", &root, "--listen", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    );
    let info = wait_for_run_file(&dir);
    let (code, out, err) = models(dir.root(), &["list", "--json"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        err.contains(&format!("sent to its admin API at {}", info.address)),
        "{err}"
    );
    assert_eq!(json(&out)["models"][0]["repo"], "acme/e2e");
    let (code, out, err) = models(dir.root(), &["remove", "acme/e2e"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.starts_with("Removed model 'acme/e2e' from the shared models\n  blobs: 1\n"),
        "{out}"
    );
    let (code, _, err) = models(dir.root(), &["remove", "acme/e2e"]);
    assert_eq!(code, 1);
    assert!(err.contains("no model 'acme/e2e'"), "{err}");
    stop_and_wait(&mut server);
}

/// Posts one JSON-RPC message to the MCP endpoint `path` (in the MCP session `session`):
/// (status, head, body).
fn mcp_post(
    address: SocketAddr,
    path: &str,
    session: Option<&str>,
    extra: &[(&str, &str)],
    message: serde_json::Value,
) -> (u16, String, String) {
    let mut headers = vec![
        ("Content-Type", "application/json"),
        ("Accept", "application/json, text/event-stream"),
    ];
    if let Some(session) = session {
        headers.push(("Mcp-Session-Id", session));
    }
    headers.extend_from_slice(extra);
    call(address, "POST", path, &headers, &message.to_string())
}

/// `initialize` on the MCP endpoint `path`: the new MCP session's id.
fn mcp_initialize(address: SocketAddr, path: &str) -> String {
    let (status, head, body) = mcp_post(
        address,
        path,
        None,
        &[],
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "clientInfo": {"name": "it", "version": "1"}}}),
    );
    assert_eq!(status, 200, "{body}");
    header_of(&head, "Mcp-Session-Id")
        .unwrap_or_else(|| panic!("no session id: {head}"))
        .to_string()
}

/// The names of the tools of the MCP endpoint `path`.
fn mcp_tools(address: SocketAddr, path: &str, session: &str) -> Vec<String> {
    let (status, _, body) = mcp_post(
        address,
        path,
        Some(session),
        &[],
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    );
    assert_eq!(status, 200, "{body}");
    json(&body)["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

/// Calls the tool `name` on the MCP endpoint `path`: (text, isError).
fn mcp_tool(
    address: SocketAddr,
    path: &str,
    session: &str,
    name: &str,
    args: serde_json::Value,
) -> (String, bool) {
    let (status, _, body) = mcp_post(
        address,
        path,
        Some(session),
        &[],
        serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": name, "arguments": args}}),
    );
    assert_eq!(status, 200, "{body}");
    let result = json(&body)["result"].clone();
    (
        result["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        result["isError"].as_bool().unwrap_or(false),
    )
}

/// The number of persons over the MCP endpoint `path` (on `graph` when given).
fn mcp_persons(address: SocketAddr, path: &str, session: &str, graph: Option<&str>) -> u64 {
    let mut args = serde_json::json!({"query": "MATCH (p:person) RETURN count(p) AS n"});
    if let Some(graph) = graph {
        args["graph"] = serde_json::json!(graph);
    }
    let (text, is_error) = mcp_tool(address, path, session, "gql", args);
    assert!(!is_error, "{text}");
    let at = text
        .find("\"n\":")
        .unwrap_or_else(|| panic!("no count: {text}"))
        + 4;
    text[at..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap()
}

#[test]
fn mcp_serves_every_database_through_one_endpoint() {
    let temp = TempDir::new("mcp");
    let dir = ServerDir::init(temp.path()).unwrap().dir;
    mcp_on(&dir);
    for name in ["a", "b", "c"] {
        modern_db(&dir, name);
    }
    let running = serve::start(
        temp.path(),
        ServeOptions::new().with_listen("127.0.0.1:0".parse().unwrap()),
    )
    .unwrap();
    let address = running.address();
    let loader = running.loader();
    for name in ["a", "b"] {
        loader.load(name).unwrap();
        assert_eq!(
            loader.wait(name, Duration::from_secs(120)).unwrap(),
            DbState::Loaded
        );
    }
    let root = json(&get(address, "/api/info").1);
    assert_eq!(root["mcp"]["endpoint"], "/mcp");

    // One endpoint: the directory tools beside the data tools.
    let session = mcp_initialize(address, "/mcp");
    let tools = mcp_tools(address, "/mcp", &session);
    for tool in ["list_graphs", "use_graph", "gql", "query"] {
        assert!(tools.iter().any(|t| t == tool), "{tool}: {tools:?}");
    }
    let (text, is_error) = mcp_tool(
        address,
        "/mcp",
        &session,
        "list_graphs",
        serde_json::json!({}),
    );
    assert!(!is_error, "{text}");
    let listed = json(&text);
    let state_of = |name: &str| {
        listed["graphs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["name"] == name)
            .map(|g| g["state"].as_str().unwrap().to_string())
    };
    assert_eq!(state_of("a").as_deref(), Some("loaded"));
    assert_eq!(state_of("b").as_deref(), Some("loaded"));
    assert_eq!(state_of("c").as_deref(), Some("unloaded"));
    assert_eq!(listed["selected"], serde_json::Value::Null);

    // Nothing selected: the error names the flow.
    let (text, is_error) = mcp_tool(
        address,
        "/mcp",
        &session,
        "gql",
        serde_json::json!({"query": "MATCH (p) RETURN count(p) AS n"}),
    );
    assert!(is_error && text.contains("use_graph"), "{text}");

    // use_graph a, a write and a read on a; `graph: "b"` for one call.
    let (text, is_error) = mcp_tool(
        address,
        "/mcp",
        &session,
        "use_graph",
        serde_json::json!({"name": "a"}),
    );
    assert!(!is_error && text.contains("\"selected\":\"a\""), "{text}");
    let (text, is_error) = mcp_tool(
        address,
        "/mcp",
        &session,
        "gql",
        serde_json::json!({"query": "INSERT (:person {name: 'zed'})"}),
    );
    assert!(!is_error, "{text}");
    assert_eq!(mcp_persons(address, "/mcp", &session, None), 5);
    assert_eq!(mcp_persons(address, "/mcp", &session, Some("b")), 4);
    // The write reached the database page too.
    assert_eq!(
        execute(address, "a", "g.v().has_label(\"person\").count().next()"),
        5
    );

    // A session keeps the alias name it selected: a flip moves the open session, a new one
    // selects the same name.
    running.registry().alias_set("cur", "a").unwrap();
    let pinned = mcp_initialize(address, "/mcp");
    let (text, is_error) = mcp_tool(
        address,
        "/mcp",
        &pinned,
        "use_graph",
        serde_json::json!({"name": "cur"}),
    );
    assert!(
        !is_error && text.contains("\"selected\":\"cur\",\"graph\":\"a\""),
        "{text}"
    );
    running.registry().alias_set("cur", "b").unwrap();
    assert_eq!(mcp_persons(address, "/mcp", &pinned, None), 4);
    let fresh = mcp_initialize(address, "/mcp");
    let (text, is_error) = mcp_tool(
        address,
        "/mcp",
        &fresh,
        "use_graph",
        serde_json::json!({"name": "cur"}),
    );
    assert!(
        !is_error && text.contains("\"selected\":\"cur\",\"graph\":\"b\""),
        "{text}"
    );
    assert_eq!(mcp_persons(address, "/mcp", &fresh, None), 4);
    let (text, _) = mcp_tool(
        address,
        "/mcp",
        &fresh,
        "list_graphs",
        serde_json::json!({}),
    );
    assert!(
        text.contains("\"name\":\"cur\",\"aliasOf\":\"b\""),
        "{text}"
    );

    // An unloaded database: the 409-style tool error, the selection unchanged; never a load.
    let (text, is_error) = mcp_tool(
        address,
        "/mcp",
        &session,
        "use_graph",
        serde_json::json!({"name": "c"}),
    );
    assert!(is_error, "{text}");
    assert!(
        text.contains("database 'c' is not loaded; ask an operator to load it"),
        "{text}"
    );
    let (text, is_error) = mcp_tool(
        address,
        "/mcp",
        &session,
        "gql",
        serde_json::json!({"query": "MATCH (p) RETURN count(p) AS n", "graph": "c"}),
    );
    assert!(is_error && text.contains("not loaded"), "{text}");
    assert_eq!(
        running.registry().get("c").unwrap().state,
        DbState::Unloaded
    );
    assert_eq!(mcp_persons(address, "/mcp", &session, None), 5);
    let (text, is_error) = mcp_tool(
        address,
        "/mcp",
        &session,
        "use_graph",
        serde_json::json!({"name": "nope"}),
    );
    assert!(is_error && text.contains("'nope'"), "{text}");

    // The runtime read-only toggle refuses an agent's write; b still takes one.
    loader.set_read_only("a", true).unwrap();
    let insert = serde_json::json!({"query": "INSERT (:person {name: 'yan'})"});
    let (text, is_error) = mcp_tool(address, "/mcp", &session, "gql", insert.clone());
    assert!(is_error, "{text}");
    assert_eq!(mcp_persons(address, "/mcp", &session, None), 5);
    let mut on_b = insert;
    on_b["graph"] = serde_json::json!("b");
    let (text, is_error) = mcp_tool(address, "/mcp", &session, "gql", on_b);
    assert!(!is_error, "{text}");
    assert_eq!(mcp_persons(address, "/mcp", &session, Some("b")), 5);
    let (text, _) = mcp_tool(
        address,
        "/mcp",
        &session,
        "list_graphs",
        serde_json::json!({}),
    );
    assert!(
        text.contains("\"name\":\"a\",\"kind\":\"plain\",\"state\":\"loaded\",\"readOnly\":true"),
        "{text}"
    );
    loader.set_read_only("a", false).unwrap();

    // The single-graph endpoint of a loaded database: today's tools, its own graph.
    let single = mcp_initialize(address, "/db/b/mcp");
    let tools = mcp_tools(address, "/db/b/mcp", &single);
    assert!(tools.iter().any(|t| t == "gql"), "{tools:?}");
    assert!(
        !tools.iter().any(|t| t == "list_graphs" || t == "use_graph"),
        "{tools:?}"
    );
    assert_eq!(mcp_persons(address, "/db/b/mcp", &single, None), 5);
    let info = json(&call(address, "GET", "/db/b/api/info", &[], "").2);
    assert_eq!(info["mcp"]["endpoint"], "/db/b/mcp");
    // Not loaded: the mount's 409.
    let (status, _, body) = mcp_post(
        address,
        "/db/c/mcp",
        None,
        &[],
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}),
    );
    assert_eq!(status, 409, "{body}");

    // A foreign Origin is refused before the endpoint; only POST and DELETE are served.
    let (status, _, body) = mcp_post(
        address,
        "/mcp",
        Some(&session),
        &[("Origin", "http://evil.example")],
        serde_json::json!({"jsonrpc": "2.0", "id": 4, "method": "tools/list"}),
    );
    assert_eq!(status, 403, "{body}");
    assert_eq!(call(address, "GET", "/mcp", &[], "").0, 405);
    let (status, _, body) = call(address, "DELETE", "/mcp", &[("Mcp-Session-Id", &fresh)], "");
    assert_eq!(status, 200, "{body}");
    running.stop().unwrap();
}

/// An admin API request with a JSON body: (status, answer).
fn admin_json(
    address: SocketAddr,
    method: &str,
    path: &str,
    body: serde_json::Value,
) -> (u16, serde_json::Value) {
    let (status, _, answer) = call(
        address,
        method,
        &format!("/api/admin/{path}"),
        &[("Content-Type", "application/json")],
        &body.to_string(),
    );
    (status, json(&answer))
}

#[test]
fn mcp_sessions_follow_their_alias_name() {
    let temp = TempDir::new("mcp-alias");
    let root = temp.path();
    let dir = ServerDir::init(root).unwrap().dir;
    mcp_on(&dir);
    for name in ["a", "b"] {
        modern_db(&dir, name);
    }
    // The console renames offline too (the alias offered to MCP at birth: `--mcp`).
    let (code, _, err) = db(root, &["alias", "set", "old", "a", "--mcp"]);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = db(root, &["alias", "rename", "old", "cur"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out,
        "Renamed alias 'old' to 'cur' -> database 'a'\n  MCP sessions moved: 0\n"
    );
    let (code, _, err) = db(root, &["alias", "rename", "cur", "b"]);
    assert_eq!(code, 1);
    assert!(err.contains("\nHelp: choose another name"), "{err}");

    let running = serve::start(
        root,
        ServeOptions::new().with_listen("127.0.0.1:0".parse().unwrap()),
    )
    .unwrap();
    let address = running.address();
    let loader = running.loader();
    for name in ["a", "b"] {
        loader.load(name).unwrap();
        assert_eq!(
            loader.wait(name, Duration::from_secs(120)).unwrap(),
            DbState::Loaded
        );
    }
    let session = mcp_initialize(address, "/mcp");
    // b gets a fifth person: the counts tell the graphs apart (a 4, b 5).
    let (text, is_error) = mcp_tool(
        address,
        "/mcp",
        &session,
        "gql",
        serde_json::json!({"query": "INSERT (:person {name: 'zed'})", "graph": "b"}),
    );
    assert!(!is_error, "{text}");
    let (text, is_error) = mcp_tool(
        address,
        "/mcp",
        &session,
        "use_graph",
        serde_json::json!({"name": "CUR"}),
    );
    assert!(
        !is_error && text.contains("\"selected\":\"cur\",\"graph\":\"a\""),
        "{text}"
    );
    let count = serde_json::json!({"query": "MATCH (p:person) RETURN count(p) AS n"});
    let counted = |expected: u64| {
        let (text, is_error) = mcp_tool(address, "/mcp", &session, "gql", count.clone());
        assert!(!is_error, "{text}");
        assert!(text.contains(&format!("\"n\":{expected}")), "{text}");
        text
    };
    assert!(!counted(4).starts_with("note:"));

    // 1. A re-pointed alias: the session follows silently.
    let (status, answer) = admin_json(
        address,
        "PUT",
        "aliases/cur",
        serde_json::json!({"target": "b"}),
    );
    assert_eq!(status, 200, "{answer}");
    assert!(!counted(5).starts_with("note:"));

    // 2. A removed alias: an error naming the flow while no graph has the name.
    let (status, _, body) = call(address, "DELETE", "/api/admin/aliases/cur", &[], "");
    assert_eq!(status, 200, "{body}");
    let (text, is_error) = mcp_tool(address, "/mcp", &session, "gql", count.clone());
    assert!(is_error, "{text}");
    assert!(
        text.contains("\"cur\"")
            && text.contains("pick another graph: list_graphs, then use_graph"),
        "{text}"
    );

    // 3. Created again under the name (offered to MCP at birth: a new alias is hidden by
    //    default, round 4 B-MCPDEF): the session works on, no note.
    let (status, answer) = admin_json(
        address,
        "PUT",
        "aliases/cur",
        serde_json::json!({"target": "a", "mcp": true}),
    );
    assert_eq!(status, 200, "{answer}");
    assert!(!counted(4).starts_with("note:"));

    // 4. Renamed (the console through the running server): one note, then none.
    let (code, out, err) = db(root, &["alias", "rename", "cur", "live"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out,
        "Renamed alias 'cur' to 'live' -> database 'a'\n  MCP sessions moved: 1\n"
    );
    assert!(err.contains("sent to its admin API"), "{err}");
    let text = counted(4);
    assert!(
        text.starts_with("note: alias \"cur\" was renamed to \"live\"\n"),
        "{text}"
    );
    assert!(!counted(4).starts_with("note:"));
    let (text, is_error) = mcp_tool(
        address,
        "/mcp",
        &session,
        "list_graphs",
        serde_json::json!({}),
    );
    assert!(!is_error, "{text}");
    let listed = json(&text);
    assert_eq!(listed["selected"], "live");
    assert_eq!(listed["selectedGraph"], "a");
    // Several renames before the next call: one note, the first name -> the current one.
    for (old, new) in [("live", "v2"), ("v2", "v3")] {
        let (status, answer) = admin_json(
            address,
            "POST",
            &format!("aliases/{old}/rename"),
            serde_json::json!({ "name": new }),
        );
        assert_eq!(status, 200, "{answer}");
        assert_eq!(answer["mcpSessionsMoved"], 1, "{answer}");
    }
    let text = counted(4);
    assert!(
        text.starts_with("note: alias \"live\" was renamed to \"v3\"\n"),
        "{text}"
    );
    assert!(!counted(4).starts_with("note:"));
    // The old names are gone over HTTP too.
    assert_eq!(call(address, "GET", "/db/live/api/info", &[], "").0, 404);
    assert_eq!(call(address, "GET", "/db/v3/api/info", &[], "").0, 200);
    running.stop().unwrap();
}

#[test]
fn mcp_token_and_off_switch_come_from_server_toml() {
    let temp = TempDir::new("mcp-config");
    let dir = ServerDir::init(temp.path()).unwrap().dir;
    mcp_on(&dir);
    let config = dir.root().join("server.toml");
    // `[mcp] token` is gone (B3: users and grants in access.toml replace it): refused.
    std::fs::write(&config, "[mcp]\ntoken = \"s3cret\"\nmax_sessions = 2\n").unwrap();
    let listen: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let err = serve::start(temp.path(), ServeOptions::new().with_listen(listen))
        .err()
        .expect("the old token key is refused")
        .to_string();
    assert!(err.contains("token"), "{err}");
    std::fs::write(&config, "[mcp]\nmax_sessions = 2\n").unwrap();
    let running = serve::start(temp.path(), ServeOptions::new().with_listen(listen)).unwrap();
    let address = running.address();
    let init = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "clientInfo": {"name": "it", "version": "1"}}});
    // A token nobody holds is refused; the admin's (and anonymous) MCP sessions open.
    let (status, _, body) = mcp_post(
        address,
        "/mcp",
        None,
        &[("Authorization", "Bearer s3cret")],
        init.clone(),
    );
    assert_eq!(status, 401, "{body}");
    let (status, head, body) = mcp_post(address, "/mcp", None, &[], init);
    assert_eq!(status, 200, "{body}");
    assert!(header_of(&head, "Mcp-Session-Id").is_some(), "{head}");
    running.stop().unwrap();

    std::fs::write(&config, "[mcp]\nenabled = false\n").unwrap();
    let running = serve::start(temp.path(), ServeOptions::new().with_listen(listen)).unwrap();
    let address = running.address();
    let (status, _, body) = mcp_post(
        address,
        "/mcp",
        None,
        &[],
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}),
    );
    assert_eq!(status, 404, "{body}");
    assert_eq!(
        json(&get(address, "/api/info").1)["mcp"],
        serde_json::Value::Null
    );
    running.stop().unwrap();
}

#[test]
fn bases_with_data_by_import_and_promote_offline_and_through_a_server() {
    use graphersal::prelude::{Graph, GraphExport, TraversalGraph};
    let temp = TempDir::new("console-promote");
    let dir = ServerDir::init(temp.path().join("srv")).unwrap().dir;
    let root = dir.root().to_str().unwrap().to_string();
    let graphson = temp.path().join("modern.json");
    Graph::new(TraversalGraph::tinkerpop_modern())
        .export_graphson_writer(std::fs::File::create(&graphson).unwrap())
        .unwrap();
    let graphson_arg = graphson.to_str().unwrap();

    // Offline: import --role base, import plain + promote, refusals.
    let (code, out, err) = import(&[&root, graphson_arg, "--name", "prod", "--role", "base"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("Imported database 'prod'\n"), "{out}");
    assert!(
        out.contains("  kind:   base\n  flags:  read-only\n"),
        "{out}"
    );
    let (code, _, err) = import(&[&root, graphson_arg, "--name", "x", "--role", "boss"]);
    assert_eq!(code, 2, "{err}");
    let (code, _, err) = import(&[&root, graphson_arg, "--name", "acme"]);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = db(dir.root(), &["promote", "acme"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.starts_with("Database 'acme' is a base now\n") && out.contains("frozen"),
        "{out}"
    );
    let (code, _, err) = db(dir.root(), &["promote", "acme"]);
    assert_eq!(code, 1);
    assert!(err.contains("a base already"), "{err}");
    let (code, _, err) = db(dir.root(), &["create", "l1", "--base", "acme"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = db(dir.root(), &["promote", "l1"]);
    assert_eq!(code, 1);
    assert!(
        err.contains("\nHelp: ") && err.contains("materialize"),
        "{err}"
    );
    assert_eq!(db(dir.root(), &["promote"]).0, 2);
    let (code, _, err) = import(&[&root, graphson_arg, "--name", "other"]);
    assert_eq!(code, 0, "{err}");

    // Through a running server: promote refuses a loaded database, then promotes it; a
    // layer over it sees its data.
    let mut server = ServerProcess::spawn(
        Command::new(BIN)
            .args(["serve", "--dir", &root, "--listen", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    );
    let info = wait_for_run_file(&dir);
    let (code, _, err) = db(dir.root(), &["load", "other"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = db(dir.root(), &["promote", "other"]);
    assert_eq!(code, 1);
    assert!(
        err.contains("unload it first") && err.contains("\nHelp: "),
        "{err}"
    );
    let (code, _, err) = db(dir.root(), &["unload", "other"]);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = db(dir.root(), &["promote", "other", "--json"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        err.contains(&format!("sent to its admin API at {}", info.address)),
        "{err}"
    );
    assert_eq!(json(&out)["role"], "base");
    let (code, _, err) = db(dir.root(), &["create", "l2", "--base", "other"]);
    assert_eq!(code, 0, "{err}");
    for layer in ["l1", "l2"] {
        let (code, _, err) = db(dir.root(), &["load", layer]);
        assert_eq!(code, 0, "{err}");
        assert_eq!(execute(info.address, layer, "g.v().count().next()"), 6);
        execute(info.address, layer, "g.add_v(\"own\").iterate()");
        assert_eq!(execute(info.address, layer, "g.v().count().next()"), 7);
    }
    let (code, _, err) = import(&[&root, graphson_arg, "--name", "srv-base", "--role", "base"]);
    assert_eq!(code, 0, "{err}");
    stop_and_wait(&mut server);
}

#[test]
fn the_first_serve_without_access_toml_prints_the_admin_token_once() {
    let temp = TempDir::new("first-admin");
    let dir = ServerDir::init(temp.path()).unwrap().dir;
    // A server directory without access.toml (made before access control) is closed: the
    // first serve makes the admin and prints its token on stderr, framed; stdout keeps only
    // the address line.
    std::fs::remove_file(dir.access_path()).unwrap();
    let root = temp.path().to_str().unwrap();
    let mut server = ServerProcess::spawn(
        Command::new(BIN)
            .args(["serve", "--dir", root, "--listen", "127.0.0.1:0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    );
    let info = wait_for_run_file(&dir);
    assert!(info.operator_token.is_some());
    let (stdout, stderr) = stop_and_wait(&mut server);
    assert_eq!(stdout, format!("Graphersal server: {}\n", info.url));
    let start = stderr
        .find("\n+--")
        .unwrap_or_else(|| panic!("no frame: {stderr}"));
    let block: String = stderr[start + 1..]
        .lines()
        .take_while(|l| l.starts_with('+') || l.starts_with('|'))
        .collect::<Vec<_>>()
        .join("\n");
    let token = first_admin_token(&block);
    let file = graphersal_server::access::AccessFile::read(&dir)
        .unwrap()
        .unwrap();
    assert!(
        file.user_by_token(&graphersal_server::access::TokenHash::of(&token))
            .is_some()
    );
    assert!(
        !std::fs::read_to_string(dir.access_path())
            .unwrap()
            .contains(&token)
    );
    assert_eq!(stderr.matches(&token).count(), 1, "{stderr}");

    // The next serve prints no token.
    let mut server = ServerProcess::spawn(
        Command::new(BIN)
            .args(["serve", "--dir", root, "--listen", "127.0.0.1:0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    );
    wait_for_run_file(&dir);
    let (_, stderr) = stop_and_wait(&mut server);
    assert!(!stderr.contains("FIRST ADMIN TOKEN"), "{stderr}");
}

/// B3 over the listener: the first admin's token (printed once by `init`) logs a browser in,
/// the cookie is the admin's credentials, anonymous meets a closed server, a reader cannot
/// write, and logout ends the session.
#[test]
fn access_is_enforced_over_the_listener_with_the_first_admin_token() {
    let temp = TempDir::new("access-e2e");
    let root = temp.path().join("srv");
    let out = run(&["init", root.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let admin_token = first_admin_token(&text(&out.stderr));
    let dir = ServerDir::new(&root);
    modern_db(&dir, "acme");
    let running = serve::start(
        &root,
        ServeOptions::new().with_listen("127.0.0.1:0".parse().unwrap()),
    )
    .unwrap();
    let address = running.address();
    let anonymous = |method: &str, path: &str, body: &str| {
        send(
            address,
            method,
            path,
            &[("Content-Type", "application/json")],
            "",
            body,
        )
    };
    // Anonymous: the minimal info, 401 on the admin API and the database, the login page.
    let (status, _, body) = anonymous("GET", "/api/info", "");
    assert_eq!(status, 200);
    let info = json(&body);
    assert_eq!(info["principal"]["kind"], "anonymous", "{info}");
    assert_eq!(info["login"], "/");
    for path in [
        "/api/admin/databases",
        "/db/acme/api/info",
        "/db/nope/api/info",
    ] {
        let (status, head, body) = anonymous("GET", path, "");
        assert_eq!(status, 401, "{path}: {body}");
        assert_eq!(json(&body)["login"], "/", "{path}");
        assert_eq!(header_of(&head, "WWW-Authenticate"), Some("Bearer"));
    }
    let (status, head, _) = anonymous("GET", "/db/acme/", "");
    assert_eq!(status, 303);
    assert_eq!(header_of(&head, "Location"), Some("/?next=/db/acme/"));
    // The first admin logs in; the cookie loads and reaches the database.
    let (status, head, body) = anonymous(
        "POST",
        "/api/login",
        &serde_json::json!({ "token": admin_token }).to_string(),
    );
    assert_eq!(status, 200, "{body}");
    let cookie = header_of(&head, "Set-Cookie").unwrap();
    assert!(
        cookie.contains("HttpOnly; SameSite=Strict; Path=/"),
        "{cookie}"
    );
    let session = cookie.split(';').next().unwrap().to_string();
    let as_browser =
        |method: &str, path: &str| send(address, method, path, &[("Cookie", &session)], "", "");
    let (status, _, body) = as_browser("POST", "/api/admin/databases/acme/load");
    assert!(status == 200 || status == 202, "{body}");
    let deadline = Instant::now() + Duration::from_secs(60);
    while as_browser("GET", "/db/acme/api/info").0 != 200 {
        assert!(Instant::now() < deadline, "acme never loaded");
        std::thread::sleep(Duration::from_millis(50));
    }
    let (_, _, body) = as_browser("GET", "/api/info");
    assert_eq!(json(&body)["principal"]["admin"], true, "{body}");
    // A reader made through the access state reads but cannot write.
    let access = running.access().unwrap();
    let operator = graphersal_server::Principal::operator();
    let reader = access.add_user(&operator, "reader", Vec::new()).unwrap();
    access
        .grant(
            &operator,
            graphersal_server::access::Grant::new(
                graphersal_server::access::Subject::parse("user:reader").unwrap(),
                "acme",
                graphersal_server::Level::Read,
            )
            .unwrap(),
        )
        .unwrap();
    let bearer = format!("Authorization: Bearer {}\r\n", reader.expose());
    let as_reader = |method: &str, path: &str, body: &str| {
        send(
            address,
            method,
            path,
            &[("Content-Type", "application/json")],
            &bearer,
            body,
        )
    };
    let (status, _, body) = as_reader("GET", "/db/acme/api/info", "");
    assert_eq!(status, 200);
    assert_eq!(json(&body)["readOnly"], true, "{body}");
    let write = serde_json::json!({ "script": "g.add_v(\"person\").iterate()" }).to_string();
    let (status, _, body) = as_reader("POST", "/db/acme/api/execute", &write);
    assert_eq!(status, 200);
    assert!(body.to_lowercase().contains("denied"), "{body}");
    assert_eq!(as_reader("GET", "/api/admin/databases", "").0, 404);
    let count = execute(address, "acme", "g.v().has_label(\"person\").count()");
    assert_eq!(count, serde_json::json!(4), "nothing written: {count}");
    // Logout ends the browser session.
    let (status, head, _) = as_browser("POST", "/api/logout");
    assert_eq!(status, 200);
    assert!(
        header_of(&head, "Set-Cookie")
            .unwrap()
            .contains("Max-Age=0")
    );
    assert_eq!(as_browser("GET", "/api/admin/databases").0, 401);
    running.stop().unwrap();
}

fn user(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(BIN)
        .arg("user")
        .args(args)
        .arg("--dir")
        .arg(dir)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        text(&out.stdout),
        text(&out.stderr),
    )
}

/// The token of `user add` / `user token`'s text (its `token:` line).
fn printed_token(out: &str) -> String {
    out.lines()
        .find_map(|l| l.trim().strip_prefix("token:"))
        .map(|t| t.trim().to_string())
        .unwrap_or_else(|| panic!("no token in {out}"))
}

#[test]
fn user_commands_offline_and_through_a_server() {
    let temp = TempDir::new("console-users");
    let dir = ServerDir::init(temp.path()).unwrap().dir;
    let root = temp.path();
    // Offline: the console edits access.toml; the token on stdout, once.
    let (code, out, err) = user(root, &["add", "bob"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.is_empty(), "{err}");
    assert!(out.starts_with("Added user 'bob'\n"), "{out}");
    let bob = printed_token(&out);
    assert_eq!(bob.len(), 64, "{out}");
    let (code, out, _) = user(root, &["list"]);
    assert_eq!(code, 0);
    assert!(out.contains(&format!("…{}", &bob[60..])), "{out}");
    assert!(!out.contains(&bob), "{out}");
    let (code, _, err) = user(root, &["remove", "admin"]);
    assert_eq!(code, 1);
    assert!(err.contains("is the last admin"), "{err}");
    assert!(
        err.contains("graphersal-server user admin USER on --dir"),
        "{err}"
    );
    let (code, _, err) = user(root, &["grant", "bob", "acme", "admin"]);
    assert_eq!(code, 2, "a usage error: {err}");

    // Through a server: its sessions follow at once.
    let mut server = ServerProcess::spawn(
        Command::new(BIN)
            .args(["serve", "--dir"])
            .arg(root)
            .args(["--listen", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    let info = wait_for_run_file(&dir);
    let note = format!("sent to its admin API at {}", info.address);
    let bob_info = |token: &str| {
        let auth = format!("Authorization: Bearer {token}\r\n");
        send(info.address, "GET", "/api/info", &[], &auth, "").0
    };
    assert_eq!(bob_info(&bob), 200);
    let (code, out, err) = user(root, &["token", "bob"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains(&note), "{err}");
    assert!(out.contains("0 MCP sessions"), "{out}");
    let renewed = printed_token(&out);
    assert_eq!(bob_info(&bob), 401, "the old token stopped working");
    assert_eq!(bob_info(&renewed), 200);
    let (code, out, err) = user(root, &["admin", "bob", "on"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("User 'bob' is an admin"), "{out}");
    let (code, _, err) = user(root, &["remove", "admin"]);
    assert_eq!(code, 0, "bob is an admin now: {err}");
    let (code, out, err) = user(root, &["list", "--json"]);
    assert_eq!(code, 0, "{err}");
    let names: Vec<String> = json(&out)["users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["bob"]);
    let (code, _, err) = user(root, &["token", "bob", "--revoke"]);
    assert_eq!(code, 1);
    assert!(err.contains("is the last admin"), "{err}");

    send_stop(&server);
    let deadline = Instant::now() + Duration::from_secs(30);
    while server.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "the server did not stop");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Starts a server on `root` (port 0) with `acme` loaded.
fn start_with_acme(root: &Path) -> serve::Running {
    let running = serve::start(
        root,
        ServeOptions::new().with_listen("127.0.0.1:0".parse().unwrap()),
    )
    .unwrap();
    running.loader().load("acme").unwrap();
    assert_eq!(
        running
            .loader()
            .wait("acme", Duration::from_secs(120))
            .unwrap(),
        DbState::Loaded
    );
    running
}

/// A browser login with `token`: the `Cookie` header value.
fn browser_login(address: SocketAddr, token: &str) -> String {
    let (status, head, body) = send(
        address,
        "POST",
        "/api/login",
        &[("Content-Type", "application/json")],
        "",
        &serde_json::json!({ "token": token }).to_string(),
    );
    assert_eq!(status, 200, "{body}");
    header_of(&head, "Set-Cookie")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

/// The principal id `/api/info` names for the browser `cookie`, or the refusal's status.
fn browser_principal(address: SocketAddr, cookie: &str) -> Result<String, u16> {
    let (status, _, body) = send(address, "GET", "/api/info", &[("Cookie", cookie)], "", "");
    if status != 200 {
        return Err(status);
    }
    Ok(json(&body)["principal"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string())
}

/// A `gql` count of persons in the MCP session `session` as the bearer of `token`, without a
/// `graph` (the session's selection): `Ok(count)`, or `Err(status)` when the request is
/// refused.
fn mcp_selected_persons(
    address: SocketAddr,
    token: &str,
    session: &str,
) -> Result<serde_json::Value, u16> {
    let bearer = format!("Bearer {token}");
    let (status, _, body) = mcp_post(
        address,
        "/mcp",
        Some(session),
        &[("Authorization", &bearer)],
        serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "gql", "arguments": {"query": "MATCH (p:person) RETURN count(p) AS n"}}}),
    );
    if status != 200 {
        return Err(status);
    }
    Ok(json(&body)["result"].clone())
}

/// `initialize` + `use_graph acme` on `/mcp` as the bearer of `token`: the session id.
fn mcp_session_on_acme(address: SocketAddr, token: &str) -> String {
    let bearer = format!("Bearer {token}");
    let (status, head, body) = mcp_post(
        address,
        "/mcp",
        None,
        &[("Authorization", &bearer)],
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "clientInfo": {"name": "it", "version": "1"}}}),
    );
    assert_eq!(status, 200, "{body}");
    let session = header_of(&head, "Mcp-Session-Id").unwrap().to_string();
    let (status, _, body) = mcp_post(
        address,
        "/mcp",
        Some(&session),
        &[("Authorization", &bearer)],
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": "use_graph", "arguments": {"name": "acme"}}}),
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("selected"), "{body}");
    session
}

/// Whether `result` (a `tools/call` result) is the modern sample's count of 4 persons.
fn counts_four(result: &serde_json::Value) -> bool {
    result["isError"] != true
        && result["content"][0]["text"]
            .as_str()
            .is_some_and(|t| t.contains("\"n\":4"))
}

#[test]
fn browser_and_mcp_sessions_survive_a_restart_until_their_token_changes() {
    let temp = TempDir::new("sessions-restart");
    let root = temp.path().join("srv");
    let dir = ServerDir::init(&root).unwrap().dir;
    mcp_on(&dir);
    modern_db(&dir, "acme");
    let running = start_with_acme(&root);
    let address = running.address();
    let mut tokens = std::collections::HashMap::new();
    for name in ["alice", "bob"] {
        let (status, answer) = admin_json(
            address,
            "POST",
            "users",
            serde_json::json!({ "name": name }),
        );
        assert_eq!(status, 201, "{answer}");
        tokens.insert(name, answer["token"].as_str().unwrap().to_string());
        let (status, answer) = admin_json(
            address,
            "POST",
            "grants",
            serde_json::json!({"subject": name, "database": "acme", "level": "read"}),
        );
        assert_eq!(status, 200, "{answer}");
    }
    let cookies: std::collections::HashMap<&str, String> = ["alice", "bob"]
        .into_iter()
        .map(|n| (n, browser_login(address, &tokens[n])))
        .collect();
    let mcp: std::collections::HashMap<&str, String> = ["alice", "bob"]
        .into_iter()
        .map(|n| (n, mcp_session_on_acme(address, &tokens[n])))
        .collect();
    for name in ["alice", "bob"] {
        assert_eq!(
            browser_principal(address, &cookies[name]).as_deref(),
            Ok(name)
        );
        assert!(counts_four(
            &mcp_selected_persons(address, &tokens[name], &mcp[name]).unwrap()
        ));
    }
    running.stop().unwrap();

    // On disk: hashes only.
    let browser_file = dir.sessions().join("browser.json");
    let mcp_file = dir.sessions().join("mcp.json");
    let stored = std::fs::read_to_string(&browser_file).unwrap()
        + std::fs::read_to_string(&mcp_file).unwrap().as_str();
    for secret in cookies.values().chain(mcp.values()).chain(tokens.values()) {
        let bare = secret.trim_start_matches("gs_session=");
        assert!(!stored.contains(bare), "a clear secret on disk: {stored}");
    }

    // 1. A restart: the cookies and the MCP sessions (with their selection) keep working.
    let running = start_with_acme(&root);
    let address = running.address();
    for name in ["alice", "bob"] {
        assert_eq!(
            browser_principal(address, &cookies[name]).as_deref(),
            Ok(name)
        );
        let result = mcp_selected_persons(address, &tokens[name], &mcp[name]).unwrap();
        assert!(counts_four(&result), "{name}: {result}");
    }
    // Another principal does not find alice's MCP session.
    assert_eq!(
        mcp_selected_persons(address, &tokens["bob"], &mcp["alice"]),
        Err(404)
    );

    // 2. Bob's token revoked, then reissued (online): his sessions end at once, on disk too.
    let (status, _, body) = call(address, "DELETE", "/api/admin/users/bob/token", &[], "");
    assert_eq!(status, 200, "{body}");
    assert_eq!(json(&body)["mcpSessionsEnded"], 1, "{body}");
    let (status, answer) = admin_json(address, "POST", "users/bob/token", serde_json::json!({}));
    assert_eq!(status, 200, "{answer}");
    let bob_new = answer["token"].as_str().unwrap().to_string();
    let stored = std::fs::read_to_string(&browser_file).unwrap();
    assert!(!stored.contains("\"bob\""), "{stored}");
    assert!(
        !std::fs::read_to_string(&mcp_file)
            .unwrap()
            .contains("user:bob")
    );
    assert_eq!(browser_principal(address, &cookies["bob"]), Err(401));
    assert_eq!(
        mcp_selected_persons(address, &bob_new, &mcp["bob"]),
        Err(404)
    );
    running.stop().unwrap();

    // 3. Alice's token reissued by the console while no server runs: her stored sessions go too.
    let (code, out, err) = user(&root, &["token", "alice"]);
    assert_eq!(code, 0, "{err}");
    let alice_new = printed_token(&out);
    assert!(
        !std::fs::read_to_string(&browser_file)
            .unwrap()
            .contains("\"alice\"")
    );
    assert!(
        !std::fs::read_to_string(&mcp_file)
            .unwrap()
            .contains("user:alice")
    );

    let running = start_with_acme(&root);
    let address = running.address();
    for (name, token) in [("alice", &alice_new), ("bob", &bob_new)] {
        assert_eq!(
            browser_principal(address, &cookies[name]),
            Err(401),
            "{name}"
        );
        assert_eq!(
            mcp_selected_persons(address, token, &mcp[name]),
            Err(404),
            "{name}"
        );
    }
    // New sessions work and survive like the old ones did.
    let cookie = browser_login(address, &alice_new);
    let session = mcp_session_on_acme(address, &alice_new);
    running.stop().unwrap();

    // 4. Broken session files: the server starts with no sessions and overwrites them.
    std::fs::write(&browser_file, "{ broken").unwrap();
    std::fs::write(&mcp_file, "\u{0}garbage").unwrap();
    let running = start_with_acme(&root);
    let address = running.address();
    assert_eq!(browser_principal(address, &cookie), Err(401));
    assert_eq!(
        mcp_selected_persons(address, &alice_new, &session),
        Err(404)
    );
    let cookie = browser_login(address, &alice_new);
    assert_eq!(browser_principal(address, &cookie).as_deref(), Ok("alice"));
    assert!(
        std::fs::read_to_string(&browser_file)
            .unwrap()
            .contains("\"version\": 1")
    );
    running.stop().unwrap();
}

#[test]
fn a_session_lifetime_from_server_toml_ends_restored_browser_sessions() {
    let temp = TempDir::new("sessions-lifetime");
    let root = temp.path().join("srv");
    let dir = ServerDir::init(&root).unwrap().dir;
    modern_db(&dir, "acme");
    let running = start_with_acme(&root);
    let address = running.address();
    let (status, answer) = admin_json(address, "POST", "users", serde_json::json!({"name": "eve"}));
    assert_eq!(status, 201, "{answer}");
    let token = answer["token"].as_str().unwrap().to_string();
    let cookie = browser_login(address, &token);
    running.stop().unwrap();
    // The stored session was made an hour ago; the lifetime is now one minute.
    let browser_file = dir.sessions().join("browser.json");
    let mut stored = json(&std::fs::read_to_string(&browser_file).unwrap());
    let created = stored["sessions"][0]["created_unix_s"].as_u64().unwrap();
    stored["sessions"][0]["created_unix_s"] = serde_json::json!(created - 3600);
    std::fs::write(&browser_file, stored.to_string()).unwrap();
    let config = std::fs::read_to_string(dir.config_path()).unwrap();
    std::fs::write(
        dir.config_path(),
        config.replace("# lifetime_secs = 2592000", "lifetime_secs = 60"),
    )
    .unwrap();
    assert!(dir.load_config().unwrap().sessions.lifetime_secs == Some(60));
    let running = start_with_acme(&root);
    let address = running.address();
    assert_eq!(browser_principal(address, &cookie), Err(401));
    // A new login lives (within its minute).
    let cookie = browser_login(address, &token);
    assert_eq!(browser_principal(address, &cookie).as_deref(), Ok("eve"));
    running.stop().unwrap();
}

/// A plain database `name` whose Store carries the backup jobs `jobs` (name, path), defined
/// directly in the catalog as an imported or restored store would carry them.
fn db_with_backup_jobs(dir: &ServerDir, name: &str, jobs: &[(&str, &Path)]) {
    use graphersal::catalog::{Definition, JobDefinition, JobTask};
    use graphersal::persist::{Store, StoreOptions};
    use graphersal::prelude::{GraphStorage, TraversalGraph};
    let db = dir.databases().join(name);
    std::fs::create_dir_all(&db).unwrap();
    std::fs::write(db.join("database.toml"), "description = \"jobs\"\n").unwrap();
    let mut graph = TraversalGraph::tinkerpop_modern();
    let definitions = jobs
        .iter()
        .map(|(job, path)| {
            Definition::job(
                *job,
                JobDefinition::new(
                    "0 3 * * *",
                    JobTask::Backup {
                        path: path.display().to_string(),
                    },
                ),
            )
        })
        .collect();
    graph.set_definitions(definitions).unwrap();
    Store::create_with(db.join("store"), graph, StoreOptions::new())
        .unwrap()
        .close()
        .unwrap();
}

/// `POST /db/<db>/api/<endpoint>` with the JSON `body`: (status, answer).
fn db_post(
    address: SocketAddr,
    db: &str,
    endpoint: &str,
    body: serde_json::Value,
) -> (u16, String) {
    let (status, _, answer) = call(
        address,
        "POST",
        &format!("/db/{db}/api/{endpoint}"),
        &[("Content-Type", "application/json")],
        &body.to_string(),
    );
    (status, answer)
}

#[test]
fn backup_jobs_and_store_directories_stay_inside_the_backup_root() {
    let temp = TempDir::new("backup-root");
    let dir = ServerDir::init(temp.path()).unwrap().dir;
    // `init` creates the default backup root.
    let root = dir.backups().join("jobs");
    assert!(root.is_dir());
    let root = std::fs::canonicalize(&root).unwrap();
    let outside = TempDir::new("backup-root-outside");
    modern_db(&dir, "a");
    db_with_backup_jobs(
        &dir,
        "carrier",
        &[
            ("evil", &outside.path().join("stolen")),
            ("keep", &root.join("kept")),
        ],
    );
    let running = serve::start(
        temp.path(),
        ServeOptions::new().with_listen("127.0.0.1:0".parse().unwrap()),
    )
    .unwrap();
    let address = running.address();
    for name in ["a", "carrier"] {
        running.loader().load(name).unwrap();
        assert_eq!(
            running
                .loader()
                .wait(name, Duration::from_secs(120))
                .unwrap(),
            DbState::Loaded
        );
    }
    let job = |name: &str, path: &str| serde_json::json!({"name": name, "schedule": "0 3 * * *", "action": "backup", "path": path});

    // A relative path is relative to the root, and the job runs into it.
    let (status, answer) = db_post(address, "a", "jobs/define", job("nightly", "nightly"));
    assert_eq!(status, 200, "{answer}");
    assert_eq!(json(&answer)["ok"], true, "{answer}");
    let (status, answer) = db_post(
        address,
        "a",
        "jobs/run",
        serde_json::json!({"name": "nightly"}),
    );
    assert_eq!(status, 200, "{answer}");
    let written = root.join("nightly");
    assert!(
        std::fs::read_dir(&written)
            .map(|mut d| d.next().is_some())
            .unwrap_or(false),
        "the backup is written into {}: {answer}",
        written.display()
    );

    // An absolute path is refused by the router, also one inside the root, with help.
    for path in [
        outside.path().join("x").display().to_string(),
        root.join("inside").display().to_string(),
    ] {
        let (status, answer) = db_post(address, "a", "jobs/define", job("abs", &path));
        assert_eq!(status, 200, "{answer}");
        let answer = json(&answer);
        assert_eq!(answer["ok"], false, "{answer}");
        assert_eq!(answer["errors"][0]["field"], "path", "{answer}");
        assert!(
            answer["errors"][0]["help"]
                .as_str()
                .unwrap()
                .contains("backup_root"),
            "{answer}"
        );
    }
    // `..` leaving the root is refused by the context's file root.
    let (_, answer) = db_post(address, "a", "jobs/define", job("up", "../escape"));
    assert_eq!(json(&answer)["ok"], false, "{answer}");
    // So is a symbolic link inside the root pointing out of it.
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.path(), root.join("link")).unwrap();
        let (_, answer) = db_post(address, "a", "jobs/define", job("linked", "link/x"));
        assert_eq!(json(&answer)["ok"], false, "{answer}");
    }
    assert!(!outside.path().join("escape").exists());
    let (_, _, jobs) = call(address, "GET", "/db/a/api/jobs", &[], "");
    for refused in ["\"abs\"", "\"up\"", "\"linked\""] {
        assert!(!jobs.contains(refused), "{refused} was not defined: {jobs}");
    }

    // The Store panel's fork and backup directories must lie inside the root too.
    let elsewhere = outside.path().join("copy").display().to_string();
    for endpoint in ["store/fork", "store/backup"] {
        let (status, answer) = db_post(
            address,
            "a",
            endpoint,
            serde_json::json!({"dir": elsewhere, "target": {"commit": 1}}),
        );
        assert_eq!(status, 403, "{endpoint}: {answer}");
        assert!(answer.contains("file root"), "{endpoint}: {answer}");
    }
    assert!(!outside.path().join("copy").exists());

    // A store carrying a job that writes outside the root: the job is dropped at load, a job
    // inside the root is kept.
    let (status, _, jobs) = call(address, "GET", "/db/carrier/api/jobs", &[], "");
    assert_eq!(status, 200, "{jobs}");
    assert!(!jobs.contains("\"evil\""), "{jobs}");
    assert!(jobs.contains("\"keep\""), "{jobs}");
    running.stop().unwrap();
    assert!(!outside.path().join("stolen").exists());
}

#[test]
fn a_configured_backup_root_is_created_at_start() {
    let temp = TempDir::new("backup-root-config");
    let dir = ServerDir::init(temp.path()).unwrap().dir;
    let mounted = TempDir::new("backup-root-mounted");
    let root = mounted.path().join("vault").join("jobs");
    let config = std::fs::read_to_string(dir.config_path()).unwrap();
    let configured = config.replace(
        "# backup_root = \"backups/jobs\"",
        &format!("backup_root = {:?}", root.display().to_string()),
    );
    assert_ne!(configured, config);
    std::fs::write(dir.config_path(), configured).unwrap();
    let running = serve::start(
        temp.path(),
        ServeOptions::new().with_listen("127.0.0.1:0".parse().unwrap()),
    )
    .unwrap();
    assert!(root.is_dir());
    running.stop().unwrap();
}

/// The mode bits of `path` (unix).
#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Sets the mode bits of `path` (unix).
#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// A console command of the binary with extra environment: (exit code, stdout, stderr).
fn console(args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    let mut command = Command::new(BIN);
    command.args(args).env_remove("GRAPHERSAL_SERVER_TOKEN");
    for (key, value) in env {
        command.env(key, value);
    }
    let out = command.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        text(&out.stdout),
        text(&out.stderr),
    )
}

#[cfg(unix)]
#[test]
fn init_makes_the_server_directory_private_to_its_owner() {
    let temp = TempDir::new("security-init");
    let root = temp.path().join("srv");
    let root_text = root.to_str().unwrap();
    let out = run(&["init", root_text]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let dir = ServerDir::new(&root);
    assert_eq!(mode_of(&root), 0o700);
    assert_eq!(mode_of(&dir.sessions()), 0o700);
    assert_eq!(mode_of(&dir.access_path()), 0o600);
    // A server keeps the modes of what it writes: server.run and the session files.
    let mut server = ServerProcess::spawn(
        Command::new(BIN)
            .args(["serve", "--dir", root_text, "--listen", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    );
    let info = wait_for_run_file(&dir);
    assert_eq!(mode_of(&dir.run_path()), 0o600);
    let (code, _, err) = console(&["user", "add", "bob", "--dir", root_text], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(mode_of(&dir.access_path()), 0o600);
    let _ = stop_and_wait(&mut server);
    drop(info);

    // An older directory with open modes: init makes it private and says so.
    set_mode(&root, 0o755);
    set_mode(&dir.access_path(), 0o644);
    set_mode(&dir.sessions(), 0o755);
    let out = run(&["init", root_text]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(
        stdout.starts_with(&format!(
            "{root_text} is already a server directory: made it private to its owner\n"
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains(&dir.access_path().display().to_string()),
        "{stdout}"
    );
    assert_eq!(mode_of(&root), 0o700);
    assert_eq!(mode_of(&dir.access_path()), 0o600);
    assert_eq!(mode_of(&dir.sessions()), 0o700);
}

#[cfg(unix)]
#[test]
fn serve_and_the_console_refuse_a_directory_others_could_change() {
    let temp = TempDir::new("security-refuse");
    let root = temp.path().join("srv");
    let root_text = root.to_str().unwrap();
    let dir = ServerDir::init(&root).unwrap().dir;

    // The directory writable by its group: every console command and serve refuse.
    set_mode(&root, 0o770);
    for args in [
        &["db", "list", "--dir", root_text][..],
        &["user", "add", "mallory", "--admin", "--dir", root_text],
        &["models", "list", "--dir", root_text],
        &["import", root_text, "--list"],
        &["serve", "--dir", root_text, "--listen", "127.0.0.1:0"],
    ] {
        let (code, out, err) = console(args, &[]);
        assert_eq!(code, 1, "{args:?}: {out}{err}");
        assert!(out.is_empty(), "{args:?}: {out}");
        assert!(
            err.contains("is not private to its owner")
                && err.contains("writable by group or others (mode 0770)"),
            "{args:?}: {err}"
        );
        assert!(
            err.contains(&format!("\n  chmod 700 {root_text}\n")),
            "{err}"
        );
        assert!(
            err.contains(&format!("graphersal-server init {root_text}")),
            "{err}"
        );
    }
    // Nothing was added.
    let file = graphersal_server::access::AccessFile::read(&dir)
        .unwrap()
        .unwrap();
    assert!(!file.users().contains_key("mallory"));
    set_mode(&root, 0o700);

    // A secret readable by others: refused, naming its chmod.
    for (path, fix) in [
        (dir.access_path(), "chmod 600"),
        (dir.sessions(), "chmod 700"),
    ] {
        let good = mode_of(&path);
        set_mode(&path, if path.is_dir() { 0o755 } else { 0o644 });
        let (code, _, err) = console(&["user", "list", "--dir", root_text], &[]);
        assert_eq!(code, 1, "{err}");
        assert!(err.contains("open to group or others"), "{err}");
        assert!(err.contains(&format!("{fix} {}", path.display())), "{err}");
        let (code, _, err) = console(&["serve", "--dir", root_text], &[]);
        assert_eq!(code, 1, "{err}");
        assert!(err.contains(&format!("{fix} {}", path.display())), "{err}");
        set_mode(&path, good);
    }
    // A server.run left behind readable by others (it carries the operator token).
    std::fs::write(dir.run_path(), "address = \"127.0.0.1:1\"\n").unwrap();
    set_mode(&dir.run_path(), 0o644);
    let (code, _, err) = console(&["db", "list", "--dir", root_text], &[]);
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains(&format!("chmod 600 {}", dir.run_path().display())),
        "{err}"
    );
    std::fs::remove_file(dir.run_path()).unwrap();
    let (code, _, err) = console(&["db", "list", "--dir", root_text], &[]);
    assert_eq!(code, 0, "{err}");
}

#[test]
fn console_actions_are_audited_with_the_os_user() {
    let temp = TempDir::new("security-audit");
    let root = temp.path().join("srv");
    let root_text = root.to_str().unwrap();
    let dir = ServerDir::init(&root).unwrap().dir;
    let os_user = graphersal_server::security::os_user().unwrap();
    let actor = format!("operator (os:{os_user})");

    // Offline: the record names the operator and this OS user (shown with RUST_LOG).
    let (code, _, err) = console(
        &["user", "add", "bob", "--dir", root_text],
        &[("RUST_LOG", "graphersal_server::audit=info")],
    );
    assert_eq!(code, 0, "{err}");
    assert!(
        err.contains(&actor) && err.contains("user 'bob' added"),
        "{err}"
    );

    // Through the server with the operator token: the server's log names the OS user it runs
    // as (only the directory's owner reads the operator token).
    let mut server = ServerProcess::spawn(
        Command::new(BIN)
            .args(["serve", "--dir", root_text, "--listen", "127.0.0.1:0"])
            .env("RUST_LOG", "info")
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    );
    let _info = wait_for_run_file(&dir);
    let (code, _, err) = console(&["db", "create", "acme", "--dir", root_text], &[]);
    assert_eq!(code, 0, "{err}");
    let (_, log) = stop_and_wait(&mut server);
    let line = log
        .lines()
        .find(|line| line.contains("database 'acme' created"))
        .unwrap_or_else(|| panic!("no audit line: {log}"));
    assert!(line.contains(&actor), "{line}");
}

#[test]
fn console_commands_with_a_token_go_through_the_running_server() {
    let temp = TempDir::new("security-token");
    let root = temp.path().join("srv");
    let root_text = root.to_str().unwrap();
    let dir = ServerDir::init(&root).unwrap().dir;
    // An admin user and a plain user, made offline by the operator.
    let (code, out, err) = console(
        &[
            "user", "add", "carol", "--admin", "--dir", root_text, "--json",
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let carol = json(&out)["token"].as_str().unwrap().to_string();
    let (code, out, err) = console(&["user", "add", "dave", "--dir", root_text, "--json"], &[]);
    assert_eq!(code, 0, "{err}");
    let dave = json(&out)["token"].as_str().unwrap().to_string();

    // No running server: a token does not act on the directory offline.
    let (code, out, err) = console(&["db", "list", "--dir", root_text, "--token", &carol], &[]);
    assert_eq!(code, 1, "{out}{err}");
    assert!(err.contains("no server runs on this directory"), "{err}");
    assert!(err.contains("--server ADDRESS"), "{err}");
    // --server without a token is a usage error.
    let (code, _, err) = console(
        &["db", "list", "--dir", root_text, "--server", "127.0.0.1:1"],
        &[],
    );
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("--server needs --token"), "{err}");

    let mut server = ServerProcess::spawn(
        Command::new(BIN)
            .args(["serve", "--dir", root_text, "--listen", "127.0.0.1:0"])
            .env("RUST_LOG", "info")
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    );
    let info = wait_for_run_file(&dir);
    let address = info.address.to_string();
    let note = format!("sent to its admin API at {address}");

    // The address from server.run, the token from --token: the audit names carol.
    let (code, out, err) = console(
        &[
            "db", "create", "acme", "--dir", root_text, "--token", &carol,
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("Created database 'acme'\n"), "{out}");
    assert!(err.contains(&note), "{err}");
    // The token from the environment, the address named: no file of the directory is touched,
    // so even a directory this user could not read works (here: one that does not exist).
    let elsewhere = temp.path().join("not-here");
    let (code, out, err) = console(
        &[
            "user",
            "list",
            "--dir",
            elsewhere.to_str().unwrap(),
            "--server",
            &format!("http://{address}/"),
        ],
        &[("GRAPHERSAL_SERVER_TOKEN", &carol)],
    );
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("carol") && out.contains("dave"), "{out}");
    // The permission checks do not apply on the token path: the directory is group-writable
    // now, the operator's console refuses, the token goes through.
    #[cfg(unix)]
    {
        set_mode(&root, 0o770);
        let (code, _, err) = console(&["db", "list", "--dir", root_text], &[]);
        assert_eq!(code, 1, "{err}");
        let (code, out, err) = console(&["db", "list", "--dir", root_text, "--token", &carol], &[]);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("acme"), "{out}");
        set_mode(&root, 0o700);
    }
    // A wrong token: 401; a user without the admin role: the admin API does not exist for it.
    let (code, _, err) = console(&["db", "list", "--dir", root_text, "--token", "wrong"], &[]);
    assert_eq!(code, 1, "{err}");
    let (code, _, err) = console(&["db", "list", "--dir", root_text, "--token", &dave], &[]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("unknown endpoint"), "{err}");

    let (_, log) = stop_and_wait(&mut server);
    let line = log
        .lines()
        .find(|line| line.contains("database 'acme' created"))
        .unwrap_or_else(|| panic!("no audit line: {log}"));
    assert!(line.contains("user:carol"), "{line}");
    assert!(!line.contains("(os:"), "{line}");
}

/// A server `start` runs in the background on a directory: killed on drop when it still runs
/// (its pid from `server.run`), so a failing test leaves no process behind.
struct Background(PathBuf);

impl Drop for Background {
    fn drop(&mut self) {
        let Ok(Some(info)) = RunInfo::read(&ServerDir::new(&self.0)) else {
            return;
        };
        if !ServerLock::is_held(&ServerDir::new(&self.0)).unwrap_or(false) {
            return;
        }
        #[cfg(unix)]
        let _ = Command::new("kill")
            .args(["-9", &info.pid.to_string()])
            .status();
        #[cfg(windows)]
        let _ = Command::new("taskkill")
            .args(["/F", "/PID", &info.pid.to_string()])
            .status();
        let deadline = Instant::now() + Duration::from_secs(10);
        while ServerLock::is_held(&ServerDir::new(&self.0)).unwrap_or(false)
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// `graphersal-server <command> --dir DIR <args>`: (exit code, stdout, stderr).
fn service(command: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(BIN)
        .arg(command)
        .arg("--dir")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        text(&out.stdout),
        text(&out.stderr),
    )
}

#[test]
fn start_status_and_stop_run_a_server_in_the_background() {
    let temp = TempDir::new("daemon");
    let root = temp.path().join("srv");
    let init = run(&["init", root.to_str().unwrap()]);
    assert!(init.status.success(), "{}", text(&init.stderr));
    // No access.toml: `start` makes the first admin and prints its token once, never into
    // the log.
    std::fs::remove_file(root.join("access.toml")).unwrap();
    let dir = ServerDir::new(&root);
    let _guard = Background(root.clone());

    let (code, out, err) = service("start", &root, &["--listen", "127.0.0.1:0"]);
    assert_eq!(code, 0, "{out}\n{err}");
    let info = RunInfo::read(&dir).unwrap().expect("a run file");
    assert_ne!(info.pid, std::process::id());
    assert!(info.started_at_ms.is_some(), "{info:?}");
    assert_eq!(
        out.trim(),
        format!("Graphersal server: {}", info.url),
        "{out}"
    );
    assert!(
        err.contains("graphersal-server - EXPERIMENTAL"),
        "the start block: {err}"
    );
    assert!(
        err.contains(&format!(
            "Started in the background: pid {}, {}",
            info.pid, info.url
        )),
        "{err}"
    );
    assert!(err.contains("graphersal-server stop --dir"), "{err}");
    let token = err
        .lines()
        .find_map(|l| l.strip_prefix("| token:"))
        .map(|t| t.trim().trim_end_matches('|').trim().to_string())
        .unwrap_or_else(|| panic!("no first admin token: {err}"));
    assert_eq!(token.len(), 64, "{err}");
    let log_path = root.join("logs").join("server.log");
    let log = std::fs::read_to_string(&log_path).unwrap();
    assert!(
        log.contains("graphersal-server start: serve --dir"),
        "{log}"
    );
    assert!(log.contains("Graphersal server: "), "{log}");
    assert!(
        !log.contains(&token),
        "the first admin's token is not logged"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&root.join("logs")), 0o700);
        assert_eq!(mode(&log_path), 0o600);
    }
    assert_eq!(get(info.address, "/readyz").0, 200);
    // The process is detached: it is not a child of this test (it survives the console).
    assert!(ServerLock::is_held(&dir).unwrap());

    // A second start is refused, naming the running server.
    let (code, _, err) = service("start", &root, &["--listen", "127.0.0.1:0"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("already runs on"), "{err}");
    assert!(err.contains(&format!("pid {}", info.pid)), "{err}");

    // status: running, its facts; exit code 0.
    let (code, out, err) = service("status", &root, &[]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("state:") && out.contains("running"), "{out}");
    assert!(out.contains(&info.pid.to_string()), "{out}");
    assert!(out.contains(&info.url), "{out}");
    assert!(out.contains(env!("CARGO_PKG_VERSION")), "{out}");
    assert!(out.contains("uptime:"), "{out}");
    assert!(
        out.contains("databases:") && out.contains("0 (0 loaded)"),
        "{out}"
    );
    assert!(out.contains("memory:"), "{out}");
    let (code, out, err) = service("status", &root, &["--json"]);
    assert_eq!(code, 0, "{err}");
    let status = json(&out);
    assert_eq!(status["state"], "running", "{out}");
    assert_eq!(status["server"]["pid"], info.pid, "{out}");
    assert_eq!(status["server"]["databases"]["total"], 0, "{out}");
    assert!(status["server"]["uptimeSecs"].is_u64(), "{out}");
    assert!(
        !out.contains("operatorToken") && !out.contains("operator_token"),
        "{out}"
    );
    // status and stop through the server with the admin's token too.
    let (code, out, err) = service("status", &root, &["--token", &token]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("running"), "{out}");

    // stop: the graceful path; the lock is free afterwards, the run file gone.
    let (code, out, err) = service("stop", &root, &[]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.starts_with("stopped: "), "{out}");
    assert!(out.contains(&info.pid.to_string()), "{out}");
    assert!(!ServerLock::is_held(&dir).unwrap());
    assert_eq!(RunInfo::read(&dir).unwrap(), None);
    let log = std::fs::read_to_string(&log_path).unwrap();
    assert!(
        log.contains("shutdown requested through the admin API"),
        "{log}"
    );
    assert!(
        log.contains("Stopped:"),
        "the same clean stop as Ctrl+C: {log}"
    );

    // Stopped now: status exits 3, stop says so and exits 0.
    let (code, out, _) = service("status", &root, &[]);
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("stopped"), "{out}");
    assert!(!out.contains("stale"), "{out}");
    let (code, out, err) = service("stop", &root, &[]);
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("not running: "), "{out}");

    // start again, stop with the admin's token (no file of the directory needed).
    let (code, out, err) = service("start", &root, &["--listen", "127.0.0.1:0"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(
        !err.contains("| token:"),
        "the first admin exists now: {err}"
    );
    let (code, out, err) = service("stop", &root, &["--token", &token]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.starts_with("stopped: "), "{out}");
    let deadline = Instant::now() + Duration::from_secs(30);
    while ServerLock::is_held(&dir).unwrap() {
        assert!(
            Instant::now() < deadline,
            "the server did not release its lock"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn start_reports_a_server_that_cannot_start_with_its_log() {
    let temp = TempDir::new("daemon-fails");
    let root = temp.path();
    ServerDir::init(root).unwrap();
    let _guard = Background(root.to_path_buf());
    // The port is taken: the server exits at once.
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = taken.local_addr().unwrap().to_string();
    let (code, out, err) = service("start", root, &["--listen", &address]);
    assert_eq!(code, 1, "{out}\n{err}");
    assert!(out.is_empty(), "{out}");
    assert!(err.contains("exited before it was ready"), "{err}");
    assert!(err.contains("The server's log (last lines):"), "{err}");
    assert!(
        err.contains(&format!("cannot listen on {address}")),
        "{err}"
    );
    assert!(!ServerLock::is_held(&ServerDir::new(root)).unwrap());
}

#[test]
fn status_and_stop_of_a_stopped_directory_and_a_stale_run_file() {
    let temp = TempDir::new("daemon-left-behind");
    let root = temp.path();
    let dir = ServerDir::init(root).unwrap().dir;
    let (code, out, err) = service("status", root, &[]);
    assert_eq!(code, 3, "{err}");
    assert!(out.contains("stopped"), "{out}");
    assert!(!out.contains("stale"), "{out}");
    let (code, out, _) = service("status", root, &["--json"]);
    assert_eq!(code, 3);
    assert_eq!(json(&out)["state"], "stopped", "{out}");
    assert_eq!(json(&out)["staleRunFile"], false, "{out}");

    // A server that did not stop cleanly left its run file (no lock held).
    RunInfo::new(
        "127.0.0.1:9".parse().unwrap(),
        "http://127.0.0.1:9/".to_string(),
    )
    .write(&dir)
    .unwrap();
    let (code, out, err) = service("status", root, &[]);
    assert_eq!(code, 3, "{err}");
    assert!(out.contains("stopped"), "{out}");
    assert!(out.contains("stale"), "{out}");
    assert!(out.contains("http://127.0.0.1:9/"), "{out}");
    assert!(dir.run_path().exists(), "status changes nothing");
    let (code, out, _) = service("status", root, &["--json"]);
    assert_eq!(code, 3);
    assert_eq!(json(&out)["staleRunFile"], true, "{out}");
    let (code, out, err) = service("stop", root, &[]);
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("not running: "), "{out}");
    assert!(out.contains("removed the server.run of pid"), "{out}");
    assert!(!dir.run_path().exists());
}

#[test]
fn the_shutdown_route_is_for_the_operator_and_admins_only() {
    let temp = TempDir::new("shutdown-route");
    let root = temp.path();
    let dir = ServerDir::init(root).unwrap().dir;
    let (code, out, err) = user(root, &["add", "bob"]);
    assert_eq!(code, 0, "{err}");
    let bob = printed_token(&out);
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let hook = graphersal_server::StopHook::new(move || {
        let _ = stop_tx.send(());
    });
    let running = serve::start(
        root,
        ServeOptions::new()
            .with_listen("127.0.0.1:0".parse().unwrap())
            .with_stop_hook(hook),
    )
    .unwrap();
    let address = running.address();
    let bob_auth = format!("Authorization: Bearer {bob}\r\n");
    // A user without the admin role: the admin API does not exist for it.
    let (status, _, body) = send(address, "POST", "/api/admin/shutdown", &[], &bob_auth, "");
    assert_eq!(status, 404, "{body}");
    let (status, _, _) = send(address, "GET", "/api/admin/server", &[], &bob_auth, "");
    assert_eq!(status, 404);
    // Anonymous: log in first.
    let (status, _, _) = send(address, "POST", "/api/admin/shutdown", &[], "", "");
    assert_eq!(status, 401);
    assert!(stop_rx.try_recv().is_err(), "nobody asked for a stop yet");

    // The operator: the server facts, then the stop request reaches the hook.
    let (status, _, body) = call(address, "GET", "/api/admin/server", &[], "");
    assert_eq!(status, 200, "{body}");
    let facts = json(&body);
    assert_eq!(facts["pid"], std::process::id());
    assert_eq!(facts["address"], address.to_string());
    assert_eq!(facts["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(facts["directory"], dir.root().display().to_string());
    assert!(facts["startedAtMs"].is_u64(), "{body}");
    assert!(facts["memory"].is_object(), "{body}");
    let (status, _, _) = call(address, "GET", "/api/admin/shutdown", &[], "");
    assert_eq!(status, 405);
    let (status, _, body) = call(address, "POST", "/api/admin/shutdown", &[], "");
    assert_eq!(status, 202, "{body}");
    assert_eq!(json(&body)["stopping"], true);
    stop_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the hook was called");
    running.stop().unwrap();
}

#[test]
fn the_shutdown_route_needs_real_credentials_with_access_off_and_a_hook() {
    let temp = TempDir::new("shutdown-open");
    let root = temp.path();
    ServerDir::init(root).unwrap();
    std::fs::write(
        root.join("server.toml"),
        "listen = \"127.0.0.1:0\"\n[access]\nenabled = false\n",
    )
    .unwrap();
    // No stop hook (a library caller): the operator gets 409.
    let running = serve::start(
        root,
        ServeOptions::new().with_listen("127.0.0.1:0".parse().unwrap()),
    )
    .unwrap();
    let address = running.address();
    // Everyone is an admin with access off, but may not stop the server.
    let (status, _, body) = send(address, "POST", "/api/admin/shutdown", &[], "", "");
    assert_eq!(status, 403, "{body}");
    let (status, _, _) = send(address, "GET", "/api/admin/server", &[], "", "");
    assert_eq!(
        status, 200,
        "reading the facts is an ordinary admin request"
    );
    let (status, _, body) = call(address, "POST", "/api/admin/shutdown", &[], "");
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("no stop hook"), "{body}");
    running.stop().unwrap();
}

/// Names made before 0.3.0 that break the rule of new names (40 characters, a trailing '-' or
/// '_', a `..`) are still served under `/db/<name>/`, granted by the console and reached by an
/// old user's token; only NEW names are refused (console and admin API).
#[test]
fn names_made_before_the_new_rule_are_still_served_and_grantable() {
    let temp = TempDir::new("old-names");
    let root = temp.path().join("srv");
    let out = run(&["init", root.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let dir = ServerDir::new(&root);
    let long = "a-database-name-of-forty-characters-0040";
    assert_eq!(long.len(), 40);
    for name in [long, "trailing-", "two..dots"] {
        modern_db(&dir, name);
    }
    // A user of an older server: made as `reader`, renamed to `reader_` in access.toml.
    let (code, out, err) = user(&root, &["add", "reader"]);
    assert_eq!(code, 0, "{err}");
    let token = printed_token(&out);
    let access = root.join("access.toml");
    let file = std::fs::read_to_string(&access).unwrap();
    assert!(file.contains("[users.reader]"), "{file}");
    std::fs::write(&access, file.replace("[users.reader]", "[users.reader_]")).unwrap();
    for database in [long, "trailing-", "two..dots"] {
        let (code, _, err) = user(&root, &["grant", "reader_", database, "read"]);
        assert_eq!(code, 0, "{database}: {err}");
    }
    // A new user or database breaking the rule is refused by the console.
    let (code, _, err) = user(&root, &["add", "reader-"]);
    assert_ne!(code, 0);
    assert!(
        err.contains("ends with a lower-case letter or a digit"),
        "{err}"
    );
    let (code, _, err) = db(&root, &["create", &"x".repeat(33)]);
    assert_ne!(code, 0);
    assert!(err.contains("at most 32 characters"), "{err}");

    let running = serve::start(
        &root,
        ServeOptions::new().with_listen("127.0.0.1:0".parse().unwrap()),
    )
    .unwrap();
    let address = running.address();
    let loader = running.loader();
    for name in [long, "trailing-", "two..dots"] {
        loader.load(name).unwrap();
        assert_eq!(
            loader.wait(name, Duration::from_secs(120)).unwrap(),
            DbState::Loaded
        );
    }
    let bearer = format!("Authorization: Bearer {token}\r\n");
    let count = serde_json::json!({ "script": "g.v().count().next()" }).to_string();
    for name in [long, "trailing-", "two..dots"] {
        let (status, _, body) = send(
            address,
            "GET",
            &format!("/db/{name}/api/info"),
            &[],
            &bearer,
            "",
        );
        assert_eq!(status, 200, "{name}: {body}");
        assert_eq!(json(&body)["database"], name);
        let (status, _, body) = send(
            address,
            "POST",
            &format!("/db/{name}/api/execute"),
            &[("Content-Type", "application/json")],
            &bearer,
            &count,
        );
        assert_eq!(status, 200, "{name}: {body}");
        assert_eq!(json(&body)["execution"]["results"][0], 6, "{name}: {body}");
    }
    // An alias may name an old database; a new database may not take such a name.
    let (status, answer) = admin_json(
        address,
        "PUT",
        "aliases/current",
        serde_json::json!({ "target": long }),
    );
    assert!(status < 300, "{answer}");
    assert_eq!(execute(address, "current", "g.v().count().next()"), 6);
    let (status, answer) = admin_json(
        address,
        "POST",
        &format!("databases/{long}/clone"),
        serde_json::json!({ "name": "copy-" }),
    );
    assert_eq!(status, 400, "{answer}");
    assert!(
        answer["error"].as_str().unwrap().contains("ends with"),
        "{answer}"
    );
    running.stop().unwrap();
}

#[test]
fn a_grant_on_an_alias_follows_its_flip_through_the_console_and_http() {
    let temp = TempDir::new("alias-grant");
    let root = temp.path().join("srv");
    let out = run(&["init", root.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let dir = ServerDir::new(&root);
    for name in ["prod-2026-10", "prod-2026-11"] {
        modern_db(&dir, name);
    }
    let (code, _, err) = db(&root, &["alias", "set", "prod", "prod-2026-10"]);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = user(&root, &["add", "reader"]);
    assert_eq!(code, 0, "{err}");
    let token = printed_token(&out);
    // Offline: the grant on the alias is taken (0.2.0 refused it) and listed with its target.
    let (code, out, err) = user(&root, &["grant", "reader", "prod", "read"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("alias of: 'prod-2026-10' now"), "{out}");
    let (code, out, err) = user(&root, &["list"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("prod → prod-2026-10 (alias) read"), "{out}");

    let running = serve::start(
        &root,
        ServeOptions::new().with_listen("127.0.0.1:0".parse().unwrap()),
    )
    .unwrap();
    let address = running.address();
    let loader = running.loader();
    for name in ["prod-2026-10", "prod-2026-11"] {
        loader.load(name).unwrap();
        assert_eq!(
            loader.wait(name, Duration::from_secs(120)).unwrap(),
            DbState::Loaded
        );
    }
    let bearer = format!("Authorization: Bearer {token}\r\n");
    let info = |name: &str| {
        send(
            address,
            "GET",
            &format!("/db/{name}/api/info"),
            &[],
            &bearer,
            "",
        )
    };
    let (status, _, body) = info("prod");
    assert_eq!(status, 200, "{body}");
    assert_eq!(json(&body)["database"], "prod-2026-10");
    for real in ["prod-2026-10", "prod-2026-11"] {
        let (status, _, body) = info(real);
        assert_eq!(status, 404, "{real} is hidden: {body}");
    }
    // The flip (through the running server) moves the access at once.
    let (code, _, err) = db(&root, &["alias", "set", "prod", "prod-2026-11"]);
    assert_eq!(code, 0, "{err}");
    let (status, _, body) = info("prod");
    assert_eq!(status, 200, "{body}");
    assert_eq!(json(&body)["database"], "prod-2026-11");
    let (status, _, _) = info("prod-2026-10");
    assert_eq!(status, 404);
    let (code, out, err) = user(&root, &["list"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("prod → prod-2026-11 (alias) read"), "{out}");
    // "My databases" lists the alias only, marked.
    let (status, _, body) = send(address, "GET", "/api/databases", &[], &bearer, "");
    assert_eq!(status, 200, "{body}");
    let rows = json(&body)["databases"].clone();
    assert_eq!(rows.as_array().unwrap().len(), 1, "{rows}");
    assert_eq!(rows[0]["name"], "prod");
    assert_eq!(rows[0]["kind"], "alias");
    assert_eq!(rows[0]["aliasOf"], "prod-2026-11");
    running.stop().unwrap();
}

#[test]
fn a_deleted_database_takes_its_grants_through_the_console_and_http() {
    let temp = TempDir::new("delete-grants");
    let root = temp.path().join("srv");
    let out = run(&["init", root.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let dir = ServerDir::new(&root);
    modern_db(&dir, "acme");
    let (code, out, err) = user(&root, &["add", "reader"]);
    assert_eq!(code, 0, "{err}");
    let token = printed_token(&out);
    let (code, _, err) = user(&root, &["grant", "reader", "acme", "read"]);
    assert_eq!(code, 0, "{err}");

    let running = serve::start(
        &root,
        ServeOptions::new().with_listen("127.0.0.1:0".parse().unwrap()),
    )
    .unwrap();
    let address = running.address();
    let loader = running.loader();
    let load = |name: &str| {
        loader.load(name).unwrap();
        assert_eq!(
            loader.wait(name, Duration::from_secs(120)).unwrap(),
            DbState::Loaded
        );
    };
    load("acme");
    let bearer = format!("Authorization: Bearer {token}\r\n");
    let info = || send(address, "GET", "/db/acme/api/info", &[], &bearer, "");
    let (status, _, body) = info();
    assert_eq!(status, 200, "{body}");
    // Deleted through the running server: the console names the removed grant.
    loader.unload("acme").unwrap();
    let (code, out, err) = db(&root, &["delete", "acme"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("grants removed: 1 (a restore does not bring them back)"),
        "{out}"
    );
    // A new database of the same name: the former reader gets the unknown-name 404.
    let (code, out, err) = db(&root, &["create", "acme"]);
    assert_eq!(code, 0, "{err}");
    assert!(!out.contains("warning"), "{out}");
    load("acme");
    let (status, _, body) = info();
    assert_eq!(status, 404, "{body}");
    let (code, out, err) = user(&root, &["list"]);
    assert_eq!(code, 0, "{err}");
    assert!(!out.contains("acme"), "no grant left: {out}");
    loader.unload("acme").unwrap();
    running.stop().unwrap();
}

#[test]
fn database_rows_and_the_server_report_disk_and_memory() {
    use graphersal::prelude::{Graph, GraphExport, TraversalGraph};
    let temp = TempDir::new("usage");
    let dir = ServerDir::init(temp.path().join("srv")).unwrap().dir;
    let root = dir.root().to_str().unwrap().to_string();
    modern_db(&dir, "acme");
    modern_db(&dir, "idle");
    let graphson = temp.path().join("modern.json");
    Graph::new(TraversalGraph::tinkerpop_modern())
        .export_graphson_writer(std::fs::File::create(&graphson).unwrap())
        .unwrap();
    let (code, _, err) = import(&[
        &root,
        graphson.to_str().unwrap(),
        "--name",
        "prod",
        "--role",
        "base",
    ]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = db(dir.root(), &["create", "l1", "--base", "prod"]);
    assert_eq!(code, 0, "{err}");
    // The headroom only: the ceiling itself is process-wide state shared with the other tests.
    std::fs::write(
        dir.root().join("server.toml"),
        "listen = \"127.0.0.1:0\"\n[limits]\nmemory_headroom = 1048576\n",
    )
    .unwrap();
    let running = serve::start(
        dir.root(),
        ServeOptions::new().with_listen("127.0.0.1:0".parse().unwrap()),
    )
    .unwrap();
    let address = running.address();
    let loader = running.loader();
    for name in ["acme", "l1"] {
        loader.load(name).unwrap();
        assert_eq!(
            loader.wait(name, Duration::from_secs(120)).unwrap(),
            DbState::Loaded
        );
    }
    execute(
        address,
        "l1",
        "g.add_v(\"own\").property(\"text\", \"x\").iterate()",
    );

    let (status, _, body) = call(address, "GET", "/api/admin/databases", &[], "");
    assert_eq!(status, 200, "{body}");
    let list = json(&body);
    let row = |name: &str| {
        list["databases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == name && r.get("aliasOf").is_none())
            .unwrap_or_else(|| panic!("no row {name}: {body}"))
            .clone()
    };
    for name in ["acme", "idle", "prod", "l1"] {
        let size = row(name)["sizeBytes"].as_u64().unwrap();
        assert!(size > 0, "{name}: {body}");
    }
    let acme = row("acme")["memoryBytes"].as_u64().unwrap();
    assert!(acme > 0, "{body}");
    assert!(row("idle")["memoryBytes"].is_null(), "{body}");
    assert!(row("idle").get("sharedMemoryBytes").is_none(), "{body}");
    let l1 = row("l1");
    let delta = l1["memoryBytes"].as_u64().unwrap();
    let base = l1["sharedMemoryBytes"].as_u64().unwrap();
    assert!(delta > 0 && base > 0, "{body}");
    // The base itself is not loaded for reading: its row has no memory, the pool holds it.
    assert!(row("prod")["memoryBytes"].is_null(), "{body}");
    let (status, _, one) = call(address, "GET", "/api/admin/databases/acme", &[], "");
    assert_eq!(status, 200, "{one}");
    assert_eq!(json(&one)["memoryBytes"], acme);

    let (status, _, body) = call(address, "GET", "/api/admin/server", &[], "");
    assert_eq!(status, 200, "{body}");
    let memory = json(&body)["memory"].clone();
    // Every graph once: acme, the layer's delta and the pooled base.
    assert_eq!(memory["databasesBytes"], acme + delta + base, "{body}");
    assert_eq!(memory["headroomBytes"], 1_048_576, "{body}");
    assert!(memory["configuredLimit"].is_null(), "{body}");
    assert!(
        memory["liveBytes"].is_null(),
        "no tracking allocator: {body}"
    );
    assert!(memory["limitSource"].is_string(), "{body}");
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    {
        let rss = memory["processBytes"].as_u64().unwrap();
        let total = memory["machineTotalBytes"].as_u64().unwrap();
        let available = memory["machineAvailableBytes"].as_u64().unwrap();
        assert!(rss > 0 && rss < total && available <= total, "{body}");
        let free = memory["freeBytes"].as_u64().unwrap();
        match memory["limitBytes"].as_u64() {
            Some(limit) => {
                // Free under the limit, never more than the machine has available.
                assert!(free <= limit && free <= available, "{body}");
                match memory["freeBasis"].as_str() {
                    Some("limit") => {}
                    Some("machine-below-limit") => assert_eq!(free, available, "{body}"),
                    other => panic!("freeBasis {other:?}: {body}"),
                }
            }
            None => {
                assert_eq!(memory["freeBasis"], "machine", "{body}");
                assert_eq!(free, available, "{body}");
            }
        }
    }

    // `status` shows the same figures, `--json` carries the server's answer.
    let (code, out, err) = service("status", dir.root(), &[]);
    assert_eq!(code, 0, "{err}");
    for row in ["memory:", "machine:", "memory limit:", "free:"] {
        assert!(out.contains(row), "{row}: {out}");
    }
    assert!(out.contains("memory_headroom = 1048576"), "{out}");
    let (code, out, _) = service("status", dir.root(), &["--json"]);
    assert_eq!(code, 0);
    assert_eq!(
        json(&out)["server"]["memory"]["databasesBytes"],
        acme + delta + base,
        "{out}"
    );

    // `db list` through the server: a memory column for the loaded ones.
    let (code, out, err) = db(dir.root(), &["list"]);
    assert_eq!(code, 0, "{err}");
    let lines: Vec<&str> = out.lines().collect();
    let memory_at = lines[1].find("MEMORY").unwrap();
    let acme_line = lines.iter().find(|l| l.starts_with("  acme ")).unwrap();
    assert!(
        acme_line[memory_at..]
            .trim_start()
            .starts_with(|c: char| c.is_ascii_digit()),
        "{out}"
    );
    running.stop().unwrap();
}

/// `mcp ...` of the console: `args` with `--dir dir`: (exit code, stdout, stderr).
fn mcp_command(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(BIN)
        .arg("mcp")
        .args(args)
        .arg("--dir")
        .arg(dir)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        text(&out.stdout),
        text(&out.stderr),
    )
}

/// Round 3 B6: the console's `mcp` commands on the directory (`mcp.toml`) and through a running
/// server (the endpoints follow at once); a switch written offline holds at the next start.
#[test]
fn mcp_commands_offline_and_through_a_server() {
    let temp = TempDir::new("console-mcp");
    let dir = ServerDir::init(temp.path()).unwrap().dir;
    modern_db(&dir, "acme");
    modern_db(&dir, "beta");
    let root = temp.path();
    // Offline: the console writes mcp.toml.
    let (code, out, err) = mcp_command(root, &["off"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("MCP is switched off\n"), "{out}");
    let settings = std::fs::read_to_string(dir.mcp_settings_path()).unwrap();
    assert!(settings.contains("enabled = false"), "{settings}");
    let (code, out, err) = mcp_command(root, &["db", "acme", "read-only", "on"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("MCP settings of 'acme'\n"), "{out}");
    assert!(out.contains("read-only: yes"), "{out}");
    let (code, _, err) = mcp_command(root, &["db", "nope", "on"]);
    assert_eq!(code, 1);
    assert!(err.contains("nope"), "{err}");
    let (code, _, err) = mcp_command(root, &["db", "acme", "maybe"]);
    assert_eq!(code, 2, "a usage error: {err}");
    let (code, out, err) = mcp_command(root, &["status"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("off (switch it on: graphersal-server mcp on --dir"),
        "{out}"
    );
    assert!(out.contains("only while the server runs"), "{out}");
    let acme = out
        .lines()
        .find(|l| l.trim_start().starts_with("acme"))
        .unwrap();
    let cells: Vec<&str> = acme.split_whitespace().collect();
    assert_eq!(
        cells,
        ["acme", "yes", "yes", "0", "0"],
        "offered and read-only: {acme}"
    );

    // Through a server: off holds at the start; on, a hidden database, statistics.
    let mut server = ServerProcess::spawn(
        Command::new(BIN)
            .args(["serve", "--dir"])
            .arg(root)
            .args(["--listen", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    let info = wait_for_run_file(&dir);
    let deadline = Instant::now() + Duration::from_secs(30);
    while get(info.address, "/readyz").0 != 200 {
        assert!(Instant::now() < deadline, "the server did not get ready");
        std::thread::sleep(Duration::from_millis(20));
    }
    let ping = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});
    let (status, _, body) = mcp_post(info.address, "/mcp", None, &[], ping.clone());
    assert_eq!(status, 503, "{body}");
    assert!(
        json(&body)["help"].as_str().unwrap().contains("mcp on"),
        "{body}"
    );
    let note = format!("sent to its admin API at {}", info.address);
    let (code, out, err) = mcp_command(root, &["on"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains(&note), "{err}");
    assert!(out.starts_with("MCP is switched on\n"), "{out}");
    let session = mcp_initialize(info.address, "/mcp");
    let (code, _, err) = mcp_command(root, &["db", "beta", "off"]);
    assert_eq!(code, 0, "{err}");
    let (listing, error) = mcp_tool(
        info.address,
        "/mcp",
        &session,
        "list_graphs",
        serde_json::json!({}),
    );
    assert!(!error, "{listing}");
    assert!(listing.contains("\"acme\""), "{listing}");
    assert!(!listing.contains("\"beta\""), "{listing}");
    let (code, out, err) = mcp_command(root, &["status", "--json"]);
    assert_eq!(code, 0, "{err}");
    let state = json(&out);
    assert_eq!(state["serving"], true);
    assert_eq!(state["online"], true);
    assert_eq!(state["usage"]["calls"]["total"], 1);
    assert_eq!(state["usage"]["calls"]["refused"], 1);
    assert_eq!(state["usage"]["sessions"]["open"], 1);
    let row = |name: &str| {
        state["databases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(row("acme")["readOnly"], true);
    assert_eq!(row("beta")["exposed"], false);
    let (code, out, _) = mcp_command(root, &["status"]);
    assert_eq!(code, 0);
    assert!(out.contains("list_graphs"), "the tool table: {out}");

    send_stop(&server);
    let deadline = Instant::now() + Duration::from_secs(30);
    while server.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "the server did not stop");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn db_info_and_db_set_offline_and_through_a_server() {
    let temp = TempDir::new("console-settings");
    let dir = ServerDir::init(temp.path()).unwrap().dir;
    modern_db(&dir, "acme");
    let root = temp.path();
    let file = dir.databases().join("acme").join("database.toml");
    // Offline: info reads the directory, set writes database.toml.
    let (code, out, err) = db(root, &["info", "acme"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("Database 'acme'"), "{out}");
    let row = |key: &str, value: &str| {
        out.lines()
            .any(|l| l.trim_start().starts_with(key) && l.trim_end().ends_with(value))
    };
    assert!(row("description:", "modern"), "{out}");
    assert!(out.contains("KEY"), "the limits table: {out}");
    assert!(out.contains("max_operations"), "{out}");
    assert!(row("damage policy:", "maintenance"), "{out}");
    assert!(row("backend:", "directory"), "{out}");
    assert!(row("store id:", ""), "{out}");
    let (code, out, err) = db(root, &["set", "acme", "description", "Acme's graph"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.starts_with("Set the description of database 'acme'"),
        "{out}"
    );
    let (code, out, err) = db(root, &["set", "acme", "limit", "max_operations", "1_000"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.starts_with("Set the query limit max_operations = 1000 of database 'acme'"),
        "{out}"
    );
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("description = \"Acme's graph\""), "{text}");
    assert!(
        text.contains("[limits.query]\nmax_operations = 1000"),
        "{text}"
    );
    let (code, out, err) = db(root, &["info", "acme", "--json"]);
    assert_eq!(code, 0, "{err}");
    let info = json(&out);
    let row = info["limits"]["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "max_operations")
        .unwrap()
        .clone();
    assert_eq!(row["override"], 1000);
    assert_eq!(info["limits"]["loaded"], false);
    assert_eq!(info["database"]["description"], "Acme's graph");
    // Usage errors: an unknown key, a bad value, --clear with a value, a missing value.
    for args in [
        &["set", "acme", "limit", "timeout", "5"][..],
        &["set", "acme", "limit", "timeout_ms", "-5"],
        &["set", "acme", "limit", "timeout_ms", "5", "--clear"],
        &["set", "acme", "limit", "timeout_ms"],
        &["set", "acme", "colour", "red"],
        &["info", "acme", "--clear"],
    ] {
        let (code, _, err) = db(root, args);
        assert_eq!(code, 2, "{args:?}: {err}");
    }
    let (code, _, err) = db(root, &["set", "nope", "description", "x"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("nope"), "{err}");

    // Through a server: the change is in effect at the next load.
    let mut server = ServerProcess::spawn(
        Command::new(BIN)
            .args(["serve", "--dir"])
            .arg(root)
            .args(["--listen", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    let info = wait_for_run_file(&dir);
    let deadline = Instant::now() + Duration::from_secs(30);
    while get(info.address, "/readyz").0 != 200 {
        assert!(Instant::now() < deadline, "the server did not get ready");
        std::thread::sleep(Duration::from_millis(20));
    }
    let (code, _, err) = db(root, &["load", "acme"]);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = db(root, &["set", "acme", "limit", "max_operations", "--clear"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        err.contains(&format!("sent to its admin API at {}", info.address)),
        "{err}"
    );
    assert!(
        out.starts_with("Cleared the query limit override max_operations of database 'acme'"),
        "{out}"
    );
    assert!(out.contains("now:"), "{out}");
    assert!(out.contains("db unload acme"), "{out}");
    let (code, out, err) = db(root, &["info", "acme"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("NOW"), "loaded: the limits in effect: {out}");
    assert!(
        out.contains("changed since the load: applies at the next load"),
        "{out}"
    );
    assert!(
        !std::fs::read_to_string(&file).unwrap().contains("limits"),
        "cleared"
    );
    let (code, _, err) = db(root, &["unload", "acme"]);
    assert_eq!(code, 0, "{err}");

    send_stop(&server);
    let deadline = Instant::now() + Duration::from_secs(30);
    while server.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "the server did not stop");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Every file below `dir`.
fn files_below(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in std::fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}

/// Damages the snapshot data of the Store `store` as a failing disk would: flips 16 bytes in
/// the middle of its largest snapshot segment file.
fn damage_snapshot(store: &Path) {
    let segment = files_below(store)
        .into_iter()
        .filter(|p| {
            let text = p.to_string_lossy();
            text.contains("snap") && !text.ends_with(".json") && !text.contains("manifest")
        })
        .max_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
        .expect("a snapshot segment");
    let mut bytes = std::fs::read(&segment).unwrap();
    let middle = bytes.len() / 2;
    let end = (middle + 16).min(bytes.len());
    for byte in &mut bytes[middle..end] {
        *byte ^= 0xA5;
    }
    std::fs::write(&segment, bytes).unwrap();
}

/// Waits until the server at `address` answers `/readyz` 200 (30 s at most).
fn wait_ready(address: SocketAddr) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while get(address, "/readyz").0 != 200 {
        assert!(Instant::now() < deadline, "the server did not get ready");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The last unclean shutdown's report once its checks ended (`GET /api/admin/recovery`, 120 s
/// at most).
#[cfg(unix)]
fn recovery_done(address: SocketAddr) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let (status, _, body) = call(address, "GET", "/api/admin/recovery", &[], "");
        assert_eq!(status, 200, "{body}");
        let report = json(&body)["uncleanShutdown"].clone();
        if report["done"] == serde_json::Value::Bool(true) {
            return report;
        }
        assert!(
            Instant::now() < deadline,
            "the checks did not end: {report}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A `serve` of `root` on port 0 whose stderr goes to the file `log`.
fn serve_logged(root: &Path, log: &Path) -> ServerProcess {
    let file = std::fs::File::create(log).unwrap();
    ServerProcess::spawn(
        Command::new(BIN)
            .args(["serve", "--dir"])
            .arg(root)
            .args(["--listen", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(Stdio::from(file)),
    )
}

/// Round 3 B-CRASH: a server killed by force (SIGKILL) in the middle of writes. The next start
/// says so, loads the database it had open again, replays its WAL and verifies it: every
/// acknowledged commit is there.
#[cfg(unix)]
#[test]
fn a_server_killed_mid_writes_is_checked_at_the_next_start() {
    let temp = TempDir::new("crash");
    let dir = ServerDir::init(temp.path().join("srv")).unwrap().dir;
    let root = dir.root().to_path_buf();
    modern_db(&dir, "acme");
    let log1 = temp.path().join("first.log");
    let mut server = serve_logged(&root, &log1);
    let info = wait_for_run_file(&dir);
    wait_ready(info.address);
    let (code, _, err) = db(&root, &["load", "acme"]);
    assert_eq!(code, 0, "{err}");
    let open = std::fs::read_to_string(dir.open_path()).unwrap();
    assert!(open.contains("\"acme\""), "{open}");

    // A writer commits one vertex per request and counts what was acknowledged.
    let acked = std::sync::Arc::new(AtomicUsize::new(0));
    let writer = {
        let acked = acked.clone();
        let address = info.address;
        let token = operator_token(address).expect("the operator token");
        std::thread::spawn(move || {
            for n in 0.. {
                let body =
                    serde_json::json!({ "script": format!("g.add_v(\"crash\").property(\"n\", {n}).iterate()") })
                        .to_string();
                let Ok(mut stream) = TcpStream::connect(address) else {
                    return;
                };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                let head = format!(
                    "POST /db/acme/api/execute HTTP/1.1\r\nHost: {address}\r\nConnection: \
                     close\r\nAuthorization: Bearer {token}\r\nContent-Type: \
                     application/json\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                );
                if stream.write_all(head.as_bytes()).is_err()
                    || stream.write_all(body.as_bytes()).is_err()
                {
                    return;
                }
                let mut answer = Vec::new();
                if stream.read_to_end(&mut answer).is_err() {
                    return;
                }
                let answer = text(&answer);
                if !answer.starts_with("HTTP/1.1 200") || answer.contains("\"ok\":false") {
                    return;
                }
                acked.store(n + 1, Ordering::SeqCst);
            }
        })
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    while acked.load(Ordering::SeqCst) < 25 {
        assert!(Instant::now() < deadline, "the writes do not go through");
        std::thread::sleep(Duration::from_millis(5));
    }
    // kill -9 while the writer keeps writing.
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    writer.join().unwrap();
    let acked = acked.load(Ordering::SeqCst);
    assert!(acked >= 25);
    // The OS released the lock; server.run and server.open stay behind.
    assert!(!ServerLock::is_held(&dir).unwrap());
    assert!(dir.run_path().exists());
    assert!(dir.open_path().exists());
    let out = run(&["status", "--dir", root.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(3));
    let status = text(&out.stdout);
    let flat = status.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(status.contains("Last unclean shutdown"), "{status}");
    assert!(
        flat.contains("next start loads and checks: acme"),
        "{status}"
    );

    // The next start: the notice, the database loaded again and checked.
    let log2 = temp.path().join("second.log");
    let mut server = serve_logged(&root, &log2);
    let deadline = Instant::now() + Duration::from_secs(30);
    let info = loop {
        if let Ok(Some(run)) = RunInfo::read(&dir)
            && run.pid == server.0.id()
        {
            break run;
        }
        assert!(Instant::now() < deadline, "no new run file");
        std::thread::sleep(Duration::from_millis(20));
    };
    wait_ready(info.address);
    let report = recovery_done(info.address);
    assert_eq!(report["databasesKnown"], true, "{report}");
    let check = &report["databases"][0];
    assert_eq!(check["name"], "acme", "{report}");
    assert_eq!(check["outcome"], "ok", "{report}");
    assert_eq!(check["state"], "loaded", "{report}");
    assert_eq!(check["recovery"]["cleanClose"], false, "{report}");
    assert!(
        check["recovery"]["commitSeq"].as_u64().unwrap() >= acked as u64,
        "{report}"
    );
    assert_eq!(check["verify"]["ok"], true, "{report}");
    assert_eq!(check["verify"]["how"], "open store", "{report}");
    // Every acknowledged commit is there (a commit in flight at the kill may be too).
    let count = execute(info.address, "acme", r#"g.v().has_label("crash").count()"#);
    let count = count.as_u64().unwrap_or_else(|| panic!("a count: {count}"));
    assert!(count >= acked as u64, "{count} < {acked}");
    let max = execute(
        info.address,
        "acme",
        r#"g.v().has_label("crash").values("n").max()"#,
    );
    assert!(max.as_u64().unwrap() + 1 >= acked as u64, "{max}");
    // status, the server facts and a verify by hand say the same.
    let (status, _, body) = call(info.address, "GET", "/api/admin/server", &[], "");
    assert_eq!(status, 200);
    assert_eq!(
        json(&body)["uncleanShutdown"]["databases"][0]["outcome"],
        "ok"
    );
    let out = run(&["status", "--dir", root.to_str().unwrap()]);
    let status = text(&out.stdout);
    let flat = status.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(status.contains("Last unclean shutdown"), "{status}");
    assert!(
        flat.contains("acme: ok;") && flat.contains("verify ok"),
        "{status}"
    );
    let (code, out, err) = db(&root, &["verify", "acme"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("verifies: no problem found"), "{out}");
    let (code, out, err) = db(&root, &["verify", "--all"]);
    assert_eq!(code, 0, "{out}{err}");
    let (code, out, err) = db(&root, &["info", "acme"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("Damage and recovery"), "{out}");
    assert!(out.contains("checked at the start of"), "{out}");
    // A clean stop removes server.open; the start block named the unclean shutdown.
    send_stop(&server);
    let deadline = Instant::now() + Duration::from_secs(30);
    while server.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "the server did not stop");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!dir.open_path().exists());
    assert!(!dir.run_path().exists());
    let log = std::fs::read_to_string(&log2).unwrap();
    assert!(log.contains("UNCLEAN SHUTDOWN"), "{log}");
    assert!(log.contains("Unclean shutdown"), "{log}");
    assert!(
        log.contains("unclean shutdown: database 'acme' is ok"),
        "{log}"
    );
    assert!(
        log.contains("database 'acme' checked: ok"),
        "the audit record: {log}"
    );
    // A clean start afterwards finds nothing to check (the last report stays for status).
    let log3 = temp.path().join("third.log");
    let server = serve_logged(&root, &log3);
    let info = loop {
        if let Ok(Some(run)) = RunInfo::read(&dir)
            && run.pid == server.0.id()
        {
            break run;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    wait_ready(info.address);
    drop(server);
    let log = std::fs::read_to_string(&log3).unwrap();
    assert!(!log.contains("UNCLEAN SHUTDOWN"), "{log}");
}

/// Round 3 B-CRASH: a damaged Store is served read-only in the state `maintenance`, repaired
/// into a new database through the console and the admin API, and the copy takes its name.
#[test]
fn a_damaged_database_is_in_maintenance_and_is_repaired_and_swapped() {
    let temp = TempDir::new("damage");
    let dir = ServerDir::init(temp.path().join("srv")).unwrap().dir;
    let root = dir.root().to_path_buf();
    modern_db(&dir, "acme");
    modern_db(&dir, "fine");
    damage_snapshot(&dir.databases().join("acme").join("store"));
    // Offline: the verify finds the damage (exit 1) and names the repair.
    let (code, out, _) = db(&root, &["verify", "acme"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("DAMAGED"), "{out}");
    assert!(out.contains("db repair acme"), "{out}");
    let (code, out, err) = db(&root, &["swap", "acme", "fine"]);
    assert_eq!(code, 2, "{out}{err}");
    assert!(err.contains("--yes"), "{err}");

    let log = temp.path().join("serve.log");
    let _server = serve_logged(&root, &log);
    let info = wait_for_run_file(&dir);
    wait_ready(info.address);
    let (code, out, err) = db(&root, &["load", "acme"]);
    assert_eq!(code, 0, "{out}{err}");
    let (_, out, _) = db(&root, &["list"]);
    assert!(out.contains("maintenance"), "{out}");
    assert!(out.contains("db repair acme"), "{out}");
    let (status, _, body) = call(info.address, "GET", "/api/admin/databases/acme", &[], "");
    assert_eq!(status, 200);
    let entry = json(&body);
    assert_eq!(entry["state"], "maintenance", "{entry}");
    assert!(
        entry["damage"].as_str().unwrap().contains("damaged item"),
        "{entry}"
    );
    let (status, _, body) = call(
        info.address,
        "GET",
        "/api/admin/databases/acme/settings",
        &[],
        "",
    );
    assert_eq!(status, 200);
    let settings = json(&body);
    assert_eq!(settings["recovery"]["maintenance"], true, "{settings}");
    let (code, out, err) = db(&root, &["info", "acme"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(
        out.contains("Damage and recovery") && out.contains("served read-only"),
        "{out}"
    );
    assert!(out.contains("db repair acme"), "{out}");
    assert!(
        settings["recovery"]["help"]
            .as_str()
            .unwrap()
            .contains("repair")
    );
    // Writes are refused, reads work.
    let body = serde_json::json!({ "script": "g.add_v(\"x\").iterate()" }).to_string();
    let (_, _, answer) = call(
        info.address,
        "POST",
        "/db/acme/api/execute",
        &[("Content-Type", "application/json")],
        &body,
    );
    assert!(answer.contains("\"ok\":false"), "{answer}");
    // Every database verified through the API: acme damaged, fine ok.
    let (status, _, body) = call(info.address, "POST", "/api/admin/verify", &[], "");
    assert_eq!(status, 200);
    let all = json(&body);
    assert_eq!(all["ok"], false, "{all}");
    let fine = all["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["database"] == "fine")
        .unwrap();
    assert_eq!(fine["ok"], true, "{all}");
    // A swap without the confirmation is refused.
    let (status, _, body) = call(
        info.address,
        "POST",
        "/api/admin/databases/acme/swap",
        &[("Content-Type", "application/json")],
        r#"{"with": "fine"}"#,
    );
    assert_eq!(status, 400, "{body}");
    // The repair through the console: a new database, verified.
    let (code, out, err) = db(&root, &["repair", "acme"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(
        out.contains("Repaired database 'acme' into 'acme-repaired'"),
        "{out}"
    );
    assert!(out.contains("db swap acme acme-repaired --yes"), "{out}");
    let (code, out, err) = db(&root, &["verify", "acme-repaired"]);
    assert_eq!(code, 0, "{out}{err}");
    // Through the API, under another name.
    let (status, _, body) = call(
        info.address,
        "POST",
        "/api/admin/databases/acme/repair",
        &[("Content-Type", "application/json")],
        r#"{"name": "acme-copy"}"#,
    );
    assert_eq!(status, 201, "{body}");
    let repaired = json(&body);
    assert_eq!(repaired["name"], "acme-copy");
    assert_eq!(repaired["repairedFrom"], "acme");
    assert!(
        repaired["repair"]["damageItems"].as_u64().unwrap() >= 1,
        "{repaired}"
    );
    assert!(repaired["next"]["swap"].as_str().unwrap().contains("swap"));
    // The swap: the copy takes the name, the damaged one goes to trash/, loaded again.
    let (code, out, err) = db(&root, &["swap", "acme", "acme-repaired", "--yes"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("took the name 'acme'"), "{out}");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (_, _, body) = call(info.address, "GET", "/api/admin/databases/acme", &[], "");
        if json(&body)["state"] == "loaded" {
            break;
        }
        assert!(Instant::now() < deadline, "{body}");
        std::thread::sleep(Duration::from_millis(50));
    }
    let (code, out, err) = db(&root, &["verify", "acme"]);
    assert_eq!(code, 0, "{out}{err}");
    let (_, out, _) = db(&root, &["list"]);
    assert!(!out.contains("acme-repaired"), "{out}");
    assert!(
        std::fs::read_dir(dir.trash())
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with("acme-")),
        "the damaged one is in trash/"
    );
    let log = std::fs::read_to_string(&log).unwrap();
    assert!(log.contains("maintenance"), "{log}");
}

/// Serves `root` on a free port (stderr piped, for [`stop_and_wait`]): the process and its
/// address.
fn serve_dir(root: &Path) -> (ServerProcess, SocketAddr) {
    let server = ServerProcess::spawn(
        Command::new(BIN)
            .args(["serve", "--dir"])
            .arg(root)
            .args(["--listen", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    );
    let info = wait_for_run_file(&ServerDir::new(root));
    let deadline = Instant::now() + Duration::from_secs(30);
    while get(info.address, "/readyz").0 != 200 {
        assert!(Instant::now() < deadline, "never ready");
        std::thread::sleep(Duration::from_millis(20));
    }
    (server, info.address)
}

/// `backup` through a running server and offline, `restore` into a new directory, `serve` it:
/// the same users (who log in with their old tokens), grants, aliases, MCP settings and
/// database settings, the data of the full backup; a second restore into the same directory
/// is refused.
#[test]
fn a_server_backup_restores_on_a_new_directory_and_serves_the_same_users_and_data() {
    let temp = TempDir::new("server-backup");
    let dir = ServerDir::init(temp.path().join("srv")).unwrap().dir;
    let root = dir.root().to_path_buf();
    let root_text = root.to_str().unwrap();
    modern_db(&dir, "acme");
    let (code, out, err) = console(&["user", "add", "bob", "--dir", root_text, "--json"], &[]);
    assert_eq!(code, 0, "{err}");
    let bob = json(&out)["token"].as_str().unwrap().to_string();
    for args in [
        vec!["user", "grant", "bob", "acme", "read"],
        vec!["db", "alias", "set", "current", "acme"],
        vec!["mcp", "db", "acme", "read-only", "on"],
        vec!["db", "set", "acme", "limit", "timeout_ms", "5000"],
    ] {
        let mut args = args.clone();
        args.extend(["--dir", root_text]);
        let (code, _, err) = console(&args, &[]);
        assert_eq!(code, 0, "{args:?}: {err}");
    }

    // Through the running server (it copies its files under its locks), with the databases.
    let full = temp.path().join("full.zip");
    let (mut server, address) = serve_dir(&root);
    let (status, _, body) = call(address, "POST", "/api/admin/databases/acme/load", &[], "");
    assert!(status == 200 || status == 202, "{body}");
    let (code, out, err) = console(
        &[
            "backup",
            "--dir",
            root_text,
            "--out",
            full.to_str().unwrap(),
            "--with-databases",
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("the backup came from its admin API"), "{err}");
    assert!(out.starts_with("Server backup written to "), "{out}");
    assert!(out.contains("1 included"), "{out}");
    assert!(out.contains("token hashes"), "{out}");
    assert!(out.contains("restore "), "{out}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&full).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    stop_and_wait(&mut server);
    // Offline: configuration only.
    let conf = temp.path().join("conf.zip");
    let (code, out, err) = console(
        &[
            "backup",
            "--dir",
            root_text,
            "--out",
            conf.to_str().unwrap(),
            "--json",
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(json(&out)["withDatabases"], false, "{out}");

    // Restore the full backup into a new directory and serve it.
    let new = temp.path().join("moved");
    let new_text = new.to_str().unwrap();
    let (code, out, err) = console(&["restore", full.to_str().unwrap(), "--dir", new_text], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("databases:"), "{out}");
    assert!(out.contains("acme"), "{out}");
    assert!(out.contains(&format!("serve --dir {new_text}")), "{out}");
    for file in [
        "access.toml",
        "aliases.toml",
        "mcp.toml",
        "databases/acme/database.toml",
    ] {
        assert_eq!(
            std::fs::read_to_string(new.join(sep(file))).unwrap(),
            std::fs::read_to_string(root.join(sep(file))).unwrap(),
            "{file}"
        );
    }
    let (code, _, err) = console(&["restore", full.to_str().unwrap(), "--dir", new_text], &[]);
    assert_eq!(code, 1);
    assert!(err.contains("NEW server directory"), "{err}");

    let (mut server, address) = serve_dir(&new);
    // bob logs in with the token he had.
    let login = serde_json::json!({ "token": bob }).to_string();
    let (status, _, body) = send(
        address,
        "POST",
        "/api/login",
        &[("Content-Type", "application/json")],
        "",
        &login,
    );
    assert_eq!(status, 200, "{body}");
    let bearer = format!("Authorization: Bearer {bob}\r\n");
    let (status, _, body) = send(address, "GET", "/api/databases", &[], &bearer, "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"acme\""), "{body}");
    let (status, _, body) = call(address, "GET", "/api/admin/aliases", &[], "");
    assert_eq!(status, 200);
    assert_eq!(json(&body)["aliases"]["current"], "acme", "{body}");
    let (status, _, body) = call(address, "POST", "/api/admin/databases/acme/load", &[], "");
    assert!(status == 200 || status == 202, "{body}");
    let deadline = Instant::now() + Duration::from_secs(60);
    while send(address, "GET", "/db/acme/api/info", &[], &bearer, "").0 != 200 {
        assert!(Instant::now() < deadline, "acme never loaded");
        std::thread::sleep(Duration::from_millis(50));
    }
    // The data, read by bob (read grant): the modern sample's 6 vertices.
    let script = serde_json::json!({ "script": "g.v().count()" }).to_string();
    let (status, _, body) = send(
        address,
        "POST",
        "/db/acme/api/execute",
        &[("Content-Type", "application/json")],
        &bearer,
        &script,
    );
    assert_eq!(status, 200, "{body}");
    let result = &json(&body)["execution"]["results"][0];
    assert!(
        result.as_i64() == Some(6) || result["value"].as_i64() == Some(6),
        "{body}"
    );
    // The settings: the query limit override came along.
    let (status, _, body) = call(
        address,
        "GET",
        "/api/admin/databases/acme/settings",
        &[],
        "",
    );
    assert_eq!(status, 200);
    assert!(body.contains("5000"), "{body}");
    stop_and_wait(&mut server);

    // The configuration-only archive: no database, the users and aliases.
    let bare = temp.path().join("bare");
    let (code, out, err) = console(
        &[
            "restore",
            conf.to_str().unwrap(),
            "--dir",
            bare.to_str().unwrap(),
            "--json",
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let report = json(&out);
    assert_eq!(report["databases"], serde_json::json!([]), "{out}");
    assert_eq!(report["skipped"][0]["name"], "acme", "{out}");
}

#[test]
fn the_openapi_command_writes_what_the_server_answers() {
    let temp = TempDir::new("openapi");
    let dir = ServerDir::init(temp.path()).unwrap().dir;
    modern_db(&dir, "a");
    let running = serve::start(
        temp.path(),
        ServeOptions::new().with_listen("127.0.0.1:0".parse().unwrap()),
    )
    .unwrap();
    let address = running.address();
    running.registry().alias_set("cur", "a").unwrap();
    // A file of a running operation in tmp/: the offline command never touches it.
    let marker = dir.tmp().join("upload-running.bin");
    std::fs::write(&marker, b"x").unwrap();
    let root = temp.path().to_str().unwrap();
    let base = format!("http://{address}");

    let output = run(&["openapi", "--dir", root, "--server", &base]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let printed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let (status, body) = get(address, "/openapi.json");
    assert_eq!(status, 200, "{body}");
    assert_eq!(printed, json(&body));
    assert_eq!(printed["servers"][0]["url"], base);

    // A database's (an alias, not loaded) into a file.
    let file = temp.path().join("cur.openapi.json");
    let output = run(&[
        "openapi",
        "--dir",
        root,
        "--db",
        "cur",
        "--out",
        file.to_str().unwrap(),
        "--server",
        &base,
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(
        text(&output.stdout).starts_with("wrote "),
        "{}",
        text(&output.stdout)
    );
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let (status, body) = get(address, "/db/cur/openapi.json");
    assert_eq!(status, 200, "{body}");
    assert_eq!(written, json(&body));
    assert_eq!(written["servers"][0]["url"], format!("{base}/db/cur"));
    assert!(marker.exists(), "the openapi command cleaned tmp/");

    // Without --server: server.toml's listen address (modulo servers the same document).
    let output = run(&["openapi", "--dir", root, "--db", "a"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let mut offline: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(offline["servers"][0]["url"], "http://127.0.0.1:8484/db/a");
    let (_, body) = get(address, "/db/a/openapi.json");
    let served = json(&body);
    offline["servers"] = served["servers"].clone();
    assert_eq!(offline, served);

    let output = run(&["openapi", "--dir", root, "--db", "nope"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output.stderr).contains("nope"),
        "{}",
        text(&output.stderr)
    );
    let output = run(&["openapi"]);
    assert_eq!(output.status.code(), Some(2));
    drop(running);
}

/// The names `list_graphs` of `/mcp` offers (an admin session: every database it may reach).
fn mcp_listed(address: SocketAddr, session: &str) -> Vec<String> {
    let (text, error) = mcp_tool(
        address,
        "/mcp",
        session,
        "list_graphs",
        serde_json::json!({}),
    );
    assert!(!error, "{text}");
    let listed = json(&text);
    let mut names: Vec<String> = listed["graphs"]
        .as_array()
        .unwrap_or_else(|| panic!("no graphs: {text}"))
        .iter()
        .map(|g| g["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    names
}

/// Round 4 B-MCPDEF: `init` makes a server with MCP off; every new database (create, import,
/// clone, a restore into a new name, repair) and every new alias is hidden from MCP unless
/// created with `--mcp` (or `"mcp": true` through the API) or `mcp expose-new` is on;
/// `--no-mcp` hides one whatever the default; a renamed alias keeps its exposure.
#[test]
fn a_new_server_has_mcp_off_and_new_names_are_hidden_from_mcp() {
    use graphersal::prelude::{Graph, GraphExport, TraversalGraph};
    let temp = TempDir::new("mcp-defaults");
    let root = temp.path().join("srv");
    let out = run(&["init", root.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let dir = ServerDir::new(&root);
    let settings = std::fs::read_to_string(dir.mcp_settings_path()).unwrap();
    assert!(
        settings.contains("enabled = false") && settings.contains("expose_new = false"),
        "{settings}"
    );
    let (code, out, err) = mcp_command(&root, &["status"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("off (switch it on: graphersal-server mcp on --dir"),
        "{out}"
    );
    assert!(
        out.contains("not offered to MCP unless created with --mcp"),
        "{out}"
    );
    // Offline: every way a name is born.
    let graph = Graph::new(TraversalGraph::tinkerpop_modern());
    let graphson = temp.path().join("modern.json");
    graph
        .export_graphson_writer(std::fs::File::create(&graphson).unwrap())
        .unwrap();
    let graphson = graphson.to_str().unwrap();
    let root_text = root.to_str().unwrap();
    for args in [
        &["create", "plain"][..],
        &["create", "shown", "--mcp"],
        &["clone", "shown", "cl"],
        &["clone", "shown", "cl-shown", "--mcp"],
        &["alias", "set", "al", "shown"],
        &["alias", "set", "al-shown", "shown", "--mcp"],
        &["backup", "shown"],
    ] {
        let (code, _, err) = db(&root, args);
        assert_eq!(code, 0, "{args:?}: {err}");
    }
    let (code, out, err) = db(&root, &["create", "plain2"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("  MCP:"), "{out}");
    assert!(out.contains("not offered"), "{out}");
    assert!(
        out.contains("graphersal-server mcp db plain2 on --dir"),
        "{out}"
    );
    let (code, out, err) = db(&root, &["create", "x", "--mcp", "--no-mcp"]);
    assert_eq!(code, 2, "{out}{err}");
    let (code, _, err) = db(&root, &["delete", "plain2", "--mcp"]);
    assert_eq!(
        code, 2,
        "--mcp belongs to the commands that make a name: {err}"
    );
    for args in [
        &[root_text, graphson, "--name", "gs"][..],
        &[root_text, graphson, "--name", "gs-shown", "--mcp"],
    ] {
        let (code, _, err) = import(args);
        assert_eq!(code, 0, "{args:?}: {err}");
    }
    let (code, out, _) = db(&root, &["backups", "shown", "--json"]);
    assert_eq!(code, 0);
    let backup = json(&out)["backups"][0]["backup"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, _, err) = db(&root, &["restore", "shown", &backup, "--name", "rs"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = db(&root, &["repair", "plain"]);
    assert_eq!(code, 0, "{err}");
    // A database made by hand (no entry in mcp.toml) stays offered: the rule of older
    // directories.
    modern_db(&dir, "byhand");
    let (code, _, err) = mcp_command(&root, &["on"]);
    assert_eq!(code, 0, "{err}");

    let running = serve::start(
        &root,
        ServeOptions::new().with_listen("127.0.0.1:0".parse().unwrap()),
    )
    .unwrap();
    let address = running.address();
    let loader = running.loader();
    let all = [
        "plain",
        "shown",
        "cl",
        "cl-shown",
        "gs",
        "gs-shown",
        "rs",
        "plain-repaired",
        "byhand",
    ];
    for name in all {
        loader.load(name).unwrap();
        assert_eq!(
            loader.wait(name, Duration::from_secs(120)).unwrap(),
            DbState::Loaded,
            "{name}"
        );
    }
    let session = mcp_initialize(address, "/mcp");
    assert_eq!(
        mcp_listed(address, &session),
        ["al-shown", "byhand", "cl-shown", "gs-shown", "shown"]
    );
    let (text, error) = mcp_tool(
        address,
        "/mcp",
        &session,
        "use_graph",
        serde_json::json!({"name": "cl"}),
    );
    assert!(error, "a hidden name is unknown to MCP: {text}");
    let init = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "clientInfo": {"name": "it", "version": "1"}}});
    for hidden in ["plain", "cl", "gs", "rs", "plain-repaired"] {
        let (status, _, body) = mcp_post(
            address,
            &format!("/db/{hidden}/mcp"),
            None,
            &[],
            init.clone(),
        );
        assert_eq!(status, 404, "{hidden}: {body}");
    }
    mcp_initialize(address, "/db/shown/mcp");
    mcp_initialize(address, "/db/gs-shown/mcp");

    // Through the running server: the API's `mcp`, the server-wide default, `--no-mcp`.
    let (status, answer) = admin_json(
        address,
        "POST",
        "databases",
        serde_json::json!({"name": "api-shown", "mcp": true}),
    );
    assert_eq!(status, 201, "{answer}");
    assert_eq!(answer["mcpExposed"], true, "{answer}");
    let (status, answer) = admin_json(
        address,
        "POST",
        "databases",
        serde_json::json!({"name": "api-hidden"}),
    );
    assert_eq!(status, 201, "{answer}");
    assert_eq!(answer["mcpExposed"], false, "{answer}");
    let (status, answer) = admin_json(
        address,
        "PUT",
        "aliases/api-al",
        serde_json::json!({"target": "shown"}),
    );
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["mcpExposed"], false, "{answer}");
    let (code, out, err) = mcp_command(&root, &["expose-new", "on"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.starts_with("New databases and aliases are offered to MCP\n"),
        "{out}"
    );
    let (status, state) = admin_json(address, "GET", "mcp", serde_json::json!(null));
    assert_eq!(status, 200, "{state}");
    assert_eq!(state["exposeNew"], true, "{state}");
    let (code, _, err) = db(&root, &["create", "later"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = db(&root, &["create", "later-hidden", "--no-mcp"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = db(&root, &["alias", "set", "later-al", "shown"]);
    assert_eq!(code, 0, "{err}");
    // A renamed alias keeps its exposure (hidden and offered alike).
    for (old, new) in [("al", "al2"), ("al-shown", "al-shown2")] {
        let (code, _, err) = db(&root, &["alias", "rename", old, new]);
        assert_eq!(code, 0, "{err}");
    }
    let session = mcp_initialize(address, "/mcp");
    let listed = mcp_listed(address, &session);
    for name in ["api-shown", "later", "later-al", "al-shown2"] {
        assert!(listed.iter().any(|n| n == name), "{name}: {listed:?}");
    }
    for name in [
        "api-hidden",
        "api-al",
        "later-hidden",
        "al2",
        "al",
        "al-shown",
    ] {
        assert!(!listed.iter().any(|n| n == name), "{name}: {listed:?}");
    }
    // Both switches in one request; a body without either is refused.
    let (status, state) = admin_json(
        address,
        "PUT",
        "mcp",
        serde_json::json!({"exposeNew": false, "enabled": false}),
    );
    assert_eq!(status, 200, "{state}");
    assert_eq!(
        (&state["exposeNew"], &state["enabled"]),
        (&serde_json::json!(false), &serde_json::json!(false))
    );
    let (status, _) = admin_json(address, "PUT", "mcp", serde_json::json!({}));
    assert_eq!(status, 400);
    running.stop().unwrap();
}

/// Round 4 B-MCPDEF: a server directory made before (no `mcp.toml`, or one without
/// `expose_new`) keeps what it offered: MCP stays on and its databases and aliases (no
/// entries) stay offered; only names created from now on are hidden.
#[test]
fn an_older_server_directory_keeps_its_mcp_exposure() {
    let temp = TempDir::new("mcp-upgrade");
    let root = temp.path().join("srv");
    let dir = ServerDir::init(&root).unwrap().dir;
    // As an older version left it: no mcp.toml, a database and an alias without entries.
    std::fs::remove_file(dir.mcp_settings_path()).unwrap();
    modern_db(&dir, "old");
    std::fs::write(dir.aliases_path(), "old-al = \"old\"\n").unwrap();
    let (code, out, err) = mcp_command(&root, &["status", "--json"]);
    assert_eq!(code, 0, "{err}");
    let state = json(&out);
    assert_eq!(state["enabled"], true, "{state}");
    assert_eq!(state["exposeNew"], false, "{state}");
    let (code, _, err) = db(&root, &["clone", "old", "new"]);
    assert_eq!(code, 0, "{err}");
    // An mcp.toml of round 3 (no expose_new) parses the same way.
    let text = std::fs::read_to_string(dir.mcp_settings_path()).unwrap();
    assert!(text.contains("[databases.new]"), "{text}");
    let running = serve::start(
        &root,
        ServeOptions::new().with_listen("127.0.0.1:0".parse().unwrap()),
    )
    .unwrap();
    let address = running.address();
    for name in ["old", "new"] {
        running.loader().load(name).unwrap();
        assert_eq!(
            running
                .loader()
                .wait(name, Duration::from_secs(120))
                .unwrap(),
            DbState::Loaded
        );
    }
    let session = mcp_initialize(address, "/mcp");
    assert_eq!(mcp_listed(address, &session), ["old", "old-al"]);
    mcp_initialize(address, "/db/old/mcp");
    running.stop().unwrap();
    std::fs::write(
        dir.mcp_settings_path(),
        "enabled = true\n\n[databases.old]\nread_only = true\n",
    )
    .unwrap();
    let (code, out, err) = mcp_command(&root, &["status", "--json"]);
    assert_eq!(code, 0, "{err}");
    let state = json(&out);
    assert_eq!(
        (&state["enabled"], &state["exposeNew"]),
        (&serde_json::json!(true), &serde_json::json!(false)),
        "{state}"
    );
}
