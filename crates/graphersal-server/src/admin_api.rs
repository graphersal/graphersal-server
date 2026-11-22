//! The admin API over HTTP (`/api/admin/...`; the `Host`/`Origin` checks ran before). Every
//! action calls the admin core ([`crate::ops::Admin`]), the same as the console's.
//!
//! | Method | Path | Body | Answer |
//! |---|---|---|---|
//! | GET | `databases` | | `200 {"databases": [entry...], "aliases": {alias: database}, "problems": [text...]}` |
//! | POST | `databases` | `{"name", "kind"?, "role"?, "base"?, "schema"?, "description"?, "autoLoad"?, "readOnly"?}` | `201` entry |
//! | GET | `databases/<name>` | | `200` entry (`<name>` may be an alias) |
//! | DELETE | `databases/<name>` | | `200 {"name", "trash"}` |
//! | POST | `databases/<name>/clone` | `{"name"}` | `201` entry of the clone |
//! | POST | `databases/<name>/load` | | `202` entry + `"load": "started" \| "already-loading"`, `200` + `"already-loaded"` |
//! | POST | `databases/<name>/unload` | | `200` entry + `"unloaded": bool` |
//! | PUT | `databases/<name>/read-only` | `{"readOnly": bool}` | `200` entry |
//! | PUT | `databases/<name>/auto-load` | `{"autoLoad": bool}` | `200` entry |
//! | POST | `databases/<name>/backup` | | `201` backup |
//! | GET | `databases/<name>/backups` | | `200 {"database", "backups": [backup...]}` (also for a deleted database's name) |
//! | POST | `databases/<name>/restore` | `{"backup", "name"?}` | `201` entry + `"restoredFrom": {"database", "backup"}`, `"replaced"`, `"trash"?` |
//! | GET | `databases/<name>/export?models=true\|false&materialize=true\|false` | | `200` the `.gsnap` file (download) |
//! | POST | `import?name=<n>&base=<b>&format=<f>&description=<d>` | the file itself (any content type but JSON) | `201` entry + `"import"` |
//! | POST | `import` | JSON `{"path", "name", "base"?, "format"?, "description"?}` (a file or directory on the server's machine, absolute) | `201` entry + `"import"` |
//! | GET | `aliases` | | `200 {"aliases": {alias: database}}` |
//! | PUT | `aliases/<alias>` | `{"target"}` | `200 {"alias", "target"}` |
//! | DELETE | `aliases/<alias>` | | `200 {"alias", "removed": true}` |
//!
//! The actions take real database names (not aliases). Errors are `{"error", "help"?}` with
//! `400` (invalid name, body or schema), `404` (no such database, alias or endpoint), `405`,
//! `409` (a rule refuses it: name taken, loaded, named by an alias, base of layers, offline,
//! failed database), `413` (body too large), `415` (an import source of no known format),
//! `500` (a Store or file failed). The repository's
//! `docs/admin-api.md` documents the routes for clients.
//!
//! A backup: `database`, `backup` (its name, the UTC time with dashes), `file`
//! (`backups/<name>/<backup>.zip`), `bytes`, and from its sidecar `created`, `kind`, `role`,
//! `base`, `layerBase` (a layer's base identity). An export answers the file with
//! `Content-Disposition: attachment; filename="<name>.gsnap"` (`<name>.layer.gsnap` for a layer
//! snapshot), `Content-Length`, and `X-Graphersal-Snapshot: full | layer`,
//! `X-Graphersal-Models: included | excluded`; it is written into `tmp/` first and streamed from
//! there (removed after the answer).
//!
//! An import upload's body is streamed into `tmp/upload-*.bin` (never held in memory; at most
//! `server.toml`'s `[limits] upload_bytes`, default 4 GiB, else `413`), imported, and removed.
//! `"import"` of the answer: `{"format": "snapshot" | "store" | "graphson" | "graphml",
//! "layerBase"?, "modelsMissing": [repo...], "skipped": n, "notes": [text...]}`; a database
//! whose snapshot names a model it does not carry has the state `needs-model`.
//!
//! An entry: `name`, `state` (`unloaded` | `loading` | `loaded` | `needs-model` | `failed`),
//! `kind` (`plain` | `layer`), `role` (`normal` | `base`), `readOnly` (a base is always
//! read-only), `autoLoad`, `description`, and when they apply `base` (a layer's base), `created`,
//! `reason` (`failed`: why; `needs-model`: the missing model), `model` (`needs-model`: the
//! model's repository), `aliasOf` (an alias row: the database it names; its other fields are
//! that database's). A database whose `database.toml` cannot be read has no `kind`, `role`,
//! `autoLoad` or `description`. The field names are those of the playground's landing view.

