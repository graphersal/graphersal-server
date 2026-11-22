//! Tests of the loader: synthetic stores (the `modern` and `large` samples) in a temporary
//! server directory, real session threads.

use std::path::PathBuf;
use std::sync::mpsc::channel;

use graphersal::persist::StoreOptions;
use graphersal::prelude::{GraphSource, TraversalGraph};
use graphersal::storage::BaseRef;
use graphersal_serve::{Exposure, layer_base_of_store};
use graphersal_session::RunOptions;
use serde_json::Value;

use super::*;
use crate::database::{DATABASE_FILE, STORE_DIR};
use crate::layout::ServerDir;
use crate::registry::tests::{add_db, server};
use crate::test_support::{TempDir, skip_on_ci};

const WAIT: Duration = Duration::from_secs(120);

fn check() -> HostCheck {
    HostCheck::new(Exposure::Loopback, 8484, "http://127.0.0.1:8484/")
}

fn store_path(dir: &ServerDir, name: &str) -> PathBuf {
    dir.databases().join(name).join(STORE_DIR)
}

/// A database `name` with the configuration text `config` and a closed Store holding `graph`.
fn store_db(dir: &ServerDir, name: &str, config: &str, graph: TraversalGraph) {
    add_db(dir, name, Some(config), false);
    Store::create_with(store_path(dir, name), graph, StoreOptions::new())
        .expect("the store is created")
        .close()
        .expect("the store closes");
}

/// A base database `name` holding the modern sample (`database.toml` as written by hand:
/// `read_only` not set).
fn base_db(dir: &ServerDir, name: &str) {
    store_db(
        dir,
        name,
        "role = \"base\"",
        TraversalGraph::tinkerpop_modern(),
    );
}

/// A layer database `name` over the base database `base` (an empty closed layer store).
fn layer_db(dir: &ServerDir, name: &str, base: &str) {
    add_db(
        dir,
        name,
        Some(&format!("kind = \"layer\"\nbase = \"{base}\"")),
        false,
    );
    let opened = layer_base_of_store(store_path(dir, base)).expect("the base");
    Store::<LayeredGraph>::create_layer(store_path(dir, name), &opened.base, StoreOptions::new())
        .expect("the layer store is created")
        .close()
        .expect("the layer store closes");
}

fn loader_of(dir: &ServerDir) -> Arc<Loader> {
    Loader::new(Registry::scan(dir).expect("the scan"), check())
}

/// Loads `name` and waits for the end of the load.
fn load(loader: &Arc<Loader>, name: &str) -> DbState {
    assert_eq!(
        loader.load(name).expect("the load starts"),
        LoadStart::Started
    );
    loader.wait(name, WAIT).expect("the state")
}

fn state(loader: &Loader, name: &str) -> DbState {
    loader.registry().get(name).expect("the entry").state
}

/// The graph info of the session behind `context`.
fn info(context: &Context) -> Value {
    context
        .call(|session| session.info().map_err(String::from))
        .expect("the session answers")
        .expect("the info")
}

fn vertices(context: &Context) -> i64 {
    info(context)["vertexCount"].as_i64().expect("a count")
}

/// Runs `script`: `Ok(answer)` or `Err(error text)`.
fn run(context: &Context, script: &'static str) -> Result<Value, String> {
    let text = context
        .call(move |session| session.execute(script, &RunOptions::default()))
        .expect("the session answers");
    let answer: Value = serde_json::from_str(&text).expect("a JSON answer");
    if answer["ok"] == Value::Bool(false) {
        return Err(answer["execution"]["error"].to_string());
    }
    Ok(answer)
}

const WRITE: &str = r#"g.add_v("person").iterate()"#;

/// The vertex count of the closed plain Store of `name`, and whether it was closed cleanly.
fn closed_store(dir: &ServerDir, name: &str) -> (usize, bool) {
    let clean = Store::inspect(store_path(dir, name))
        .expect("inspect")
        .clean;
    let store = Store::open(store_path(dir, name), StoreOptions::new()).expect("the store opens");
    let count = store.graph().read().vertex_count();
    store.close().expect("the store closes");
    (count, clean)
}

