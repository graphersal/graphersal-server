//! Tests of backups, restores and exports ([`Admin::backup`], [`Admin::backups`],
//! [`Admin::restore`], [`Admin::export`]): offline and online, and through the admin HTTP API of
//! a real listener on port 0. Synthetic stores only (empty databases written through the core,
//! data added through the Store API or the served sessions, the `modern` sample as a base, a
//! tiny synthetic embedding model).

use std::fs;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use graphersal::persist::{
    ImportModelOptions, ModelSource, ModelState, ReadOptions, read_snapshot_file,
    read_snapshot_info,
};
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

fn online(dir: &ServerDir) -> (Arc<Loader>, Admin) {
    let loader = Loader::new(
        Registry::scan(dir).expect("scan"),
        HostCheck::new(Exposure::Loopback, 8484, "http://127.0.0.1:8484/"),
    );
    let admin = Admin::online(loader.clone());
    (loader, admin)
}

fn load(loader: &Arc<Loader>, name: &str) {
    loader.load(name).expect("starts");
    assert_eq!(loader.wait(name, WAIT).expect("state"), DbState::Loaded);
}

/// Adds `n` persons to the closed plain Store of `name`.
fn add_plain(dir: &ServerDir, name: &str, n: usize) {
    let store = Store::open(
        dir.databases().join(name).join(STORE_DIR),
        StoreOptions::new(),
    )
    .expect("opens");
    for i in 0..n {
        store
            .graph()
            .write()
            .traversal_mut()
            .add_v("person")
            .property("name", format!("p{i}"))
            .to_list()
            .expect("adds");
    }
    store.close().expect("closes");
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

/// Adds a person through the served session of the loaded database `name`.
fn add_served(loader: &Loader, name: &str) {
    let context = loader.context(name).expect("loaded");
    let text = context
        .call(|session| {
            session.execute(
                r#"g.add_v("person").iterate()"#,
                &graphersal_session::RunOptions::default(),
            )
        })
        .expect("answers");
    let answer: Value = serde_json::from_str(&text).expect("JSON");
    assert_ne!(answer["ok"], Value::Bool(false), "{answer}");
}

fn served_count(loader: &Loader, name: &str) -> i64 {
    let context = loader.context(name).expect("loaded");
    let info = context
        .call(|session| session.info().map_err(String::from))
        .expect("answers")
        .expect("info");
    info["vertexCount"].as_i64().expect("a count")
}

/// A base database `name` holding the modern sample (6 vertices).
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

/// Adds `n` vertices to the unloaded layer database `name` (loaded for it, then unloaded).
fn add_layer(dir: &ServerDir, name: &str, n: usize) {
    let (loader, _admin) = online(dir);
    load(&loader, name);
    for _ in 0..n {
        add_served(&loader, name);
    }
    loader.shutdown().expect("unloads");
}

fn tmp_count(dir: &ServerDir) -> usize {
    fs::read_dir(dir.tmp()).map(|d| d.count()).unwrap_or(0)
}

#[test]
fn backup_and_restore_into_a_new_name_keep_the_data() {
    let temp = TempDir::new("xfer-backup");
    let dir = server(&temp);
    let admin = offline(&dir);
    admin
        .create("acme", &CreateSpec::plain().with_description("Acme"))
        .unwrap();
    add_plain(&dir, "acme", 3);

    // Unloaded: from its files.
    let first = admin.backup("acme").expect("a backup");
    assert_eq!(first.database, "acme");
    assert!(first.bytes > 0);
    assert!(!first.backup.contains(':'), "{}", first.backup);
    assert_eq!(
        first.file,
        dir.backups()
            .join("acme")
            .join(format!("{}.zip", first.backup))
    );
    assert!(first.file.is_file());
    assert!(
        dir.backups()
            .join("acme")
            .join(format!("{}.toml", first.backup))
            .is_file()
    );
    assert_eq!(first.kind, Some(DatabaseKind::Plain));
    // A second backup in the same second gets its own name.
    let second = admin.backup("acme").unwrap();
    assert_ne!(first.backup, second.backup);
    let list = admin.backups("acme").unwrap();
    assert_eq!(
        list.iter().map(|b| b.backup.as_str()).collect::<Vec<_>>(),
        [first.backup.as_str(), second.backup.as_str()]
    );
    assert_eq!(tmp_count(&dir), 0);

    let restored = admin
        .restore("acme", &first.backup, Some("acme-r"))
        .expect("restored");
    assert_eq!(restored.entry.name, "acme-r");
    assert_eq!(restored.entry.state, DbState::Unloaded);
    assert!(restored.replaced.is_none());
    let config = restored.entry.config.clone().unwrap();
    assert_eq!(config.description, "Acme");
    assert!(!config.auto_load);
    assert_eq!(plain_count(&dir, "acme-r"), 3);
    // `<backup>.zip` names it too; an unknown backup or a path is refused.
    admin
        .restore("acme", &format!("{}.zip", second.backup), Some("acme-s"))
        .expect("by file name");
    for bad in ["nope", "../acme/x", ""] {
        let err = admin.restore("acme", bad, Some("x")).unwrap_err();
        assert!(
            matches!(err, ServerError::BackupNotFound { .. }),
            "{bad}: {err}"
        );
    }
    let err = admin
        .restore("acme", &first.backup, Some("acme-r"))
        .unwrap_err();
    assert!(err.to_string().contains("acme-r"), "{err}");
    assert_eq!(tmp_count(&dir), 0);

    // Loaded: from its open Store, with the writes made while it is served.
    let (loader, admin) = online(&dir);
    load(&loader, "acme");
    add_served(&loader, "acme");
    let online_backup = admin.backup("acme").expect("a backup of a loaded database");
    let restored = admin
        .restore("acme", &online_backup.backup, Some("acme-live"))
        .unwrap();
    assert_eq!(restored.entry.state, DbState::Unloaded);
    load(&loader, "acme-live");
    assert_eq!(served_count(&loader, "acme-live"), 4);
    loader.shutdown().unwrap();

    // The backups of a deleted database stay listed and restorable.
    let admin = offline(&dir);
    admin.delete("acme").unwrap();
    assert_eq!(admin.backups("acme").unwrap().len(), 3);
    admin.restore("acme", &first.backup, None).expect("back");
    assert_eq!(plain_count(&dir, "acme"), 3);
    assert!(matches!(
        admin.backups("never").unwrap_err(),
        ServerError::NotFound { .. }
    ));
}

#[test]
fn restore_over_the_same_name_needs_it_unloaded() {
    let temp = TempDir::new("xfer-restore-same");
    let dir = server(&temp);
    offline(&dir).create("acme", &CreateSpec::plain()).unwrap();
    add_plain(&dir, "acme", 1);
    let (loader, admin) = online(&dir);
    let backup = admin.backup("acme").unwrap();
    load(&loader, "acme");
    add_served(&loader, "acme");
    add_served(&loader, "acme");
    let err = admin.restore("acme", &backup.backup, None).unwrap_err();
    assert!(matches!(err, ServerError::Refused { .. }), "{err}");
    assert!(err.to_string().contains("unload it first"), "{err}");
    assert_eq!(served_count(&loader, "acme"), 3);
    assert!(loader.unload("acme").unwrap());

    let restored = admin
        .restore("acme", &backup.backup, None)
        .expect("restored");
    let trash = restored.replaced.clone().expect("replaced");
    assert!(trash.starts_with(dir.trash()) && trash.join(STORE_DIR).is_dir());
    assert_eq!(restored.entry.state, DbState::Unloaded);
    load(&loader, "acme");
    assert_eq!(served_count(&loader, "acme"), 1);
    loader.shutdown().unwrap();
    assert_eq!(tmp_count(&dir), 0);
}

#[test]
fn a_failure_or_a_crash_mid_restore_keeps_the_old_database() {
    let temp = TempDir::new("xfer-restore-crash");
    let dir = server(&temp);
    let mut admin = offline(&dir);
    admin.create("acme", &CreateSpec::plain()).unwrap();
    add_plain(&dir, "acme", 1);
    let backup = admin.backup("acme").unwrap();
    add_plain(&dir, "acme", 2);

    admin.fail = Some(FailPoint::AfterStore);
    assert!(admin.restore("acme", &backup.backup, None).is_err());
    assert!(admin.restore("acme", &backup.backup, Some("new")).is_err());
    assert_eq!(tmp_count(&dir), 0);
    admin.fail = Some(FailPoint::CrashAfterStore);
    assert!(admin.restore("acme", &backup.backup, None).is_err());
    assert!(admin.restore("acme", &backup.backup, Some("new")).is_err());
    assert_eq!(tmp_count(&dir), 2, "the crashed builds stay in tmp/");
    assert!(!dir.databases().join("new").exists());
    drop(admin);
    // The next start cleans tmp/; the old database is untouched.
    let admin = offline(&dir);
    assert_eq!(tmp_count(&dir), 0);
    assert!(matches!(
        admin.registry().get("new"),
        Err(ServerError::NotFound { .. })
    ));
    assert_eq!(plain_count(&dir, "acme"), 3);
    admin
        .restore("acme", &backup.backup, None)
        .expect("now it restores");
    assert_eq!(plain_count(&dir, "acme"), 1);
}

#[test]
fn layer_backups_restore_only_over_their_base() {
    let temp = TempDir::new("xfer-layer");
    let dir = server(&temp);
    let admin = offline(&dir);
    modern_base(&admin, "prod");
    admin.create("l1", &CreateSpec::layer("prod")).unwrap();
    add_layer(&dir, "l1", 2);
    let backup = admin.backup("l1").expect("a layer backup");
    assert_eq!(backup.kind, Some(DatabaseKind::Layer));
    assert_eq!(backup.base.as_deref(), Some("prod"));
    let identity = backup.layer_base.clone().expect("the base identity");
    assert!(identity.starts_with("graph "), "{identity}");

    // Over the same base: a layer over it, with its data.
    let restored = admin
        .restore("l1", &backup.backup, Some("l2"))
        .expect("restored over its base");
    let config = restored.entry.config.clone().unwrap();
    assert_eq!(config.kind, DatabaseKind::Layer);
    assert_eq!(config.base.as_deref(), Some("prod"));
    // Without the sidecar, the base is found by its identity.
    let dir_l1 = dir.backups().join("l1");
    fs::copy(
        dir_l1.join(format!("{}.zip", backup.backup)),
        dir_l1.join("bare.zip"),
    )
    .unwrap();
    let bare = admin
        .restore("l1", "bare", Some("l3"))
        .expect("found by identity");
    assert_eq!(bare.entry.config.unwrap().base.as_deref(), Some("prod"));
    {
        let (loader, _admin) = online(&dir);
        load(&loader, "l2");
        assert_eq!(served_count(&loader, "l2"), 8);
        loader.shutdown().unwrap();
    }

    // A layer's own backup over it (unloaded); a base that layers name is not replaced.
    let own = admin
        .restore("l1", &backup.backup, None)
        .expect("over itself");
    assert!(own.replaced.is_some());
    assert_eq!(own.entry.config.unwrap().base.as_deref(), Some("prod"));
    let base_backup = admin.backup("prod").unwrap();
    assert_eq!(base_backup.role, Some(DatabaseRole::Base));
    let err = admin
        .restore("prod", &base_backup.backup, None)
        .unwrap_err();
    assert!(err.to_string().contains("base of layers"), "{err}");
    let copy = admin
        .restore("prod", &base_backup.backup, Some("prod-copy"))
        .unwrap();
    let config = copy.entry.config.unwrap();
    assert_eq!(config.role, DatabaseRole::Base);
    assert!(config.read_only);
    admin.delete("prod-copy").unwrap();

    // Another base under the same name: refused, naming the identity.
    for name in ["l1", "l2", "l3"] {
        admin.delete(name).unwrap();
    }
    admin.delete("prod").unwrap();
    modern_base(&admin, "prod");
    let err = admin.restore("l1", &backup.backup, Some("l4")).unwrap_err();
    assert!(matches!(err, ServerError::Refused { .. }), "{err}");
    let text = err.to_string();
    assert!(
        text.contains(&identity) && text.contains("'prod'"),
        "{text}"
    );
    let err = admin.restore("l1", "bare", Some("l4")).unwrap_err();
    assert!(err.to_string().contains(&identity), "{err}");
    // No base of that name at all.
    admin.delete("prod").unwrap();
    let err = admin.restore("l1", &backup.backup, Some("l4")).unwrap_err();
    assert!(err.to_string().contains("not a base database"), "{err}");
    assert!(matches!(
        admin.registry().get("l4"),
        Err(ServerError::NotFound { .. })
    ));
    assert_eq!(tmp_count(&dir), 0);
}

#[test]
fn exports_carry_the_models_or_leave_them_out() {
    let temp = TempDir::new("xfer-models");
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
    add_plain(&dir, "acme", 2);

    let with = temp.path().join("with.gsnap");
    let exported = admin
        .export_to_file("acme", ExportSpec::new(), &with)
        .expect("exported");
    assert!(exported.models && !exported.layer);
    assert_eq!(exported.file_name, "acme.gsnap");
    assert_eq!(exported.bytes, fs::metadata(&with).unwrap().len());
    let without = temp.path().join("without.gsnap");
    let exported = admin
        .export_to_file("acme", ExportSpec::new().with_models(false), &without)
        .unwrap();
    assert!(!exported.models);
    assert!(fs::metadata(&without).unwrap().len() < fs::metadata(&with).unwrap().len());

    let (graph, report) = read_snapshot_file(&with, &ReadOptions::default()).unwrap();
    assert_eq!(graph.read().vertex_count(), 2);
    assert_eq!(report.models.len(), 1, "{:?}", report.models);
    assert_ne!(report.models[0].state, ModelState::Missing);
    let (_, report) = read_snapshot_file(&without, &ReadOptions::default()).unwrap();
    assert!(
        report.models.iter().all(|m| m.state == ModelState::Missing),
        "{:?}",
        report.models
    );
    // Into a Store: the blob present, or the definition kept with the blob missing.
    let a = temp.path().join("a");
    Store::create_from_packed_dir(&a, fs::File::open(&with).unwrap(), StoreOptions::new()).unwrap();
    let models = Store::models_dir(&a).unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].repo, "acme/tiny");
    assert_ne!(models[0].state, ModelState::Missing);
    let b = temp.path().join("b");
    Store::create_from_packed_dir(&b, fs::File::open(&without).unwrap(), StoreOptions::new())
        .unwrap();
    let models = Store::models_dir(&b).unwrap();
    assert_eq!(models.len(), 1, "the definition stays");
    assert_eq!(models[0].state, ModelState::Missing);

    // Loaded: the open graph, with the model.
    let (loader, admin) = online(&dir);
    load(&loader, "acme");
    add_served(&loader, "acme");
    let live = temp.path().join("live.gsnap");
    admin
        .export_to_file("acme", ExportSpec::new(), &live)
        .unwrap();
    let (graph, report) = read_snapshot_file(&live, &ReadOptions::default()).unwrap();
    assert_eq!(graph.read().vertex_count(), 3);
    assert_eq!(report.models.len(), 1);
    loader.shutdown().unwrap();
    assert!(!temp.path().join("live.gsnap.part").exists());
}

