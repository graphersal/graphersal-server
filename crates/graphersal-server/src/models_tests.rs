//! Tests of the shared models: adoption by hard link (one inode for every database), the copy
//! fallback, exports, `needs-model` resolution by `models add`, adding from a model directory
//! and by repository, the removal refusal, the admin routes and the listing fields. Synthetic
//! data only (a tiny synthetic model).

use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use graphersal::persist::{ImportModelOptions, StoreOptions};
use graphersal_serve::{Exposure, HostCheck, Method, Request, Response};
use serde_json::{Value, json};

use super::*;
use crate::ops::{Admin, CreateSpec, ExportSpec, ImportSource, ModelAddSpec};
use crate::registry::tests::server;
use crate::registry::{DbState, Registry};
use crate::test_support::TempDir;
use crate::{DatabaseRole, Loader, ServerError};

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

/// Two packed snapshots of a database holding the tiny model `acme/tiny`: with and without the
/// model's blob. The source database is deleted again (into the trash).
fn snapshots(temp: &TempDir, admin: &Admin) -> (PathBuf, PathBuf) {
    let entry = admin.create("source", &CreateSpec::plain()).unwrap();
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
        .export_to_file("source", ExportSpec::new(), &with)
        .unwrap();
    let without = temp.path().join("without.gsnap");
    admin
        .export_to_file("source", ExportSpec::new().with_models(false), &without)
        .unwrap();
    admin.delete("source").unwrap();
    (with, without)
}

/// The model state of the database `name`'s Store for `repo` (`None`: not listed).
fn model_state(admin: &Admin, name: &str, repo: &str) -> Option<ModelState> {
    let entry = admin.registry().get(name).unwrap();
    Store::models_dir(entry.store_dir())
        .unwrap()
        .into_iter()
        .find(|m| m.repo == repo)
        .map(|m| m.state)
}

/// The blob files of the database `name`.
fn db_blobs(admin: &Admin, name: &str) -> Vec<(String, PathBuf, u64)> {
    blob_files(
        &admin
            .registry()
            .get(name)
            .unwrap()
            .store_dir()
            .join("blobs"),
    )
    .unwrap()
}

/// The admin route over `admin` with an optional JSON body.
fn route(admin: &Admin, method: Method, endpoint: &str, body: Option<Value>) -> (u16, Value) {
    let bytes = body.map(|b| b.to_string()).unwrap_or_default().into_bytes();
    let length = bytes.len();
    let url = format!("/api/admin/{endpoint}");
    let mut request = Request::new(method.clone(), &url, Cursor::new(bytes))
        .with_header("Content-Type", "application/json")
        .with_body_length(Some(length));
    let response: Response = match crate::admin_api::route(admin, &method, endpoint, &mut request) {
        Ok(response) => response,
        Err(err) => return (err.status(), json!({ "error": err.message() })),
    };
    let json = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (response.status(), json)
}

#[test]
fn two_databases_share_one_copy_of_a_model_and_exports_carry_it() {
    let temp = TempDir::new("models-share");
    let dir = server(&temp);
    let admin = offline(&dir);
    let (with, _) = snapshots(&temp, &admin);
    for name in ["a", "b"] {
        let imported = admin
            .import(&ImportSource::new(&with), name, None, None)
            .expect("imported");
        assert_eq!(imported.entry.state, DbState::Unloaded);
        assert!(imported.notes.is_empty(), "{:?}", imported.notes);
    }
    let index = admin.models_index().unwrap();
    assert_eq!(index.models.len(), 1, "{index:?}");
    let model = &index.models[0];
    assert_eq!(model.repo.as_deref(), Some("acme/tiny"));
    assert!(
        model
            .identity
            .as_deref()
            .unwrap()
            .starts_with("model2vec/acme/tiny@")
    );
    assert_eq!(model.license.as_deref(), Some("mit"));
    assert_eq!(model.users, ["a", "b"]);
    assert!(model.copied.is_empty(), "{model:?}");
    let shared = admin.models().blob_path(&model.blob);
    assert_eq!(fs::metadata(&shared).unwrap().len(), model.bytes);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            fs::metadata(&shared).unwrap().nlink(),
            3,
            "models + 2 databases"
        );
        assert_eq!(model.links, Some(3));
        for name in ["a", "b"] {
            let blobs = db_blobs(&admin, name);
            assert_eq!(blobs.len(), 1);
            assert_eq!(
                fs::metadata(&blobs[0].1).unwrap().ino(),
                fs::metadata(&shared).unwrap().ino()
            );
        }
    }
    let toml = fs::read_to_string(dir.models().join(MODELS_FILE)).unwrap();
    assert!(
        toml.contains("acme/tiny") && toml.contains(&model.blob),
        "{toml}"
    );

    // The export of a database whose blob is a link carries the model's bytes.
    let again = temp.path().join("again.gsnap");
    let exported = admin
        .export_to_file("a", ExportSpec::new(), &again)
        .unwrap();
    assert!(exported.models);
    admin
        .import(&ImportSource::new(&again), "c", None, None)
        .expect("re-imported");
    assert!(matches!(
        model_state(&admin, "c", "acme/tiny"),
        Some(state) if state != ModelState::Missing
    ));
    let index = admin.models_index().unwrap();
    assert_eq!(index.models[0].users, ["a", "b", "c"]);
    #[cfg(unix)]
    assert_eq!(index.models[0].links, Some(4));

    // A used model is not removed; once no database uses it, it is.
    let err = admin.model_remove("acme/tiny").unwrap_err();
    assert!(
        matches!(&err, ServerError::Refused { message, .. } if message.contains("a, b, c")),
        "{err}"
    );
    assert!(shared.is_file());
    for name in ["a", "b", "c"] {
        admin.delete(name).unwrap();
    }
    let removed = admin.model_remove("acme/tiny").unwrap();
    assert_eq!(removed.blobs, std::slice::from_ref(&model.blob));
    assert_eq!(removed.bytes, model.bytes);
    assert!(!shared.exists());
    assert!(admin.models_index().unwrap().models.is_empty());
    assert!(matches!(
        admin.model_remove("acme/tiny"),
        Err(ServerError::ModelNotFound { .. })
    ));
}

