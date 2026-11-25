//! Tests of the two ways to a base database holding data: an import with role base
//! ([`Admin::import_as`]) and a promotion ([`Admin::promote`]), in the core (offline and
//! online) and through the admin HTTP API of a real listener on port 0. Synthetic data only
//! (the `modern` sample, a tiny synthetic model).

use std::fs;
use std::net::SocketAddr;
use std::time::Duration;

use graphersal::persist::{
    ImportModelOptions, ModelCopy, ModelSource, WriteOptions, write_graph_snapshot,
};
use graphersal::prelude::{Graph, GraphExport};
use graphersal::storage::BaseRef;
use graphersal_serve::{Exposure, HostCheck};
use serde_json::{Value, json};

use super::*;
use crate::database::STORE_DIR;
use crate::registry::DbState;
use crate::registry::tests::server;
use crate::serve::{Running, ServeOptions};
use crate::test_support::TempDir;

const WAIT: Duration = Duration::from_secs(120);
const WRITE: &str = r#"g.add_v("layer-own").iterate()"#;
const COUNT: &str = "g.v().count().next()";

fn offline(dir: &ServerDir) -> Admin {
    Admin::offline(Registry::scan(dir).expect("scan"))
}

fn online(dir: &ServerDir) -> (Arc<Loader>, Admin) {
    let loader = Loader::new(
        Registry::scan(dir).expect("scan"),
        HostCheck::new(Exposure::Loopback, 8484, "http://127.0.0.1:8484/"),
    );
    let admin = Admin::online(loader.clone());
    (loader, admin)
}

fn tmp_count(dir: &ServerDir) -> usize {
    fs::read_dir(dir.tmp()).map(|d| d.count()).unwrap_or(0)
}

/// The modern sample as GraphSON at `path`.
fn modern_graphson(path: &Path) {
    Graph::new(TraversalGraph::tinkerpop_modern())
        .export_graphson_writer(fs::File::create(path).unwrap())
        .unwrap();
}

/// Runs `script` on the loaded database `name`: `Ok(answer)` or `Err(error text)`.
fn run(loader: &Loader, name: &str, script: &'static str) -> Result<Value, String> {
    let context = loader.context(name).expect("loaded");
    let text = context
        .call(move |session| session.execute(script, &graphersal_session::RunOptions::default()))
        .expect("the session answers");
    let answer: Value = serde_json::from_str(&text).expect("JSON");
    if answer["ok"] == Value::Bool(false) {
        return Err(answer["execution"]["error"].to_string());
    }
    Ok(answer)
}

fn count(loader: &Loader, name: &str) -> i64 {
    run(loader, name, COUNT).expect("counts")["execution"]["results"][0]
        .as_i64()
        .expect("a count")
}

fn load(loader: &Arc<Loader>, name: &str) {
    loader.load(name).expect("starts");
    assert_eq!(loader.wait(name, WAIT).unwrap(), DbState::Loaded, "{name}");
}

/// The identity a layer binds to, read from the closed Store of `name`.
fn identity(dir: &ServerDir, name: &str) -> BaseRef {
    let info = Store::inspect(dir.databases().join(name).join(STORE_DIR)).expect("inspects");
    BaseRef::new(info.graph_id, info.commit_seq, info.last_commit_at)
}

fn config(entry: &DatabaseEntry) -> &DatabaseConfig {
    entry.config.as_ref().expect("a configuration")
}

/// A layer `layer` over the base `base`, loaded: it sees the base's 6 vertices and its own
/// writes; the base itself, loaded too, refuses a write and keeps its identity.
fn a_layer_sees_the_base(dir: &ServerDir, base: &str, layer: &str) {
    let before = identity(dir, base);
    let (loader, admin) = online(dir);
    let entry = admin
        .create(layer, &CreateSpec::layer(base))
        .expect("a layer");
    assert_eq!(config(&entry).base.as_deref(), Some(base));
    load(&loader, layer);
    assert_eq!(count(&loader, layer), 6);
    run(&loader, layer, WRITE).expect("the layer is writable");
    assert_eq!(count(&loader, layer), 7);
    load(&loader, base);
    assert_eq!(
        count(&loader, base),
        6,
        "the base does not see the layer's writes"
    );
    let err = run(&loader, base, WRITE).expect_err("a base refuses writes");
    assert!(err.contains("read-only"), "{err}");
    // Unloading and loading the layer again: its writes are kept, over the same base.
    assert!(loader.unload(layer).unwrap());
    load(&loader, layer);
    assert_eq!(count(&loader, layer), 7);
    loader.shutdown().unwrap();
    assert_eq!(identity(dir, base), before, "nothing wrote the base");
}