#[test]
fn a_plain_database_loads_and_unloads_cleanly() {
    let temp = TempDir::new("loader-plain");
    let dir = server(&temp);
    store_db(&dir, "acme", "", TraversalGraph::tinkerpop_modern());
    let loader = loader_of(&dir);
    assert!(loader.context("acme").is_none());
    assert_eq!(load(&loader, "acme"), DbState::Loaded);
    assert_eq!(loader.loaded(), ["acme"]);
    // A second load is no error.
    assert_eq!(loader.load("ACME").unwrap(), LoadStart::AlreadyLoaded);
    let context = loader.context("acme").expect("served");
    assert_eq!(context.mount_path(), Some("/db/acme"));
    assert!(
        context.store().is_some(),
        "served as the session's database"
    );
    assert!(!context.is_read_only());
    assert_eq!(vertices(&context), 6);
    run(&context, WRITE).expect("a write");
    drop(context);
    assert!(loader.unload("acme").unwrap());
    assert_eq!(state(&loader, "acme"), DbState::Unloaded);
    assert!(loader.context("acme").is_none());
    assert!(!loader.unload("acme").unwrap(), "not loaded any more");
    assert_eq!(closed_store(&dir, "acme"), (7, true));
    // Loaded again: the lock was released.
    assert_eq!(load(&loader, "acme"), DbState::Loaded);
    assert_eq!(vertices(&loader.context("acme").unwrap()), 7);
    loader.shutdown().expect("the shutdown");
    assert!(loader.loaded().is_empty());
    assert_eq!(closed_store(&dir, "acme"), (7, true));
}

#[test]
fn a_layer_and_its_base_share_one_layer_base() {
    let temp = TempDir::new("loader-layers");
    let dir = server(&temp);
    base_db(&dir, "base");
    layer_db(&dir, "l1", "base");
    layer_db(&dir, "l2", "base");
    let loader = loader_of(&dir);

    // The base enters the pool with its first layer, without its own HTTP graph.
    assert_eq!(load(&loader, "l1"), DbState::Loaded);
    assert_eq!(state(&loader, "base"), DbState::Unloaded);
    assert!(loader.context("base").is_none());
    assert_eq!(
        loader.pooled_bases(),
        BTreeMap::from([("base".to_string(), vec!["l1".to_string()])])
    );
    let pooled = loader.lock().pool["base"].base.clone();
    // S0: the identity read from the files equals the pooled base's.
    let inspected = Store::inspect(store_path(&dir, "base")).expect("inspect");
    assert_eq!(
        pooled.base_ref(),
        Some(&BaseRef::new(
            inspected.graph_id,
            inspected.commit_seq,
            inspected.last_commit_at
        ))
    );
    let base_graph_of = |name: &str| -> Arc<TraversalGraph> {
        let state = loader.lock();
        let db = &state.loaded[name];
        match (&db.layer_store, &db.served_layer) {
            (Some(store), _) => store.graph().read().base().clone(),
            (None, Some(layer)) => layer.read().base().clone(),
            (None, None) => panic!("{name} is no layer"),
        }
    };
    assert!(Arc::ptr_eq(&base_graph_of("l1"), pooled.graph()));

    // A second layer and the base loaded for reading: the same Arc.
    assert_eq!(load(&loader, "l2"), DbState::Loaded);
    assert_eq!(load(&loader, "base"), DbState::Loaded);
    assert!(Arc::ptr_eq(&base_graph_of("l2"), pooled.graph()));
    assert!(Arc::ptr_eq(&base_graph_of("base"), pooled.graph()));
    assert_eq!(
        loader.pooled_bases()["base"],
        ["base".to_string(), "l1".to_string(), "l2".to_string()]
    );

    // The layers write their own data; the base is read-only although database.toml does not
    // say so.
    let l1 = loader.context("l1").unwrap();
    run(&l1, WRITE).expect("a layer writes");
    assert_eq!(vertices(&l1), 7);
    assert_eq!(vertices(&loader.context("l2").unwrap()), 6);
    let base = loader.context("base").unwrap();
    assert!(base.is_read_only());
    let refused = run(&base, WRITE).expect_err("the base refuses");
    assert!(refused.contains("read-only"), "{refused}");
    assert!(refused.contains("base database"), "{refused}");
    assert_eq!(vertices(&base), 6);
    assert!(matches!(
        loader.set_read_only("base", false),
        Err(ServerError::Refused { .. })
    ));
    drop((l1, base));

    // Unloading the base is refused while a loaded layer uses it.
    let err = loader.unload("base").expect_err("refused");
    assert!(err.to_string().contains("l1, l2"), "{err}");
    assert!(loader.unload("l1").unwrap());
    assert!(loader.unload("l2").unwrap());
    assert_eq!(loader.pooled_bases()["base"], ["base".to_string()]);
    assert!(loader.unload("base").unwrap());
    assert!(
        loader.pooled_bases().is_empty(),
        "released with its last user"
    );

    // A layer alone: the base leaves the pool with it.
    assert_eq!(load(&loader, "l1"), DbState::Loaded);
    assert_eq!(
        vertices(&loader.context("l1").unwrap()),
        7,
        "the layer kept its commit"
    );
    assert_eq!(loader.pooled_bases().len(), 1);
    assert!(loader.unload("l1").unwrap());
    assert!(loader.pooled_bases().is_empty());
    let layer = Store::inspect(store_path(&dir, "l1")).expect("inspect");
    assert!(layer.clean, "the layer store closed cleanly");
}

