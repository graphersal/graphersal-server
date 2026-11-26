//! Tests of the layer reset (round 4 B-LAYER, [`Admin::reset_layer`]): synthetic stores (the
//! `modern` sample as a base, vertices added and removed through the served sessions), offline
//! and online, the trash entry put back by hand, the refusals and the scan after a crash.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use graphersal_serve::{Exposure, HostCheck};
use serde_json::{Value, json};

use super::*;
use crate::database::{RESET_STORE_DIR, STORE_DIR};
use crate::registry::DbState;
use crate::registry::tests::server;
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

/// Runs `query` through the served session of the loaded database `name`: its JSON answer.
fn run(loader: &Loader, name: &str, query: &str) -> Value {
    let context = loader.context(name).expect("loaded");
    let query = query.to_string();
    let text = context
        .call(move |session| session.execute(&query, &graphersal_session::RunOptions::default()))
        .expect("answers");
    let answer: Value = serde_json::from_str(&text).expect("JSON");
    assert_ne!(answer["ok"], Value::Bool(false), "{answer}");
    assert!(answer["execution"]["error"].is_null(), "{answer}");
    answer
}

fn served_count(loader: &Loader, name: &str) -> i64 {
    let context = loader.context(name).expect("loaded");
    let info = context
        .call(|session| session.info().map_err(String::from))
        .expect("answers")
        .expect("info");
    info["vertexCount"].as_i64().expect("a count")
}

/// The names of the vertices of the loaded database `name` (the known names among the
/// answer's strings, wherever the answer puts its rows), sorted.
fn served_names(loader: &Loader, name: &str) -> Vec<String> {
    const KNOWN: [&str; 10] = [
        "marko", "vadas", "lop", "josh", "ripple", "peter", "new1", "new2", "new3", "new-l2",
    ];
    fn walk(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::String(s) if KNOWN.contains(&s.as_str()) => out.push(s.clone()),
            Value::Array(items) => items.iter().for_each(|v| walk(v, out)),
            Value::Object(map) => map.values().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    let answer = run(loader, name, r#"g.v().values("name").to_list()"#);
    let mut names = Vec::new();
    walk(&answer, &mut names);
    names.sort();
    names.dedup();
    names
}

fn job_entry(loader: &Loader, name: &str, job: &str) -> Value {
    let context = loader.context(name).expect("loaded");
    let job = job.to_string();
    context
        .call(move |session| session.job_entry(&job).map_err(|e| e.to_string()))
        .expect("answers")
        .expect("the jobs")
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

/// Every file below `dir` with its bytes.
fn contents(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.insert(path.clone(), fs::read(&path).unwrap());
            }
        }
    }
    out
}

