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
            "Created the server directory {root_text}\n  config: {} (listen 127.0.0.1:8484, MCP \
             on: edit it before serving)\n  # start it\n  graphersal-server serve --dir \
             {root_text}\n  # or create a database first\n  graphersal-server db create NAME \
             --dir {root_text}\n",
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
    assert_eq!(
        out,
        "Created database 'acme'\n  kind:        plain\n  description: Acme graph\n  \
         state:       unloaded\n"
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
        lines[1].starts_with(&format!("{}SIZE", cells("NAME", "STATE", "KIND", "FLAGS"))),
        "{out}"
    );
    let size_at = lines[1].find("SIZE").unwrap();
    let description_at = lines[1].find("DESCRIPTION").unwrap();
    assert_eq!(lines[2][..size_at], cells("acme", "unloaded", "plain", ""));
    assert_eq!(&lines[2][description_at..], "Acme graph");
    assert!(lines[2][size_at..description_at].contains("KiB"), "{out}");
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
             kind:        plain\n  description: From GraphSON\n  state:       unloaded\n"
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
    for name in ["a", "b"] {
        modern_db(&dir, name);
    }
    // The console renames offline too.
    let (code, _, err) = db(root, &["alias", "set", "old", "a"]);
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

    // 3. Created again under the name: the session works on, no note.
    let (status, answer) = admin_json(
        address,
        "PUT",
        "aliases/cur",
        serde_json::json!({"target": "a"}),
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