#[test]
fn an_unload_lets_a_running_request_finish() {
    let temp = TempDir::new("loader-unload-busy");
    let dir = server(&temp);
    store_db(&dir, "acme", "", TraversalGraph::tinkerpop_modern());
    let loader = loader_of(&dir);
    assert_eq!(load(&loader, "acme"), DbState::Loaded);
    let context = loader.context("acme").unwrap();
    let (started, running) = channel();
    let request = std::thread::spawn(move || {
        context
            .call(move |session| {
                let _ = started.send(());
                std::thread::sleep(Duration::from_millis(400));
                session.execute(WRITE, &RunOptions::default())
            })
            .expect("the session answers")
    });
    running.recv().expect("the request runs");
    let begun = Instant::now();
    assert!(loader.unload("acme").unwrap());
    assert!(
        begun.elapsed() >= Duration::from_millis(200),
        "the unload waited for the request"
    );
    let answer: Value = serde_json::from_str(&request.join().unwrap()).unwrap();
    assert_eq!(answer["ok"], Value::Bool(true), "{answer}");
    assert_eq!(
        closed_store(&dir, "acme"),
        (7, true),
        "its commit is durable"
    );
}

#[test]
fn a_failed_load_is_failed_with_its_reason() {
    let temp = TempDir::new("loader-failed");
    let dir = server(&temp);
    // A store directory holding no Store.
    add_db(&dir, "broken", Some(""), true);
    // A layer whose base store is gone after the scan.
    base_db(&dir, "base");
    layer_db(&dir, "layer", "base");
    let loader = loader_of(&dir);
    std::fs::remove_dir_all(store_path(&dir, "base")).unwrap();
    std::fs::create_dir(store_path(&dir, "base")).unwrap();

    let state = load(&loader, "broken");
    assert_eq!(state.as_str(), "failed", "{state}");
    assert!(state.to_string().contains("store"), "{state}");
    assert!(loader.context("broken").is_none());
    let state = load(&loader, "layer");
    assert_eq!(state.as_str(), "failed", "{state}");
    assert!(loader.pooled_bases().is_empty());
    // Failed loads may be tried again; a database without configuration cannot be loaded.
    assert_eq!(loader.load("broken").unwrap(), LoadStart::Started);
    loader.wait("broken", WAIT).unwrap();
    add_db(&dir, "no-config", None, true);
    let loader = loader_of(&dir);
    assert!(matches!(
        loader.load("no-config"),
        Err(ServerError::DatabaseFailed { .. })
    ));
    assert!(matches!(
        loader.load("missing"),
        Err(ServerError::NotFound { .. })
    ));
}

#[test]
fn the_runtime_read_only_toggle_refuses_writes_and_is_persisted() {
    let temp = TempDir::new("loader-read-only");
    let dir = server(&temp);
    store_db(&dir, "acme", "", TraversalGraph::tinkerpop_modern());
    store_db(
        &dir,
        "frozen",
        "read_only = true",
        TraversalGraph::tinkerpop_modern(),
    );
    let loader = loader_of(&dir);
    assert_eq!(load(&loader, "acme"), DbState::Loaded);
    assert_eq!(load(&loader, "frozen"), DbState::Loaded);
    let acme = loader.context("acme").unwrap();
    run(&acme, WRITE).expect("writable");

    loader.set_read_only("acme", true).expect("turned on");
    assert!(acme.is_read_only());
    let refused = run(&acme, WRITE).expect_err("refused");
    assert!(refused.contains("read-only"), "{refused}");
    assert!(refused.contains("'acme'"), "{refused}");
    let text = std::fs::read_to_string(dir.databases().join("acme").join(DATABASE_FILE)).unwrap();
    assert!(text.contains("read_only = true"), "{text}");

    loader.set_read_only("acme", false).expect("turned off");
    run(&acme, WRITE).expect("writable again");
    assert_eq!(vertices(&acme), 8);

    // Loaded read-only from its database.toml.
    let frozen = loader.context("frozen").unwrap();
    assert!(frozen.is_read_only());
    run(&frozen, WRITE).expect_err("refused");
    loader.set_read_only("frozen", false).unwrap();
    run(&frozen, WRITE).expect("writable");
    drop((acme, frozen));
    loader.shutdown().unwrap();
}

