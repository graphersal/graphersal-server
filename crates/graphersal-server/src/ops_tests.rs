//! Tests of the admin core: every operation directly ([`Admin`] offline and online) and through
//! the admin HTTP API of a real listener on port 0. Synthetic stores only (empty databases
//! written through the core, data added through the served sessions).

use std::fs;
use std::net::SocketAddr;
use std::time::Duration;

use graphersal::persist::DamagePolicy;
use graphersal_serve::{Exposure, HostCheck};
use serde_json::{Value, json};

use super::*;
use crate::database::STORE_DIR;
use crate::registry::DbState;
use crate::registry::tests::server;
use crate::serve::{Running, ServeOptions};
use crate::test_support::TempDir;

const WAIT: Duration = Duration::from_secs(120);
const WRITE: &str = r#"g.add_v("person").iterate()"#;

fn schema() -> Value {
    json!({"vertices": {"person": {"schema": {"type": "object", "properties": {
        "name": {"type": "string"}}}}}})
}

/// The vertex count of the closed plain Store of `name`.
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

fn offline(temp: &TempDir) -> (ServerDir, Admin) {
    let dir = server(temp);
    let admin = Admin::offline(Registry::scan(&dir).expect("scan"));
    (dir, admin)
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
    let context = loader.context(name).expect("loaded");
    let info = context
        .call(|session| session.info().map_err(String::from))
        .expect("answers")
        .expect("info");
    info["vertexCount"].as_i64().expect("a count")
}

fn load(loader: &Arc<Loader>, name: &str) -> DbState {
    loader.load(name).expect("starts");
    loader.wait(name, WAIT).expect("state")
}

#[test]
fn create_writes_a_closed_store_with_the_defaults_and_the_schema() {
    let temp = TempDir::new("ops-create");
    let dir = server(&temp);
    fs::write(
        dir.config_path(),
        "[defaults]\nchunk_bytes = 4096\ndamage_policy = \"continue\"\n",
    )
    .unwrap();
    let admin = Admin::offline(Registry::scan(&dir).unwrap());
    let entry = admin
        .create(
            "acme",
            &CreateSpec::plain()
                .with_schema(schema())
                .with_description("Acme")
                .with_auto_load(true),
        )
        .expect("created");
    assert_eq!(entry.state, DbState::Unloaded);
    let config = entry.config.clone().unwrap();
    assert!(config.auto_load && !config.read_only);
    assert_eq!(config.description, "Acme");
    assert!(config.created.is_some());
    let info = Store::inspect(entry.store_dir()).expect("inspect");
    assert!(info.clean);
    assert_eq!(info.chunk_bytes, 4096);
    assert_eq!(info.damage_policy, DamagePolicy::Continue);
    let store = Store::open(entry.store_dir(), StoreOptions::new()).unwrap();
    let schema = store
        .graph()
        .read()
        .get_schema()
        .unwrap()
        .expect("a schema");
    assert!(schema.to_json().contains("person"), "{}", schema.to_json());
    assert_eq!(store.graph().read().vertex_count(), 0);
    store.close().unwrap();
    assert_eq!(tmp_count(&dir), 0);
    // The registry and a rescan agree.
    let again = Registry::scan(&dir).unwrap();
    assert_eq!(again.get("acme").unwrap(), entry);
    // A base is created read-only.
    let base = admin
        .create("base", &CreateSpec::plain().with_role(DatabaseRole::Base))
        .unwrap();
    assert!(base.config.unwrap().read_only);
}

