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

    // models/ lacks it: imported without the blob (graphersal 0.7.1
    // `with_skip_missing_blobs`), the database waits in `needs-model` like a snapshot import;
    // the core, the path form and the upload form (.gstore). A base is refused (409).
    let imported = admin
        .import(&ImportSource::new(&source), "early", None, None)
        .expect("imported without its blob");
    assert_eq!(imported.entry.state, DbState::needs_model("acme/tiny"));
    assert_eq!(imported.models_missing, ["acme/tiny"], "{imported:?}");
    assert!(imported.models_resolved.is_empty(), "{imported:?}");
    assert_eq!(
        model_state(&admin, "early", "acme/tiny"),
        Some(ModelState::Missing)
    );
    let body = json!({"path": source.to_str().unwrap(), "name": "early-path"});
    let (status, answer) = route(&admin, Method::Post, "import", Some(body));
    assert_eq!(status, 201, "{answer}");
    assert_eq!(answer["state"], "needs-model", "{answer}");
    assert_eq!(answer["model"], "acme/tiny", "{answer}");
    assert_eq!(answer["import"]["format"], "store");
    assert_eq!(answer["import"]["modelsMissing"], json!(["acme/tiny"]));
    let (status, answer) = upload(
        &admin,
        "import?name=early-upload",
        fs::read(&gstore).unwrap(),
    );
    assert_eq!(status, 201, "{answer}");
    assert_eq!(answer["import"]["modelsMissing"], json!(["acme/tiny"]));
    let body = json!({"path": source.to_str().unwrap(), "name": "early-base", "role": "base"});
    let (status, answer) = route(&admin, Method::Post, "import", Some(body));
    assert_eq!(status, 409, "{answer}");
    assert!(
        answer["error"].as_str().unwrap().contains("'acme/tiny'"),
        "{answer}"
    );
    assert!(admin.registry().get("early-base").is_err());
    assert!(tmp_is_empty(&dir));
    let loader = loader_of(&dir);
    loader.load("early").unwrap();
    assert_eq!(
        loader.wait("early", WAIT).unwrap(),
        DbState::needs_model("acme/tiny")
    );
    loader.shutdown().unwrap();

    // Adding the model resolves the waiting copies (then the imports below fill it at once).
    let added = admin.model_add(&ModelAddSpec::path(&with)).unwrap();
    let mut resolved = added.resolved.clone();
    resolved.sort();
    assert_eq!(
        resolved,
        ["early", "early-path", "early-upload"],
        "{added:?}"
    );
    for name in ["early", "early-path", "early-upload"] {
        assert_eq!(admin.registry().get(name).unwrap().state, DbState::Unloaded);
    }

    // models/ holds it: the core, the path form and the upload form (.gstore).
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
        shared_links(
            &admin,
            &[
                "early",
                "early-path",
                "early-upload",
                "core",
                "by-path",
                "by-upload"
            ]
        ),
        7,
        "models + 6 databases: the filled and the resolved blobs are hard links"
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

#[test]
fn a_gzip_compressed_model_archive_is_added_and_bombs_and_broken_files_are_refused() {
    use crate::model_archive::tests::{archive, entry, gzip, model_tar};
    let temp = TempDir::new("models-upload-gz");
    let dir = server(&temp);
    // A small upload limit: the decompressed bytes are held to it as well.
    let config_file = dir.root().join(crate::layout::CONFIG_FILE);
    let config = fs::read_to_string(&config_file).unwrap();
    assert!(config.contains("upload_bytes = 4294967296"), "{config}");
    fs::write(
        &config_file,
        config.replace("upload_bytes = 4294967296", "upload_bytes = 1048576"),
    )
    .unwrap();
    let admin = offline(&dir);
    let (_, without) = snapshots(&temp, &admin);
    admin
        .import(&ImportSource::new(&without), "waits", None, None)
        .unwrap();

    // A .tar.gz upload: detected from the content, and named by format=tar as well.
    let tgz = gzip(&model_tar(1, Some("tiny")));
    for query in [
        "models?repo=acme%2Ftiny&license=mit",
        "models?repo=acme%2Ftiny&format=tar",
    ] {
        let (status, answer) = upload(&admin, query, tgz.clone());
        assert_eq!(status, 201, "{query}: {answer}");
        assert_eq!(answer["added"][0]["repo"], "acme/tiny", "{answer}");
        assert!(
            tmp_is_empty(&dir),
            "the upload, the .tar and the unpack are removed"
        );
    }
    assert_eq!(
        admin.registry().get("waits").unwrap().state,
        DbState::Unloaded
    );
    let (status, answer) = upload(&admin, "models?format=gsnap", tgz.clone());
    assert_eq!(status, 400, "{answer}");
    assert!(
        answer["error"]
            .as_str()
            .unwrap()
            .contains("gzip-compressed tar archive"),
        "{answer}"
    );

    // A decompression bomb: 8 MiB of zeros in a tar (a few KiB as gzip), over the 1 MiB limit.
    let bomb = gzip(&archive(&[entry("weights.bin", b'0', &vec![0u8; 8 << 20])]));
    assert!(bomb.len() < 64 * 1024);
    let (status, answer) = upload(&admin, "models?repo=acme%2Fbomb", bomb);
    assert_eq!(status, 400, "{answer}");
    let error = answer["error"].as_str().unwrap();
    assert!(
        error.contains("not a usable model archive") && error.contains("more than 1048576"),
        "{answer}"
    );
    assert!(tmp_is_empty(&dir));

    // A corrupt gzip file (the magic intact), and a gzip that holds no tar.
    let mut corrupt = tgz.clone();
    for byte in &mut corrupt[20..80] {
        *byte ^= 0x5a;
    }
    for (bytes, needle) in [
        (corrupt, "not a readable gzip file"),
        (gzip(b"not a tar"), "does not hold a tar archive"),
    ] {
        let (status, answer) = upload(&admin, "models?repo=acme%2Fbroken", bytes);
        assert_eq!(status, 400, "{answer}");
        assert!(
            answer["error"].as_str().unwrap().contains(needle),
            "{answer}"
        );
        assert!(tmp_is_empty(&dir));
    }

    // The console's path form takes a .tar.gz too.
    let file = temp.path().join("tiny.tar.gz");
    fs::write(&file, &tgz).unwrap();
    let added = admin
        .model_add(&ModelAddSpec::path(&file).with_repo(Some("acme/tiny".into())))
        .expect("from a .tar.gz path");
    assert_eq!(added.added[0].repo.as_deref(), Some("acme/tiny"));
    assert!(tmp_is_empty(&dir));
}