use std::fs::File;
use std::path::{Path, PathBuf};

use graphersal_serve::{HttpError, Method, Request, Response};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::database::{DatabaseKind, DatabaseRole};
use crate::error::ServerError;
use crate::loader::LoadStart;
use crate::names::fold;
use crate::ops::{
    Admin, BackupInfo, CreateSpec, ExportFile, ExportSpec, ImportFormat, ImportSource, TmpFile,
};
use crate::registry::{DatabaseEntry, DbState, Registry};

/// Answers a request below `/api/admin/` (`endpoint` = the path after `/api/admin/`, without
/// the query).
pub(crate) fn route(
    admin: &Admin,
    method: &Method,
    endpoint: &str,
    request: &mut Request<'_>,
) -> Result<Response, HttpError> {
    let registry = admin.registry();
    let not_allowed = || {
        HttpError::new(
            405,
            format!("method {method} is not allowed on /api/admin/{endpoint}"),
        )
    };
    let read = matches!(method, Method::Get | Method::Head);
    let segments: Vec<&str> = endpoint.split('/').collect();
    let answer = match (method, segments.as_slice()) {
        (_, ["databases"]) if read => Ok(Response::json(200, &list(registry))),
        (Method::Post, ["databases"]) => {
            let body: CreateBody = body_of(request)?;
            let name = body.name.clone();
            let spec = body.into_spec();
            admin
                .create(&name, &spec)
                .map(|entry| Response::json(201, &entry_json(&entry, None)))
                .map_err(|err| (err, None))
        }
        (_, ["databases", name]) if read && !name.is_empty() => {
            return one(registry, name)
                .map(|entry| Response::json(200, &entry))
                .ok_or_else(|| HttpError::new(404, format!("no database or alias '{name}'")));
        }
        (Method::Delete, ["databases", name]) if !name.is_empty() => admin
            .delete(name)
            .map(|deleted| {
                Response::json(
                    200,
                    &json!({
                        "name": deleted.name,
                        "trash": relative(registry.dir().root(), &deleted.trash),
                    }),
                )
            })
            .map_err(|err| {
                let help = delete_help(registry, name);
                (err, help)
            }),
        (Method::Post, ["databases", name, "clone"]) => {
            let body: CloneBody = body_of(request)?;
            admin
                .clone_database(name, &body.name)
                .map(|entry| Response::json(201, &entry_json(&entry, None)))
                .map_err(|err| (err, None))
        }
        (Method::Post, ["databases", name, "load"]) => admin
            .load(name)
            .and_then(|start| {
                let (status, word) = match start {
                    LoadStart::Started => (202, "started"),
                    LoadStart::AlreadyLoading => (202, "already-loading"),
                    LoadStart::AlreadyLoaded => (200, "already-loaded"),
                };
                let mut entry = entry_json(&registry.get(name)?, None);
                entry["load"] = Value::String(word.to_string());
                Ok(Response::json(status, &entry))
            })
            .map_err(|err| (err, None)),
        (Method::Post, ["databases", name, "unload"]) => admin
            .unload(name)
            .and_then(|unloaded| {
                let mut entry = entry_json(&registry.get(name)?, None);
                entry["unloaded"] = Value::Bool(unloaded);
                Ok(Response::json(200, &entry))
            })
            .map_err(|err| (err, None)),
        (Method::Put, ["databases", name, "read-only"]) => {
            let body: ReadOnlyBody = body_of(request)?;
            admin
                .set_read_only(name, body.read_only)
                .map(|entry| Response::json(200, &entry_json(&entry, None)))
                .map_err(|err| (err, None))
        }
        (Method::Put, ["databases", name, "auto-load"]) => {
            let body: AutoLoadBody = body_of(request)?;
            admin
                .set_auto_load(name, body.auto_load)
                .map(|entry| Response::json(200, &entry_json(&entry, None)))
                .map_err(|err| (err, None))
        }
        (Method::Post, ["databases", name, "backup"]) => admin
            .backup(name)
            .map(|info| Response::json(201, &backup_json(registry, &info)))
            .map_err(|err| (err, None)),
        (_, ["databases", name, "backups"]) if read => admin
            .backups(name)
            .map(|list| {
                let rows: Vec<Value> = list.iter().map(|b| backup_json(registry, b)).collect();
                Response::json(200, &json!({ "database": fold(name), "backups": rows }))
            })
            .map_err(|err| (err, None)),
        (Method::Post, ["databases", name, "restore"]) => {
            let body: RestoreBody = body_of(request)?;
            admin
                .restore(name, &body.backup, body.name.as_deref())
                .map(|restored| {
                    let mut entry = entry_json(&restored.entry, None);
                    entry["restoredFrom"] = json!({
                        "database": restored.from_database,
                        "backup": restored.backup,
                    });
                    entry["replaced"] = Value::Bool(restored.replaced.is_some());
                    if let Some(trash) = &restored.replaced {
                        entry["trash"] = Value::String(relative(registry.dir().root(), trash));
                    }
                    Response::json(201, &entry)
                })
                .map_err(|err| {
                    let target = body.name.as_deref().unwrap_or(name);
                    let help = restore_help(registry, target, &err);
                    (err, help)
                })
        }
        (Method::Post, ["import"]) => {
            return if is_json(request) {
                import_path(admin, request)
            } else {
                import_upload(admin, request)
            };
        }
        (_, ["aliases"]) if read => Ok(Response::json(
            200,
            &json!({ "aliases": registry.aliases() }),
        )),
        (Method::Put, ["aliases", alias]) if !alias.is_empty() => {
            let body: AliasBody = body_of(request)?;
            admin
                .alias_set(alias, &body.target)
                .map(|()| {
                    Response::json(
                        200,
                        &json!({ "alias": fold(alias), "target": fold(&body.target) }),
                    )
                })
                .map_err(|err| (err, None))
        }
        (Method::Delete, ["aliases", alias]) if !alias.is_empty() => admin
            .alias_remove(alias)
            .map(|()| Response::json(200, &json!({ "alias": fold(alias), "removed": true })))
            .map_err(|err| (err, None)),
        (_, ["databases"])
        | (_, ["databases", _])
        | (
            _,
            [
                "databases",
                _,
                "clone" | "load" | "unload" | "read-only" | "auto-load" | "backup" | "backups"
                | "restore" | "export",
            ],
        )
        | (_, ["aliases"])
        | (_, ["aliases", _])
        | (_, ["import"]) => return Err(not_allowed()),
        _ => {
            return Err(HttpError::new(
                404,
                format!("unknown admin endpoint /api/admin/{endpoint}"),
            ));
        }
    };
    Ok(answer.unwrap_or_else(|(err, help)| error_answer(&err, help)))
}