#[test]
fn the_copy_fallback_keeps_the_data_and_warns() {
    let temp = TempDir::new("models-copy");
    let dir = server(&temp);
    let mut admin = offline(&dir);
    let (with, _) = snapshots(&temp, &admin);
    admin.no_links = true;
    let imported = admin
        .import(&ImportSource::new(&with), "a", None, None)
        .expect("imported");
    assert!(
        imported
            .notes
            .iter()
            .any(|n| n.contains("copied, not linked")),
        "{:?}",
        imported.notes
    );
    assert!(matches!(
        model_state(&admin, "a", "acme/tiny"),
        Some(state) if state != ModelState::Missing
    ));
    let index = admin.models_index().unwrap();
    assert_eq!(index.models.len(), 1);
    let model = &index.models[0];
    assert_eq!(model.users, ["a"]);
    let shared = admin.models().blob_path(&model.blob);
    let own = &db_blobs(&admin, "a")[0].1;
    assert_eq!(fs::read(&shared).unwrap(), fs::read(own).unwrap());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(model.copied, ["a"]);
        assert_ne!(
            fs::metadata(&shared).unwrap().ino(),
            fs::metadata(own).unwrap().ino()
        );
        // Surfaced in the API: the model row and the database row.
        let (status, models) = route(&admin, Method::Get, "models", None);
        assert_eq!(status, 200);
        assert_eq!(models["models"][0]["copied"], true);
        assert_eq!(models["models"][0]["copiedIn"], json!(["a"]));
        assert!(models["models"][0]["linkWarning"].is_string());
        let (_, row) = route(&admin, Method::Get, "databases/a", None);
        assert!(
            row["linkWarning"].as_str().unwrap().contains("copied"),
            "{row}"
        );
    }
    // The copy loads and exports like a link.
    let out = temp.path().join("a.gsnap");
    assert!(
        admin
            .export_to_file("a", ExportSpec::new(), &out)
            .unwrap()
            .models
    );
    let loader = loader_of(&dir);
    loader.load("a").unwrap();
    assert_eq!(loader.wait("a", WAIT).unwrap(), DbState::Loaded);
    loader.shutdown().unwrap();
}