#[test]
fn an_import_with_role_base_makes_a_base_that_layers_see() {
    let temp = TempDir::new("promote-import");
    let dir = server(&temp);
    let admin = offline(&dir);
    let graphson = temp.path().join("modern.json");
    modern_graphson(&graphson);

    let imported = admin
        .import_as(
            &ImportSource::new(&graphson),
            "prod",
            None,
            Some("Production"),
            DatabaseRole::Base,
        )
        .expect("imported");
    let c = config(&imported.entry);
    assert_eq!(c.kind, DatabaseKind::Plain);
    assert_eq!(c.role, DatabaseRole::Base);
    assert!(c.read_only);
    assert_eq!(c.description, "Production");
    assert_eq!(imported.entry.state, DbState::Unloaded);
    // A scan reads it back as a base.
    let registry = Registry::scan(&dir).unwrap();
    assert_eq!(
        config(&registry.get("prod").unwrap()).role,
        DatabaseRole::Base
    );
    drop(registry);
    // A base refuses to become writable.
    assert!(matches!(
        admin.set_read_only("prod", false),
        Err(ServerError::Refused { .. })
    ));

    a_layer_sees_the_base(&dir, "prod", "l1");

    // A full snapshot and a plain Store become bases too.
    let admin = offline(&dir);
    let snapshot = temp.path().join("modern.gsnap");
    write_graph_snapshot(
        &Graph::new(TraversalGraph::tinkerpop_modern()),
        fs::File::create(&snapshot).unwrap(),
        &WriteOptions::new(),
        ModelCopy::Include,
    )
    .unwrap();
    let imported = admin
        .import_as(
            &ImportSource::new(&snapshot),
            "snap-base",
            None,
            None,
            DatabaseRole::Base,
        )
        .expect("imported");
    assert_eq!(config(&imported.entry).role, DatabaseRole::Base);
    let imported = admin
        .import_as(
            &ImportSource::new(dir.databases().join("prod").join(STORE_DIR)),
            "store-base",
            None,
            None,
            DatabaseRole::Base,
        )
        .expect("imported");
    assert_eq!(imported.format, ImportFormat::Store);
    assert_eq!(config(&imported.entry).role, DatabaseRole::Base);
    // The default role stays normal.
    let imported = admin
        .import_as(
            &ImportSource::new(&graphson),
            "plain",
            None,
            None,
            DatabaseRole::Normal,
        )
        .expect("imported");
    assert_eq!(config(&imported.entry).role, DatabaseRole::Normal);
    assert!(!config(&imported.entry).read_only);
}

#[test]
fn an_import_with_role_base_refuses_layers_and_missing_models() {
    let temp = TempDir::new("promote-import-refusals");
    let dir = server(&temp);
    let admin = offline(&dir);
    let graphson = temp.path().join("modern.json");
    modern_graphson(&graphson);
    admin
        .import_as(
            &ImportSource::new(&graphson),
            "prod",
            None,
            None,
            DatabaseRole::Base,
        )
        .unwrap();
    admin.create("l1", &CreateSpec::layer("prod")).unwrap();
    let layer_file = temp.path().join("l1.layer.gsnap");
    admin
        .export_to_file("l1", ExportSpec::new(), &layer_file)
        .unwrap();

    // A layer snapshot and a layer Store: 400 (InvalidInput), nothing left.
    for source in [
        ImportSource::new(&layer_file),
        ImportSource::new(dir.databases().join("l1").join(STORE_DIR)),
    ] {
        let err = admin
            .import_as(&source, "x", None, None, DatabaseRole::Base)
            .unwrap_err();
        assert!(matches!(err, ServerError::InvalidInput { .. }), "{err}");
        assert!(err.to_string().contains("not a whole graph"), "{err}");
    }
    // A base name for a plain source: refused as before.
    let err = admin
        .import_as(
            &ImportSource::new(&graphson),
            "x",
            Some("prod"),
            None,
            DatabaseRole::Base,
        )
        .unwrap_err();
    assert!(matches!(err, ServerError::InvalidInput { .. }), "{err}");

    // A snapshot naming a model it does not carry: refused (a base is never written).
    let entry = admin.create("acme", &CreateSpec::plain()).unwrap();
    let store = Store::open(entry.store_dir(), StoreOptions::new()).unwrap();
    store
        .import_model(
            ModelSource::Files(crate::test_models::tiny_model(1, "acme/tiny")),
            ImportModelOptions::new(),
        )
        .expect("the model");
    store.close().unwrap();
    let without = temp.path().join("without.gsnap");
    admin
        .export_to_file("acme", ExportSpec::new().with_models(false), &without)
        .unwrap();
    let err = admin
        .import_as(
            &ImportSource::new(&without),
            "x",
            None,
            None,
            DatabaseRole::Base,
        )
        .unwrap_err();
    assert!(matches!(err, ServerError::Refused { .. }), "{err}");
    assert!(err.to_string().contains("acme/tiny"), "{err}");
    // With its model it is a base.
    let with = temp.path().join("with.gsnap");
    admin
        .export_to_file("acme", ExportSpec::new(), &with)
        .unwrap();
    let imported = admin
        .import_as(
            &ImportSource::new(&with),
            "acme-base",
            None,
            None,
            DatabaseRole::Base,
        )
        .expect("imported");
    assert_eq!(config(&imported.entry).role, DatabaseRole::Base);

    assert!(!dir.databases().join("x").exists());
    assert_eq!(tmp_count(&dir), 0);
}