#[test]
fn layers_export_as_layer_snapshots_or_materialized() {
    let temp = TempDir::new("xfer-layer-export");
    let dir = server(&temp);
    let admin = offline(&dir);
    modern_base(&admin, "prod");
    admin.create("l1", &CreateSpec::layer("prod")).unwrap();
    add_layer(&dir, "l1", 1);
    let base_identity = Store::inspect(dir.databases().join("l1").join(STORE_DIR))
        .unwrap()
        .layer_base
        .unwrap();

    let check = |admin: &Admin, extra: usize| {
        let layer = temp.path().join("l1.layer.gsnap");
        let exported = admin
            .export_to_file("l1", ExportSpec::new(), &layer)
            .unwrap();
        assert!(exported.layer && !exported.models);
        assert_eq!(exported.file_name, "l1.layer.gsnap");
        let info = read_snapshot_info(fs::File::open(&layer).unwrap()).unwrap();
        assert_eq!(info.layer_base, Some(base_identity));
        let full = temp.path().join("l1.gsnap");
        let exported = admin
            .export_to_file("l1", ExportSpec::new().with_materialize(true), &full)
            .unwrap();
        assert!(!exported.layer);
        let info = read_snapshot_info(fs::File::open(&full).unwrap()).unwrap();
        assert_eq!(info.layer_base, None);
        let (graph, _) = read_snapshot_file(&full, &ReadOptions::default()).unwrap();
        assert_eq!(graph.read().vertex_count(), 6 + extra);
    };
    // Unloaded, then loaded (with a write made while served).
    check(&admin, 1);
    let (loader, admin) = online(&dir);
    load(&loader, "l1");
    add_served(&loader, "l1");
    check(&admin, 2);
    // A base exports as a plain database.
    let base = temp.path().join("prod.gsnap");
    let exported = admin
        .export_to_file("prod", ExportSpec::new(), &base)
        .unwrap();
    assert!(!exported.layer);
    loader.shutdown().unwrap();
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

fn download(address: SocketAddr, path: &str, out: &Path) -> crate::client::Downloaded {
    let mut file = fs::File::create(out).unwrap();
    crate::client::download(
        address,
        crate::test_support::operator_token(address).as_deref(),
        path,
        &mut file,
    )
    .expect("an answer")
}

#[test]
fn backup_restore_and_export_over_http() {
    let temp = TempDir::new("xfer-http");
    let dir = server(&temp);
    offline(&dir).create("acme", &CreateSpec::plain()).unwrap();
    add_plain(&dir, "acme", 2);
    let running = start(&dir);
    let address = running.address();
    let (status, _) = call(address, "POST", "/api/admin/databases/acme/load", None);
    assert_eq!(status, 202);
    assert_eq!(
        running.loader().wait("acme", WAIT).unwrap(),
        DbState::Loaded
    );

    let (status, backup) = call(address, "POST", "/api/admin/databases/acme/backup", None);
    assert_eq!(status, 201, "{backup}");
    let name = backup["backup"].as_str().unwrap().to_string();
    assert_eq!(backup["database"], "acme");
    assert_eq!(backup["file"], format!("backups/acme/{name}.zip"));
    assert!(backup["bytes"].as_u64().unwrap() > 0);
    assert_eq!(backup["kind"], "plain");
    let (status, list) = call(address, "GET", "/api/admin/databases/acme/backups", None);
    assert_eq!(status, 200);
    assert_eq!(list["database"], "acme");
    assert_eq!(list["backups"][0]["backup"], json!(name));

    // Restore: over the loaded database refused with help; into a new name.
    let (status, refused) = call(
        address,
        "POST",
        "/api/admin/databases/acme/restore",
        Some(json!({ "backup": name })),
    );
    assert_eq!(status, 409, "{refused}");
    assert!(
        refused["help"]
            .as_str()
            .unwrap()
            .contains("POST /api/admin/databases/acme/unload"),
        "{refused}"
    );
    let (status, entry) = call(
        address,
        "POST",
        "/api/admin/databases/acme/restore",
        Some(json!({ "backup": name, "name": "acme-r" })),
    );
    assert_eq!(status, 201, "{entry}");
    assert_eq!(entry["name"], "acme-r");
    assert_eq!(entry["state"], "unloaded");
    assert_eq!(
        entry["restoredFrom"],
        json!({"database": "acme", "backup": name})
    );
    assert_eq!(entry["replaced"], false);
    let (status, missing) = call(
        address,
        "POST",
        "/api/admin/databases/acme/restore",
        Some(json!({ "backup": "nope", "name": "x" })),
    );
    assert_eq!(status, 404, "{missing}");
    assert!(missing["help"].as_str().unwrap().contains("/backups"));
    let (status, _) = call(
        address,
        "POST",
        "/api/admin/databases/acme/restore",
        Some(json!({ "backup": name, "extra": 1 })),
    );
    assert_eq!(status, 400);

    // Export: a download of the loaded database.
    let file = temp.path().join("acme.gsnap");
    let answer = download(address, "/api/admin/databases/acme/export", &file);
    assert_eq!(answer.status, 200);
    assert_eq!(
        answer.header("Content-Disposition"),
        Some("attachment; filename=\"acme.gsnap\"")
    );
    assert_eq!(answer.header("X-Graphersal-Snapshot"), Some("full"));
    assert_eq!(answer.header("X-Graphersal-Models"), Some("included"));
    assert_eq!(
        answer
            .header("Content-Length")
            .and_then(|v| v.parse::<u64>().ok()),
        Some(answer.bytes)
    );
    assert_eq!(fs::metadata(&file).unwrap().len(), answer.bytes);
    let (graph, _) = read_snapshot_file(&file, &ReadOptions::default()).unwrap();
    assert_eq!(graph.read().vertex_count(), 2);
    let answer = download(
        address,
        "/api/admin/databases/acme/export?models=false&materialize=true",
        &file,
    );
    assert_eq!(answer.status, 200);
    assert_eq!(answer.header("X-Graphersal-Models"), Some("excluded"));
    let answer = download(address, "/api/admin/databases/acme/export?x=1", &file);
    assert_eq!(answer.status, 400);
    assert!(
        answer.error["error"]
            .as_str()
            .unwrap()
            .contains("unknown parameter")
    );
    let answer = download(address, "/api/admin/databases/nope/export", &file);
    assert_eq!(answer.status, 404);
    let (status, _) = call(address, "POST", "/api/admin/databases/acme/export", None);
    assert_eq!(status, 405);
    // The exports in tmp/ are gone after their answers.
    assert_eq!(tmp_count(&dir), 0);
    running.stop().unwrap();
}
