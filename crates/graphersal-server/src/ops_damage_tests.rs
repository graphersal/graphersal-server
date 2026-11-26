//! Tests of verify, repair and swap (round 3 B-CRASH) and of the open-set record: synthetic
//! stores in a temporary server directory, a snapshot file damaged by hand.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use graphersal::persist::{Store, StoreOptions};
use graphersal::prelude::TraversalGraph;
use graphersal_serve::{Exposure, HostCheck};

use super::Admin;
use crate::database::STORE_DIR;
use crate::layout::ServerDir;
use crate::loader::Loader;
use crate::recovery::{OpenFile, OpenRecord};
use crate::registry::tests::{add_db, server};
use crate::registry::{DbState, Registry};
use crate::test_support::TempDir;

const WAIT: Duration = Duration::from_secs(120);

fn check() -> HostCheck {
    HostCheck::new(Exposure::Loopback, 8484, "http://127.0.0.1:8484/")
}

fn store_path(dir: &ServerDir, name: &str) -> PathBuf {
    dir.databases().join(name).join(STORE_DIR)
}

/// A plain database `name` (description "orders") with a closed Store of the modern sample.
fn plain_db(dir: &ServerDir, name: &str) {
    add_db(dir, name, Some("description = \"orders\""), false);
    Store::create_with(
        store_path(dir, name),
        TraversalGraph::tinkerpop_modern(),
        StoreOptions::new(),
    )
    .expect("the store is created")
    .close()
    .expect("the store closes");
}

/// Every file below `dir`.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Damages the snapshot data of the Store of `name`: flips bytes in the middle of its largest
/// snapshot segment file (a chunk), the way a failing disk would.
pub(crate) fn damage_snapshot(store: &Path) -> PathBuf {
    let segment = files(store)
        .into_iter()
        .filter(|p| {
            let text = p.to_string_lossy();
            text.contains("snap") && !text.ends_with(".json") && !text.contains("manifest")
        })
        .max_by_key(|p| fs::metadata(p).map(|m| m.len()).unwrap_or(0))
        .unwrap_or_else(|| panic!("a snapshot segment in {:?}", files(store)));
    let mut bytes = fs::read(&segment).unwrap();
    let middle = bytes.len() / 2;
    let end = (middle + 16).min(bytes.len());
    for byte in &mut bytes[middle..end] {
        *byte ^= 0xA5;
    }
    fs::write(&segment, bytes).unwrap();
    segment
}

fn loader_of(dir: &ServerDir) -> Arc<Loader> {
    Loader::new(Registry::scan(dir).expect("the scan"), check())
}

fn load(loader: &Arc<Loader>, name: &str) -> DbState {
    loader.load(name).expect("the load starts");
    loader.wait(name, WAIT).expect("the state")
}

#[test]
fn a_healthy_database_verifies_loaded_and_unloaded() {
    let temp = TempDir::new("damage-healthy");
    let dir = server(&temp);
    plain_db(&dir, "acme");
    let loader = loader_of(&dir);
    let admin = Admin::online(loader.clone());
    let verified = admin.verify("acme").unwrap();
    assert!(verified.facts.ok, "{:?}", verified.facts);
    assert_eq!(verified.facts.how, "files");
    assert_eq!(verified.state, DbState::Unloaded);
    assert_eq!(load(&loader, "acme"), DbState::Loaded);
    let verified = admin.verify("acme").unwrap();
    assert!(verified.facts.ok, "{:?}", verified.facts);
    assert_eq!(verified.facts.how, "open store");
    assert!(verified.facts.snapshots_checked >= 1);
    assert_eq!(verified.state, DbState::Loaded);
    let report = loader.open_report_of("acme").expect("an open report");
    assert!(report.clean_close);
    loader.shutdown().unwrap();
}