#[test]
fn create_refusals_leave_nothing_behind() {
    let temp = TempDir::new("ops-create-refused");
    let (dir, admin) = offline(&temp);
    admin.create("acme", &CreateSpec::plain()).unwrap();
    type Check = fn(&ServerError) -> bool;
    let cases: Vec<(&str, CreateSpec, Check)> = vec![
        ("Bad Name", CreateSpec::plain(), |e| {
            matches!(e, ServerError::InvalidName { .. })
        }),
        ("acme", CreateSpec::plain(), |e| {
            matches!(e, ServerError::NameInUse { .. })
        }),
        (
            "x",
            CreateSpec::plain().with_schema(json!({"vertices": 5})),
            |e| matches!(e, ServerError::InvalidInput { .. }),
        ),
        ("x", CreateSpec::layer("acme"), |e| {
            matches!(e, ServerError::Refused { .. }) && e.to_string().contains("not a base")
        }),
        ("x", CreateSpec::layer("gone"), |e| {
            matches!(e, ServerError::Refused { .. })
        }),
    ];
    for (name, spec, check) in cases {
        let err = admin.create(name, &spec).expect_err(name);
        assert!(check(&err), "{name}: {err}");
    }
    assert_eq!(tmp_count(&dir), 0);
    assert!(!dir.databases().join("x").exists());
    assert_eq!(admin.registry().list().len(), 1);
}

#[test]
fn a_failure_or_a_crash_mid_create_leaves_no_database() {
    let temp = TempDir::new("ops-create-atomic");
    let (dir, mut admin) = offline(&temp);
    admin.fail = Some(FailPoint::AfterStore);
    let err = admin.create("acme", &CreateSpec::plain()).unwrap_err();
    assert!(err.to_string().contains("injected failure"), "{err}");
    assert_eq!(tmp_count(&dir), 0, "the dropped build is removed");
    assert!(!dir.databases().join("acme").exists());
    assert!(admin.registry().get("acme").is_err());

    admin.fail = Some(FailPoint::CrashAfterStore);
    admin.create("acme", &CreateSpec::plain()).unwrap_err();
    assert_eq!(tmp_count(&dir), 1, "a crash leaves its build in tmp/");
    let debris = fs::read_dir(dir.tmp())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(debris.join(STORE_DIR).is_dir(), "the Store was written");
    assert!(!dir.databases().join("acme").exists());
    // The next scan (a restart) removes the debris; the name is free.
    let registry = Registry::scan(&dir).unwrap();
    assert_eq!(tmp_count(&dir), 0);
    assert!(registry.list().is_empty());
    let admin = Admin::offline(registry);
    admin
        .create("acme", &CreateSpec::plain())
        .expect("the name is free");
}

#[test]
fn layers_are_created_over_bases_offline_and_online() {
    let temp = TempDir::new("ops-layers");
    let (dir, admin) = offline(&temp);
    admin
        .create("base", &CreateSpec::plain().with_role(DatabaseRole::Base))
        .unwrap();
    // Offline: the base is read from its Store.
    let layer = admin
        .create("l1", &CreateSpec::layer("base").with_schema(schema()))
        .expect("a layer");
    let config = layer.config.clone().unwrap();
    assert_eq!(config.kind, DatabaseKind::Layer);
    assert_eq!(config.base.as_deref(), Some("base"));
    assert!(
        Store::inspect(layer.store_dir())
            .unwrap()
            .layer_base
            .is_some()
    );
    // A layer is never a base.
    let err = admin
        .create(
            "bad",
            &CreateSpec::layer("base").with_role(DatabaseRole::Base),
        )
        .unwrap_err();
    assert!(matches!(err, ServerError::Refused { .. }), "{err}");

    // Online with the base in the pool (l1 loaded): the pooled base carries the new layer.
    let (loader, admin) = online(&dir);
    assert_eq!(load(&loader, "l1"), DbState::Loaded);
    assert!(loader.pooled_base("base").is_some());
    admin.create("l2", &CreateSpec::layer("base")).expect("l2");
    assert_eq!(load(&loader, "l2"), DbState::Loaded);
    run(&loader, "l2", WRITE).expect("a layer writes");
    assert_eq!(count(&loader, "l2"), 1);
    assert_eq!(count(&loader, "l1"), 0);
    loader.shutdown().unwrap();
}