#[test]
fn models_add_makes_a_needs_model_database_loadable() {
    let temp = TempDir::new("models-resolve");
    let dir = server(&temp);
    let admin = offline(&dir);
    let (with, without) = snapshots(&temp, &admin);
    let imported = admin
        .import(&ImportSource::new(&without), "w", None, None)
        .unwrap();
    assert_eq!(imported.entry.state, DbState::needs_model("acme/tiny"));
    let index = admin.models_index().unwrap();
    assert!(index.models.is_empty());
    assert_eq!(index.missing, [("w".to_string(), "acme/tiny".to_string())]);

    let added = admin.model_add(&ModelAddSpec::path(&with)).expect("added");
    assert_eq!(added.added.len(), 1, "{added:?}");
    assert_eq!(added.added[0].repo.as_deref(), Some("acme/tiny"));
    assert_eq!(added.resolved, ["w"]);
    assert!(added.missing.is_empty(), "{added:?}");
    let entry = admin.registry().get("w").unwrap();
    assert_eq!(entry.state, DbState::Unloaded);
    assert_eq!(entry.config.as_ref().unwrap().needs_model, None);
    assert!(matches!(
        model_state(&admin, "w", "acme/tiny"),
        Some(state) if state != ModelState::Missing
    ));
    assert_eq!(added.added[0].users, ["w"]);
    #[cfg(unix)]
    assert!(
        added.added[0].copied.is_empty(),
        "the restored blob is a link"
    );

    // The marker is gone for good (a new scan) and the database loads.
    let loader = loader_of(&dir);
    assert_eq!(loader.registry().get("w").unwrap().state, DbState::Unloaded);
    loader.load("w").unwrap();
    assert_eq!(loader.wait("w", WAIT).unwrap(), DbState::Loaded);
    loader.shutdown().unwrap();
}

#[test]
fn models_from_a_directory_by_repository_and_without_a_definition() {
    let temp = TempDir::new("models-dir");
    let dir = server(&temp);
    let admin = offline(&dir);
    let source = temp.path().join("tiny-model");
    crate::test_models::tiny_model_dir(2, &source);
    let added = admin
        .model_add(
            &ModelAddSpec::path(&source)
                .with_repo(Some("acme/dir".to_string()))
                .with_license(Some("mit".to_string())),
        )
        .expect("added from a directory");
    assert_eq!(added.added.len(), 1);
    let model = added.added[0].clone();
    assert_eq!(model.repo.as_deref(), Some("acme/dir"));
    assert!(model.users.is_empty());
    #[cfg(unix)]
    assert_eq!(model.links, Some(1), "the scratch Store is gone");
    assert!(
        fs::read_dir(dir.tmp()).unwrap().next().is_none(),
        "no scratch left in tmp/"
    );
    // By repository: a model the shared models hold; an unknown one is refused.
    let again = admin.model_add(&ModelAddSpec::repo("acme/dir")).unwrap();
    assert_eq!(again.added[0].blob, model.blob);
    assert!(matches!(
        admin.model_add(&ModelAddSpec::repo("nobody/none")),
        Err(ServerError::ModelNotFound { .. })
    ));

    // A Store without the model's definition gets it from the blob's files.
    let entry = admin.create("e", &CreateSpec::plain()).unwrap();
    let resolution = admin
        .models()
        .resolve(&entry.store_dir(), &["acme/dir".to_string()]);
    assert_eq!(resolution.resolved, ["acme/dir"], "{resolution:?}");
    let listed = Store::models_dir(entry.store_dir()).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(Some(listed[0].identity.as_str()), model.identity.as_deref());
    let err = admin.model_remove("acme/dir").unwrap_err();
    assert!(matches!(err, ServerError::Refused { .. }), "{err}");
}

