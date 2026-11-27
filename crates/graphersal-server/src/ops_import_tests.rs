//! Tests of imports ([`Admin::import`]): every source kind offline, model handling, layer
//! snapshots with and without their base, injected failures, and the upload and path routes
//! of a real listener on port 0 (a body over 64 MiB streamed, the upload limit). Synthetic
//! data only (the `modern` sample, generated GraphSON, a tiny synthetic model).

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

use graphersal::persist::{
    ImportModelOptions, ModelCopy, ModelSource, ModelState, WriteOptions, read_snapshot_info,
    write_graph_snapshot,
};
use graphersal::prelude::{Graph, GraphExport};
use graphersal_serve::{Exposure, HostCheck};
use serde_json::{Value, json};

use super::*;
use crate::database::STORE_DIR;
use crate::registry::DbState;
use crate::registry::tests::server;
use crate::serve::{Running, ServeOptions};
use crate::test_support::TempDir;

const WAIT: Duration = Duration::from_secs(120);

fn offline(dir: &ServerDir) -> Admin {
    Admin::offline(Registry::scan(dir).expect("scan"))
}

fn loader_of(dir: &ServerDir) -> Arc<Loader> {
    Loader::new(
        Registry::scan(dir).expect("scan"),
        HostCheck::new(Exposure::Loopback, 8484, "http://127.0.0.1:8484/"),
    )
}

/// The modern sample as a packed snapshot at `path` (no extension needed).
fn modern_snapshot(path: &Path) {
    let file = fs::File::create(path).unwrap();
    write_graph_snapshot(
        &Graph::new(TraversalGraph::tinkerpop_modern()),
        file,
        &WriteOptions::new(),
        ModelCopy::Include,
    )
    .expect("a snapshot");
}

/// The vertex count of the closed plain Store of the database `name`.
fn plain_count(dir: &ServerDir, name: &str) -> usize {
    let store = Store::open(
        dir.databases().join(name).join(STORE_DIR),
        StoreOptions::new(),
    )
    .expect("opens");
    let count = store.graph().read().vertex_count();
    store.close().expect("closes");
    count
}

/// A base database `name` holding the modern sample.
fn modern_base(admin: &Admin, name: &str) {
    let entry = admin
        .create(name, &CreateSpec::plain().with_role(DatabaseRole::Base))
        .expect("a base");
    let store_dir = entry.store_dir();
    fs::remove_dir_all(&store_dir).unwrap();
    Store::create_with(
        &store_dir,
        TraversalGraph::tinkerpop_modern(),
        StoreOptions::new(),
    )
    .and_then(Store::close)
    .expect("the modern base");
}

fn tmp_count(dir: &ServerDir) -> usize {
    fs::read_dir(dir.tmp()).map(|d| d.count()).unwrap_or(0)
}

#[test]
fn a_full_snapshot_becomes_a_plain_database_whatever_its_file_name() {
    let temp = TempDir::new("import-gsnap");
    let dir = server(&temp);
    let admin = offline(&dir);
    // Named like GraphSON: the head decides.
    let file = temp.path().join("modern.json");
    modern_snapshot(&file);
    let imported = admin
        .import(&ImportSource::new(&file), "modern", None, Some("Modern"))
        .expect("imported");
    assert_eq!(imported.format, ImportFormat::Snapshot);
    assert_eq!(imported.entry.state, DbState::Unloaded);
    let config = imported.entry.config.as_ref().unwrap();
    assert_eq!(config.kind, DatabaseKind::Plain);
    assert_eq!(config.description, "Modern");
    assert!(imported.models_missing.is_empty());
    assert_eq!(plain_count(&dir, "modern"), 6);
    assert_eq!(tmp_count(&dir), 0);
    // The name is taken now; a plain snapshot takes no base.
    assert!(matches!(
        admin.import(&ImportSource::new(&file), "modern", None, None),
        Err(ServerError::NameInUse { .. })
    ));
    assert!(matches!(
        admin.import(&ImportSource::new(&file), "other", Some("modern"), None),
        Err(ServerError::InvalidInput { .. })
    ));
    assert!(!dir.databases().join("other").exists());
}