// ---------------------------------------------------------------- B-VERIFY: missing model blobs

/// The problems of a verify answer.
fn problems_of(answer: &Value) -> Vec<Value> {
    answer["problems"].as_array().cloned().unwrap_or_default()
}

#[test]
fn a_verify_names_a_missing_model_blob_and_its_remedy_and_keeps_the_last_result() {
    let temp = TempDir::new("models-verify");
    let dir = server(&temp);
    let admin = offline(&dir);
    let (with, without) = snapshots(&temp, &admin);
    // A database that never got its model blob (an import of a snapshot without models, the
    // shared models lack it): verify names the model by elimination, not shared.
    admin
        .import(&ImportSource::new(&without), "kb", None, None)
        .unwrap();
    assert!(crate::recovery::LastVerify::read(&admin.registry().get("kb").unwrap().dir).is_none());
    let (status, answer) = route(&admin, Method::Post, "databases/kb/verify", None);
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["ok"], false, "{answer}");
    assert_eq!(answer["verdict"], "needs-model", "{answer}");
    assert_eq!(answer["state"], "needs-model", "{answer}");
    assert_eq!(answer["modelsMissing"], json!(["acme/tiny"]), "{answer}");
    let problems = problems_of(&answer);
    assert_eq!(problems.len(), 1, "{answer}");
    let problem = &problems[0];
    assert_eq!(problem["kind"], "model", "{problem}");
    assert_eq!(problem["model"], "acme/tiny", "{problem}");
    assert!(problem.get("shared").is_none(), "{problem}");
    let blob = problem["blob"].as_str().unwrap();
    assert!(
        problem["path"].as_str().unwrap().ends_with(blob),
        "{problem}"
    );
    let remedy = problem["remedy"].as_str().unwrap();
    assert!(
        remedy.contains("download the model acme/tiny")
            && remedy.contains("Models tab")
            && remedy.contains("graphersal-server models add SOURCE"),
        "{remedy}"
    );
    let help = answer["help"].as_str().unwrap();
    assert!(
        help.contains("not damage") && !help.contains("/repair"),
        "{help}"
    );
    // The text of a verify problem names the model.
    let verified = admin.verify("kb").unwrap();
    assert_eq!(
        verified.facts.problems_text(),
        format!(
            "the embedding model acme/tiny (blob {}) is missing",
            &blob[..8]
        )
    );
    // The last verify is kept: in the list row and the settings, also after a restart.
    let restarted = offline(&dir);
    let entry = restarted.registry().get("kb").unwrap();
    assert_eq!(entry.state, DbState::needs_model("acme/tiny"));
    let last = crate::recovery::LastVerify::read(&entry.dir).expect("kept");
    assert_eq!(last.verdict, "needs-model");
    assert_eq!(last.problem_count, 1);
    assert_eq!(last.problems[0].model.as_deref(), Some("acme/tiny"));
    assert!(
        last.headline().starts_with("Last verify 20"),
        "{}",
        last.headline()
    );
    let (status, settings) = route(&restarted, Method::Get, "databases/kb/settings", None);
    assert_eq!(status, 200, "{settings}");
    let recovery = &settings["recovery"];
    assert_eq!(recovery["needsModel"], "acme/tiny", "{recovery}");
    assert_eq!(recovery["maintenance"], false, "{recovery}");
    assert_eq!(
        recovery["lastVerify"]["verdict"], "needs-model",
        "{recovery}"
    );
    assert_eq!(
        recovery["lastVerify"]["problems"][0]["kind"], "model",
        "{recovery}"
    );
    assert!(
        recovery["lastVerify"]["headline"]
            .as_str()
            .unwrap()
            .contains("acme/tiny"),
        "{recovery}"
    );
    let (status, list) = route(&restarted, Method::Get, "databases", None);
    assert_eq!(status, 200, "{list}");
    let row = list["databases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "kb")
        .cloned()
        .unwrap();
    assert_eq!(row["lastVerify"]["verdict"], "needs-model", "{row}");
    assert_eq!(row["lastVerify"]["problemCount"], 1, "{row}");

    // The remedy works: the model added, the database resolved, the verify clean.
    let added = restarted.model_add(&ModelAddSpec::path(&with)).unwrap();
    assert_eq!(added.resolved, ["kb"], "{added:?}");
    let verified = restarted.verify("kb").unwrap();
    assert!(verified.facts.ok, "{:?}", verified.facts);
    assert_eq!(verified.facts.verdict(), "ok");
    let entry = restarted.registry().get("kb").unwrap();
    assert_eq!(
        crate::recovery::LastVerify::read(&entry.dir)
            .unwrap()
            .verdict,
        "ok"
    );

    // Its link removed while the shared models hold the blob: named exactly, `shared`, and the
    // database is needs-model (marked) until `models add acme/tiny` gives it back.
    let (hash, path, _) = db_blobs(&restarted, "kb").remove(0);
    fs::remove_file(&path).unwrap();
    let (_, answer) = route(&restarted, Method::Post, "databases/kb/verify", None);
    assert_eq!(answer["verdict"], "needs-model", "{answer}");
    let problem = &problems_of(&answer)[0];
    assert_eq!(problem["model"], "acme/tiny", "{problem}");
    assert_eq!(problem["blob"], hash, "{problem}");
    assert_eq!(problem["shared"], true, "{problem}");
    assert!(
        problem["remedy"]
            .as_str()
            .unwrap()
            .contains("graphersal-server models add acme/tiny"),
        "{problem}"
    );
    let entry = restarted.registry().get("kb").unwrap();
    assert_eq!(entry.state, DbState::needs_model("acme/tiny"));
    assert_eq!(
        entry.config.unwrap().needs_model.as_deref(),
        Some("acme/tiny")
    );
    let added = restarted
        .model_add(&ModelAddSpec::repo("acme/tiny"))
        .unwrap();
    assert_eq!(added.resolved, ["kb"], "{added:?}");
    assert!(restarted.verify("kb").unwrap().facts.ok);
}