#[test]
fn a_layer_reset_discards_its_changes_and_keeps_everything_else() {
    let temp = TempDir::new("layer-reset");
    let dir = server(&temp);
    {
        let admin = offline(&dir);
        modern_base(&admin, "prod");
        admin
            .create(
                "l1",
                &CreateSpec::layer("prod")
                    .with_description("scratch")
                    .with_auto_load(true),
            )
            .unwrap();
        admin.create("l2", &CreateSpec::layer("prod")).unwrap();
        admin.alias_set("cur", "l1").unwrap();
    }
    // A query limit override by hand (kept by every admin change, and by the reset).
    let config_path = dir.databases().join("l1").join("database.toml");
    let mut config = fs::read_to_string(&config_path).unwrap();
    config.push_str("\n[limits.query]\ntimeout_ms = 12345\n");
    fs::write(&config_path, &config).unwrap();
    let config_before = fs::read(&config_path).unwrap();

    let (loader, admin) = online(&dir);
    load(&loader, "l1");
    load(&loader, "l2");
    run(
        &loader,
        "l1",
        r#"g.add_v("person").property("name", "new1").iterate()"#,
    );
    run(
        &loader,
        "l1",
        r#"g.add_v("person").property("name", "new2").iterate()"#,
    );
    run(
        &loader,
        "l1",
        r#"g.v().has("name", "marko").drop().iterate()"#,
    );
    run(
        &loader,
        "l1",
        r#"g.v().has("name", "josh").property("age", 99).iterate()"#,
    );
    run(
        &loader,
        "l2",
        r#"g.add_v("person").property("name", "new-l2").iterate()"#,
    );
    let defined = loader
        .context("l1")
        .unwrap()
        .call(|session| {
            session.define_job(&json!({
                "name": "nightly", "schedule": "0 3 * * *", "action": "checkpoint",
            }))
        })
        .unwrap();
    assert_eq!(defined["ok"], Value::Bool(true), "{defined}");
    assert_eq!(served_count(&loader, "l1"), 7);
    assert_eq!(served_count(&loader, "l2"), 7);
    let l2_names = served_names(&loader, "l2");
    let base_before = contents(&dir.databases().join("prod"));

    // Reset while loaded: unloaded, reset, loaded again.
    let done = admin.reset_layer("l1").expect("the reset");
    assert!(done.reloaded);
    assert_eq!(done.base, "prod");
    assert_eq!(done.discarded.added, (2, 0));
    assert_eq!(done.discarded.removed.0, 1);
    assert!(done.discarded.modified.0 >= 1, "{:?}", done.discarded);
    assert_eq!(done.jobs_kept, 1);
    assert!(done.trash.starts_with(dir.trash()), "{:?}", done.trash);
    assert!(done.trash.join(STORE_DIR).is_dir());
    assert!(done.trash.join("database.toml").is_file());
    assert!(!dir.databases().join("l1").join(RESET_STORE_DIR).exists());

    // The layer reads exactly its base; the other layer and the base are untouched.
    assert_eq!(loader.wait("l1", WAIT).unwrap(), DbState::Loaded);
    assert_eq!(served_count(&loader, "l1"), 6);
    assert_eq!(
        served_names(&loader, "l1"),
        ["josh", "lop", "marko", "peter", "ripple", "vadas"]
    );
    let josh = run(
        &loader,
        "l1",
        r#"g.v().has("name", "josh").values("age").to_list()"#,
    );
    assert!(josh.to_string().contains("32"), "{josh}");
    assert_eq!(served_count(&loader, "l2"), 7);
    assert_eq!(served_names(&loader, "l2"), l2_names);
    assert_eq!(contents(&dir.databases().join("prod")), base_before);
    // Writes work after the reset.
    run(
        &loader,
        "l1",
        r#"g.add_v("person").property("name", "new3").iterate()"#,
    );
    assert_eq!(served_count(&loader, "l1"), 7);

    // Kept: database.toml byte for byte, the alias, the job definition.
    assert_eq!(fs::read(&config_path).unwrap(), config_before);
    let entry = admin.registry().get("l1").unwrap();
    let kept = entry.config.clone().unwrap();
    assert_eq!(kept.description, "scratch");
    assert!(kept.auto_load);
    assert_eq!(admin.registry().aliases()["cur"], "l1");
    let job = job_entry(&loader, "l1", "nightly");
    assert_eq!(job["schedule"], "0 3 * * *", "{job}");
    loader.shutdown().unwrap();

    // The old layer is recoverable by hand: its trash directory is a whole database directory.
    fs::rename(&done.trash, dir.databases().join("l1-old")).unwrap();
    let (loader, _admin) = online(&dir);
    load(&loader, "l1-old");
    assert_eq!(served_count(&loader, "l1-old"), 7);
    let names = served_names(&loader, "l1-old");
    assert!(names.contains(&"new1".to_string()), "{names:?}");
    assert!(!names.contains(&"marko".to_string()), "{names:?}");
    loader.shutdown().unwrap();
}