#[test]
fn delete_moves_an_unloaded_database_to_the_trash() {
    let temp = TempDir::new("ops-delete");
    let (dir, admin) = offline(&temp);
    admin.create("acme", &CreateSpec::plain()).unwrap();
    admin
        .create("base", &CreateSpec::plain().with_role(DatabaseRole::Base))
        .unwrap();
    admin.create("l1", &CreateSpec::layer("base")).unwrap();
    admin.alias_set("current", "acme").unwrap();

    // Refused: named by an alias; the base of a layer; unknown; an alias's name.
    let err = admin.delete("acme").unwrap_err();
    assert!(err.to_string().contains("current"), "{err}");
    let err = admin.delete("base").unwrap_err();
    assert!(err.to_string().contains("l1"), "{err}");
    assert!(matches!(
        admin.delete("nope").unwrap_err(),
        ServerError::NotFound { .. }
    ));
    assert!(matches!(
        admin.delete("current").unwrap_err(),
        ServerError::NotFound { .. }
    ));

    admin.alias_remove("current").unwrap();
    let deleted = admin.delete("ACME").expect("deleted");
    assert_eq!(deleted.name, "acme");
    assert!(deleted.trash.starts_with(dir.trash()));
    let file_name = deleted.trash.file_name().unwrap().to_str().unwrap();
    assert!(file_name.starts_with("acme-20"), "{file_name}");
    assert!(!file_name.contains(':'), "{file_name}");
    assert!(deleted.trash.join(STORE_DIR).is_dir());
    assert!(!dir.databases().join("acme").exists());
    assert!(admin.registry().get("acme").is_err());
    // The name is free again; a second delete of the same name gets its own trash directory.
    admin.create("acme", &CreateSpec::plain()).unwrap();
    let again = admin.delete("acme").unwrap();
    assert_ne!(again.trash, deleted.trash);
    // A layer, then its base.
    admin.delete("l1").unwrap();
    admin.delete("base").unwrap();
    assert!(admin.registry().list().is_empty());
}

#[test]
fn delete_of_a_loaded_database_is_refused() {
    let temp = TempDir::new("ops-delete-loaded");
    let dir = server(&temp);
    Admin::offline(Registry::scan(&dir).unwrap())
        .create("acme", &CreateSpec::plain())
        .unwrap();
    let (loader, admin) = online(&dir);
    assert_eq!(load(&loader, "acme"), DbState::Loaded);
    let err = admin.delete("acme").unwrap_err();
    assert!(matches!(err, ServerError::Refused { .. }), "{err}");
    assert!(err.to_string().contains("unload it first"), "{err}");
    assert!(dir.databases().join("acme").is_dir());
    assert!(admin.unload("acme").unwrap());
    admin.delete("acme").expect("deleted once unloaded");
    assert!(matches!(
        loader.load("acme").unwrap_err(),
        ServerError::NotFound { .. }
    ));
}

#[test]
fn clone_of_a_loaded_database_is_independent() {
    let temp = TempDir::new("ops-clone");
    let dir = server(&temp);
    Admin::offline(Registry::scan(&dir).unwrap())
        .create(
            "acme",
            &CreateSpec::plain()
                .with_description("Acme")
                .with_auto_load(true),
        )
        .unwrap();
    let (loader, admin) = online(&dir);
    assert_eq!(load(&loader, "acme"), DbState::Loaded);
    run(&loader, "acme", WRITE).unwrap();

    let clone = admin.clone_database("acme", "acme-copy").expect("cloned");
    let config = clone.config.clone().unwrap();
    assert_eq!(config.description, "Acme");
    assert!(!config.auto_load, "a clone is not loaded at start");
    assert_eq!(clone.state, DbState::Unloaded);
    assert_eq!(tmp_count(&dir), 0);

    // Writes after the clone stay where they were made.
    run(&loader, "acme", WRITE).unwrap();
    assert_eq!(load(&loader, "acme-copy"), DbState::Loaded);
    assert_eq!(count(&loader, "acme-copy"), 1);
    run(&loader, "acme-copy", WRITE).unwrap();
    run(&loader, "acme-copy", WRITE).unwrap();
    assert_eq!(count(&loader, "acme"), 2);
    assert_eq!(count(&loader, "acme-copy"), 3);
    loader.shutdown().unwrap();
    assert_eq!(plain_count(&dir, "acme"), 2);
    assert_eq!(plain_count(&dir, "acme-copy"), 3);
    // Another store.
    let a = Store::inspect(dir.databases().join("acme").join(STORE_DIR)).unwrap();
    let b = Store::inspect(dir.databases().join("acme-copy").join(STORE_DIR)).unwrap();
    assert_ne!(a.store_id, b.store_id);

    let err = admin.clone_database("acme", "acme-copy").unwrap_err();
    assert!(matches!(err, ServerError::NameInUse { .. }), "{err}");
    let err = admin.clone_database("nope", "x").unwrap_err();
    assert!(matches!(err, ServerError::NotFound { .. }), "{err}");
}