#[test]
fn name_models_by_catalog_elimination_and_leaves_other_blobs_as_damage() {
    use crate::recovery::{ProblemFacts, VerifyFacts};
    let problem = |path: &str, reason: &str| {
        serde_json::from_value::<ProblemFacts>(json!({
            "path": path, "offset": null, "reason": reason
        }))
        .unwrap()
    };
    let facts = |problems: Vec<ProblemFacts>| {
        serde_json::from_value::<VerifyFacts>(json!({
            "ok": false, "problems": problems, "notes": [], "snapshotsChecked": 1,
            "segmentsChecked": 0, "recordsChecked": 0, "firstCommit": 0, "lastCommit": 0,
            "modelsChecked": 0, "blobsChecked": 2, "how": "files"
        }))
        .unwrap()
    };
    let a = "blobs/aa/aa00000000000000000000000000000000000000000000000000000000000000";
    let b = "blobs/bb/bb00000000000000000000000000000000000000000000000000000000000000";
    let missing = "the blob is missing";
    // A missing blob no model accounts for stays damage (a blob property's blob).
    let mut f = facts(vec![problem(a, missing)]);
    assert_eq!(f.verdict(), "damaged");
    f.name_models(&|_| None, &[], &|_| false);
    assert_eq!(f.problems[0].kind, "damage");
    assert_eq!(f.verdict(), "damaged");
    // Known by hash (the catalog or models/), the other by elimination: needs-model.
    let mut f = facts(vec![problem(a, missing), problem(b, missing)]);
    let known = |hash: &str| hash.starts_with("aa").then(|| "x/one".to_string());
    f.name_models(&known, &["x/one".to_string(), "x/two".to_string()], &|r| {
        r == "x/one"
    });
    assert_eq!(f.problems[0].model.as_deref(), Some("x/one"));
    assert!(f.problems[0].shared);
    assert_eq!(f.problems[1].model.as_deref(), Some("x/two"));
    assert!(!f.problems[1].shared);
    assert_eq!(f.verdict(), "needs-model");
    assert_eq!(f.models_missing, ["x/one", "x/two"]);
    // Two unknown blobs, two missing models: each names the candidates.
    let mut f = facts(vec![problem(a, missing), problem(b, missing)]);
    f.name_models(
        &|_| None,
        &["x/one".to_string(), "x/two".to_string()],
        &|_| false,
    );
    assert_eq!(f.problems[0].model, None);
    assert_eq!(f.problems[0].candidates, ["x/one", "x/two"]);
    assert!(
        f.problems_text()
            .contains("one of the embedding models x/one, x/two")
    );
    assert_eq!(f.verdict(), "needs-model");
    // Damage beside a missing model: damaged, the model still named.
    let mut f = facts(vec![
        problem(a, missing),
        problem("snapshots/1/chunk-0", "the CRC-32C check failed"),
    ]);
    f.name_models(&|_| None, &["x/one".to_string()], &|_| false);
    assert_eq!(f.problems[0].model.as_deref(), Some("x/one"));
    assert_eq!(f.problems[1].kind, "damage");
    assert_eq!(f.verdict(), "damaged");
    // An old report (no kind, no verdict) reads as before.
    let f = facts(vec![problem(a, missing)]);
    assert_eq!(f.problems[0].kind, "damage");
}

