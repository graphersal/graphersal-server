//! The command line and the HTTP listener end to end: `init`, `serve` on port 0, the probes,
//! `/api/info`, the server lock, the run file and a clean stop.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use graphersal_server::{
    DbState, RunInfo, ServeOptions, ServerDir, ServerError, ServerLock, serve,
};

const BIN: &str = env!("CARGO_BIN_EXE_graphersal-server");

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
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A server process, killed on drop if it still runs.
struct ServerProcess(Child);

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

/// Sends `head` (and `body`) on a new connection and reads the answer: (status, body).
fn exchange(address: SocketAddr, head: &str, body: &[u8]) -> (u16, String) {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(head.as_bytes()).unwrap();
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
    assert!(out.stderr.is_empty(), "{}", text(&out.stderr));
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
fn serve_answers_refuses_a_second_server_and_stops_cleanly() {
    let temp = TempDir::new("serve");
    let dir = ServerDir::init(temp.path()).unwrap().dir;
    let root = temp.path().to_str().unwrap();
    let mut server = ServerProcess(
        Command::new(BIN)
            .args(["serve", "--dir", root, "--listen", "127.0.0.1:0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
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

/// Sends SIGTERM (Unix) and checks a clean exit (`0`) within 30 s; elsewhere kills it.
/// Returns what the server wrote: (stdout, stderr).
#[cfg(unix)]
fn stop_and_wait(server: &mut ServerProcess) -> (String, String) {
    let status = Command::new("kill")
        .args(["-TERM", &server.0.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
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

#[cfg(not(unix))]
fn stop_and_wait(server: &mut ServerProcess) -> (String, String) {
    // No portable SIGTERM from a test: the in-process tests cover the stop path. Killed, the
    // server wrote its start block but no stop lines.
    let _ = server.0.kill();
    let _ = server.0.wait();
    let mut out = (String::new(), String::new());
    if let Some(mut pipe) = server.0.stdout.take() {
        let _ = pipe.read_to_string(&mut out.0);
    }
    if let Some(mut pipe) = server.0.stderr.take() {
        let _ = pipe.read_to_string(&mut out.1);
    }
    out
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
    // A huge declared body gets no answer (never allocated); the server keeps serving.
    let mut huge = TcpStream::connect(address).unwrap();
    huge.write_all(
        format!(
            "POST /api/info HTTP/1.1\r\nHost: {address}\r\nContent-Length: {}\r\n\r\n",
            1u64 << 62
        )
        .as_bytes(),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(get(address, "/healthz").0, 200);
    drop(huge);
    running.stop().unwrap();
}

/// Sends one request (`headers` beside `Host` and `Connection: close`) and reads the answer:
/// (status, head, body).
fn call(
    address: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String, String) {
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n");
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
    let mut server = ServerProcess(
        Command::new(BIN)
            .args(["serve", "--dir"])
            .arg(root)
            .args(["--listen", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
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
    assert_eq!(out.trim(), "Database 'acme' is loaded");
    let (status, _) = get(info.address, "/db/acme/api/info");
    assert_eq!(status, 200);
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

    #[cfg(unix)]
    {
        let status = Command::new("kill")
            .args(["-TERM", &server.0.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        let deadline = Instant::now() + Duration::from_secs(30);
        while server.0.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "the server did not stop");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let _ = &mut server;
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
    let mut server = ServerProcess(
        Command::new(BIN)
            .args(["serve", "--dir"])
            .arg(&root)
            .args(["--listen", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
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

    #[cfg(unix)]
    {
        let status = Command::new("kill")
            .args(["-TERM", &server.0.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        let deadline = Instant::now() + Duration::from_secs(30);
        while server.0.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "the server did not stop");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let _ = &mut server;
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
    let mut server = ServerProcess(
        Command::new(BIN)
            .args(["serve", "--dir", &root, "--listen", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
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
    let mut server = ServerProcess(
        Command::new(BIN)
            .args(["serve", "--dir", &root, "--listen", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
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

    // An alias is resolved at use_graph: a flip keeps the open session, a new one follows it.
    running.registry().alias_set("cur", "a").unwrap();
    let pinned = mcp_initialize(address, "/mcp");
    let (text, is_error) = mcp_tool(
        address,
        "/mcp",
        &pinned,
        "use_graph",
        serde_json::json!({"name": "cur"}),
    );
    assert!(!is_error && text.contains("\"selected\":\"a\""), "{text}");
    running.registry().alias_set("cur", "b").unwrap();
    assert_eq!(mcp_persons(address, "/mcp", &pinned, None), 5);
    let fresh = mcp_initialize(address, "/mcp");
    let (text, is_error) = mcp_tool(
        address,
        "/mcp",
        &fresh,
        "use_graph",
        serde_json::json!({"name": "cur"}),
    );
    assert!(!is_error && text.contains("\"selected\":\"b\""), "{text}");
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

#[test]
fn mcp_token_and_off_switch_come_from_server_toml() {
    let temp = TempDir::new("mcp-config");
    let dir = ServerDir::init(temp.path()).unwrap().dir;
    let config = dir.root().join("server.toml");
    std::fs::write(&config, "[mcp]\ntoken = \"s3cret\"\nmax_sessions = 2\n").unwrap();
    let listen: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let running = serve::start(temp.path(), ServeOptions::new().with_listen(listen)).unwrap();
    let address = running.address();
    let init = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "clientInfo": {"name": "it", "version": "1"}}});
    let (status, _, body) = mcp_post(address, "/mcp", None, &[], init.clone());
    assert_eq!(status, 401, "{body}");
    let (status, head, body) = mcp_post(
        address,
        "/mcp",
        None,
        &[("Authorization", "Bearer s3cret")],
        init,
    );
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
    let mut server = ServerProcess(
        Command::new(BIN)
            .args(["serve", "--dir", &root, "--listen", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
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