/// An import into a name breaking the rule of new names is refused before anything is made.
#[test]
fn an_import_refuses_a_name_breaking_the_new_rule() {
    let temp = TempDir::new("import-new-names");
    let dir = server(&temp);
    let admin = offline(&dir);
    let file = temp.path().join("modern.gsnap");
    modern_snapshot(&file);
    let too_long = "x".repeat(33);
    for (name, reason) in [
        (too_long.as_str(), "at most 32 characters"),
        ("modern-", "ends with a lower-case letter or a digit"),
        ("a..b", "cannot hold '..'"),
    ] {
        let err = admin
            .import(&ImportSource::new(&file), name, None, None)
            .unwrap_err();
        assert!(matches!(err, ServerError::InvalidName { .. }), "{err}");
        assert!(err.to_string().contains(reason), "{name}: {err}");
    }
    assert!(admin.registry().list().is_empty());
    assert_eq!(tmp_count(&dir), 0);
}

#[test]
fn snapshots_with_and_without_models() {
    let temp = TempDir::new("import-models");
    let dir = server(&temp);
    let admin = offline(&dir);
    let entry = admin.create("acme", &CreateSpec::plain()).unwrap();
    let store = Store::open(entry.store_dir(), StoreOptions::new()).unwrap();
    store
        .import_model(
            ModelSource::Files(crate::test_models::tiny_model(1, "acme/tiny")),
            ImportModelOptions::new(),
        )
        .expect("the model");
    store.close().unwrap();
    let with = temp.path().join("with.gsnap");
    admin
        .export_to_file("acme", ExportSpec::new(), &with)
        .unwrap();
    let without = temp.path().join("without.gsnap");
    admin
        .export_to_file("acme", ExportSpec::new().with_models(false), &without)
        .unwrap();

    // Without its model while models/ lacks it: needs-model (the import of `with` below
    // shares the blob into models/, after which an import resolves it).
    let imported = admin
        .import(&ImportSource::new(&without), "no-model", None, None)
        .expect("imported");
    assert_eq!(imported.entry.state, DbState::needs_model("acme/tiny"));
    assert_eq!(imported.models_missing, ["acme/tiny"]);
    assert!(
        imported.notes[0].contains("acme/tiny"),
        "{:?}",
        imported.notes
    );
    assert_eq!(
        imported
            .entry
            .config
            .as_ref()
            .unwrap()
            .needs_model
            .as_deref(),
        Some("acme/tiny")
    );

    let imported = admin
        .import(&ImportSource::new(&with), "with-model", None, None)
        .expect("imported");
    assert_eq!(imported.entry.state, DbState::Unloaded);
    assert!(imported.models_missing.is_empty());
    let models = Store::models_dir(imported.entry.store_dir()).unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].repo, "acme/tiny");
    assert_ne!(models[0].state, ModelState::Missing);
    // A scan keeps the state (the marker in database.toml).
    let registry = Registry::scan(&dir).unwrap();
    assert_eq!(
        registry.get("no-model").unwrap().state,
        DbState::needs_model("acme/tiny")
    );
    assert_eq!(registry.get("with-model").unwrap().state, DbState::Unloaded);
    drop(registry);

    // A load finds the model still missing: needs-model again; the other one loads.
    let loader = loader_of(&dir);
    loader.load("no-model").expect("starts");
    assert_eq!(
        loader.wait("no-model", WAIT).unwrap(),
        DbState::needs_model("acme/tiny")
    );
    loader.load("with-model").expect("starts");
    assert_eq!(loader.wait("with-model", WAIT).unwrap(), DbState::Loaded);
    loader.shutdown().unwrap();
}