#[test]
fn a_loaded_database_names_a_vanished_model_blob_from_its_catalog_and_stays_loaded() {
    let temp = TempDir::new("models-verify-loaded");
    let dir = server(&temp);
    let admin = offline(&dir);
    let entry = admin.create("kb", &CreateSpec::plain()).unwrap();
    let store = Store::open(entry.store_dir(), StoreOptions::new()).unwrap();
    store
        .import_model(
            ModelSource::Files(crate::test_models::tiny_model(1, "acme/tiny")),
            ImportModelOptions::new(),
        )
        .unwrap();
    store.close().unwrap();
    let loader = loader_of(&dir);
    let online = Admin::online(loader.clone());
    loader.load("kb").unwrap();
    assert_eq!(loader.wait("kb", WAIT).unwrap(), DbState::Loaded);
    // The blob file vanishes under the open Store; models/ never held it: the catalog names it.
    let (hash, path, _) = db_blobs(&online, "kb").remove(0);
    assert!(online.models().repo_of_blob(&hash).is_none());
    fs::remove_file(&path).unwrap();
    let verified = online.verify("kb").unwrap();
    assert_eq!(verified.facts.how, "open store");
    assert_eq!(
        verified.facts.verdict(),
        "needs-model",
        "{:?}",
        verified.facts
    );
    assert_eq!(
        verified.facts.problems[0].model.as_deref(),
        Some("acme/tiny")
    );
    assert_eq!(
        verified.state,
        DbState::Loaded,
        "a loaded database is left as it is"
    );
    let config = online.registry().get("kb").unwrap().config.unwrap();
    assert_eq!(config.needs_model, None);
    loader.shutdown().unwrap();
}

// S1: the server-side download from Hugging Face (`Admin::model_download_start`), against the
// fake endpoint of `test_models::FakeHub` (no network).

/// The download spec of `acme/tiny` from `hub`.
fn tiny_download(hub: &crate::test_models::FakeHub) -> crate::ops::ModelDownloadSpec {
    let mut spec = crate::ops::ModelDownloadSpec::new("acme/tiny");
    spec.endpoint = Some(hub.url.clone());
    spec
}

/// The entries of `tmp/` (a download leaves none behind).
fn tmp_entries(dir: &ServerDir) -> Vec<String> {
    fs::read_dir(dir.tmp())
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn an_offline_download_adds_the_model_with_its_licence_and_audits() {
    use crate::model_download::DownloadState;
    let temp = TempDir::new("models-download-offline");
    let dir = server(&temp);
    let hub = crate::test_models::FakeHub::start("cc-by-nc-4.0", false);
    let memory = Arc::new(crate::audit::MemoryAudit::default());
    let admin = offline(&dir).with_audit(
        crate::audit::Audit::new(memory.clone()).with_actor(Some("operator".to_string())),
    );
    let done = admin.model_download_start(&tiny_download(&hub)).unwrap();
    assert_eq!(done.state, DownloadState::Done, "{done:?}");
    assert_eq!(done.revision, "main");
    let report = done.report.clone().unwrap();
    assert_eq!(report.license, "cc-by-nc-4.0");
    assert!(
        !report.warnings.is_empty(),
        "a non-commercial licence warns"
    );
    assert!(
        report.identity.starts_with("model2vec/acme/tiny@"),
        "{report:?}"
    );
    assert!(
        report
            .files
            .iter()
            .any(|(name, _)| name == "model.safetensors"),
        "{report:?}"
    );
    assert!(done.received_bytes() > 0);
    let added = done.added.clone().unwrap();
    assert_eq!(added.added.len(), 1, "{added:?}");
    assert_eq!(added.added[0].repo.as_deref(), Some("acme/tiny"));
    assert_eq!(added.added[0].license.as_deref(), Some("cc-by-nc-4.0"));
    let index = admin.models_index().unwrap();
    assert_eq!(index.of_repo("acme/tiny").count(), 1);
    assert!(tmp_entries(&dir).is_empty(), "{:?}", tmp_entries(&dir));
    let lines = memory.lines();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("model download of acme/tiny (main) started")),
        "{lines:#?}"
    );
    assert!(
        lines.iter().any(
            |l| l.contains("model acme/tiny (main) downloaded into models/")
                && l.contains("licence cc-by-nc-4.0")
        ),
        "{lines:#?}"
    );
    // A repository the endpoint does not have: failed, nothing kept, audited.
    let mut missing = crate::ops::ModelDownloadSpec::new("acme/none");
    missing.endpoint = Some(hub.url.clone());
    let failed = admin.model_download_start(&missing).unwrap();
    assert_eq!(failed.state, DownloadState::Failed, "{failed:?}");
    assert_eq!(failed.error_kind, Some("not-found"), "{failed:?}");
    assert!(tmp_entries(&dir).is_empty());
    assert!(
        memory
            .lines()
            .iter()
            .any(|l| l.contains("model download of acme/none (main) failed"))
    );
    // Bad names are refused before anything runs; offline there is nothing to cancel.
    for repo in ["tiny", "a/b/c", "../x", ""] {
        assert!(
            matches!(
                admin.model_download_start(&crate::ops::ModelDownloadSpec::new(repo)),
                Err(ServerError::InvalidInput { .. })
            ),
            "{repo}"
        );
    }
    let bad = crate::ops::ModelDownloadSpec::new("acme/tiny").with_revision(Some("../x".into()));
    assert!(matches!(
        admin.model_download_start(&bad),
        Err(ServerError::InvalidInput { .. })
    ));
    assert!(matches!(
        admin.model_download_cancel(),
        Err(ServerError::Refused { .. })
    ));
}