#[test]
fn clones_of_unloaded_layers_and_bases_keep_their_kind() {
    let temp = TempDir::new("ops-clone-layer");
    let (dir, admin) = offline(&temp);
    admin
        .create("base", &CreateSpec::plain().with_role(DatabaseRole::Base))
        .unwrap();
    admin.create("l1", &CreateSpec::layer("base")).unwrap();
    let clone = admin
        .clone_database("l1", "l1-copy")
        .expect("a layer clone");
    let config = clone.config.clone().unwrap();
    assert_eq!(config.kind, DatabaseKind::Layer);
    assert_eq!(config.base.as_deref(), Some("base"));
    assert!(
        Store::inspect(clone.store_dir())
            .unwrap()
            .layer_base
            .is_some()
    );
    let base_clone = admin.clone_database("base", "base-2").unwrap();
    let config = base_clone.config.unwrap();
    assert_eq!(config.role, DatabaseRole::Base);
    assert!(config.read_only);
    // Served online: the cloned layer loads over its base.
    let (loader, _admin) = online(&dir);
    assert_eq!(load(&loader, "l1-copy"), DbState::Loaded);
    loader.shutdown().unwrap();
}

#[test]
fn flags_aliases_and_offline_refusals() {
    let temp = TempDir::new("ops-flags");
    let (_dir, admin) = offline(&temp);
    admin.create("acme", &CreateSpec::plain()).unwrap();
    admin
        .create("base", &CreateSpec::plain().with_role(DatabaseRole::Base))
        .unwrap();
    assert!(!admin.is_online());
    let err = admin.load("acme").unwrap_err();
    assert!(err.to_string().contains("serve --dir"), "{err}");
    assert!(admin.unload("acme").is_err());
    let entry = admin.set_read_only("acme", true).unwrap();
    assert!(entry.config.unwrap().read_only);
    let entry = admin.set_auto_load("acme", true).unwrap();
    assert!(entry.config.unwrap().auto_load);
    let err = admin.set_read_only("base", false).unwrap_err();
    assert!(err.to_string().contains("always served read-only"), "{err}");
    admin.alias_set("a", "acme").unwrap();
    assert_eq!(admin.registry().aliases()["a"], "acme");
    admin.alias_remove("a").unwrap();
    assert!(admin.registry().aliases().is_empty());
}

// ---------------------------------------------------------------------------------------------
// The admin HTTP API over a real listener
// ---------------------------------------------------------------------------------------------

fn start(dir: &ServerDir) -> Running {
    crate::serve::start(
        dir.root(),
        ServeOptions::new().with_listen("127.0.0.1:0".parse().unwrap()),
    )
    .expect("the server starts")
}

fn call(address: SocketAddr, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    crate::client::request(
        address,
        crate::test_support::operator_token(address).as_deref(),
        method,
        path,
        body.as_ref(),
    )
    .expect("an answer")
}

/// Polls the database until its load ended.
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

fn post_create(address: SocketAddr, body: Value) -> (u16, Value) {
    call(address, "POST", "/api/admin/databases", Some(body))
}