#[test]
fn a_layer_snapshot_needs_its_base() {
    let temp = TempDir::new("import-layer");
    let dir = server(&temp);
    let admin = offline(&dir);
    modern_base(&admin, "prod");
    admin.create("l1", &CreateSpec::layer("prod")).unwrap();
    let file = temp.path().join("l1.layer.gsnap");
    let exported = admin
        .export_to_file("l1", ExportSpec::new(), &file)
        .unwrap();
    assert!(exported.layer);
    let identity = read_snapshot_info(fs::File::open(&file).unwrap())
        .unwrap()
        .layer_base
        .expect("a layer snapshot")
        .to_string();

    // Over the only matching base, or the one named.
    let imported = admin
        .import(&ImportSource::new(&file), "l2", None, None)
        .expect("imported");
    let config = imported.entry.config.as_ref().unwrap();
    assert_eq!(config.kind, DatabaseKind::Layer);
    assert_eq!(config.base.as_deref(), Some("prod"));
    assert_eq!(imported.layer_base.as_deref(), Some(identity.as_str()));
    let imported = admin
        .import(&ImportSource::new(&file), "l3", Some("prod"), None)
        .expect("imported");
    assert_eq!(imported.entry.config.unwrap().base.as_deref(), Some("prod"));
    // The layer database loads over its base.
    let loader = loader_of(&dir);
    loader.load("l2").expect("starts");
    assert_eq!(loader.wait("l2", WAIT).unwrap(), DbState::Loaded);
    loader.shutdown().unwrap();

    // A named database that is not a base: refused naming the identity.
    let admin = offline(&dir);
    admin.create("plain", &CreateSpec::plain()).unwrap();
    let err = admin
        .import(&ImportSource::new(&file), "l4", Some("plain"), None)
        .unwrap_err();
    assert!(matches!(err, ServerError::Refused { .. }), "{err}");
    assert!(err.to_string().contains(&identity), "{err}");

    // Another server directory without the base: refused naming the identity, nothing left.
    let other = TempDir::new("import-layer-nobase");
    let other_dir = server(&other);
    let err = offline(&other_dir)
        .import(&ImportSource::new(&file), "l1", None, None)
        .unwrap_err();
    assert!(matches!(err, ServerError::Refused { .. }), "{err}");
    let text = err.to_string();
    assert!(
        text.contains(&identity) && text.contains("no base database"),
        "{text}"
    );
    assert!(!other_dir.databases().join("l1").exists());
    assert_eq!(tmp_count(&other_dir), 0);
}

#[test]
fn stores_are_copied_from_directories_and_single_files() {
    let temp = TempDir::new("import-store");
    let dir = server(&temp);
    let admin = offline(&dir);
    let source_dir = temp.path().join("source-store");
    Store::create_with(
        &source_dir,
        TraversalGraph::tinkerpop_modern(),
        StoreOptions::new(),
    )
    .and_then(Store::close)
    .unwrap();
    let imported = admin
        .import(&ImportSource::new(&source_dir), "from-dir", None, None)
        .expect("imported");
    assert_eq!(imported.format, ImportFormat::Store);
    assert_eq!(plain_count(&dir, "from-dir"), 6);
    assert!(Store::is_store(&source_dir).unwrap(), "the source is kept");

    let single = temp.path().join("modern.gstore");
    Store::create_with(
        &single,
        TraversalGraph::tinkerpop_modern(),
        StoreOptions::new(),
    )
    .and_then(Store::close)
    .unwrap();
    assert!(single.is_file());
    let imported = admin
        .import(&ImportSource::new(&single), "from-file", None, None)
        .expect("imported");
    assert_eq!(imported.format, ImportFormat::Store);
    assert!(imported.entry.store_dir().is_dir(), "a directory Store");
    assert_eq!(plain_count(&dir, "from-file"), 6);

    // A layer Store is matched with its base like a layer snapshot.
    modern_base(&admin, "prod");
    let layer = admin.create("l1", &CreateSpec::layer("prod")).unwrap();
    let imported = admin
        .import(&ImportSource::new(layer.store_dir()), "l1-copy", None, None)
        .expect("imported");
    assert_eq!(imported.entry.config.unwrap().base.as_deref(), Some("prod"));
    // A directory without a Store.
    let empty = temp.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    assert!(matches!(
        admin.import(&ImportSource::new(&empty), "x", None, None),
        Err(ServerError::UnsupportedSource { .. })
    ));
}

