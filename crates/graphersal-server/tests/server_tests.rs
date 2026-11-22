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
    assert!(text(&out.stdout).contains("created server directory"));
    for parts in [
        &["server.toml"][..],
        &["databases"],
        &["models", "blobs"],
        &["backups"],
        &["trash"],
        &["tmp"],
        &["logs"],
    ] {
        let path = parts.iter().fold(root.clone(), |p, c| p.join(c));
        assert!(path.exists(), "{}", path.display());
    }
    let config = std::fs::read_to_string(root.join("server.toml")).unwrap();
    assert!(config.contains("listen = \"127.0.0.1:8484\""), "{config}");
    assert!(
        config.contains("[mcp]") && config.contains("[defaults]"),
        "{config}"
    );

    let again = run(&["init", root_text]);
    assert!(again.status.success());
    assert!(text(&again.stdout).contains("already a server directory"));
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

    assert_eq!(run(&["init"]).status.code(), Some(2));
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
            .stdout(Stdio::null())
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
    assert_eq!(info_json["admin"], true);
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

    stop_and_wait(&mut server);
    assert_eq!(RunInfo::read(&dir).unwrap(), None);
    assert!(!ServerLock::is_held(&dir).unwrap());
    assert!(TcpStream::connect(info.address).is_err());
}

/// Sends SIGTERM (Unix) and checks a clean exit (`0`) within 30 s; elsewhere kills it.
#[cfg(unix)]
fn stop_and_wait(server: &mut ServerProcess) {
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
            assert!(status.success(), "{status}: {stderr}");
            assert!(stderr.contains("stopping"), "{stderr}");
            return;
        }
        assert!(Instant::now() < deadline, "the server did not stop");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(not(unix))]
fn stop_and_wait(_server: &mut ServerProcess) {
    // No portable SIGTERM from a test: the in-process tests cover the stop path.
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

    // Per-database info: the mount, the real name, the admin API.
    let (status, _, body) = call(address, "GET", "/db/a/api/info", &[], "");
    assert_eq!(status, 200, "{body}");
    let info = json(&body);
    assert_eq!(info["mountPath"], "/db/a");
    assert_eq!(info["database"], "a");
    assert_eq!(info["admin"], true);
    let info = json(&call(address, "GET", "/db/current/api/info", &[], "").2);
    assert_eq!(info["database"], "b", "an alias shows the real database");
    assert_eq!(info["mountPath"], "/db/b");
    // The root info announces the admin API (the playground shows its landing view).
    let root = json(&get(address, "/api/info").1);
    assert_eq!(root["admin"], true);
    assert_eq!(root["server"], "graphersal-server");
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

    // The playground under each mount and at the root (its landing view).
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