/// Whether `request` carries a JSON body (`Content-Type: application/json`).
fn is_json(request: &Request<'_>) -> bool {
    request
        .header("Content-Type")
        .is_some_and(|value| value.to_ascii_lowercase().contains("application/json"))
}

/// Whether `method path` with the content type `content_type` is an import upload (the
/// listener lets its body pass up to the upload limit).
pub(crate) fn is_upload(method: &Method, path: &str, content_type: Option<&str>) -> bool {
    matches!(method, Method::Post)
        && path == "/api/admin/import"
        && !content_type
            .is_some_and(|value| value.to_ascii_lowercase().contains("application/json"))
}

/// `POST import {"path", ...}`: a file or directory on the server's machine.
fn import_path(admin: &Admin, request: &mut Request<'_>) -> Result<Response, HttpError> {
    let body: ImportBody = body_of(request)?;
    let path = PathBuf::from(&body.path);
    if !path.is_absolute() {
        return Err(HttpError::bad_request(format!(
            "import: the path {:?} is not absolute (the server resolves it on its own machine)",
            body.path
        )));
    }
    let format = parse_format(body.format.as_deref())?;
    let source = ImportSource::new(path).with_format(format);
    Ok(import_answer(
        admin,
        &source,
        &body.name,
        body.base.as_deref(),
        body.description.as_deref(),
    ))
}