#[test]
fn graphson_and_graphml_files_become_plain_databases() {
    let temp = TempDir::new("import-text");
    let dir = server(&temp);
    let admin = offline(&dir);
    let graph = Graph::new(TraversalGraph::tinkerpop_modern());
    let graphson = temp.path().join("modern.json");
    graph
        .export_graphson_writer(fs::File::create(&graphson).unwrap())
        .unwrap();
    let graphml = temp.path().join("modern.graphml");
    graph
        .export_graphml_writer(fs::File::create(&graphml).unwrap())
        .unwrap();

    let imported = admin
        .import(&ImportSource::new(&graphson), "gs", None, None)
        .expect("imported");
    assert_eq!(imported.format, ImportFormat::GraphSon);
    assert_eq!(imported.skipped, 0);
    assert_eq!(plain_count(&dir, "gs"), 6);
    let imported = admin
        .import(&ImportSource::new(&graphml), "gm", None, None)
        .expect("imported");
    assert_eq!(imported.format, ImportFormat::GraphMl);
    assert_eq!(plain_count(&dir, "gm"), 6);

    // Without an extension (an upload): by the first character; a format hint wins.
    let bare = temp.path().join("upload-1");
    fs::copy(&graphml, &bare).unwrap();
    assert_eq!(
        admin
            .import(&ImportSource::new(&bare), "bare", None, None)
            .unwrap()
            .format,
        ImportFormat::GraphMl
    );
    let err = admin
        .import(
            &ImportSource::new(&bare).with_format(Some(ImportFormat::GraphSon)),
            "hinted",
            None,
            None,
        )
        .unwrap_err();
    assert!(matches!(err, ServerError::InvalidInput { .. }), "{err}");

    // Data GraphSON cannot hold is skipped and noted.
    let skipping = temp.path().join("skipping.json");
    fs::write(
        &skipping,
        concat!(
            r#"{"id":{"@type":"g:Int64","@value":1},"label":"person","properties":{"#,
            r#""born":[{"id":{"@type":"g:Int64","@value":7},"value":{"@type":"gx:Year","@value":"2020"}}],"#,
            r#""name":[{"id":{"@type":"g:Int64","@value":8},"value":"ann"}]}}"#,
            "\n"
        ),
    )
    .unwrap();
    let imported = admin
        .import(&ImportSource::new(&skipping), "skipping", None, None)
        .expect("imported");
    assert!(imported.skipped >= 1, "{imported:?}");
    assert!(!imported.notes.is_empty());
    assert_eq!(plain_count(&dir, "skipping"), 1);

    // A base for a text file, an unknown format, a missing source.
    assert!(matches!(
        admin.import(&ImportSource::new(&graphson), "x", Some("gs"), None),
        Err(ServerError::InvalidInput { .. })
    ));
    let junk = temp.path().join("junk.bin");
    fs::write(&junk, b"\x00\x01 not a graph").unwrap();
    assert!(matches!(
        admin.import(&ImportSource::new(&junk), "x", None, None),
        Err(ServerError::UnsupportedSource { .. })
    ));
    assert!(matches!(
        admin.import(
            &ImportSource::new(temp.path().join("missing.json")),
            "x",
            None,
            None
        ),
        Err(ServerError::SourceNotFound { .. })
    ));
    assert!(!dir.databases().join("x").exists());
}

#[test]
fn a_failure_or_a_crash_mid_import_leaves_no_database() {
    let temp = TempDir::new("import-fail");
    let dir = server(&temp);
    let file = temp.path().join("modern.gsnap");
    modern_snapshot(&file);
    let mut admin = offline(&dir);
    admin.fail = Some(FailPoint::AfterStore);
    assert!(
        admin
            .import(&ImportSource::new(&file), "broken", None, None)
            .is_err()
    );
    assert!(!dir.databases().join("broken").exists());
    assert_eq!(tmp_count(&dir), 0, "the build is removed");
    admin.fail = Some(FailPoint::CrashAfterStore);
    assert!(
        admin
            .import(&ImportSource::new(&file), "crashed", None, None)
            .is_err()
    );
    assert!(!dir.databases().join("crashed").exists());
    assert_eq!(tmp_count(&dir), 1, "debris of the crash");
    let registry = Registry::scan(&dir).unwrap();
    assert!(registry.get("crashed").is_err());
    assert_eq!(tmp_count(&dir), 0, "the scan removes the debris");
}

// --- over a real listener ----------------------------------------------------------------

fn start(dir: &ServerDir) -> Running {
    crate::serve::start(
        dir.root(),
        ServeOptions::new().with_listen("127.0.0.1:0".parse().unwrap()),
    )
    .expect("starts")
}

/// Sends `POST path` declaring `length` bytes and streams `body` (`None`: the headers only);
/// answers the status and the JSON body.
fn upload(
    address: SocketAddr,
    path: &str,
    length: u64,
    body: Option<&mut dyn Read>,
) -> (u16, Value) {
    let stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(300)))
        .unwrap();
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n{}Content-Type: \
         application/octet-stream\r\nContent-Length: {length}\r\n\r\n",
        crate::test_support::admin_header(address)
    );
    let mut writer = std::io::BufWriter::with_capacity(256 * 1024, &stream);
    writer.write_all(head.as_bytes()).unwrap();
    if let Some(body) = body {
        std::io::copy(body, &mut writer).unwrap();
    }
    writer.flush().unwrap();
    drop(writer);
    read_answer(stream)
}