#[test]
fn an_unloaded_layer_resets_offline_and_online_without_a_load() {
    let temp = TempDir::new("layer-reset-offline");
    let dir = server(&temp);
    {
        let admin = offline(&dir);
        modern_base(&admin, "prod");
        admin.create("l1", &CreateSpec::layer("prod")).unwrap();
    }
    {
        let (loader, _admin) = online(&dir);
        load(&loader, "l1");
        run(&loader, "l1", r#"g.add_v("x").iterate()"#);
        loader.shutdown().unwrap();
    }
    // Offline (the console with the server lock): no loader, nothing loaded again.
    let done = offline(&dir).reset_layer("l1").expect("offline");
    assert!(!done.reloaded);
    assert_eq!(done.discarded.added, (1, 0));
    assert_eq!(done.jobs_kept, 0);
    // Online but unloaded: it stays unloaded; a second reset of an unchanged layer works too.
    let (loader, admin) = online(&dir);
    let again = admin.reset_layer("l1").expect("online");
    assert!(!again.reloaded);
    assert!(again.discarded.is_empty());
    assert_eq!(again.entry.state, DbState::Unloaded);
    assert_ne!(again.trash, done.trash);
    load(&loader, "l1");
    assert_eq!(served_count(&loader, "l1"), 6);
    loader.shutdown().unwrap();
}

#[test]
fn plain_base_failed_and_unknown_databases_are_refused() {
    let temp = TempDir::new("layer-reset-refused");
    let dir = server(&temp);
    let admin = offline(&dir);
    modern_base(&admin, "prod");
    admin.create("acme", &CreateSpec::plain()).unwrap();
    admin.create("l1", &CreateSpec::layer("prod")).unwrap();
    let refused = |name: &str| match admin.reset_layer(name) {
        Err(ServerError::Refused { message, .. }) => message,
        other => panic!("{name}: {other:?}"),
    };
    assert!(refused("acme").contains("plain database, not a layer"));
    assert!(refused("prod").contains("base database"));
    assert!(matches!(
        admin.reset_layer("nope"),
        Err(ServerError::NotFound { .. })
    ));
    // A failed layer (its Store is gone): refused, nothing moved.
    fs::write(
        dir.databases().join("acme").join("database.toml"),
        "kind = 12\n",
    )
    .unwrap();
    let admin = offline(&dir);
    assert!(
        matches!(admin.reset_layer("acme"), Err(ServerError::Refused { ref message, .. }) if message.contains("failed")),
    );
    // Nothing went to the trash.
    assert!(fs::read_dir(dir.trash()).map(|d| d.count()).unwrap_or(0) == 0);
}

#[test]
fn the_scan_finishes_or_discards_an_interrupted_reset() {
    let temp = TempDir::new("layer-reset-crash");
    let dir = server(&temp);
    {
        let admin = offline(&dir);
        modern_base(&admin, "prod");
        admin.create("l1", &CreateSpec::layer("prod")).unwrap();
        admin.create("l2", &CreateSpec::layer("prod")).unwrap();
    }
    {
        let (loader, _admin) = online(&dir);
        load(&loader, "l1");
        run(&loader, "l1", r#"g.add_v("kept").iterate()"#);
        loader.shutdown().unwrap();
    }
    // Killed before the first rename: store/ and store-reset/ both there: the old layer stays.
    let l1 = dir.databases().join("l1");
    fs::create_dir_all(l1.join(RESET_STORE_DIR).join("debris")).unwrap();
    // Killed between the renames: the old store is in trash/, only store-reset/ is left. The
    // new layer Store is an empty layer over the base: here l2's (equally empty) Store.
    let l2 = dir.databases().join("l2");
    fs::create_dir_all(dir.trash()).unwrap();
    fs::rename(l2.join(STORE_DIR), dir.trash().join("l2-old-store")).unwrap();
    {
        let admin = offline(&dir);
        modern_base(&admin, "spare");
        admin.create("donor", &CreateSpec::layer("prod")).unwrap();
    }
    fs::rename(
        dir.databases().join("donor").join(STORE_DIR),
        l2.join(RESET_STORE_DIR),
    )
    .unwrap();
    fs::remove_dir_all(dir.databases().join("donor")).unwrap();

    let (loader, admin) = online(&dir);
    assert!(!l1.join(RESET_STORE_DIR).exists());
    assert!(l1.join(STORE_DIR).is_dir());
    assert!(!l2.join(RESET_STORE_DIR).exists());
    assert!(l2.join(STORE_DIR).is_dir());
    assert_eq!(admin.registry().get("l2").unwrap().state, DbState::Unloaded);
    load(&loader, "l1");
    assert_eq!(served_count(&loader, "l1"), 7);
    load(&loader, "l2");
    assert_eq!(served_count(&loader, "l2"), 6);
    loader.shutdown().unwrap();
}