#[test]
fn the_models_routes_and_the_listing_fields() {
    let temp = TempDir::new("models-routes");
    let dir = server(&temp);
    let admin = offline(&dir);
    let (with, _) = snapshots(&temp, &admin);
    let (status, body) = route(
        &admin,
        Method::Post,
        "models",
        Some(json!({ "path": with.to_string_lossy() })),
    );
    assert_eq!(status, 201, "{body}");
    assert_eq!(body["added"][0]["repo"], "acme/tiny");
    let (status, body) = route(
        &admin,
        Method::Post,
        "models",
        Some(json!({ "path": "relative.gsnap" })),
    );
    assert_eq!(status, 400, "{body}");
    let (status, body) = route(
        &admin,
        Method::Post,
        "models",
        Some(json!({ "repo": "nobody/none" })),
    );
    assert_eq!(status, 404, "{body}");
    assert!(
        body["help"]
            .as_str()
            .unwrap()
            .contains("GET /api/admin/models")
    );
    admin
        .import(&ImportSource::new(&with), "a", None, None)
        .unwrap();
    let (status, body) = route(&admin, Method::Delete, "models/acme/tiny", None);
    assert_eq!(status, 409, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("databases a use"),
        "{body}"
    );
    assert!(
        body["help"]
            .as_str()
            .unwrap()
            .contains("delete those databases")
    );
    let (status, body) = route(&admin, Method::Get, "models", None);
    assert_eq!(status, 200);
    assert_eq!(body["models"][0]["users"], json!(["a"]));
    assert_eq!(body["models"][0]["copied"], false);
    assert_eq!(route(&admin, Method::Put, "models", None).0, 405);

    // The listing: sizeBytes; the 409s with their help.
    let (_, listing) = route(&admin, Method::Get, "databases", None);
    let row = &listing["databases"][0];
    assert_eq!(row["name"], "a");
    assert!(row["sizeBytes"].as_u64().unwrap() > 0, "{row}");
    assert!(row.get("linkWarning").is_none(), "{row}");
    let (status, body) = route(
        &admin,
        Method::Post,
        "databases",
        Some(json!({"name": "a"})),
    );
    assert_eq!(status, 409);
    assert!(
        body["help"].as_str().unwrap().contains("another name"),
        "{body}"
    );
    admin
        .create("prod", &CreateSpec::plain().with_role(DatabaseRole::Base))
        .unwrap();
    admin.create("l1", &CreateSpec::layer("prod")).unwrap();
    let (status, body) = route(
        &admin,
        Method::Put,
        "databases/prod/read-only",
        Some(json!({"readOnly": false})),
    );
    assert_eq!(status, 409);
    assert!(
        body["help"]
            .as_str()
            .unwrap()
            .contains("\"base\": \"prod\""),
        "{body}"
    );
    let (status, body) = route(&admin, Method::Delete, "databases/prod", None);
    assert_eq!(status, 409);
    assert!(
        body["help"]
            .as_str()
            .unwrap()
            .contains("DELETE /api/admin/databases/l1"),
        "{body}"
    );

    // Online: a base held in the pool by its loaded layer; its unload refused with help.
    let loader = loader_of(&dir);
    let online = Admin::online(loader.clone());
    for name in ["prod", "l1"] {
        loader.load(name).unwrap();
        assert_eq!(loader.wait(name, WAIT).unwrap(), DbState::Loaded);
    }
    let (_, row) = route(&online, Method::Get, "databases/prod", None);
    assert_eq!(row["pooled"], true, "{row}");
    assert_eq!(row["pooledBy"], json!(["l1"]));
    let (_, row) = route(&online, Method::Get, "databases/l1", None);
    assert!(row.get("pooled").is_none());
    let (status, body) = route(&online, Method::Post, "databases/prod/unload", None);
    assert_eq!(status, 409, "{body}");
    assert!(
        body["help"]
            .as_str()
            .unwrap()
            .contains("POST /api/admin/databases/l1/unload"),
        "{body}"
    );
    loader.shutdown().unwrap();
}

/// The hard-link count of the shared blob of `acme/tiny` (unix), and that every database in
/// `names` holds that same file.
#[cfg(unix)]
fn shared_links(admin: &Admin, names: &[&str]) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let index = admin.models_index().unwrap();
    let model = index.of_repo("acme/tiny").next().expect("in models/");
    let shared = fs::metadata(admin.models().blob_path(&model.blob)).unwrap();
    for name in names {
        let blobs = db_blobs(admin, name);
        assert_eq!(blobs.len(), 1, "{name}");
        assert_eq!(
            fs::metadata(&blobs[0].1).unwrap().ino(),
            shared.ino(),
            "{name} links the shared blob"
        );
    }
    shared.nlink()
}

/// An import of a source that names `acme/tiny` without carrying it, as `name`, resolved from
/// `models/`: usable at once (unloaded, no marker, the model present).
fn assert_resolved_at_import(admin: &Admin, imported: &crate::ops::Imported, name: &str) {
    assert_eq!(imported.entry.state, DbState::Unloaded, "{name}");
    assert!(imported.models_missing.is_empty(), "{name}: {imported:?}");
    assert_eq!(imported.models_resolved, ["acme/tiny"], "{name}");
    assert!(
        imported
            .notes
            .iter()
            .any(|n| n.contains("acme/tiny") && n.contains("taken from the shared models")),
        "{name}: {:?}",
        imported.notes
    );
    let entry = admin.registry().get(name).unwrap();
    assert_eq!(entry.state, DbState::Unloaded);
    assert_eq!(entry.config.as_ref().unwrap().needs_model, None);
    assert!(matches!(
        model_state(admin, name, "acme/tiny"),
        Some(state) if state != ModelState::Missing
    ));
}