#[test]
fn every_admin_action_over_http() {
    let temp = TempDir::new("ops-http");
    let dir = server(&temp);
    let running = start(&dir);
    let address = running.address();

    // Create: plain with a schema, a base, a layer; refusals.
    let (status, entry) = post_create(
        address,
        json!({"name": "acme", "description": "Acme", "schema": schema()}),
    );
    assert_eq!(status, 201, "{entry}");
    assert_eq!(entry["name"], "acme");
    assert_eq!(entry["state"], "unloaded");
    assert_eq!(entry["kind"], "plain");
    assert_eq!(entry["description"], "Acme");
    let (status, entry) = post_create(address, json!({"name": "base", "role": "base"}));
    assert_eq!(status, 201, "{entry}");
    assert_eq!(entry["readOnly"], true);
    let (status, entry) = post_create(address, json!({"name": "l1", "base": "base"}));
    assert_eq!(status, 201, "{entry}");
    assert_eq!(entry["kind"], "layer");
    assert_eq!(entry["base"], "base");
    for (body, expected) in [
        (json!({"name": "l2", "base": "acme"}), 409),
        (json!({"name": "acme"}), 409),
        (json!({"name": "Bad"}), 400),
        (json!({"name": "x", "schema": {"vertices": 1}}), 400),
        (json!({"name": "x", "kind": "other"}), 400),
        (json!({"name": "x", "colour": "red"}), 400),
        (json!({"nom": "x"}), 400),
    ] {
        let (status, answer) = post_create(address, body.clone());
        assert_eq!(status, expected, "{body}: {answer}");
        assert!(answer["error"].is_string(), "{answer}");
    }
    assert!(!dir.databases().join("x").exists());

    // The listing.
    let (_, listing) = call(address, "GET", "/api/admin/databases", None);
    let names: Vec<&str> = listing["databases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["acme", "base", "l1"]);

    // Load (answered at once), then read-only refuses a write on the database's API.
    let (status, entry) = call(address, "POST", "/api/admin/databases/acme/load", None);
    assert!(status == 202 || status == 200, "{status} {entry}");
    assert_eq!(wait_loaded(address, "acme")["state"], "loaded");
    let (status, entry) = call(address, "POST", "/api/admin/databases/acme/load", None);
    assert_eq!(status, 200, "{entry}");
    assert_eq!(entry["load"], "already-loaded");
    let write = json!({"script": WRITE});
    let (status, answer) = call(address, "POST", "/db/acme/api/execute", Some(write.clone()));
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["ok"], true, "{answer}");
    let (status, entry) = call(
        address,
        "PUT",
        "/api/admin/databases/acme/read-only",
        Some(json!({"readOnly": true})),
    );
    assert_eq!(status, 200, "{entry}");
    assert_eq!(entry["readOnly"], true);
    let (status, answer) = call(address, "POST", "/db/acme/api/execute", Some(write.clone()));
    let refused = status >= 400 || answer["ok"] == false;
    assert!(refused, "a write under read-only: {status} {answer}");
    assert!(answer.to_string().contains("read-only"), "{answer}");
    let (status, _) = call(
        address,
        "PUT",
        "/api/admin/databases/acme/read-only",
        Some(json!({"readOnly": false})),
    );
    assert_eq!(status, 200);
    let (_, answer) = call(address, "POST", "/db/acme/api/execute", Some(write));
    assert_eq!(answer["ok"], true, "writable again: {answer}");
    let (status, answer) = call(
        address,
        "PUT",
        "/api/admin/databases/base/read-only",
        Some(json!({"readOnly": false})),
    );
    assert_eq!(status, 409, "{answer}");
    let (status, _) = call(
        address,
        "PUT",
        "/api/admin/databases/acme/read-only",
        Some(json!({"readOnly": "yes"})),
    );
    assert_eq!(status, 400);

    // Clone of the loaded database; delete of a loaded one refused with help.
    let (status, entry) = call(
        address,
        "POST",
        "/api/admin/databases/acme/clone",
        Some(json!({"name": "acme-copy"})),
    );
    assert_eq!(status, 201, "{entry}");
    assert_eq!(entry["description"], "Acme");
    let (status, answer) = call(address, "DELETE", "/api/admin/databases/acme", None);
    assert_eq!(status, 409, "{answer}");
    let help = answer["help"].as_str().unwrap_or_default();
    assert!(
        help.contains("/api/admin/databases/acme/unload"),
        "{answer}"
    );

    // Auto-load, aliases.
    let (status, entry) = call(
        address,
        "PUT",
        "/api/admin/databases/acme-copy/auto-load",
        Some(json!({"autoLoad": true})),
    );
    assert_eq!(status, 200, "{entry}");
    assert_eq!(entry["autoLoad"], true);
    let (status, answer) = call(
        address,
        "PUT",
        "/api/admin/aliases/current",
        Some(json!({"target": "acme-copy"})),
    );
    assert_eq!(status, 200, "{answer}");
    assert_eq!(
        answer,
        json!({"alias": "current", "target": "acme-copy", "mcpExposed": false})
    );
    let (_, aliases) = call(address, "GET", "/api/admin/aliases", None);
    assert_eq!(aliases["aliases"], json!({"current": "acme-copy"}));
    assert!(aliases["revision"].is_string(), "{aliases}");
    let (status, answer) = call(address, "DELETE", "/api/admin/databases/acme-copy", None);
    assert_eq!(status, 409, "{answer}");
    let help = answer["help"].as_str().unwrap_or_default();
    assert!(help.contains("aliases/current"), "{answer}");
    let (status, answer) = call(
        address,
        "PUT",
        "/api/admin/aliases/x",
        Some(json!({"target": "nope"})),
    );
    assert_eq!(status, 404, "{answer}");
    let (status, _) = call(address, "DELETE", "/api/admin/aliases/current", None);
    assert_eq!(status, 200);
    let (status, _) = call(address, "DELETE", "/api/admin/aliases/current", None);
    assert_eq!(status, 404);

    // Unload, then delete into the trash.
    let (status, entry) = call(address, "POST", "/api/admin/databases/acme/unload", None);
    assert_eq!(status, 200, "{entry}");
    assert_eq!(entry["unloaded"], true);
    assert_eq!(entry["state"], "unloaded");
    let (status, entry) = call(address, "POST", "/api/admin/databases/acme/unload", None);
    assert_eq!(status, 200);
    assert_eq!(entry["unloaded"], false);
    let (status, answer) = call(address, "DELETE", "/api/admin/databases/acme", None);
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["name"], "acme");
    let trash = answer["trash"].as_str().unwrap_or_default();
    assert!(trash.starts_with("trash/acme-"), "{answer}");
    let (status, answer) = call(address, "GET", "/db/acme/api/info", None);
    assert_eq!(status, 404, "{answer}");

    // Unknown names, endpoints and methods.
    for (method, path, expected) in [
        ("POST", "/api/admin/databases/nope/load", 404),
        ("DELETE", "/api/admin/databases/nope", 404),
        ("POST", "/api/admin/databases/acme-copy/explode", 404),
        ("GET", "/api/admin/databases/acme-copy/load", 405),
        ("PUT", "/api/admin/databases", 405),
        ("POST", "/api/admin/aliases", 405),
    ] {
        let (status, answer) = call(address, method, path, None);
        assert_eq!(status, expected, "{method} {path}: {answer}");
    }
    // A body without the JSON content type.
    let (status, _) = call(
        address,
        "PUT",
        "/api/admin/databases/acme-copy/auto-load",
        None,
    );
    assert_eq!(status, 415);
    running.stop().unwrap();
}