/// `POST import?name=...` with the file as the body: streamed into `tmp/`, then imported.
fn import_upload(admin: &Admin, request: &mut Request<'_>) -> Result<Response, HttpError> {
    let query = request
        .url()
        .split_once('?')
        .map(|(_, query)| query.to_string())
        .unwrap_or_default();
    let mut name = None;
    let mut base = None;
    let mut format = None;
    let mut description = None;
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = query_decode(value)
            .ok_or_else(|| HttpError::bad_request(format!("import: {key}: a broken escape")))?;
        let slot = match key {
            "name" => &mut name,
            "base" => &mut base,
            "format" => &mut format,
            "description" => &mut description,
            other => {
                return Err(HttpError::bad_request(format!(
                    "import: unknown parameter {other:?} (name, base, format, description)"
                )));
            }
        };
        if slot.replace(value).is_some() {
            return Err(HttpError::bad_request(format!("import: {key} given twice")));
        }
    }
    let Some(name) = name.filter(|n| !n.is_empty()) else {
        return Err(HttpError::bad_request(
            "import: name the new database: POST /api/admin/import?name=<name> with the file \
             as the body",
        ));
    };
    let format = parse_format(format.as_deref())?;
    // The quick refusals before the body is read (the listener drains it).
    let key = fold(&name);
    if let Err(err) = crate::names::validate_name(&key) {
        return Ok(error_answer(&err, None));
    }
    if let Ok(entry) = admin.registry().get(&key) {
        let err = ServerError::NameInUse {
            name: entry.name,
            holder: "database",
        };
        return Ok(error_answer(&err, None));
    }
    let registry = admin.registry();
    let limit = match registry.dir().load_config() {
        Ok(config) => config.limits.upload_bytes,
        Err(err) => return Ok(error_answer(&err, None)),
    };
    let upload = match TmpFile::new(&registry.dir().tmp(), "upload", "bin") {
        Ok(upload) => upload,
        Err(err) => return Ok(error_answer(&err, None)),
    };
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(upload.path())
        .map_err(|err| HttpError::new(500, format!("cannot store the upload: {err}")))?;
    let mut out = std::io::BufWriter::new(file);
    let received = request.copy_body_to(&mut out, limit).map_err(|err| {
        if err.status() == 413 {
            HttpError::new(
                413,
                format!(
                    "{}; the server's upload limit is [limits] upload_bytes in server.toml",
                    err.message()
                ),
            )
        } else {
            err
        }
    })?;
    let file = out
        .into_inner()
        .map_err(|err| HttpError::new(500, format!("cannot store the upload: {err}")))?;
    file.sync_all()
        .map_err(|err| HttpError::new(500, format!("cannot store the upload: {err}")))?;
    drop(file);
    if received == 0 {
        return Err(HttpError::bad_request(
            "import: the body is empty; send the file as the request body",
        ));
    }
    tracing::info!(
        target: "graphersal_server",
        "import upload for '{key}': {received} bytes received"
    );
    let source = ImportSource::new(upload.path()).with_format(format);
    let answer = import_answer(
        admin,
        &source,
        &name,
        base.as_deref(),
        description.as_deref(),
    );
    // The upload is removed now (the import copied what it needs).
    drop(upload);
    Ok(answer)
}

/// The format hint of an import (`400` for an unknown one).
fn parse_format(hint: Option<&str>) -> Result<Option<ImportFormat>, HttpError> {
    match hint.filter(|h| !h.is_empty()) {
        None => Ok(None),
        Some(hint) => ImportFormat::parse(hint).map(Some).ok_or_else(|| {
            HttpError::bad_request(format!(
                "import: unknown format {hint:?} (snapshot, store, graphson, graphml)"
            ))
        }),
    }
}