#[test]
fn an_import_takes_a_named_model_from_the_shared_models() {
    let temp = TempDir::new("models-import-resolve");
    let dir = server(&temp);
    let admin = offline(&dir);
    let (with, without) = snapshots(&temp, &admin);
    // models/ lacks the model: needs-model as before, and a base is refused.
    let imported = admin
        .import(&ImportSource::new(&without), "early", None, None)
        .unwrap();
    assert_eq!(imported.entry.state, DbState::needs_model("acme/tiny"));
    assert_eq!(imported.models_missing, ["acme/tiny"]);
    assert!(imported.models_resolved.is_empty());
    let err = admin
        .import_as(
            &ImportSource::new(&without),
            "early-base",
            None,
            None,
            DatabaseRole::Base,
        )
        .unwrap_err();
    assert!(matches!(err, ServerError::Refused { .. }), "{err}");
    assert!(!dir.databases().join("early-base").exists());

    // The model enters models/ (and resolves the two waiting databases).
    let added = admin.model_add(&ModelAddSpec::path(&with)).unwrap();
    assert_eq!(added.resolved, ["early"]);

    // Now an import resolves it right away, also for a base.
    let imported = admin
        .import(&ImportSource::new(&without), "snap", None, None)
        .expect("imported");
    assert_resolved_at_import(&admin, &imported, "snap");
    let imported = admin
        .import_as(
            &ImportSource::new(&without),
            "base",
            None,
            None,
            DatabaseRole::Base,
        )
        .expect("a base whose model the shared models hold");
    assert_resolved_at_import(&admin, &imported, "base");
    assert_eq!(
        imported.entry.config.as_ref().unwrap().role,
        DatabaseRole::Base
    );
    assert!(
        fs::read_dir(dir.tmp()).map(|d| d.count()).unwrap_or(0) == 0,
        "no scratch left in tmp/"
    );
    let index = admin.models_index().unwrap();
    assert!(index.missing.is_empty(), "{index:?}");
    assert_eq!(index.models.len(), 1, "one blob, deduplicated");
    #[cfg(unix)]
    assert_eq!(
        shared_links(&admin, &["early", "snap", "base"]),
        4,
        "models + 3 databases"
    );

    // The resolved databases load.
    let loader = loader_of(&dir);
    for name in ["snap", "base"] {
        loader.load(name).unwrap();
        assert_eq!(loader.wait(name, WAIT).unwrap(), DbState::Loaded, "{name}");
    }
    loader.shutdown().unwrap();
}

#[test]
fn the_import_routes_take_a_named_model_from_the_shared_models() {
    let temp = TempDir::new("models-import-routes");
    let dir = server(&temp);
    let admin = offline(&dir);
    let (with, without) = snapshots(&temp, &admin);
    let body = json!({"path": without.to_str().unwrap(), "name": "waits"});
    let (status, answer) = route(&admin, Method::Post, "import", Some(body));
    assert_eq!(status, 201, "{answer}");
    assert_eq!(answer["state"], "needs-model");
    assert_eq!(answer["import"]["modelsMissing"], json!(["acme/tiny"]));
    assert_eq!(answer["import"]["modelsResolved"], json!([]));

    admin.model_add(&ModelAddSpec::path(&with)).unwrap();
    // The path form.
    let body = json!({"path": without.to_str().unwrap(), "name": "by-path"});
    let (status, answer) = route(&admin, Method::Post, "import", Some(body));
    assert_eq!(status, 201, "{answer}");
    assert_eq!(answer["state"], "unloaded");
    assert_eq!(answer["import"]["modelsMissing"], json!([]));
    assert_eq!(answer["import"]["modelsResolved"], json!(["acme/tiny"]));
    // The upload form.
    let bytes = fs::read(&without).unwrap();
    let length = bytes.len();
    let mut request = Request::new(
        Method::Post,
        "/api/admin/import?name=by-upload",
        Cursor::new(bytes),
    )
    .with_header("Content-Type", "application/octet-stream")
    .with_body_length(Some(length));
    let response =
        crate::admin_api::route(&admin, &Method::Post, "import", &mut request).expect("answer");
    let answer: Value = serde_json::from_slice(response.body()).unwrap();
    assert_eq!(response.status(), 201, "{answer}");
    assert_eq!(answer["state"], "unloaded");
    assert_eq!(answer["import"]["modelsResolved"], json!(["acme/tiny"]));
    #[cfg(unix)]
    assert_eq!(
        shared_links(&admin, &["waits", "by-path", "by-upload"]),
        4,
        "models + 3 databases"
    );
}