#[test]
fn a_damaged_store_loads_in_maintenance_and_repairs_into_a_new_database_then_swaps() {
    let temp = TempDir::new("damage-repair");
    let dir = server(&temp);
    plain_db(&dir, "acme");
    damage_snapshot(&store_path(&dir, "acme"));
    // Offline: the files verify with problems.
    let offline = Admin::offline(Registry::scan(&dir).unwrap());
    let verified = offline.verify("acme").unwrap();
    assert!(
        !verified.facts.ok,
        "the damage is found: {:?}",
        verified.facts
    );

    let loader = loader_of(&dir);
    let admin = Admin::online(loader.clone());
    let state = load(&loader, "acme");
    let DbState::Maintenance { damage } = &state else {
        panic!("maintenance expected, got {state}");
    };
    assert!(damage.contains("damaged item"), "{damage}");
    assert!(state.is_open());
    // Its context serves it (read-only): the context exists.
    assert!(loader.context("acme").is_some());
    let json = crate::admin_api::entry_json(&loader.registry().get("acme").unwrap(), None);
    assert_eq!(json["state"], "maintenance");
    assert!(json["damage"].as_str().unwrap().contains("damaged item"));
    let verified = admin.verify("acme").unwrap();
    assert!(matches!(verified.state, DbState::Maintenance { .. }));

    // A repair into a new database (default name), the damaged one unchanged.
    let repaired = admin.repair("acme", None).unwrap();
    assert_eq!(repaired.entry.name, "acme-repaired");
    assert_eq!(repaired.source, "acme");
    let config = repaired.entry.config.clone().unwrap();
    assert_eq!(config.description, "orders");
    assert!(!config.auto_load);
    assert!(matches!(
        loader.registry().get("acme").unwrap().state,
        DbState::Maintenance { .. }
    ));
    let json = super::repair_json(&repaired.report);
    assert!(json["damageItems"].as_u64().unwrap() >= 1, "{json}");
    // The repaired database verifies and loads healthy.
    let verified = admin.verify("acme-repaired").unwrap();
    assert!(verified.facts.ok, "{:?}", verified.facts);
    assert_eq!(load(&loader, "acme-repaired"), DbState::Loaded);
    // A second repair takes the next free name.
    let again = admin.repair("acme", None).unwrap();
    assert_eq!(again.entry.name, "acme-repaired-2");
    admin.delete("acme-repaired-2").unwrap();

    // The swap: both unloaded, the copy takes the name, the old one to trash/, loaded again.
    let swapped = admin.swap("acme", "acme-repaired").unwrap();
    assert_eq!(swapped.with, "acme-repaired");
    assert!(swapped.reloaded);
    assert!(swapped.trash.starts_with(dir.trash()));
    assert!(swapped.trash.join(STORE_DIR).is_dir());
    assert!(loader.registry().get("acme-repaired").is_err());
    let config = loader.registry().get("acme").unwrap().config.unwrap();
    assert_eq!(config.description, "orders");
    assert_eq!(loader.wait("acme", WAIT).unwrap(), DbState::Loaded);
    let verified = admin.verify("acme").unwrap();
    assert!(verified.facts.ok, "{:?}", verified.facts);
    loader.shutdown().unwrap();
}

#[test]
fn layers_and_bases_are_refused_and_swaps_check_their_rules() {
    let temp = TempDir::new("damage-rules");
    let dir = server(&temp);
    plain_db(&dir, "acme");
    plain_db(&dir, "other");
    add_db(&dir, "frozen", Some("role = \"base\""), false);
    Store::create_with(
        store_path(&dir, "frozen"),
        TraversalGraph::tinkerpop_modern(),
        StoreOptions::new(),
    )
    .unwrap()
    .close()
    .unwrap();
    let admin = Admin::offline(Registry::scan(&dir).unwrap());
    let err = admin.repair("frozen", None).unwrap_err();
    assert!(err.to_string().contains("restore a backup"), "{err}");
    let err = admin.swap("acme", "acme").unwrap_err();
    assert!(err.to_string().contains("its own place"), "{err}");
    let err = admin.swap("acme", "frozen").unwrap_err();
    assert!(err.to_string().contains("plain normal"), "{err}");
    admin.alias_set("orders", "other").unwrap();
    let err = admin.swap("acme", "other").unwrap_err();
    assert!(err.to_string().contains("aliases orders"), "{err}");
    // Offline, a swap works without a loader.
    admin.alias_remove("orders").unwrap();
    let swapped = admin.swap("acme", "other").unwrap();
    assert!(!swapped.reloaded);
    assert_eq!(admin.registry().list().len(), 2);
}

#[test]
fn the_loader_records_its_open_set_in_server_open() {
    let temp = TempDir::new("damage-open-set");
    let dir = server(&temp);
    plain_db(&dir, "acme");
    plain_db(&dir, "beta");
    let loader = loader_of(&dir);
    loader.set_open_record(Arc::new(OpenRecord::new(dir.clone(), 1)));
    assert_eq!(
        OpenFile::read(&dir).unwrap().unwrap().databases,
        Vec::<String>::new()
    );
    load(&loader, "acme");
    load(&loader, "beta");
    assert_eq!(
        OpenFile::read(&dir).unwrap().unwrap().databases,
        ["acme", "beta"]
    );
    loader.unload("acme").unwrap();
    assert_eq!(OpenFile::read(&dir).unwrap().unwrap().databases, ["beta"]);
    loader.shutdown().unwrap();
    assert_eq!(
        OpenFile::read(&dir).unwrap().unwrap().databases,
        Vec::<String>::new()
    );
    // The graph data is untouched by all of this.
    let store = Store::open(store_path(&dir, "acme"), StoreOptions::new()).unwrap();
    assert_eq!(store.graph().read().vertex_count(), 6);
    store.close().unwrap();
}