/// The answer of an import: `201` entry + `"import"`, or the error.
fn import_answer(
    admin: &Admin,
    source: &ImportSource,
    name: &str,
    base: Option<&str>,
    description: Option<&str>,
) -> Response {
    match admin.import(source, name, base, description) {
        Ok(imported) => {
            let mut entry = entry_json(&imported.entry, None);
            let mut details = json!({
                "format": imported.format.as_str(),
                "modelsMissing": imported.models_missing,
                "skipped": imported.skipped,
                "notes": imported.notes,
            });
            if let Some(layer_base) = &imported.layer_base {
                details["layerBase"] = json!(layer_base);
            }
            entry["import"] = details;
            Response::json(201, &entry)
        }
        Err(err) => {
            let help = match &err {
                ServerError::Refused { .. } => Some(
                    "a layer is imported over a base database with its base identity: import \
                     or restore that base first (role = base), or name it with base=<name>"
                        .to_string(),
                ),
                _ => None,
            };
            error_answer(&err, help)
        }
    }
}

/// A query value `%XX`- and `+`-decoded; `None` for a broken escape or a result that is not
/// UTF-8.
fn query_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = value.get(i + 1..i + 3)?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// `POST import` with a JSON body.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportBody {
    path: String,
    name: String,
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

/// The JSON body of `request` as `T` (`400` for a body that does not parse or fit).
fn body_of<T: for<'de> Deserialize<'de>>(request: &mut Request<'_>) -> Result<T, HttpError> {
    let value = request.json_body()?;
    serde_json::from_value(value)
        .map_err(|err| HttpError::bad_request(format!("invalid request body: {err}")))
}

/// `POST /api/admin/databases`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CreateBody {
    name: String,
    #[serde(default)]
    kind: Option<DatabaseKind>,
    #[serde(default)]
    role: Option<DatabaseRole>,
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    schema: Option<Value>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    auto_load: bool,
    #[serde(default)]
    read_only: bool,
}