#[test]
fn models_add_leaves_a_marked_base_alone() {
    let temp = TempDir::new("models-skip-base");
    let dir = server(&temp);
    let admin = offline(&dir);
    let (with, _) = snapshots(&temp, &admin);
    admin
        .create("frozen", &CreateSpec::plain().with_role(DatabaseRole::Base))
        .unwrap();
    // Only a hand-edited database.toml marks a base needs-model.
    let file = dir
        .databases()
        .join("frozen")
        .join(crate::database::DATABASE_FILE);
    let text = fs::read_to_string(&file).unwrap();
    fs::write(&file, format!("needs_model = \"acme/tiny\"\n{text}")).unwrap();
    let admin = offline(&dir);
    assert_eq!(
        admin.registry().get("frozen").unwrap().state,
        DbState::needs_model("acme/tiny")
    );
    let added = admin.model_add(&ModelAddSpec::path(&with)).unwrap();
    assert!(added.resolved.is_empty(), "{added:?}");
    assert_eq!(added.added[0].waiting, ["frozen"], "{added:?}");
    assert_eq!(model_state(&admin, "frozen", "acme/tiny"), None);
    assert_eq!(
        admin.registry().get("frozen").unwrap().state,
        DbState::needs_model("acme/tiny")
    );
}

// ---------------------------------------------------------------- V18: Store fill, model upload

/// A closed Store directory `name` (outside the server directory) holding the tiny model
/// `acme/tiny` (seed 1, as in [`snapshots`]); answers it and its blob's relative name
/// `blobs/<2 hex>/<hash>`.
fn store_with_model(temp: &TempDir, name: &str) -> (PathBuf, String) {
    let dir = temp.path().join(name);
    let store = Store::create_with(
        &dir,
        graphersal::prelude::TraversalGraph::new(),
        StoreOptions::new(),
    )
    .unwrap();
    store
        .import_model(
            ModelSource::Files(crate::test_models::tiny_model(1, "acme/tiny")),
            ImportModelOptions::new(),
        )
        .expect("the model");
    store.close().unwrap();
    let blobs = blob_files(&dir.join("blobs")).unwrap();
    assert_eq!(blobs.len(), 1);
    let hash = blobs[0].0.clone();
    let blob = format!("blobs/{}/{hash}", &hash[..2]);
    (dir, blob)
}

/// [`store_with_model`] with its blob file removed: a source Store lacking its model blob.
fn store_without_blob(temp: &TempDir, name: &str) -> PathBuf {
    let (dir, blob) = store_with_model(temp, name);
    fs::remove_file(dir.join(blob)).unwrap();
    dir
}

/// A single-file `.gstore` holding the tiny model's definition without its blob.
fn gstore_without_blob(temp: &TempDir) -> PathBuf {
    use graphersal::persist::{SingleFileDir, StoreDir};
    let (dir, blob) = store_with_model(temp, "gstore-source");
    let file = temp.path().join("lacks.gstore");
    Store::convert(&dir, &file).unwrap();
    SingleFileDir::new(&file).remove_file(&blob).unwrap();
    file
}

/// An upload to `endpoint` (with its query) of the admin route over `admin`.
fn upload(admin: &Admin, endpoint: &str, bytes: Vec<u8>) -> (u16, Value) {
    let length = bytes.len();
    let url = format!("/api/admin/{endpoint}");
    let path = endpoint.split('?').next().unwrap();
    let mut request = Request::new(Method::Post, &url, Cursor::new(bytes))
        .with_header("Content-Type", "application/octet-stream")
        .with_body_length(Some(length));
    match crate::admin_api::route(admin, &Method::Post, path, &mut request) {
        Ok(response) => (
            response.status(),
            serde_json::from_slice(response.body()).unwrap_or(Value::Null),
        ),
        Err(err) => (err.status(), json!({ "error": err.message() })),
    }
}

fn tmp_is_empty(dir: &ServerDir) -> bool {
    fs::read_dir(dir.tmp()).map_or(true, |mut l| l.next().is_none())
}