#[test]
fn an_online_download_shows_its_progress_runs_alone_and_is_cancelled() {
    use crate::model_download::DownloadState;
    let temp = TempDir::new("models-download-online");
    let dir = server(&temp);
    let loader = loader_of(&dir);
    let admin = Admin::online(loader.clone());
    assert!(admin.model_download_status().is_none());
    assert_eq!(admin.model_download_cancel().unwrap(), None);
    let slow = crate::test_models::FakeHub::start("mit", true);
    let started = admin.model_download_start(&tiny_download(&slow)).unwrap();
    assert_eq!(started.state, DownloadState::Running);
    // Progress: the weights trickle in.
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let now = admin.model_download_status().unwrap();
        if now.file.as_deref() == Some("model.safetensors") && now.file_bytes > 0 {
            assert_eq!(now.file_total, Some(64 * 1024 * 1024));
            assert!(now.files_done.len() >= 2, "{now:?}");
            break;
        }
        assert_eq!(now.state, DownloadState::Running, "{now:?}");
        assert!(std::time::Instant::now() < deadline, "no progress: {now:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
    // One at a time.
    let hub = crate::test_models::FakeHub::start("mit", false);
    assert!(matches!(
        admin.model_download_start(&tiny_download(&hub)),
        Err(ServerError::Refused { .. })
    ));
    // Cancelled: stops at its next chunk, nothing kept.
    let (stopped, status) = admin.model_download_cancel().unwrap().unwrap();
    assert!(stopped && status.cancel_requested);
    let deadline = std::time::Instant::now() + WAIT;
    let ended = loop {
        let now = admin.model_download_status().unwrap();
        if now.state != DownloadState::Running {
            break now;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "not cancelled: {now:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(ended.state, DownloadState::Cancelled, "{ended:?}");
    assert_eq!(ended.error_kind, Some("cancelled"));
    assert!(admin.models_index().unwrap().models.is_empty());
    assert!(tmp_entries(&dir).is_empty(), "{:?}", tmp_entries(&dir));
    assert!(!admin.model_download_cancel().unwrap().unwrap().0);
    // The next one runs to its end in the background.
    let second = admin.model_download_start(&tiny_download(&hub)).unwrap();
    assert_eq!(second.id, started.id + 1);
    let deadline = std::time::Instant::now() + WAIT;
    let done = loop {
        let now = admin.model_download_status().unwrap();
        if now.state != DownloadState::Running {
            break now;
        }
        assert!(std::time::Instant::now() < deadline, "never ended: {now:?}");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(done.state, DownloadState::Done, "{done:?}");
    assert_eq!(done.phase, "install");
    assert_eq!(
        admin.models_index().unwrap().of_repo("acme/tiny").count(),
        1
    );
    loader.shutdown().unwrap();
}

// S2 (follow-up of A7): a database waiting for a model whose blob it lost is resolved by any
// copy of the same model (the same repository and model files: the same identity), also when
// that copy's blob differs in what travels beside the files (the revision named, the card, the
// licence): the Store takes the copy's blob under its model's name (the engine's restore of an
// import of the same identity).

/// A database `name` that waits for `acme/tiny`: a Store made with the model `model`, exported
/// without models, imported back (`needs-model`), the source deleted. Answers the blob hash the
/// database waits for.
fn waiting_for(admin: &Admin, temp: &TempDir, name: &str, model: ModelSource) -> String {
    let source = format!("{name}-source");
    let entry = admin.create(&source, &CreateSpec::plain()).unwrap();
    let store = Store::open(entry.store_dir(), StoreOptions::new()).unwrap();
    store
        .import_model(model, ImportModelOptions::new())
        .expect("the model");
    store.close().unwrap();
    let hash = db_blobs(admin, &source)[0].0.clone();
    let without = temp.path().join(format!("{name}.gsnap"));
    admin
        .export_to_file(&source, ExportSpec::new().with_models(false), &without)
        .unwrap();
    admin.delete(&source).unwrap();
    let imported = admin
        .import(&ImportSource::new(&without), name, None, None)
        .unwrap();
    assert_eq!(imported.entry.state, DbState::needs_model("acme/tiny"));
    hash
}

/// The model `acme/tiny` downloaded from `hub` at `revision` as the engine stages it (what an
/// earlier download, by the server or the graphersal CLI, put into a database).
fn downloaded(temp: &TempDir, hub: &crate::test_models::FakeHub, revision: &str) -> ModelSource {
    use graphersal::persist::{ModelCache, ModelDownload};
    let request = ModelDownload::new("acme/tiny")
        .with_endpoint(hub.url.as_str())
        .with_revision(revision);
    let cache = temp.path().join(format!("cache-{revision}"));
    let staged = ModelCache::at(&cache)
        .download_staged(&request, |_| {})
        .unwrap();
    ModelSource::Files(staged.new_model().unwrap())
}

/// A server-side download of `acme/tiny` from `hub` at `revision`, to its end.
fn download_now(
    admin: &Admin,
    hub: &crate::test_models::FakeHub,
    revision: Option<&str>,
) -> crate::ops::ModelAdded {
    use crate::model_download::DownloadState;
    let spec = tiny_download(hub).with_revision(revision.map(str::to_string));
    let done = admin.model_download_start(&spec).unwrap();
    assert_eq!(done.state, DownloadState::Done, "{done:?}");
    done.added.clone().unwrap()
}

/// The database `name` is resolved: unloaded, no marker, its model present, and it loads.
fn assert_resolved(dir: &ServerDir, admin: &Admin, name: &str) {
    let entry = admin.registry().get(name).unwrap();
    assert_eq!(entry.state, DbState::Unloaded, "{name}");
    assert_eq!(entry.config.as_ref().unwrap().needs_model, None, "{name}");
    assert!(
        matches!(model_state(admin, name, "acme/tiny"), Some(s) if s != ModelState::Missing),
        "{name}"
    );
    let loader = loader_of(dir);
    loader.load(name).unwrap();
    assert_eq!(loader.wait(name, WAIT).unwrap(), DbState::Loaded, "{name}");
    loader.shutdown().unwrap();
}

#[test]
fn a_fresh_download_of_the_same_revision_gives_the_same_blob_and_resolves() {
    let temp = TempDir::new("models-redownload");
    let dir = server(&temp);
    let admin = offline(&dir);
    let hub = crate::test_models::FakeHub::start("mit", false);
    let wanted = waiting_for(&admin, &temp, "w", downloaded(&temp, &hub, "main"));
    let added = download_now(&admin, &hub, None);
    assert_eq!(added.added.len(), 1, "{added:?}");
    assert_eq!(added.added[0].blob, wanted, "the same blob: {added:?}");
    assert_eq!(added.resolved, ["w"], "{added:?}");
    assert_resolved(&dir, &admin, "w");
}

#[test]
fn a_download_by_branch_resolves_a_database_made_from_the_commit() {
    let temp = TempDir::new("models-redownload-commit");
    let dir = server(&temp);
    let admin = offline(&dir);
    let hub = crate::test_models::FakeHub::start("mit", false);
    let commit = crate::test_models::FakeHub::COMMIT;
    // Made from the commit, downloaded again by the branch name: other blob bytes (the revision
    // string is in the blob), the same model files.
    let wanted = waiting_for(&admin, &temp, "c", downloaded(&temp, &hub, commit));
    let added = download_now(&admin, &hub, None);
    assert_ne!(added.added[0].blob, wanted, "{added:?}");
    assert_eq!(added.resolved, ["c"], "{added:?}");
    assert_resolved(&dir, &admin, "c");
    // An import of another database waiting for it is resolved at once from models/.
    waiting_for_resolved_at_import(&admin, &temp, downloaded(&temp, &hub, "main"));
    // And the other way round (a new server directory): made by the branch, a download by the
    // commit resolves it.
    let other = TempDir::new("models-redownload-branch");
    let dir = server(&other);
    let admin = offline(&dir);
    let wanted = waiting_for(&admin, &other, "b", downloaded(&other, &hub, "main"));
    let added = download_now(&admin, &hub, Some(commit));
    assert_ne!(added.added[0].blob, wanted, "{added:?}");
    assert_eq!(added.resolved, ["b"], "{added:?}");
    assert_resolved(&dir, &admin, "b");
}

/// An import of a database made with `model` (another blob of the model `models/` holds) is
/// resolved in its build: `unloaded`, its model present.
fn waiting_for_resolved_at_import(admin: &Admin, temp: &TempDir, model: ModelSource) {
    let entry = admin.create("r-source", &CreateSpec::plain()).unwrap();
    let store = Store::open(entry.store_dir(), StoreOptions::new()).unwrap();
    store
        .import_model(model, ImportModelOptions::new())
        .expect("the model");
    store.close().unwrap();
    let without = temp.path().join("r.gsnap");
    admin
        .export_to_file("r-source", ExportSpec::new().with_models(false), &without)
        .unwrap();
    admin.delete("r-source").unwrap();
    let imported = admin
        .import(&ImportSource::new(&without), "r", None, None)
        .unwrap();
    assert_eq!(imported.entry.state, DbState::Unloaded);
    assert!(matches!(
        model_state(admin, "r", "acme/tiny"),
        Some(state) if state != ModelState::Missing
    ));
}

#[test]
fn a_download_resolves_a_database_made_with_another_copy_of_the_same_model() {
    let temp = TempDir::new("models-redownload-copy");
    let dir = server(&temp);
    let admin = offline(&dir);
    // Made from the same model files with another card, no revision, another licence.
    let hub = crate::test_models::FakeHub::start("apache-2.0", false);
    waiting_for(
        &admin,
        &temp,
        "w",
        ModelSource::Files(crate::test_models::tiny_model(1, "acme/tiny")),
    );
    let added = download_now(&admin, &hub, None);
    assert_eq!(added.resolved, ["w"], "{added:?}");
    assert_resolved(&dir, &admin, "w");
}

#[test]
fn another_model_of_the_repository_does_not_resolve_the_database() {
    let temp = TempDir::new("models-redownload-other");
    let dir = server(&temp);
    let admin = offline(&dir);
    // Other weights (seed 2) under the same repository: another identity.
    let wanted = waiting_for(
        &admin,
        &temp,
        "w",
        ModelSource::Files(crate::test_models::tiny_model(2, "acme/tiny")),
    );
    let hub = crate::test_models::FakeHub::start("mit", false);
    let added = download_now(&admin, &hub, None);
    assert!(added.resolved.is_empty(), "{added:?}");
    let entry = admin.registry().get("w").unwrap();
    assert_eq!(entry.state, DbState::needs_model("acme/tiny"));
    assert_eq!(
        model_state(&admin, "w", "acme/tiny"),
        Some(ModelState::Missing)
    );
    // Its catalog still names the blob it waits for; nothing was replaced.
    let models = Store::models_dir(entry.store_dir()).unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].blob_hash, wanted);
}

// ---------------------------------------------------------------- KBFIX: the default model

/// The plain database `name` holding the tiny models `acme/tiny` and `acme/other`, none of
/// them the default.
fn db_with_two_models(admin: &Admin, name: &str) -> crate::registry::DatabaseEntry {
    let entry = admin.create(name, &CreateSpec::plain()).unwrap();
    let store = Store::open(entry.store_dir(), StoreOptions::new()).unwrap();
    for (seed, repo) in [(1, "acme/tiny"), (2, "acme/other")] {
        store
            .import_model(
                ModelSource::Files(crate::test_models::tiny_model(seed, repo)),
                ImportModelOptions::new(),
            )
            .unwrap();
    }
    store.set_default_model(None).unwrap();
    store.close().unwrap();
    entry
}

/// The default model of the database `name`, read from its files.
fn default_on_disk(admin: &Admin, name: &str) -> Option<String> {
    let entry = admin.registry().get(name).unwrap();
    Store::inspect(entry.store_dir()).unwrap().default_model
}

#[test]
fn the_default_model_of_an_unloaded_database_is_set_and_cleared_on_its_closed_store() {
    let temp = TempDir::new("models-default-offline");
    let dir = server(&temp);
    let admin = offline(&dir);
    db_with_two_models(&admin, "kb");
    assert_eq!(default_on_disk(&admin, "kb"), None);

    let done = admin.set_default_model("kb", Some("acme/tiny")).unwrap();
    assert!(done.changed);
    assert!(!done.loaded);
    assert_eq!(done.previous, None);
    assert_eq!(done.model.as_deref(), Some("acme/tiny"));
    assert_eq!(done.models.len(), 2, "{:?}", done.models);
    assert_eq!(default_on_disk(&admin, "kb").as_deref(), Some("acme/tiny"));
    // The same again: nothing to commit.
    let again = admin.set_default_model("KB", Some("acme/tiny")).unwrap();
    assert!(!again.changed);
    assert_eq!(again.previous.as_deref(), Some("acme/tiny"));

    // Over HTTP: another model, then none.
    let (status, answer) = route(
        &admin,
        Method::Put,
        "databases/kb/default-model",
        Some(json!({"model": "acme/other"})),
    );
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["defaultModel"], "acme/other");
    assert_eq!(answer["previousDefaultModel"], "acme/tiny");
    assert_eq!(answer["changed"], true);
    assert_eq!(answer["loaded"], false);
    assert_eq!(answer["name"], "kb");
    let (status, answer) = route(
        &admin,
        Method::Put,
        "databases/kb/default-model",
        Some(json!({"model": null})),
    );
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["defaultModel"], Value::Null);
    assert_eq!(default_on_disk(&admin, "kb"), None);

    // A model it does not hold: 400 naming the ones it holds; nothing changed.
    let (status, answer) = route(
        &admin,
        Method::Put,
        "databases/kb/default-model",
        Some(json!({"model": "acme/missing"})),
    );
    assert_eq!(status, 400, "{answer}");
    let error = answer["error"].as_str().unwrap();
    assert!(error.contains("acme/missing"), "{answer}");
    assert!(
        error.contains("acme/tiny") && error.contains("acme/other"),
        "{answer}"
    );
    assert!(
        answer["help"].as_str().unwrap().contains("settings"),
        "{answer}"
    );
    // A body without "model", or of another type: 400.
    for body in [
        json!({}),
        json!({"model": 3}),
        json!({"model": "acme/tiny", "x": 1}),
    ] {
        let (status, answer) = route(
            &admin,
            Method::Put,
            "databases/kb/default-model",
            Some(body),
        );
        assert_eq!(status, 400, "{answer}");
    }
    let (status, _) = route(
        &admin,
        Method::Put,
        "databases/nope/default-model",
        Some(json!({"model": null})),
    );
    assert_eq!(status, 404);
    // The settings view lists the models and the default.
    admin.set_default_model("kb", Some("acme/tiny")).unwrap();
    let (status, view) = route(&admin, Method::Get, "databases/kb/settings", None);
    assert_eq!(status, 200, "{view}");
    assert_eq!(view["store"]["defaultModel"], "acme/tiny", "{view}");
    assert_eq!(
        view["store"]["models"].as_array().unwrap().len(),
        2,
        "{view}"
    );
}

