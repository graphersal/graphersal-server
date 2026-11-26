//! Tests of the server backup and its restore ([`Admin::server_backup_to_file`],
//! [`crate::server_backup::restore`]): a configuration-only and a full backup restored into a
//! new directory (users, grants, aliases, MCP settings, database settings and data equal),
//! the modes of the restored files, and the refusals (a changed entry, a tampered manifest, an
//! unknown or missing entry, a newer format, a non-empty directory). Synthetic stores only.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use graphersal_serve::{Exposure, HostCheck};
use serde_json::Value;

use super::*;
use crate::archive::{ZipReader, ZipWriter, sha256_hex};
use crate::database::STORE_DIR;
use crate::registry::DbState;
use crate::registry::tests::server;
use crate::server_backup::{RestoreOptions, restore};
use crate::settings::SettingsChange;
use crate::test_support::TempDir;

const WAIT: Duration = Duration::from_secs(120);

fn offline(dir: &ServerDir) -> Admin {
    Admin::offline(Registry::scan(dir).expect("scan"))
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

/// The vertex count of the database `name`, loaded for it (a layer shows its base's too).
fn served_count(dir: &ServerDir, name: &str) -> i64 {
    let loader = Loader::new(
        Registry::scan(dir).expect("scan"),
        HostCheck::new(Exposure::Loopback, 8484, "http://127.0.0.1:8484/"),
    );
    loader.load(name).expect("starts");
    assert_eq!(loader.wait(name, WAIT).expect("state"), DbState::Loaded);
    let context = loader.context(name).expect("loaded");
    let info = context
        .call(|session| session.info().map_err(String::from))
        .expect("answers")
        .expect("info");
    drop(context);
    loader.shutdown().expect("unloads");
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

/// A server with a plain database `acme` (3 persons, a description, auto-load, a query limit),
/// a base `prod` (modern), a layer `l1` over it (2 more vertices), the alias `current` ->
/// `acme`, the user `bob` (read on `acme`, write on the alias) and MCP hiding `acme`. Answers
/// bob's token.
fn populated(dir: &ServerDir) -> String {
    let admin = offline(dir);
    admin
        .create("acme", &CreateSpec::plain().with_description("Acme"))
        .unwrap();
    add_plain(dir, "acme", 3);
    modern_base(&admin, "prod");
    admin.create("l1", &CreateSpec::layer("prod")).unwrap();
    {
        let loader = Loader::new(
            Registry::scan(dir).unwrap(),
            HostCheck::new(Exposure::Loopback, 8484, "http://127.0.0.1:8484/"),
        );
        loader.load("l1").unwrap();
        assert_eq!(loader.wait("l1", WAIT).unwrap(), DbState::Loaded);
        let context = loader.context("l1").unwrap();
        for _ in 0..2 {
            context
                .call(|session| {
                    session.execute(
                        r#"g.add_v("person").iterate()"#,
                        &graphersal_session::RunOptions::default(),
                    )
                })
                .unwrap();
        }
        drop(context);
        loader.shutdown().unwrap();
    }
    let admin = offline(dir);
    admin.alias_set("current", "acme").unwrap();
    let token = admin.user_add("bob", false).unwrap();
    admin.grant_set("user:bob", "acme", "read").unwrap();
    admin.grant_set("user:bob", "current", "write").unwrap();
    admin.mcp_set_name("acme", Some(false), None).unwrap();
    admin
        .change_settings(
            "acme",
            &SettingsChange {
                description: None,
                read_only: None,
                auto_load: Some(true),
                limits: vec![("timeout_ms".to_string(), Some(5000))],
            },
        )
        .unwrap();
    token.expose().to_string()
}

fn read(path: PathBuf) -> String {
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// The archive's entries (name, bytes).
fn entries(archive: &Path) -> Vec<(String, Vec<u8>)> {
    let bytes = fs::read(archive).unwrap();
    let mut reader = ZipReader::new(bytes.as_slice());
    let mut out = Vec::new();
    while let Some(mut entry) = reader.next_entry().unwrap() {
        let name = entry.name.clone();
        let mut data = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut data).unwrap();
        entry.finish().unwrap();
        out.push((name, data));
    }
    out
}

/// Writes `entries` as a new archive at `path`.
fn write_entries(path: &Path, entries: &[(String, Vec<u8>)]) {
    let mut zip = ZipWriter::new(Vec::new());
    for (name, data) in entries {
        zip.add_bytes(name, data).unwrap();
    }
    fs::write(path, zip.finish().unwrap()).unwrap();
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_configuration_backup_restores_users_grants_aliases_and_settings() {
    let temp = TempDir::new("conf-config");
    let dir = server(&temp);
    let _bob = populated(&dir);
    let archive = temp.path().join("srv-backup.zip");
    let made = offline(&dir)
        .server_backup_to_file(false, &archive)
        .expect("a backup");
    assert!(!made.manifest.with_databases);
    assert_eq!(made.manifest.source_dir, "srv");
    assert!(
        made.file_name.starts_with("srv-server-backup-"),
        "{}",
        made.file_name
    );
    assert_eq!(fs::metadata(&archive).unwrap().len(), made.bytes);
    #[cfg(unix)]
    assert_eq!(mode(&archive), 0o600);
    assert!(!temp.path().join("srv-backup.zip.part").exists());

    // What it holds: the configuration, every database.toml, the models list, the manifest
    // last; never sessions, run files, tmp or backups.
    let names: Vec<String> = entries(&archive).into_iter().map(|(n, _)| n).collect();
    for wanted in [
        "server.toml",
        "aliases.toml",
        "access.toml",
        "mcp.toml",
        "databases/acme/database.toml",
        "databases/prod/database.toml",
        "databases/l1/database.toml",
        "models.json",
    ] {
        assert!(names.contains(&wanted.to_string()), "{wanted}: {names:?}");
    }
    assert_eq!(names.last().map(String::as_str), Some("manifest.json"));
    assert!(
        names
            .iter()
            .all(|n| !n.starts_with("sessions") && !n.contains("store/")),
        "{names:?}"
    );
    let manifest: Value =
        serde_json::from_slice(&entries(&archive).last().unwrap().1).expect("JSON");
    assert_eq!(manifest["format"], "graphersal-server-backup");
    assert_eq!(manifest["formatVersion"], 1);
    let excluded: Vec<&str> = manifest["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    for path in [
        "sessions/",
        "tmp/",
        "trash/",
        "backups/",
        "databases/acme/store/",
    ] {
        assert!(excluded.contains(&path), "{path}: {excluded:?}");
    }
    for file in manifest["files"].as_array().unwrap() {
        assert_eq!(file["sha256"].as_str().unwrap().len(), 64, "{file}");
    }

    // Restored into a new directory: the same files, no database (no data in the archive).
    let new = temp.path().join("new");
    let report = restore(&archive, &new, RestoreOptions::new()).expect("restored");
    let new_dir = ServerDir::new(&new);
    for file in ["server.toml", "aliases.toml", "access.toml", "mcp.toml"] {
        assert_eq!(
            read(new.join(file)),
            read(dir.root().join(file)),
            "{file} restored as it was"
        );
    }
    assert!(report.databases.is_empty());
    let mut skipped: Vec<&str> = report.skipped.iter().map(|(n, _)| n.as_str()).collect();
    skipped.sort();
    assert_eq!(skipped, ["acme", "l1", "prod"]);
    assert!(
        report.notes.iter().any(|n| n.contains("current")),
        "{:?}",
        report.notes
    );
    assert_eq!(fs::read_dir(new_dir.databases()).unwrap().count(), 0);
    for sub in ["tmp", "trash", "imports", "sessions", "models"] {
        assert!(new.join(sub).is_dir(), "{sub}");
    }
    assert!(!new.join("server.run").exists() && !new.join("recovery.json").exists());
    #[cfg(unix)]
    {
        assert_eq!(mode(&new), 0o700);
        for file in ["access.toml", "server.toml", "aliases.toml", "mcp.toml"] {
            assert_eq!(mode(&new.join(file)), 0o600, "{file}");
        }
        for sub in ["sessions", "databases", "models", "tmp", "backups"] {
            assert_eq!(mode(&new.join(sub)), 0o700, "{sub}");
        }
        crate::security::check(&new_dir).expect("a private directory");
    }
    // The users' token hashes came along: bob's token is still bob's.
    let access = crate::access::AccessFile::read(&new_dir).unwrap().unwrap();
    assert_eq!(
        access.revision(),
        crate::access::AccessFile::read(&dir)
            .unwrap()
            .unwrap()
            .revision()
    );
}

#[test]
fn a_full_backup_restores_every_database_with_its_data_and_settings() {
    let temp = TempDir::new("conf-full");
    let dir = server(&temp);
    populated(&dir);
    let archive = temp.path().join("full.zip");
    let made = offline(&dir)
        .server_backup_to_file(true, &archive)
        .expect("a full backup");
    assert!(made.manifest.with_databases);
    assert!(
        made.manifest.databases.iter().all(|d| d.included),
        "{:?}",
        made.manifest.databases
    );
    let names: Vec<String> = entries(&archive).into_iter().map(|(n, _)| n).collect();
    for db in ["acme", "prod", "l1"] {
        assert!(
            names.contains(&format!("databases/{db}/backup.zip")),
            "{names:?}"
        );
        assert!(
            names.contains(&format!("databases/{db}/backup.toml")),
            "{names:?}"
        );
    }
    // Nothing was left in tmp/ or backups/ of the source.
    assert_eq!(fs::read_dir(dir.tmp()).unwrap().count(), 0);
    assert!(
        fs::read_dir(dir.backups())
            .unwrap()
            .flatten()
            .all(|e| e.file_name() == "jobs")
    );

    let new = temp.path().join("moved");
    let report = restore(&archive, &new, RestoreOptions::new()).expect("restored");
    let mut restored = report.databases.clone();
    restored.sort();
    assert_eq!(restored, ["acme", "l1", "prod"]);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    let new_dir = ServerDir::new(&new);
    for db in ["acme", "prod", "l1"] {
        assert_eq!(
            read(new_dir.databases().join(db).join("database.toml")),
            read(dir.databases().join(db).join("database.toml")),
            "{db}'s settings"
        );
    }
    assert_eq!(plain_count(&new_dir, "acme"), 3);
    assert_eq!(served_count(&new_dir, "prod"), 6);
    assert_eq!(served_count(&new_dir, "l1"), 8);
    // The embedded backups were not kept; the registry reads it as the source was.
    assert!(
        fs::read_dir(new_dir.backups())
            .unwrap()
            .flatten()
            .all(|e| e.file_name() == "jobs")
    );
    let registry = Registry::scan(&new_dir).unwrap();
    assert_eq!(
        registry.aliases().get("current").map(String::as_str),
        Some("acme")
    );
    let acme = registry.get("acme").unwrap().config.unwrap();
    assert!(acme.auto_load);
    assert_eq!(acme.description, "Acme");
    for file in ["aliases.toml", "access.toml", "mcp.toml"] {
        assert_eq!(read(new.join(file)), read(dir.root().join(file)), "{file}");
    }
    #[cfg(unix)]
    crate::security::check(&new_dir).expect("a private directory");
}

/// The archive of `dir` (configuration only) at `temp/<name>`.
fn config_archive(temp: &TempDir, dir: &ServerDir, name: &str) -> PathBuf {
    let archive = temp.path().join(name);
    offline(dir).server_backup_to_file(false, &archive).unwrap();
    archive
}

/// Restoring `archive` into a new directory fails with a message containing `needle`, and
/// leaves no directory behind.
fn refused(temp: &TempDir, archive: &Path, needle: &str) {
    let new = temp.path().join("refused");
    let err = restore(archive, &new, RestoreOptions::new()).unwrap_err();
    assert!(err.to_string().contains(needle), "{needle}: {err}");
    assert!(!new.exists(), "nothing left behind after: {err}");
}

#[test]
fn a_changed_entry_or_a_tampered_manifest_is_refused() {
    let temp = TempDir::new("conf-tamper");
    let dir = server(&temp);
    populated(&dir);
    let archive = config_archive(&temp, &dir, "a.zip");
    let original = entries(&archive);

    // An entry changed after the backup (a well-formed archive: only the manifest's hash
    // finds it).
    let mut changed = original.clone();
    for (name, data) in &mut changed {
        if name == "access.toml" {
            data.extend_from_slice(b"\n[users.mallory]\nroles = [\"admin\"]\n");
        }
    }
    let path = temp.path().join("changed.zip");
    write_entries(&path, &changed);
    refused(&temp, &path, "checksum mismatch");

    // The manifest changed: another hash for a file.
    let mut tampered = original.clone();
    let manifest = tampered.last_mut().unwrap();
    let mut json: Value = serde_json::from_slice(&manifest.1).unwrap();
    json["files"][0]["sha256"] = Value::String(sha256_hex(b"something else"));
    manifest.1 = serde_json::to_vec(&json).unwrap();
    let path = temp.path().join("tampered.zip");
    write_entries(&path, &tampered);
    refused(&temp, &path, "checksum mismatch");

    // The manifest is not JSON / missing.
    let mut broken = original.clone();
    broken.last_mut().unwrap().1 = b"{ not json".to_vec();
    let path = temp.path().join("broken.zip");
    write_entries(&path, &broken);
    refused(&temp, &path, "not JSON");
    let path = temp.path().join("nomanifest.zip");
    write_entries(&path, &original[..original.len() - 1]);
    refused(&temp, &path, "no manifest.json");

    // An entry the manifest does not list, one it lists that is missing.
    let mut extra = original.clone();
    extra.insert(0, ("databases/evil/database.toml".to_string(), Vec::new()));
    let path = temp.path().join("extra.zip");
    write_entries(&path, &extra);
    refused(&temp, &path, "not in the manifest");
    let missing: Vec<(String, Vec<u8>)> = original
        .iter()
        .filter(|(n, _)| n != "aliases.toml")
        .cloned()
        .collect();
    let path = temp.path().join("missing.zip");
    write_entries(&path, &missing);
    refused(&temp, &path, "not in the archive");

    // A damaged byte: the ZIP's CRC finds it.
    let mut bytes = fs::read(&archive).unwrap();
    let at = bytes
        .windows(6)
        .position(|w| w == b"listen")
        .expect("server.toml's text");
    bytes[at] ^= 0x20;
    let path = temp.path().join("damaged.zip");
    fs::write(&path, &bytes).unwrap();
    refused(&temp, &path, "CRC-32");

    // A newer format: refused, unless forced.
    let mut newer = original.clone();
    let manifest = newer.last_mut().unwrap();
    let mut json: Value = serde_json::from_slice(&manifest.1).unwrap();
    json["formatVersion"] = Value::from(2);
    manifest.1 = serde_json::to_vec(&json).unwrap();
    let path = temp.path().join("newer.zip");
    write_entries(&path, &newer);
    refused(&temp, &path, "--force-version");
    let forced = temp.path().join("forced");
    restore(
        &path,
        &forced,
        RestoreOptions::new().with_force_version(true),
    )
    .expect("forced");
    assert!(forced.join("access.toml").is_file());

    // Not a server backup at all: a database's own backup.
    let db_backup = offline(&dir).backup("acme").unwrap();
    refused(&temp, &db_backup.file, "no manifest.json");
}

#[test]
fn a_restore_never_writes_into_an_existing_directory() {
    let temp = TempDir::new("conf-nonempty");
    let dir = server(&temp);
    populated(&dir);
    let archive = config_archive(&temp, &dir, "a.zip");
    // A non-empty directory (the source itself included) is refused and left alone.
    let busy = temp.path().join("busy");
    fs::create_dir_all(&busy).unwrap();
    fs::write(busy.join("keep.txt"), "mine").unwrap();
    for target in [busy.as_path(), dir.root()] {
        let err = restore(&archive, target, RestoreOptions::new()).unwrap_err();
        assert!(
            matches!(err, ServerError::Refused { .. }),
            "{}: {err}",
            target.display()
        );
        assert!(err.to_string().contains("NEW server directory"), "{err}");
    }
    assert_eq!(read(busy.join("keep.txt")), "mine");
    assert_eq!(fs::read_dir(&busy).unwrap().count(), 1);
    // An empty directory is fine.
    let empty = temp.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    restore(&archive, &empty, RestoreOptions::new()).expect("into an empty directory");
    assert!(empty.join("server.toml").is_file());
}

/// Round 4 B-MCPDEF: the databases a server backup restores are not new names: they keep the
/// MCP exposure the source had, also when the archive has no `mcp.toml` (a directory of an
/// older version: every name offered, MCP on).
#[test]
fn a_restored_server_keeps_its_mcp_exposure_without_mcp_toml() {
    let temp = TempDir::new("conf-mcp");
    let dir = server(&temp);
    offline(&dir)
        .exposing(Some(true))
        .create("acme", &CreateSpec::plain())
        .unwrap();
    add_plain(&dir, "acme", 2);
    fs::remove_file(dir.mcp_settings_path()).unwrap();
    let archive = temp.path().join("full.zip");
    offline(&dir)
        .server_backup_to_file(true, &archive)
        .expect("a full backup");
    let new = temp.path().join("moved");
    let report = restore(&archive, &new, RestoreOptions::new()).expect("restored");
    assert_eq!(report.databases, ["acme"]);
    let new_dir = ServerDir::new(&new);
    let control = crate::mcp_control::McpControl::open(&new_dir).unwrap();
    assert!(control.enabled(), "{:?}", control.settings());
    assert!(control.exposed("acme", "acme"), "{:?}", control.settings());
}