#[test]
fn a_store_import_fills_a_missing_model_blob_from_the_shared_models() {
    let temp = TempDir::new("models-fill");
    let dir = server(&temp);
    let admin = offline(&dir);
    let (with, _) = snapshots(&temp, &admin);
    let source = store_without_blob(&temp, "lacks");
    let gstore = gstore_without_blob(&temp);

    // models/ lacks it: refused (409 + help naming models add), nothing is created.
    let err = admin
        .import(&ImportSource::new(&source), "early", None, None)
        .unwrap_err();
    match &err {
        ServerError::ModelUnavailable { repos, message, .. } => {
            assert_eq!(repos, &["acme/tiny"], "{err}");
            assert!(message.contains("blobs/"), "{message}");
        }
        other => panic!("{other}"),
    }
    assert!(err.to_string().contains("'acme/tiny'"), "{err}");
    let body = json!({"path": source.to_str().unwrap(), "name": "early"});
    let (status, answer) = route(&admin, Method::Post, "import", Some(body));
    assert_eq!(status, 409, "{answer}");
    assert!(
        answer["help"]
            .as_str()
            .unwrap()
            .contains("POST /api/admin/models"),
        "{answer}"
    );
    assert!(admin.registry().get("early").is_err());
    assert!(tmp_is_empty(&dir));

    // models/ holds it: the core, the path form and the upload form (.gstore).
    admin.model_add(&ModelAddSpec::path(&with)).unwrap();
    let imported = admin
        .import(&ImportSource::new(&source), "core", None, None)
        .expect("imported");
    assert_eq!(imported.entry.state, DbState::Unloaded);
    assert!(imported.models_missing.is_empty(), "{imported:?}");
    assert_eq!(imported.models_resolved, ["acme/tiny"]);
    assert!(
        imported
            .notes
            .iter()
            .any(|n| n.contains("'acme/tiny'") && n.contains("filled from the shared models")),
        "{:?}",
        imported.notes
    );
    let body = json!({"path": source.to_str().unwrap(), "name": "by-path"});
    let (status, answer) = route(&admin, Method::Post, "import", Some(body));
    assert_eq!(status, 201, "{answer}");
    assert_eq!(answer["state"], "unloaded");
    assert_eq!(answer["import"]["format"], "store");
    assert_eq!(answer["import"]["modelsResolved"], json!(["acme/tiny"]));
    assert!(
        answer["import"]["notes"][0]
            .as_str()
            .unwrap()
            .contains("filled from the shared models"),
        "{answer}"
    );
    let (status, answer) = upload(&admin, "import?name=by-upload", fs::read(&gstore).unwrap());
    assert_eq!(status, 201, "{answer}");
    assert_eq!(answer["import"]["modelsResolved"], json!(["acme/tiny"]));
    #[cfg(unix)]
    assert_eq!(
        shared_links(&admin, &["core", "by-path", "by-upload"]),
        4,
        "models + 3 databases: the filled blob is a hard link"
    );
    assert!(tmp_is_empty(&dir));
    // The source is unchanged and the copies load with their model.
    assert!(blob_files(&source.join("blobs")).unwrap().is_empty());
    for name in ["core", "by-upload"] {
        assert!(matches!(
            model_state(&admin, name, "acme/tiny"),
            Some(state) if state != ModelState::Missing
        ));
    }
    let loader = loader_of(&dir);
    for name in ["core", "by-upload"] {
        loader.load(name).unwrap();
        assert_eq!(loader.wait(name, WAIT).unwrap(), DbState::Loaded, "{name}");
    }
    loader.shutdown().unwrap();
}

#[test]
fn a_damaged_shared_blob_refuses_the_store_import() {
    let temp = TempDir::new("models-fill-damaged");
    let dir = server(&temp);
    let admin = offline(&dir);
    let (with, _) = snapshots(&temp, &admin);
    let source = store_without_blob(&temp, "lacks");
    admin.model_add(&ModelAddSpec::path(&with)).unwrap();
    let index = admin.models_index().unwrap();
    let shared = admin.models().blob_path(&index.models[0].blob);
    let len = fs::metadata(&shared).unwrap().len();
    fs::write(&shared, vec![0x5au8; len as usize]).unwrap();

    let err = admin
        .import(&ImportSource::new(&source), "d", None, None)
        .unwrap_err();
    match &err {
        ServerError::ModelDamaged { blob, .. } => {
            assert_eq!(blob.as_deref(), Some(shared.as_path()), "{err}")
        }
        other => panic!("{other}"),
    }
    assert!(
        err.to_string().contains(&shared.display().to_string()),
        "{err}"
    );
    let body = json!({"path": source.to_str().unwrap(), "name": "d"});
    let (status, answer) = route(&admin, Method::Post, "import", Some(body));
    assert_eq!(status, 500, "{answer}");
    assert!(
        answer["error"].as_str().unwrap().contains("damaged"),
        "{answer}"
    );
    assert!(
        answer["help"].as_str().unwrap().contains(&format!(
            "DELETE /api/admin/models/{}",
            index.models[0].blob
        )),
        "{answer}"
    );
    assert!(admin.registry().get("d").is_err());
    assert!(tmp_is_empty(&dir));
}