#[test]
fn loading_a_large_database_does_not_delay_requests_to_a_loaded_one() {
    if skip_on_ci("timing") {
        return;
    }
    let temp = TempDir::new("loader-large");
    let dir = server(&temp);
    store_db(&dir, "small", "", TraversalGraph::tinkerpop_modern());
    store_db(&dir, "big", "", GraphSource::large_110k().into_inner());
    let loader = loader_of(&dir);
    assert_eq!(load(&loader, "small"), DbState::Loaded);
    let small = loader.context("small").unwrap();

    let begun = Instant::now();
    assert_eq!(loader.load("big").unwrap(), LoadStart::Started);
    let mut samples = 0;
    let mut slowest = Duration::ZERO;
    while state(&loader, "big") == DbState::Loading {
        let asked = Instant::now();
        assert_eq!(vertices(&small), 6);
        slowest = slowest.max(asked.elapsed());
        samples += 1;
    }
    let load_time = begun.elapsed();
    assert_eq!(loader.wait("big", WAIT).unwrap(), DbState::Loaded);
    eprintln!(
        "big loaded in {} ms; {samples} requests to small meanwhile, slowest {} ms",
        load_time.as_millis(),
        slowest.as_millis()
    );
    assert!(samples > 0, "no request ran during the load");
    assert!(
        slowest < Duration::from_millis(250),
        "a request waited {} ms",
        slowest.as_millis()
    );
    drop(small);
    loader.shutdown().unwrap();
}

#[test]
fn the_server_auto_loads_at_start_and_unloads_at_stop() {
    let temp = TempDir::new("loader-serve");
    let dir = server(&temp);
    store_db(
        &dir,
        "auto",
        "auto_load = true",
        TraversalGraph::tinkerpop_modern(),
    );
    store_db(&dir, "manual", "", TraversalGraph::tinkerpop_modern());
    base_db(&dir, "base");
    add_db(
        &dir,
        "layer",
        Some("kind = \"layer\"\nbase = \"base\"\nauto_load = true"),
        false,
    );
    {
        let opened = layer_base_of_store(store_path(&dir, "base")).unwrap();
        Store::<LayeredGraph>::create_layer(
            store_path(&dir, "layer"),
            &opened.base,
            StoreOptions::new(),
        )
        .unwrap()
        .close()
        .unwrap();
    }
    // Failed at the scan: skipped.
    add_db(&dir, "broken", Some("auto_load = true"), false);

    let running = crate::serve::start(
        dir.root(),
        crate::serve::ServeOptions::new().with_listen("127.0.0.1:0".parse().unwrap()),
    )
    .expect("the server starts");
    let loader = running.loader().clone();
    assert_eq!(loader.wait("auto", WAIT).unwrap(), DbState::Loaded);
    assert_eq!(loader.wait("layer", WAIT).unwrap(), DbState::Loaded);
    assert_eq!(state(&loader, "manual"), DbState::Unloaded);
    assert_eq!(state(&loader, "base"), DbState::Unloaded);
    assert_eq!(state(&loader, "broken").as_str(), "failed");
    run(&loader.context("auto").unwrap(), WRITE).expect("a write");
    running.stop().expect("the server stops");
    assert!(loader.loaded().is_empty());
    assert!(loader.pooled_bases().is_empty());
    assert_eq!(closed_store(&dir, "auto"), (7, true));
    assert!(Store::inspect(store_path(&dir, "layer")).unwrap().clean);
    // No load starts once the server stopped.
    assert!(matches!(
        loader.load("manual"),
        Err(ServerError::Refused { .. })
    ));
}