#[test]
fn an_alias_is_renamed_over_http() {
    let temp = TempDir::new("ops-http-rename");
    let dir = server(&temp);
    let running = start(&dir);
    let address = running.address();
    for name in ["acme", "other"] {
        let (status, entry) = post_create(address, json!({ "name": name }));
        assert_eq!(status, 201, "{entry}");
    }
    let (status, _) = call(
        address,
        "PUT",
        "/api/admin/aliases/current",
        Some(json!({"target": "acme"})),
    );
    assert_eq!(status, 200);
    let rename = |alias: &str, body: Value| {
        call(
            address,
            "POST",
            &format!("/api/admin/aliases/{alias}/rename"),
            Some(body),
        )
    };

    let (status, answer) = rename("CURRENT", json!({"name": "live"}));
    assert_eq!(status, 200, "{answer}");
    assert_eq!(
        answer,
        json!({"alias": "live", "renamedFrom": "current", "target": "acme", "mcpSessionsMoved": 0,
               "grantsMoved": 0})
    );
    let (_, aliases) = call(address, "GET", "/api/admin/aliases", None);
    assert_eq!(aliases["aliases"], json!({"live": "acme"}));
    assert!(aliases["revision"].is_string(), "{aliases}");
    let (status, _) = call(address, "GET", "/db/current/api/info", None);
    assert_eq!(status, 404);

    // Refusals, each with its status and help where there is one.
    let (status, answer) = rename("live", json!({"name": "other"}));
    assert_eq!(status, 409, "{answer}");
    assert!(
        answer["help"]
            .as_str()
            .unwrap_or_default()
            .contains("choose another name")
    );
    let (status, answer) = rename("live", json!({"name": "live"}));
    assert_eq!(status, 409, "{answer}");
    let (status, answer) = rename("acme", json!({"name": "x"}));
    assert_eq!(status, 409, "{answer}");
    let help = answer["help"].as_str().unwrap_or_default();
    assert!(help.contains("databases have no rename"), "{answer}");
    let (status, answer) = rename("current", json!({"name": "x"}));
    assert_eq!(status, 404, "{answer}");
    assert!(answer["help"].is_string(), "{answer}");
    let (status, answer) = rename("live", json!({"name": "Bad Name"}));
    assert_eq!(status, 400, "{answer}");
    let (status, answer) = rename("live", json!({"target": "x"}));
    assert_eq!(status, 400, "{answer}");
    let (status, _) = call(address, "GET", "/api/admin/aliases/live/rename", None);
    assert_eq!(status, 405);
    let (_, aliases) = call(address, "GET", "/api/admin/aliases", None);
    assert_eq!(aliases["aliases"], json!({"live": "acme"}));
    assert!(aliases["revision"].is_string(), "{aliases}");
    running.stop().unwrap();
}