impl CreateBody {
    /// A `base` without a `kind` makes a layer.
    fn into_spec(self) -> CreateSpec {
        let kind = self.kind.unwrap_or(if self.base.is_some() {
            DatabaseKind::Layer
        } else {
            DatabaseKind::Plain
        });
        let mut spec = match (kind, self.base) {
            (DatabaseKind::Layer, Some(base)) => CreateSpec::layer(base),
            (kind, base) => {
                let mut spec = CreateSpec::plain();
                spec.kind = kind;
                spec.base = base;
                spec
            }
        }
        .with_role(self.role.unwrap_or_default())
        .with_description(self.description.unwrap_or_default())
        .with_auto_load(self.auto_load)
        .with_read_only(self.read_only);
        spec.schema = self.schema.filter(|schema| !schema.is_null());
        spec
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RestoreBody {
    backup: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CloneBody {
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ReadOnlyBody {
    read_only: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct AutoLoadBody {
    auto_load: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AliasBody {
    target: String,
}

/// The status of an admin core error.
pub(crate) fn status_of(err: &ServerError) -> u16 {
    match err {
        ServerError::InvalidName { .. } | ServerError::InvalidInput { .. } => 400,
        ServerError::NotFound { .. }
        | ServerError::BackupNotFound { .. }
        | ServerError::SourceNotFound { .. } => 404,
        ServerError::UnsupportedSource { .. } => 415,
        ServerError::NameInUse { .. }
        | ServerError::Refused { .. }
        | ServerError::DatabaseFailed { .. }
        | ServerError::Locked { .. } => 409,
        ServerError::Remote { status, .. } => *status,
        _ => 500,
    }
}

/// `{"error", "help"?}` for an admin core error.
fn error_answer(err: &ServerError, help: Option<String>) -> Response {
    let help = help.or_else(|| match err {
        ServerError::NotFound { .. } => {
            Some("GET /api/admin/databases lists the databases and aliases".to_string())
        }
        ServerError::BackupNotFound { database, .. } => Some(format!(
            "GET /api/admin/databases/{database}/backups lists its backups"
        )),
        _ => None,
    });
    let mut body = json!({ "error": err.to_string() });
    if let Some(help) = help {
        body["help"] = Value::String(help);
    }
    Response::json(status_of(err), &body)
}

/// The help of a refused delete: what to do first.
fn delete_help(registry: &Registry, name: &str) -> Option<String> {
    let key = fold(name);
    let entry = registry.get(&key).ok()?;
    if matches!(entry.state, DbState::Loaded | DbState::Loading) {
        return Some(format!(
            "unload it first: POST /api/admin/databases/{key}/unload"
        ));
    }
    let alias = registry
        .aliases()
        .into_iter()
        .find(|(_, target)| *target == key)?;
    Some(format!(
        "remove the alias first: DELETE /api/admin/aliases/{}, or point it at another          database: PUT /api/admin/aliases/{} {{\"target\": \"<database>\"}}",
        alias.0, alias.0
    ))
}

/// The help of a refused restore: unload the target first.
fn restore_help(registry: &Registry, target: &str, err: &ServerError) -> Option<String> {
    let key = fold(target);
    let entry = registry.get(&key).ok()?;
    (matches!(err, ServerError::Refused { .. })
        && matches!(entry.state, DbState::Loaded | DbState::Loading))
    .then(|| format!("unload it first: POST /api/admin/databases/{key}/unload"))
}

/// A backup as JSON (module docs).
fn backup_json(registry: &Registry, info: &BackupInfo) -> Value {
    let mut row = json!({
        "database": info.database,
        "backup": info.backup,
        "file": relative(registry.dir().root(), &info.file),
        "bytes": info.bytes,
    });
    if let Some(created) = &info.created {
        row["created"] = json!(created);
    }
    if let Some(kind) = info.kind {
        row["kind"] = json!(kind.as_str());
    }
    if let Some(role) = info.role {
        row["role"] = json!(role.as_str());
    }
    if let Some(base) = &info.base {
        row["base"] = json!(base);
    }
    if let Some(layer_base) = &info.layer_base {
        row["layerBase"] = json!(layer_base);
    }
    row
}

/// The database of `GET databases/<name>/export` (the router streams its answer), else `None`.
pub(crate) fn export_target<'a>(method: &Method, endpoint: &'a str) -> Option<&'a str> {
    match endpoint.split('/').collect::<Vec<_>>().as_slice() {
        ["databases", name, "export"] if matches!(method, Method::Get) && !name.is_empty() => {
            Some(name)
        }
        _ => None,
    }
}

/// The export spec of the query `models=true|false&materialize=true|false` (`400` for any
/// other parameter or value).
pub(crate) fn export_spec(query: Option<&str>) -> Result<ExportSpec, HttpError> {
    let mut spec = ExportSpec::new();
    for pair in query
        .unwrap_or_default()
        .split('&')
        .filter(|p| !p.is_empty())
    {
        let (key, value) = pair.split_once('=').unwrap_or((pair, "true"));
        let flag = match value {
            "true" | "1" | "yes" => true,
            "false" | "0" | "no" => false,
            other => {
                return Err(HttpError::bad_request(format!(
                    "export: {key}={other} is not true or false"
                )));
            }
        };
        spec = match key {
            "models" => spec.with_models(flag),
            "materialize" => spec.with_materialize(flag),
            other => {
                return Err(HttpError::bad_request(format!(
                    "export: unknown parameter {other:?} (models, materialize)"
                )));
            }
        };
    }
    Ok(spec)
}

/// An export download: the `.gsnap` written into `tmp/`, removed when this is dropped.
pub(crate) struct Download {
    export: ExportFile,
    file: File,
    length: u64,
}

impl Download {
    /// The answer's headers (module docs).
    pub(crate) fn headers(&self) -> Vec<(String, String)> {
        let exported = &self.export.exported;
        vec![
            ("Content-Type".into(), "application/octet-stream".into()),
            (
                "Content-Disposition".into(),
                format!("attachment; filename=\"{}\"", exported.file_name),
            ),
            ("Cache-Control".into(), "no-store".into()),
            ("X-Content-Type-Options".into(), "nosniff".into()),
            (
                "X-Graphersal-Snapshot".into(),
                if exported.layer { "layer" } else { "full" }.into(),
            ),
            (
                "X-Graphersal-Models".into(),
                if exported.models {
                    "included"
                } else {
                    "excluded"
                }
                .into(),
            ),
        ]
    }

    /// The file's length in bytes.
    pub(crate) fn length(&self) -> u64 {
        self.length
    }

    /// The open file to stream and the guard that removes it (drop the guard after the file).
    pub(crate) fn into_parts(self) -> (File, ExportFile) {
        (self.file, self.export)
    }

    /// The download as an in-memory answer (tests; a file that cannot be read answers `500`).
    #[cfg(test)]
    pub(crate) fn into_response(self) -> Response {
        use std::io::Read;
        let headers = self.headers();
        let (mut file, _guard) = self.into_parts();
        let mut bytes = Vec::new();
        if let Err(err) = file.read_to_end(&mut bytes) {
            return Response::json(
                500,
                &json!({ "error": format!("cannot read the export: {err}") }),
            );
        }
        headers
            .iter()
            .fold(Response::new(200, bytes), |r, (name, value)| {
                r.with_header(name, value)
            })
    }
}

/// `GET databases/<name>/export?...`: the export written into `tmp/`, as a download; an error
/// as its JSON answer.
pub(crate) fn export(admin: &Admin, name: &str, query: Option<&str>) -> Result<Download, Response> {
    let spec = export_spec(query)
        .map_err(|err| Response::json(err.status(), &json!({ "error": err.message() })))?;
    let export = admin
        .export_to_tmp(name, spec)
        .map_err(|err| error_answer(&err, None))?;
    let opened = File::open(export.path()).and_then(|file| {
        let length = file.metadata()?.len();
        Ok((file, length))
    });
    match opened {
        Ok((file, length)) => Ok(Download {
            export,
            file,
            length,
        }),
        Err(err) => Err(Response::json(
            500,
            &json!({ "error": format!("cannot read the export: {err}") }),
        )),
    }
}

/// `path` relative to the server directory `root`, with `/` separators.
fn relative(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// `GET /api/admin/databases`.
fn list(registry: &Registry) -> Value {
    let entries = registry.list();
    let aliases = registry.aliases();
    let mut databases: Vec<Value> = entries.iter().map(|e| entry_json(e, None)).collect();
    for (alias, target) in &aliases {
        if let Some(entry) = entries.iter().find(|e| &e.name == target) {
            databases.push(entry_json(entry, Some(alias)));
        }
    }
    databases.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    json!({
        "databases": databases,
        "aliases": aliases,
        "problems": registry.problems(),
    })
}

/// `GET /api/admin/databases/<name>`: the database or alias `name`, `None` when unknown.
fn one(registry: &Registry, name: &str) -> Option<Value> {
    if let Ok(entry) = registry.get(name) {
        return Some(entry_json(&entry, None));
    }
    let alias = fold(name);
    registry.aliases().get(&alias)?;
    let entry = registry.resolve(&alias).ok()?;
    Some(entry_json(&entry, Some(&alias)))
}

/// One entry of the listing (module docs); `alias`: the row of that alias of `entry`.
pub(crate) fn entry_json(entry: &DatabaseEntry, alias: Option<&str>) -> Value {
    let mut out = Map::new();
    out.insert(
        "name".into(),
        Value::String(alias.unwrap_or(&entry.name).to_string()),
    );
    if alias.is_some() {
        out.insert("aliasOf".into(), Value::String(entry.name.clone()));
    }
    out.insert("state".into(), Value::String(entry.state.as_str().into()));
    match &entry.state {
        DbState::Failed { reason } => {
            out.insert("reason".into(), Value::String(reason.clone()));
        }
        DbState::NeedsModel { repo } => {
            out.insert(
                "reason".into(),
                Value::String(format!("the embedding model '{repo}' is missing")),
            );
            out.insert("model".into(), Value::String(repo.clone()));
        }
        DbState::Unloaded | DbState::Loading | DbState::Loaded => {}
    }
    if let Some(config) = &entry.config {
        out.insert("kind".into(), Value::String(config.kind.as_str().into()));
        out.insert("role".into(), Value::String(config.role.as_str().into()));
        out.insert(
            "readOnly".into(),
            Value::Bool(config.read_only || config.role == DatabaseRole::Base),
        );
        out.insert("autoLoad".into(), Value::Bool(config.auto_load));
        out.insert(
            "description".into(),
            Value::String(config.description.clone()),
        );
        if let Some(base) = &config.base {
            out.insert("base".into(), Value::String(base.clone()));
        }
        if let Some(created) = &config.created {
            out.insert("created".into(), Value::String(created.clone()));
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::tests::{add_db, server};
    use crate::test_support::TempDir;

    /// The route over an offline admin of `registry`, without a body.
    fn route(
        registry: &std::sync::Arc<Registry>,
        method: &Method,
        endpoint: &str,
    ) -> Result<Response, HttpError> {
        let mut request = Request::new(method.clone(), "/", std::io::empty());
        super::route(
            &Admin::offline(registry.clone()),
            method,
            endpoint,
            &mut request,
        )
    }

    fn json_of(response: &Response) -> Value {
        serde_json::from_slice(response.body()).unwrap()
    }

    #[test]
    fn the_listing_shows_databases_aliases_and_states() {
        let temp = TempDir::new("admin-api-list");
        let dir = server(&temp);
        add_db(
            &dir,
            "acme",
            Some("description = \"Acme\"\nauto_load = true"),
            true,
        );
        add_db(&dir, "prod", Some("role = \"base\""), true);
        add_db(&dir, "l1", Some("kind = \"layer\"\nbase = \"prod\""), true);
        add_db(&dir, "broken", Some("bogus = 1"), true);
        let registry = Registry::scan(&dir).unwrap();
        registry.alias_set("current", "acme").unwrap();
        registry
            .set_state("l1", DbState::needs_model("org/m"))
            .unwrap();
        let listing = json_of(&route(&registry, &Method::Get, "databases").unwrap());
        let names: Vec<&str> = listing["databases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["acme", "broken", "current", "l1", "prod"]);
        let rows = &listing["databases"];
        assert_eq!(rows[0]["state"], "unloaded");
        assert_eq!(rows[0]["kind"], "plain");
        assert_eq!(rows[0]["role"], "normal");
        assert_eq!(rows[0]["readOnly"], false);
        assert_eq!(rows[0]["autoLoad"], true);
        assert_eq!(rows[0]["description"], "Acme");
        assert!(rows[0].get("aliasOf").is_none());
        assert_eq!(rows[1]["state"], "failed");
        assert!(rows[1]["reason"].as_str().unwrap().contains("bogus"));
        assert!(rows[1].get("kind").is_none());
        assert_eq!(rows[2]["aliasOf"], "acme");
        assert_eq!(rows[2]["description"], "Acme");
        assert_eq!(rows[3]["state"], "needs-model");
        assert_eq!(rows[3]["model"], "org/m");
        assert_eq!(rows[3]["base"], "prod");
        assert_eq!(rows[4]["role"], "base");
        assert_eq!(rows[4]["readOnly"], true, "a base is always read-only");
        assert_eq!(listing["aliases"], json!({"current": "acme"}));

        let one = json_of(&route(&registry, &Method::Get, "databases/ACME").unwrap());
        assert_eq!(one["name"], "acme");
        let alias = json_of(&route(&registry, &Method::Get, "databases/current").unwrap());
        assert_eq!(alias["aliasOf"], "acme");
        assert_eq!(
            route(&registry, &Method::Get, "databases/none")
                .err()
                .unwrap()
                .status(),
            404
        );
        assert_eq!(
            route(&registry, &Method::Put, "databases")
                .err()
                .unwrap()
                .status(),
            405
        );
        assert_eq!(
            route(&registry, &Method::Get, "nothing")
                .err()
                .unwrap()
                .status(),
            404
        );
    }
}