#[test]
fn a_promoted_database_is_a_base_that_layers_see() {
    let temp = TempDir::new("promote-core");
    let dir = server(&temp);
    let graphson = temp.path().join("modern.json");
    modern_graphson(&graphson);
    offline(&dir)
        .import(&ImportSource::new(&graphson), "acme", None, None)
        .unwrap();

    // Online: written while loaded, refused while loaded, promoted once unloaded.
    let (loader, admin) = online(&dir);
    load(&loader, "acme");
    run(&loader, "acme", r#"g.add_v("extra").iterate()"#).unwrap();
    run(
        &loader,
        "acme",
        r#"g.v().has_label("extra").drop().iterate()"#,
    )
    .unwrap();
    let err = admin.promote("acme").unwrap_err();
    assert!(matches!(err, ServerError::Refused { .. }), "{err}");
    assert!(err.to_string().contains("unload it first"), "{err}");
    assert!(loader.unload("acme").unwrap());
    let entry = admin.promote("acme").expect("promoted");
    assert_eq!(config(&entry).role, DatabaseRole::Base);
    assert_eq!(config(&entry).kind, DatabaseKind::Plain);
    assert!(config(&entry).read_only);
    // One way: promoting again, or making it writable, is refused.
    let err = admin.promote("acme").unwrap_err();
    assert!(err.to_string().contains("a base already"), "{err}");
    assert!(matches!(
        admin.set_read_only("acme", false),
        Err(ServerError::Refused { .. })
    ));
    loader.shutdown().unwrap();
    drop(admin);
    // Persisted in database.toml.
    let registry = Registry::scan(&dir).unwrap();
    let entry = registry.get("acme").unwrap();
    assert_eq!(config(&entry).role, DatabaseRole::Base);
    assert!(config(&entry).read_only);
    drop(registry);

    a_layer_sees_the_base(&dir, "acme", "l1");

    // Offline too.
    let admin = offline(&dir);
    admin
        .import(&ImportSource::new(&graphson), "other", None, None)
        .unwrap();
    let entry = admin.promote("other").expect("promoted offline");
    assert_eq!(config(&entry).role, DatabaseRole::Base);
}

#[test]
fn a_promotion_refuses_layers_bases_and_databases_that_are_not_unloaded() {
    let temp = TempDir::new("promote-refusals");
    let dir = server(&temp);
    let admin = offline(&dir);
    admin
        .create("base", &CreateSpec::plain().with_role(DatabaseRole::Base))
        .unwrap();
    admin.create("l1", &CreateSpec::layer("base")).unwrap();
    for (name, says) in [("base", "a base already"), ("l1", "a layer cannot")] {
        let err = admin.promote(name).unwrap_err();
        assert!(matches!(err, ServerError::Refused { .. }), "{name}: {err}");
        assert!(err.to_string().contains(says), "{name}: {err}");
    }
    assert!(matches!(
        admin.promote("missing"),
        Err(ServerError::NotFound { .. })
    ));
    // A needs-model database: add the model first.
    let entry = admin.create("acme", &CreateSpec::plain()).unwrap();
    let store = Store::open(entry.store_dir(), StoreOptions::new()).unwrap();
    store
        .import_model(
            ModelSource::Files(crate::test_models::tiny_model(1, "acme/tiny")),
            ImportModelOptions::new(),
        )
        .unwrap();
    store.close().unwrap();
    let without = temp.path().join("without.gsnap");
    admin
        .export_to_file("acme", ExportSpec::new().with_models(false), &without)
        .unwrap();
    admin
        .import(&ImportSource::new(&without), "waiting", None, None)
        .unwrap();
    let err = admin.promote("waiting").unwrap_err();
    assert!(err.to_string().contains("acme/tiny"), "{err}");
    // A database whose Store is a layer store although its database.toml says plain.
    let l1_store = dir.databases().join("l1").join(STORE_DIR);
    let fake = admin.create("fake", &CreateSpec::plain()).unwrap();
    fs::remove_dir_all(fake.store_dir()).unwrap();
    Store::convert(&l1_store, fake.store_dir()).unwrap();
    let err = admin.promote("fake").unwrap_err();
    assert!(err.to_string().contains("layer store"), "{err}");
    // Nothing changed.
    let registry = Registry::scan(&dir).unwrap();
    for name in ["l1", "waiting", "fake"] {
        assert_eq!(
            config(&registry.get(name).unwrap()).role,
            DatabaseRole::Normal
        );
    }

    // Online: a database being loaded is refused too (while_idle).
    let (loader, admin) = online(&dir);
    load(&loader, "acme");
    let err = admin.promote("acme").unwrap_err();
    assert!(err.to_string().contains("unload it first"), "{err}");
    loader.shutdown().unwrap();
}

// --- over a real listener ----------------------------------------------------------------

fn start(dir: &ServerDir) -> Running {
    crate::serve::start(
        dir.root(),
        ServeOptions::new().with_listen("127.0.0.1:0".parse().unwrap()),
    )
    .expect("starts")
}

fn call(address: SocketAddr, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    crate::client::request(
        address,
        crate::test_support::operator_token(address).as_deref(),
        method,
        path,
        body.as_ref(),
    )
    .expect("answers")
}

/// `POST path` with `body` as an upload (not JSON).
fn upload(address: SocketAddr, path: &str, body: &[u8]) -> (u16, Value) {
    use std::io::{BufRead, BufReader, Read, Write};
    let mut stream = std::net::TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(120)))
        .unwrap();
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n{}Content-Type: \
         application/octet-stream\r\nContent-Length: {}\r\n\r\n",
        crate::test_support::admin_header(address),
        body.len()
    );
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line).unwrap();
    let status: u16 = status_line.split(' ').nth(1).unwrap().parse().unwrap();
    let mut length = 0;
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
            length = value.trim().parse::<usize>().unwrap();
        }
    }
    let mut answer = vec![0; length];
    reader.read_exact(&mut answer).unwrap();
    (
        status,
        serde_json::from_slice(&answer).unwrap_or(Value::Null),
    )
}