#[test]
fn a_model_upload_adds_a_gsnap_or_a_tar_archive_and_resolves_the_waiting() {
    let temp = TempDir::new("models-upload");
    let dir = server(&temp);
    let admin = offline(&dir);
    let (with, without) = snapshots(&temp, &admin);
    admin
        .import(&ImportSource::new(&without), "waits", None, None)
        .unwrap();

    // A tar archive of the model directory (the same files: the same blob).
    let tar = crate::model_archive::tests::model_tar(1, Some("tiny"));
    let (status, answer) = upload(
        &admin,
        "models?repo=acme%2Ftiny&license=mit&format=tar",
        tar.clone(),
    );
    assert_eq!(status, 201, "{answer}");
    assert_eq!(answer["added"][0]["repo"], "acme/tiny", "{answer}");
    assert_eq!(answer["resolved"], json!(["waits"]), "{answer}");
    assert_eq!(answer["stillMissing"], json!([]));
    assert_eq!(
        admin.registry().get("waits").unwrap().state,
        DbState::Unloaded
    );
    assert!(tmp_is_empty(&dir));
    let loader = loader_of(&dir);
    loader.load("waits").unwrap();
    assert_eq!(loader.wait("waits", WAIT).unwrap(), DbState::Loaded);
    loader.shutdown().unwrap();

    // A .gsnap upload (format detected) on another server directory.
    let other = TempDir::new("models-upload-2");
    let dir2 = server(&other);
    let admin2 = offline(&dir2);
    let (_, without2) = snapshots(&other, &admin2);
    admin2
        .import(&ImportSource::new(&without2), "w", None, None)
        .unwrap();
    let (status, answer) = upload(&admin2, "models", fs::read(&with).unwrap());
    assert_eq!(status, 201, "{answer}");
    assert_eq!(answer["added"][0]["repo"], "acme/tiny");
    assert_eq!(answer["resolved"], json!(["w"]));
    assert!(tmp_is_empty(&dir2));

    // The console's path form takes the archive too.
    let third = TempDir::new("models-upload-3");
    let dir3 = server(&third);
    let admin3 = offline(&dir3);
    let file = third.path().join("tiny.tar");
    fs::write(&file, &tar).unwrap();
    let added = admin3
        .model_add(&ModelAddSpec::path(&file).with_repo(Some("acme/tiny".into())))
        .expect("from a .tar path");
    assert_eq!(added.added[0].repo.as_deref(), Some("acme/tiny"));
    assert!(tmp_is_empty(&dir3));
}

#[test]
fn a_model_upload_refuses_malicious_archives_and_other_files() {
    use crate::model_archive::tests::{archive, entry};
    let temp = TempDir::new("models-upload-refused");
    let dir = server(&temp);
    let admin = offline(&dir);
    let cases = [
        archive(&[
            entry("config.json", b'0', b"{}"),
            entry("../escape", b'0', b"x"),
        ]),
        archive(&[entry("/abs", b'0', b"x")]),
        archive(&[entry("link", b'2', b"")]),
        archive(&[entry("hard", b'1', b"")]),
    ];
    for bytes in cases {
        let (status, answer) = upload(&admin, "models?repo=acme%2Fevil", bytes);
        assert_eq!(status, 400, "{answer}");
        assert!(
            answer["error"]
                .as_str()
                .unwrap()
                .contains("not a usable model archive"),
            "{answer}"
        );
        assert!(!temp.path().join("escape").exists());
        assert!(!dir.root().join("escape").exists());
        assert!(tmp_is_empty(&dir), "the unpack is removed");
    }
    // Neither a snapshot nor a tar; a format that does not match; an unknown parameter; empty.
    let (status, answer) = upload(&admin, "models", b"{\"not\": \"a model\"}".to_vec());
    assert_eq!(status, 400, "{answer}");
    assert!(answer["error"].as_str().unwrap().contains("neither"));
    let tar = crate::model_archive::tests::model_tar(1, None);
    let (status, answer) = upload(&admin, "models?format=gsnap", tar);
    assert_eq!(status, 400, "{answer}");
    assert!(answer["error"].as_str().unwrap().contains("not a packed"));
    let (status, _) = upload(&admin, "models?path=x", b"x".to_vec());
    assert_eq!(status, 400);
    let (status, _) = upload(&admin, "models?format=zip", b"x".to_vec());
    assert_eq!(status, 400);
    let (status, _) = upload(&admin, "models", Vec::new());
    assert_eq!(status, 400);
    assert!(admin.models_index().unwrap().models.is_empty());
    assert!(tmp_is_empty(&dir));
    // The listener lets a model upload pass up to the upload limit.
    assert!(crate::admin_api::is_upload(
        &Method::Post,
        "/api/admin/models",
        Some("application/octet-stream")
    ));
    assert!(!crate::admin_api::is_upload(
        &Method::Post,
        "/api/admin/models",
        Some("application/json")
    ));
}