#[test]
fn admin_requests_keep_the_host_check() {
    use std::io::{Read, Write};
    let temp = TempDir::new("ops-http-host");
    let dir = server(&temp);
    let running = start(&dir);
    let address = running.address();
    let mut stream = std::net::TcpStream::connect(address).unwrap();
    let body = r#"{"name":"evil"}"#;
    write!(
        stream,
        "POST /api/admin/databases HTTP/1.1\r\nHost: {address}\r\nOrigin: http://evil.example\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");
    assert!(!dir.databases().join("evil").exists());
    running.stop().unwrap();
}

#[test]
fn the_run_administration_lists_and_cancels_a_databases_runs_over_http() {
    let temp = TempDir::new("ops-runs");
    let dir = server(&temp);
    // Offline: nothing runs, the core refuses (409 over the admin route).
    {
        let (admin, _lock) = Admin::open_offline(&dir).unwrap();
        assert!(matches!(admin.runs(None), Err(ServerError::Refused { .. })));
        assert!(matches!(
            admin.cancel_run("acme", 1),
            Err(ServerError::Refused { .. })
        ));
    }
    let running = start(&dir);
    let address = running.address();
    let (status, _) = post_create(address, json!({"name": "acme"}));
    assert_eq!(status, 201);
    let (status, _) = post_create(address, json!({"name": "idle"}));
    assert_eq!(status, 201);

    // Unloaded: an empty list, `loaded: false`; an unknown database 404; a cancel `false`.
    let (status, answer) = call(address, "GET", "/api/admin/databases/acme/runs", None);
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["database"], "acme");
    assert_eq!(answer["loaded"], false);
    assert_eq!(answer["runs"], json!([]));
    assert_eq!(answer["textHeadChars"], 200);
    let (status, answer) = call(address, "GET", "/api/admin/databases/none/runs", None);
    assert_eq!(status, 404, "{answer}");
    let (status, answer) = call(
        address,
        "POST",
        "/api/admin/databases/acme/runs/cancel",
        Some(json!({"id": 1})),
    );
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["cancelled"], false);
    for (method, path, body, expected) in [
        ("POST", "/api/admin/runs", None, 405),
        ("DELETE", "/api/admin/databases/acme/runs", None, 405),
        ("GET", "/api/admin/databases/acme/runs/cancel", None, 405),
        (
            "POST",
            "/api/admin/databases/acme/runs/cancel",
            Some(json!({"id": "x"})),
            400,
        ),
        (
            "POST",
            "/api/admin/databases/acme/runs/cancel",
            Some(json!({"id": 1, "all": true})),
            400,
        ),
    ] {
        let (status, answer) = call(address, method, path, body);
        assert_eq!(status, expected, "{method} {path}: {answer}");
    }

    for name in ["acme", "idle"] {
        call(
            address,
            "POST",
            &format!("/api/admin/databases/{name}/load"),
            None,
        );
        assert_eq!(wait_loaded(address, name)["state"], "loaded");
    }
    // The database's own mount keeps graphersal-serve's run administration off.
    let (status, _) = call(address, "GET", "/db/acme/api/admin/runs", None);
    assert_eq!(status, 404);

    // A page's endless query: listed (per database and server-wide), then cancelled.
    let endless = "let n = 0; loop { n += 1; }";
    let page = std::thread::spawn(move || {
        call(
            address,
            "POST",
            "/db/acme/api/execute",
            Some(json!({"script": endless, "runId": "r1"})),
        )
    });
    let deadline = std::time::Instant::now() + WAIT;
    let run = loop {
        let (status, answer) = call(address, "GET", "/api/admin/databases/acme/runs", None);
        assert_eq!(status, 200, "{answer}");
        assert_eq!(answer["loaded"], true);
        let running = answer["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|run| run["state"] == "running")
            .cloned();
        if let Some(run) = running {
            break run;
        }
        assert!(std::time::Instant::now() < deadline, "the run never showed");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(run["database"], "acme");
    assert_eq!(run["kind"], "query");
    assert_eq!(run["alias"], "r1");
    assert_eq!(run["text"], endless);
    assert_eq!(run["cancelled"], false);
    // `<length>:<identity>/page:<id>`: graphersal-serve keys a run by the identity too.
    let client = run["client"].as_str().unwrap();
    assert!(client.starts_with("8:operator/page"), "{run}");
    assert!(run["startedAtMs"].as_u64().unwrap() > 0, "{run}");
    assert!(run["durationMs"].is_u64(), "{run}");
    let (_, all) = call(address, "GET", "/api/admin/runs", None);
    let rows = all["runs"].as_array().unwrap();
    assert!(
        rows.iter()
            .any(|r| r["database"] == "acme" && r["id"] == run["id"]),
        "{all}"
    );
    assert!(rows.iter().all(|r| r["database"] == "acme"), "{all}");
    let (_, idle) = call(address, "GET", "/api/admin/databases/idle/runs", None);
    assert_eq!(
        idle["runs"],
        json!([]),
        "another database's runs stay apart"
    );
    // The id names a run of `acme` only: the other database has no such run.
    let kill = json!({"id": run["id"]});
    let (status, answer) = call(
        address,
        "POST",
        "/api/admin/databases/idle/runs/cancel",
        Some(kill.clone()),
    );
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["cancelled"], false);
    let (status, answer) = call(
        address,
        "POST",
        "/api/admin/databases/acme/runs/cancel",
        Some(kill.clone()),
    );
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["cancelled"], true, "{answer}");
    assert_eq!(answer["id"], run["id"]);
    let (status, answer) = page.join().unwrap();
    let stopped = status >= 400 || answer["ok"] == false;
    assert!(stopped, "the page's query was cancelled: {status} {answer}");
    // Ended: no longer listed, a second cancel is `false`.
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let (_, answer) = call(address, "GET", "/api/admin/databases/acme/runs", None);
        if answer["runs"] == json!([]) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{answer}");
        std::thread::sleep(Duration::from_millis(20));
    }
    let (_, answer) = call(
        address,
        "POST",
        "/api/admin/databases/acme/runs/cancel",
        Some(kill),
    );
    assert_eq!(answer["cancelled"], false);
    running.stop().unwrap();
}
