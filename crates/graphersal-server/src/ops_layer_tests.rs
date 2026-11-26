//! Tests of the layer reset (round 4 B-LAYER, S1: the engine's `reset_layer`,
//! [`Admin::reset_layer`]): synthetic stores (the `modern` sample as a base, vertices added and
//! removed through the served sessions), in place while loaded, offline and online unloaded,
//! the history before the reset, a running query, the refusals.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use graphersal_serve::{Exposure, HostCheck};
use serde_json::{Value, json};

use super::*;
use graphersal::persist::RecoveryTarget;
use graphersal::storage::LayeredGraph;

use crate::database::STORE_DIR;

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

    // Reset while loaded: in place, on its open Store (the same context keeps serving).
    let context_before = loader.context("l1").unwrap();
    let done = admin.reset_layer("l1").expect("the reset");
    assert!(done.loaded);
    assert_eq!(done.base, "prod");
    assert_eq!(done.discarded.added, (2, 0));
    assert_eq!(done.discarded.removed.0, 1);
    assert!(done.discarded.modified.0 >= 1, "{:?}", done.discarded);
    assert_eq!(done.jobs_kept, 1);
    assert_eq!(done.entry.state, DbState::Loaded);
    assert!(Arc::ptr_eq(&context_before, &loader.context("l1").unwrap()));
    drop(context_before);
    assert!(
        fs::read_dir(dir.trash()).map(|d| d.count()).unwrap_or(0) == 0,
        "nothing goes to the trash"
    );

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

    // The state before the reset is in the Store's history: read it back at the commit
    // before the reset's (the reset, then new3: two commits after it).
    let seq = Store::inspect(entry.store_dir().as_path())
        .unwrap()
        .commit_seq;
    let base = admin.base_of("prod").unwrap();
    let before = Store::<LayeredGraph>::open_layer_read_only(
        entry.store_dir().as_path(),
        &base,
        RecoveryTarget::CommitSeq(seq - 2),
    )
    .unwrap()
    .into_graph();
    let names: Vec<String> = graphersal::prelude::GraphTraversalSource::new(&*before.read())
        .v(None)
        .values("name")
        .to_list()
        .unwrap()
        .iter()
        .map(|v| format!("{v:?}"))
        .collect();
    assert_eq!(names.len(), 7, "{names:?}");
    assert!(names.iter().any(|n| n.contains("new1")), "{names:?}");
    assert!(!names.iter().any(|n| n.contains("marko")), "{names:?}");
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
    let store_dir = dir.databases().join("l1").join(STORE_DIR);
    let seq = Store::inspect(store_dir.as_path()).unwrap().commit_seq;
    // Offline (the console with the server lock): the layer Store opened directly.
    let done = offline(&dir).reset_layer("l1").expect("offline");
    assert!(!done.loaded);
    assert_eq!(done.discarded.added, (1, 0));
    assert_eq!(done.jobs_kept, 0);
    assert_eq!(
        Store::inspect(store_dir.as_path()).unwrap().commit_seq,
        seq + 1,
        "one commit"
    );
    // Online but unloaded: it stays unloaded; a second reset of an unchanged layer commits
    // nothing.
    let (loader, admin) = online(&dir);
    let again = admin.reset_layer("l1").expect("online");
    assert!(!again.loaded);
    assert!(again.discarded.is_empty());
    assert_eq!(again.entry.state, DbState::Unloaded);
    assert_eq!(
        Store::inspect(store_dir.as_path()).unwrap().commit_seq,
        seq + 1
    );
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

/// A loaded layer with a query holding its graph: the reset waits, then refuses naming the
/// runs to cancel; nothing changed.
#[test]
fn a_reset_waits_for_a_running_query_then_refuses() {
    let temp = TempDir::new("layer-reset-busy");
    let dir = server(&temp);
    {
        let admin = offline(&dir);
        modern_base(&admin, "prod");
        admin.create("l1", &CreateSpec::layer("prod")).unwrap();
    }
    let (loader, _admin) = online(&dir);
    load(&loader, "l1");
    run(&loader, "l1", r#"g.add_v("x").iterate()"#);
    let Some((_context, crate::loader::OpenStoreRef::Layer(store))) = loader.open_store_of("l1")
    else {
        panic!("a layer store");
    };
    let reading = store.graph().read();
    let refused = super::layer::reset_graph("l1", store.graph(), Duration::from_millis(200));
    assert!(
        matches!(&refused, Err(ServerError::Refused { message, .. }) if message.contains("runs/cancel")),
        "{refused:?}"
    );
    drop(reading);
    drop(store);
    assert_eq!(served_count(&loader, "l1"), 7, "nothing changed");
    loader.shutdown().unwrap();
}