#[test]
fn the_default_model_is_refused_for_a_layer_a_base_and_a_read_only_database() {
    let temp = TempDir::new("models-default-refused");
    let dir = server(&temp);
    let admin = offline(&dir);
    db_with_two_models(&admin, "kb");
    admin.set_read_only("kb", true).unwrap();
    let err = admin
        .set_default_model("kb", Some("acme/tiny"))
        .unwrap_err();
    assert!(matches!(err, ServerError::Refused { .. }), "{err}");
    assert!(err.to_string().contains("read-only"), "{err}");
    let (status, answer) = route(
        &admin,
        Method::Put,
        "databases/kb/default-model",
        Some(json!({"model": "acme/tiny"})),
    );
    assert_eq!(status, 409, "{answer}");
    assert!(
        answer["help"].as_str().unwrap().contains("/read-only"),
        "{answer}"
    );
    assert_eq!(default_on_disk(&admin, "kb"), None);

    admin
        .create("frozen", &CreateSpec::plain().with_role(DatabaseRole::Base))
        .unwrap();
    let err = admin.set_default_model("frozen", None).unwrap_err();
    assert!(err.to_string().contains("base"), "{err}");
    admin.create("lay", &CreateSpec::layer("frozen")).unwrap();
    let err = admin.set_default_model("lay", None).unwrap_err();
    assert!(err.to_string().contains("layer"), "{err}");
}