fn wait_loaded(address: SocketAddr, name: &str) -> Value {
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let (status, entry) = call(
            address,
            "GET",
            &format!("/api/admin/databases/{name}"),
            None,
        );
        assert_eq!(status, 200, "{entry}");
        if entry["state"] != "loading" || std::time::Instant::now() > deadline {
            return entry;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn execute(address: SocketAddr, name: &str, script: &str) -> Value {
    let (status, answer) = call(
        address,
        "POST",
        &format!("/db/{name}/api/execute"),
        Some(json!({ "script": script })),
    );
    assert_eq!(status, 200, "{answer}");
    answer
}

fn http_count(address: SocketAddr, name: &str) -> i64 {
    let answer = execute(address, name, COUNT);
    answer["execution"]["results"][0]
        .as_i64()
        .unwrap_or_else(|| panic!("a count: {answer}"))
}

#[test]
fn bases_by_import_and_by_promotion_over_http() {
    let temp = TempDir::new("promote-http");
    let dir = server(&temp);
    let graphson = temp.path().join("modern.json");
    modern_graphson(&graphson);
    let running = start(&dir);
    let address = running.address();

    // Upload with role=base.
    let bytes = fs::read(&graphson).unwrap();
    let (status, entry) = upload(address, "/api/admin/import?name=up&role=base", &bytes);
    assert_eq!(status, 201, "{entry}");
    assert_eq!(entry["role"], "base");
    assert_eq!(entry["readOnly"], true);
    assert_eq!(entry["kind"], "plain");
    assert_eq!(entry["import"]["format"], "graphson");
    // An unknown role: 400 before the body is read.
    let (status, answer) = upload(address, "/api/admin/import?name=x&role=boss", &bytes);
    assert_eq!(status, 400, "{answer}");
    // The JSON path form.
    let path = graphson.to_str().unwrap();
    let (status, entry) = call(
        address,
        "POST",
        "/api/admin/import",
        Some(json!({"path": path, "name": "by-path", "role": "base"})),
    );
    assert_eq!(status, 201, "{entry}");
    assert_eq!(entry["role"], "base");
    let (status, answer) = call(
        address,
        "POST",
        "/api/admin/import",
        Some(json!({"path": path, "name": "x", "role": "boss"})),
    );
    assert_eq!(status, 400, "{answer}");
    // A layer source with role=base: 400.
    let (status, _) = call(
        address,
        "POST",
        "/api/admin/databases",
        Some(json!({"name": "l0", "base": "up"})),
    );
    assert_eq!(status, 201);
    let layer_store = dir.databases().join("l0").join(STORE_DIR);
    let (status, answer) = call(
        address,
        "POST",
        "/api/admin/import",
        Some(json!({"path": layer_store.to_str().unwrap(), "name": "x", "role": "base"})),
    );
    assert_eq!(status, 400, "{answer}");
    assert!(!dir.databases().join("x").exists());

    // Promotion: a plain database with data, loaded -> 409 with the unload help.
    let (status, _) = call(
        address,
        "POST",
        "/api/admin/import",
        Some(json!({"path": path, "name": "acme"})),
    );
    assert_eq!(status, 201);
    call(address, "POST", "/api/admin/databases/acme/load", None);
    assert_eq!(wait_loaded(address, "acme")["state"], "loaded");
    let (status, answer) = call(address, "POST", "/api/admin/databases/acme/promote", None);
    assert_eq!(status, 409, "{answer}");
    let help = answer["help"].as_str().unwrap_or_default();
    assert!(
        help.contains("POST /api/admin/databases/acme/unload"),
        "{answer}"
    );
    let (status, _) = call(address, "POST", "/api/admin/databases/acme/unload", None);
    assert_eq!(status, 200);
    let (status, entry) = call(address, "POST", "/api/admin/databases/acme/promote", None);
    assert_eq!(status, 200, "{entry}");
    assert_eq!(entry["role"], "base");
    assert_eq!(entry["readOnly"], true);
    // Again (a base), a layer, an unknown database, GET.
    let (status, answer) = call(address, "POST", "/api/admin/databases/acme/promote", None);
    assert_eq!(status, 409, "{answer}");
    let (status, answer) = call(address, "POST", "/api/admin/databases/l0/promote", None);
    assert_eq!(status, 409, "{answer}");
    assert!(
        answer["help"]
            .as_str()
            .unwrap_or_default()
            .contains("materialize=true"),
        "{answer}"
    );
    let (status, _) = call(address, "POST", "/api/admin/databases/nope/promote", None);
    assert_eq!(status, 404);
    let (status, _) = call(address, "GET", "/api/admin/databases/acme/promote", None);
    assert_eq!(status, 405);

    // A layer over the promoted base sees its data and its own writes.
    let (status, entry) = call(
        address,
        "POST",
        "/api/admin/databases",
        Some(json!({"name": "l1", "base": "acme"})),
    );
    assert_eq!(status, 201, "{entry}");
    call(address, "POST", "/api/admin/databases/l1/load", None);
    assert_eq!(wait_loaded(address, "l1")["state"], "loaded");
    assert_eq!(http_count(address, "l1"), 6);
    assert_eq!(execute(address, "l1", WRITE)["ok"], true);
    assert_eq!(http_count(address, "l1"), 7);
    // And a layer over the imported base.
    call(address, "POST", "/api/admin/databases/l0/load", None);
    assert_eq!(wait_loaded(address, "l0")["state"], "loaded");
    assert_eq!(http_count(address, "l0"), 6);
    running.stop().unwrap();
}