fn read_answer(stream: TcpStream) -> (u16, Value) {
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line).unwrap();
    let status: u16 = status_line.split(' ').nth(1).unwrap().parse().unwrap();
    let mut length = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((field, value)) = line.split_once(':')
            && field.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let body = match length {
        Some(length) => {
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            body
        }
        // No length: the JSON body is read until it parses (every answer of the listener
        // carries its length; kept for a close-delimited answer).
        None => {
            reader
                .get_ref()
                .set_read_timeout(Some(Duration::from_millis(200)))
                .unwrap();
            let mut body = Vec::new();
            let mut chunk = [0u8; 4096];
            for _ in 0..50 {
                match reader.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => body.extend_from_slice(&chunk[..n]),
                    Err(_) => {}
                }
                if serde_json::from_slice::<Value>(&body).is_ok() {
                    break;
                }
            }
            body
        }
    };
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

/// GraphSON lines of `n` vertices, each with a string property of `pad` bytes, generated as
/// they are read (never held whole in memory).
struct BigGraphson {
    next: usize,
    n: usize,
    pad: usize,
    line: Vec<u8>,
    at: usize,
}

impl BigGraphson {
    fn new(n: usize, pad: usize) -> Self {
        Self {
            next: 0,
            n,
            pad,
            line: Vec::new(),
            at: 0,
        }
    }

    fn line(i: usize, pad: usize) -> Vec<u8> {
        let fill = ((b'a' + (i % 26) as u8) as char).to_string().repeat(pad);
        format!(
            "{{\"id\":\"v{i}\",\"label\":\"blob\",\"properties\":{{\"text\":[{{\"id\":\"p{i}\",\
             \"value\":\"{fill}\"}}]}}}}\n"
        )
        .into_bytes()
    }

    /// The total length in bytes.
    fn length(n: usize, pad: usize) -> u64 {
        (0..n).map(|i| Self::line(i, pad).len() as u64).sum()
    }
}