#[test]
fn the_default_model_of_a_loaded_database_changes_at_once_on_its_open_store() {
    let temp = TempDir::new("models-default-loaded");
    let dir = server(&temp);
    db_with_two_models(&offline(&dir), "kb");
    let loader = loader_of(&dir);
    let online = Admin::online(loader.clone());
    loader.load("kb").unwrap();
    assert_eq!(loader.wait("kb", WAIT).unwrap(), DbState::Loaded);

    let done = online.set_default_model("kb", Some("acme/other")).unwrap();
    assert!(done.changed);
    assert!(done.loaded);
    assert_eq!(done.entry.state, DbState::Loaded);
    // The open Store (the graph's registry) has it now, and the files too.
    let Some((_context, crate::loader::OpenStoreRef::Plain(store))) = loader.open_store_of("kb")
    else {
        panic!("kb is loaded");
    };
    let models = store.models().unwrap();
    let default: Vec<&str> = models
        .iter()
        .filter(|m| m.default)
        .map(|m| m.repo.as_str())
        .collect();
    assert_eq!(default, ["acme/other"]);
    drop(store);
    assert_eq!(
        default_on_disk(&online, "kb").as_deref(),
        Some("acme/other")
    );
    // It stays across an unload and a load.
    loader.unload("kb").unwrap();
    loader.load("kb").unwrap();
    assert_eq!(loader.wait("kb", WAIT).unwrap(), DbState::Loaded);
    assert_eq!(
        default_on_disk(&online, "kb").as_deref(),
        Some("acme/other")
    );
    loader.shutdown().unwrap();
}