impl Read for BigGraphson {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.at == self.line.len() {
            if self.next == self.n {
                return Ok(0);
            }
            self.line = Self::line(self.next, self.pad);
            self.at = 0;
            self.next += 1;
        }
        let n = buf.len().min(self.line.len() - self.at);
        buf[..n].copy_from_slice(&self.line[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

#[test]
fn imports_over_http_by_upload_and_by_path() {
    let temp = TempDir::new("import-http");
    let dir = server(&temp);
    let file = temp.path().join("modern.gsnap");
    modern_snapshot(&file);
    let running = start(&dir);
    let address = running.address();

    // The file as the body.
    let bytes = fs::read(&file).unwrap();
    let (status, answer) = upload(
        address,
        "/api/admin/import?name=up&description=An+upload%21",
        bytes.len() as u64,
        Some(&mut bytes.as_slice()),
    );
    assert_eq!(status, 201, "{answer}");
    assert_eq!(answer["name"], "up");
    assert_eq!(answer["state"], "unloaded");
    assert_eq!(answer["description"], "An upload!");
    assert_eq!(answer["import"]["format"], "snapshot");
    assert_eq!(tmp_count(&dir), 0, "the upload is removed");
    // Again: the name is taken (409; the body is drained, the server keeps answering).
    let (status, answer) = upload(
        address,
        "/api/admin/import?name=up",
        bytes.len() as u64,
        Some(&mut bytes.as_slice()),
    );
    assert_eq!(status, 409, "{answer}");
    // No name, an unknown parameter or format, an invalid name: 400.
    for query in [
        "",
        "?name=x&color=red",
        "?name=x&format=csv",
        "?name=Bad%20Name",
    ] {
        let (status, answer) = upload(
            address,
            &format!("/api/admin/import{query}"),
            bytes.len() as u64,
            Some(&mut bytes.as_slice()),
        );
        assert_eq!(status, 400, "{query}: {answer}");
    }
    // Junk: 415; an empty body: 400.
    let junk = b"\x00\x01\x02 junk";
    let (status, answer) = upload(
        address,
        "/api/admin/import?name=junk",
        junk.len() as u64,
        Some(&mut junk.as_slice()),
    );
    assert_eq!(status, 415, "{answer}");
    let (status, _) = upload(address, "/api/admin/import?name=empty", 0, None);
    assert_eq!(status, 400);
    assert!(!dir.databases().join("junk").exists());
    assert_eq!(tmp_count(&dir), 0);

    // By path (the console's form while a server runs).
    let call = |body: Value| {
        crate::client::request(
            address,
            crate::test_support::operator_token(address).as_deref(),
            "POST",
            "/api/admin/import",
            Some(&body),
        )
        .unwrap()
    };
    let (status, answer) = call(json!({ "path": file.to_str().unwrap(), "name": "by-path" }));
    assert_eq!(status, 201, "{answer}");
    assert_eq!(answer["import"]["format"], "snapshot");
    let (status, _) = call(json!({ "path": "relative.gsnap", "name": "x" }));
    assert_eq!(status, 400);
    let missing = temp.path().join("missing.gsnap");
    let (status, answer) = call(json!({ "path": missing.to_str().unwrap(), "name": "x" }));
    assert_eq!(status, 404, "{answer}");
    let (status, _) = call(json!({ "path": "/x", "name": "x", "bogus": 1 }));
    assert_eq!(status, 400);
    let (status, _) = crate::client::request(
        address,
        crate::test_support::operator_token(address).as_deref(),
        "GET",
        "/api/admin/import",
        None,
    )
    .unwrap();
    assert_eq!(status, 405);

    // A cross-site upload is refused.
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .write_all(
            format!(
                "POST /api/admin/import?name=evil HTTP/1.1\r\nHost: {address}\r\nOrigin: \
                 http://evil.example\r\nConnection: close\r\nContent-Length: 4\r\n\r\nabcd"
            )
            .as_bytes(),
        )
        .unwrap();
    assert_eq!(read_answer(stream).0, 403);
    assert!(!dir.databases().join("evil").exists());
    running.stop().unwrap();
}

#[test]
fn an_upload_over_64_mib_is_streamed() {
    if crate::test_support::skip_on_ci("memory") {
        return;
    }
    let temp = TempDir::new("import-big");
    let dir = server(&temp);
    let running = start(&dir);
    // 72 lines of 1 MiB: 72 MiB, more than MAX_DECLARED_BODY_BYTES (64 MiB).
    let (n, pad) = (72, 1 << 20);
    let length = BigGraphson::length(n, pad);
    assert!(length > crate::serve::MAX_DECLARED_BODY_BYTES as u64);
    let (status, answer) = upload(
        running.address(),
        "/api/admin/import?name=big",
        length,
        Some(&mut BigGraphson::new(n, pad)),
    );
    assert_eq!(status, 201, "{answer}");
    assert_eq!(answer["import"]["format"], "graphson");
    running.stop().unwrap();
    assert_eq!(plain_count(&dir, "big"), n);
    assert_eq!(tmp_count(&dir), 0);
}

#[test]
fn uploads_over_the_limit_are_refused_with_413() {
    let temp = TempDir::new("import-limit");
    let dir = server(&temp);
    let config = dir.root().join(crate::layout::CONFIG_FILE);
    let text = fs::read_to_string(&config).unwrap();
    assert!(text.contains("upload_bytes = 4294967296"), "{text}");
    fs::write(
        &config,
        text.replace("upload_bytes = 4294967296", "upload_bytes = 1048576"),
    )
    .unwrap();
    let running = start(&dir);
    let address = running.address();
    // Declared over the limit: refused before reading; the client sends its body anyway.
    let body = vec![b' '; 2 << 20];
    let (status, answer) = upload(
        address,
        "/api/admin/import?name=a",
        body.len() as u64,
        Some(&mut body.as_slice()),
    );
    assert_eq!(status, 413, "{answer}");
    assert!(
        answer["error"].as_str().unwrap().contains("1048576"),
        "{answer}"
    );
    // Declared absurdly large: answered without reading anything.
    let (status, answer) = upload(address, "/api/admin/import?name=b", 1 << 50, None);
    assert_eq!(status, 413, "{answer}");
    assert!(
        answer["help"]
            .as_str()
            .is_some_and(|h| h.contains("upload_bytes")),
        "{answer}"
    );
    // The limit is announced; the server keeps answering.
    let (status, info) = crate::client::request(
        address,
        crate::test_support::operator_token(address).as_deref(),
        "GET",
        "/api/info",
        None,
    )
    .unwrap();
    assert_eq!(status, 200);
    assert_eq!(info["uploadBytes"], 1048576);
    let (status, _) = crate::client::request(
        address,
        crate::test_support::operator_token(address).as_deref(),
        "GET",
        "/api/admin/databases",
        None,
    )
    .unwrap();
    assert_eq!(status, 200);
    assert!(!dir.databases().join("a").exists());
    assert!(!dir.databases().join("b").exists());
    running.stop().unwrap();
    assert_eq!(tmp_count(&dir), 0);
}

#[test]
fn an_import_on_a_taken_name_answers_the_help_by_upload_and_by_path() {
    let temp = TempDir::new("import-taken-help");
    let dir = server(&temp);
    let file = temp.path().join("modern.gsnap");
    modern_snapshot(&file);
    let running = start(&dir);
    let address = running.address();
    let help = "choose another name: GET /api/admin/databases lists the taken names (databases \
                and aliases share one namespace, case-insensitive)";

    let bytes = fs::read(&file).unwrap();
    let (status, answer) = upload(
        address,
        "/api/admin/import?name=taken",
        bytes.len() as u64,
        Some(&mut bytes.as_slice()),
    );
    assert_eq!(status, 201, "{answer}");
    // The upload form: refused before the body is read, case-insensitive.
    for name in ["taken", "TAKEN"] {
        let (status, answer) = upload(
            address,
            &format!("/api/admin/import?name={name}"),
            bytes.len() as u64,
            Some(&mut bytes.as_slice()),
        );
        assert_eq!(status, 409, "{answer}");
        assert_eq!(answer["help"], help, "{answer}");
    }
    // The path form (the console's while a server runs).
    let (status, answer) = crate::client::request(
        address,
        crate::test_support::operator_token(address).as_deref(),
        "POST",
        "/api/admin/import",
        Some(&json!({ "path": file.to_str().unwrap(), "name": "taken" })),
    )
    .unwrap();
    assert_eq!(status, 409, "{answer}");
    assert_eq!(answer["help"], help, "{answer}");
    assert_eq!(tmp_count(&dir), 0);
}

// ---------------------------------------------------------------- KBFIX: stored jobs

/// A graph (the modern sample) carrying the backup job `evil` writing to `outside` (with its
/// file effect, as `jobs/define` stores it), the backup job `inside` writing into `root`, and
/// a checkpoint job.
fn graph_with_jobs(outside: &Path, root: &Path) -> TraversalGraph {
    use graphersal::auth::{AccessRequest, Action, Resource};
    use graphersal::catalog::{Definition, JobDefinition, JobTask};
    use graphersal::prelude::GraphStorage;
    let mut graph = TraversalGraph::tinkerpop_modern();
    let backup = |path: &Path| {
        JobDefinition::new(
            "0 3 * * *",
            JobTask::Backup {
                path: path.display().to_string(),
            },
        )
        .with_effects(vec![
            AccessRequest::new(Action::Create, Resource::File { path }).to_owned_request(),
        ])
    };
    graph
        .set_definitions(vec![
            Definition::job("evil", backup(outside).with_description("old machine")),
            Definition::job("inside", backup(&root.join("kept"))),
            Definition::job("cp", JobDefinition::new("0 4 * * *", JobTask::Checkpoint)),
        ])
        .unwrap();
    graph
}

/// The job definitions of the database `name` (its closed Store opened read-only).
fn stored_jobs(dir: &ServerDir, name: &str) -> Vec<graphersal::catalog::Definition> {
    use graphersal::prelude::GraphStorage;
    let recovered = Store::open_read_only(
        dir.databases().join(name).join(STORE_DIR),
        graphersal::persist::RecoveryTarget::Latest,
    )
    .unwrap();
    let graph = recovered.into_graph();
    let read = graph.read();
    read.definitions()
        .unwrap()
        .filter(|d| d.kind() == graphersal::catalog::DefinitionKind::JOB)
        .cloned()
        .collect()
}

#[test]
fn an_import_reports_jobs_outside_the_backup_root_and_rebases_them_when_asked() {
    let temp = TempDir::new("import-jobs");
    let dir = server(&temp);
    let root = dir
        .ensure_backup_root(&dir.backup_root(&dir.load_config().unwrap()))
        .unwrap();
    let outside = temp.path().join("elsewhere").join("kb-backups");
    let source = temp.path().join("source-store");
    Store::create_with(
        &source,
        graph_with_jobs(&outside, &root),
        StoreOptions::new(),
    )
    .unwrap()
    .close()
    .unwrap();
    let admin = offline(&dir);

    // Reported, not rewritten: it will not run.
    let imported = admin
        .import(&ImportSource::new(&source), "kb", None, None)
        .unwrap();
    assert_eq!(
        imported.jobs_outside.len(),
        1,
        "{:?}",
        imported.jobs_outside
    );
    let job = &imported.jobs_outside[0];
    assert_eq!(job.job, "evil");
    assert_eq!(job.path, outside);
    assert_eq!(job.rebased_to, None);
    assert!(
        imported
            .notes
            .iter()
            .any(|n| n.contains("'evil'") && n.contains("dropped at the first load")),
        "{:?}",
        imported.notes
    );
    assert_eq!(stored_jobs(&dir, "kb").len(), 3);

    // Rebased: the path (and its file effect) under <root>/<name>/<job>, the rest kept.
    let rebasing = offline(&dir).rebasing_jobs(true);
    let imported = rebasing
        .import(&ImportSource::new(&source), "kb2", None, None)
        .unwrap();
    let target = root.join("kb2").join("evil");
    assert_eq!(
        imported.jobs_outside[0].rebased_to.as_deref(),
        Some(target.as_path())
    );
    assert!(
        imported.notes.iter().any(|n| n.contains("rebased to")),
        "{:?}",
        imported.notes
    );
    let jobs = stored_jobs(&dir, "kb2");
    assert_eq!(jobs.len(), 3);
    let evil = jobs
        .iter()
        .find(|d| d.name() == "evil")
        .unwrap()
        .as_job()
        .unwrap();
    assert_eq!(
        evil.action(),
        &graphersal::catalog::JobTask::Backup {
            path: target.display().to_string()
        }
    );
    assert_eq!(evil.description(), "old machine");
    assert!(graphersal_serve::jobs_outside_file_root(&jobs, &root).is_empty());

    // A packed snapshot carries the jobs too; over HTTP with "rebaseJobs".
    let snapshot = temp.path().join("kb.gsnap");
    write_graph_snapshot(
        &Graph::new(graph_with_jobs(&outside, &root)),
        fs::File::create(&snapshot).unwrap(),
        &WriteOptions::new(),
        ModelCopy::Include,
    )
    .unwrap();
    let answer = import_route(
        &admin,
        json!({"path": snapshot.display().to_string(), "name": "kb3", "rebaseJobs": true}),
    );
    let jobs = &answer["import"]["jobsOutside"];
    assert_eq!(jobs.as_array().unwrap().len(), 1, "{answer}");
    assert_eq!(jobs[0]["job"], "evil");
    assert_eq!(jobs[0]["willRun"], true);
    assert_eq!(
        jobs[0]["rebasedTo"],
        root.join("kb3").join("evil").display().to_string()
    );
    let answer = import_route(
        &admin,
        json!({"path": snapshot.display().to_string(), "name": "kb4"}),
    );
    assert_eq!(
        answer["import"]["jobsOutside"][0]["willRun"], false,
        "{answer}"
    );
    assert_eq!(answer["import"]["jobsOutside"][0]["rebasedTo"], Value::Null);
    // A load drops the job that was not rebased, keeps the rebased one: the runner never
    // writes outside the root.
    let loader = Loader::with_backup_root(
        Registry::scan(&dir).unwrap(),
        HostCheck::new(Exposure::Loopback, 8484, "http://127.0.0.1:8484/"),
        None,
        Some(root.clone()),
    );
    for name in ["kb", "kb2"] {
        loader.load(name).unwrap();
        assert_eq!(loader.wait(name, WAIT).unwrap(), DbState::Loaded);
    }
    loader.shutdown().unwrap();
    let names = |name: &str| -> Vec<String> {
        let mut names: Vec<String> = stored_jobs(&dir, name)
            .iter()
            .map(|d| d.name().to_string())
            .collect();
        names.sort();
        names
    };
    assert_eq!(names("kb"), ["cp", "inside"]);
    assert_eq!(names("kb2"), ["cp", "evil", "inside"]);
    assert!(!outside.exists());
}

/// `POST /api/admin/import` with the JSON `body` over `admin`: the `201` answer.
fn import_route(admin: &Admin, body: Value) -> Value {
    use graphersal_serve::{Method, Request};
    let bytes = body.to_string().into_bytes();
    let length = bytes.len();
    let mut request = Request::new(
        Method::Post,
        "/api/admin/import",
        std::io::Cursor::new(bytes),
    )
    .with_header("Content-Type", "application/json")
    .with_body_length(Some(length));
    let response = crate::admin_api::route(admin, &Method::Post, "import", &mut request)
        .unwrap_or_else(|err| panic!("{}", err.message()));
    let answer: Value = serde_json::from_slice(response.body()).unwrap();
    assert_eq!(response.status(), 201, "{answer}");
    answer
}
