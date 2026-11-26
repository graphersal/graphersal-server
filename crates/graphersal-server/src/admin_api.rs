//! The admin API over HTTP (`/api/admin/...`; the `Host`/`Origin` checks ran before). Every
//! action calls the admin core ([`crate::ops::Admin`]), the same as the console's.
//!
//! | Method | Path | Body | Answer |
//! |---|---|---|---|
//! | GET | `databases` | | `200 {"databases": [entry...], "aliases": {alias: database}, "aliasesRevision", "problems": [text...], "newDatabases": {"autoLoad", "readOnly", "mcp"}}` (`newDatabases`: what a create or import gets when it does not say) |
//! | POST | `databases` | `{"name", "kind"?, "role"?, "base"?, "schema"?, "description"?, "autoLoad"?, "readOnly"?, "mcp"?}` | `201` entry |
//! | GET | `databases/<name>` | | `200` entry (`<name>` may be an alias); `ETag` = its `revision` |
//! | DELETE | `databases/<name>` | | `200 {"name", "trash", "grantsRemoved"}` (its grants go with it) |
//! | POST | `databases/<name>/clone` | `{"name", "mcp"?}` | `201` entry of the clone |
//! | POST | `databases/<name>/load` | | `202` entry + `"load": "started" \| "already-loading"`, `200` + `"already-loaded"` |
//! | POST | `databases/<name>/unload` | | `200` entry + `"unloaded": bool` |
//! | POST | `databases/<name>/promote` | | `200` entry (now `role: base`, read-only; one way) |
//! | POST | `databases/<name>/reset-layer` | `{"confirm": true}` | `200` entry + `"base"`, `"trash"`, `"discarded"` (`{"added", "modified", "removed"}`, each `{"vertices", "edges"}`), `"jobsKept"`, `"reloaded"`: a layer's changes discarded ([`crate::ops::Admin::reset_layer`]); `400` without the confirmation, `409` for a plain, base or failed database |
//! | PUT | `databases/<name>/read-only` | `{"readOnly": bool, "revision"?}` | `200` entry |
//! | PUT | `databases/<name>/auto-load` | `{"autoLoad": bool, "revision"?}` | `200` entry |
//! | POST | `databases/<name>/backup` | | `201` backup |
//! | GET | `databases/<name>/backups` | | `200 {"database", "backups": [backup...]}` (also for a deleted database's name) |
//! | POST | `databases/<name>/restore` | `{"backup", "name"?, "mcp"?}` | `201` entry + `"restoredFrom": {"database", "backup"}`, `"replaced"`, `"trash"?` |
//! | GET | `databases/<name>/export?models=true\|false&materialize=true\|false` | | `200` the `.gsnap` file (download) |
//! | GET | `server-backup?withDatabases=1` | | `200` the server backup ZIP (download, [`crate::server_backup`]); the operator and admin users only (`everyone` `403`, answered by the router), audited |
//! | POST | `import?name=<n>&base=<b>&format=<f>&description=<d>&role=<normal\|base>&mcp=<true\|false>` | the file itself (any content type but JSON) | `201` entry + `"import"` |
//! | POST | `import` | JSON `{"path" \| "inbox", "name", "base"?, "format"?, "description"?, "role"?, "mcp"?}` (`path`: a file or directory on the server's machine, absolute; `inbox`: an entry name of the import inbox `imports/`) | `201` entry + `"import"` |
//! | GET | `imports` | | `200 {"dir", "imports": [{"name", "kind", "directory", "bytes", "modified"}...]}`: the import inbox |
//! | GET | `models` | | `200 {"models": [model...], "missing": [{"database", "model"}...]}` (rebuilds `models/models.toml`) |
//! | POST | `models` | `{"path"?, "repo"?, "license"?}` (`path`: a Model2Vec directory or a `.gsnap`, absolute, on the server's machine; `repo` alone: a model `models/` holds) | `201` (from a path) / `200` `{"added": [model...], "resolved": [database...], "stillMissing": [{"database", "model"}...], "notes": [text...]}` |
//! | POST | `models?repo=<r>&license=<l>&format=<gsnap\|tar>` | the file itself (any content type but JSON): a `.gsnap` carrying model blobs or a tar archive (or `.tar.gz`) of a Model2Vec model directory (format detected unless given) | `201` as above |
//! | DELETE | `models/<repo or blob hash>` | | `200 {"model", "removed": [blob...], "bytes"}`; `409` while a database uses it |
//! | GET | `runs` | | `200 {"runs": [run...], "textHeadChars"}`: every loaded database's queued and running runs |
//! | GET | `databases/<name>/runs` | | `200 {"database", "loaded", "runs": [run...], "textHeadChars"}` |
//! | POST | `databases/<name>/runs/cancel` | `{"id"}` | `200 {"database", "id", "cancelled": bool}` (`false`: no such run now) |
//! | GET | `aliases` | | `200 {"aliases": {alias: database}, "revision"}` (+ `ETag`) |
//! | PUT | `aliases/<alias>` | `{"target", "mcp"?, "revision"?}` | `200 {"alias", "target", "mcpExposed"}` |
//! | DELETE | `aliases/<alias>` | | `200 {"alias", "removed": true, "grantsRemoved"}` (its grants go with it) |
//! | POST | `aliases/<alias>/rename` | `{"name", "revision"?}` | `200 {"alias", "renamedFrom", "target", "mcpSessionsMoved", "grantsMoved"}` |
//! | GET, POST, PUT, DELETE | `users...`, `grants...` | | the users and grants of `access.toml` (the `admin_api_users.rs` table) |
//! | GET, PUT | `mcp`, `mcp/databases/<name>` | | the MCP switches of `mcp.toml` and the usage statistics (the `admin_api_mcp.rs` table) |
//! | GET, PUT | `config` | `{"set": {key: value \| null}, "revision"?}` | every key of `server.toml` and its change at runtime (the `admin_api_config.rs` table) |
//! | POST | `databases/<name>/verify`, `verify` | | verify one database's Store, or every database's (the `admin_api_damage.rs` table) |
//! | POST | `databases/<name>/repair`, `databases/<name>/swap` | `{"name"?}`, `{"with", "confirm"}` | repair into a new database; let one database take another's name (the `admin_api_damage.rs` table) |
//! | GET | `recovery` | | `200 {"uncleanShutdown"}`: the last unclean shutdown and its check |
//! | GET, PUT | `databases/<name>/settings` | `{"description"?, "readOnly"?, "autoLoad"?, "limits"?: {key: n \| null}, "revision"?}` | everything about one database, and the change of its editable settings (the `admin_api_settings.rs` table) |
//!
//! MCP exposure of new names (round 4 B-MCPDEF; [`crate::mcp_control`]): a new database
//! (create, clone, import, a restore into a new name, repair) or a new alias is hidden from
//! MCP unless the body says `"mcp": true` (an upload import: `mcp=true`) or `mcp.toml`'s
//! `expose_new` is on; `"mcp": false` hides it whatever the default. The answer carries
//! `"mcpExposed"` (the new name's own MCP setting).
//!
//! A new database or alias never inherits grants left on its name (from a database or alias
//! deleted while an older version ran, or whose grants could not be removed then): create,
//! clone, import, a restore into a new or a deleted database's name, a new alias (`PUT
//! aliases/<alias>`) and a rename's new name remove them first, each audited as a revoked
//! grant, and the answer then carries `"leftoverGrantsRemoved": n` (absent when none).
//!
//! Revisions ([`crate::revision`]): the changes of users and grants, of aliases and of a
//! database's settings (read-only, auto-load, `settings`) are based on the revision in the body's `revision`, else in
//! `If-Match` (none: no check); a stale one is `409 {"error", "help": "changed by someone else:
//! reload", "revision": <current>}`, nothing changed.
//!
//! The actions take real database names (not aliases). Errors are `{"error", "help"?}` with
//! `400` (invalid name, body or schema), `404` (no such database, alias or endpoint), `405`,
//! `409` (a rule refuses it: name taken, loaded, named by an alias, base of layers, offline,
//! failed database, a stale revision), `413` (body too large), `415` (an import source of no known format),
//! `500` (a Store or file failed). The repository's
//! `book/src/reference/admin_api.md` documents the routes for clients.
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
//! A model upload (`POST models?...` with the file as the body) is streamed into `tmp/` the
//! same way (same limit), then added like a path: a tar archive (ustar, uncompressed or
//! gzip-compressed: decompressed first, at most the upload limit) is unpacked into `tmp/` first, refusing absolute paths, `..`, links, devices and more than
//! 1000 entries (`400`: `not a usable model archive: ...`); a file that is neither a `.gsnap`
//! nor a tar archive, or not of the `format` given, is `400`.
//!
//! The import inbox (`imports/` of the server directory, [`crate::ops::Admin::inbox`]): `dir`
//! is its absolute path on the server's machine; an entry's `kind` (`snapshot` | `store` |
//! `graphson` | `graphml` | `null`) is guessed from its name (a directory: a Store when it holds
//! one; the import still detects the content), `bytes` its size (a directory: every file below
//! it), `modified` RFC 3339 UTC or `null`. Hidden entries (a leading `.`) and links leading
//! outside `imports/` are not listed. An import by `inbox` takes ONE entry name: a name with a
//! separator, a leading `.` (`..` included) is `400`, a missing entry `404`, a link leading
//! outside `imports/` `409`.
//!
//! `"import"` of the answer: `{"format": "snapshot" | "store" | "graphson" | "graphml",
//! "layerBase"?, "modelsMissing": [repo...], "modelsResolved": [repo...], "skipped": n,
//! "notes": [text...]}`; a model the source names without carrying it is taken from the shared
//! `models/` when that holds it (`modelsResolved`), else the database has the state
//! `needs-model` (`modelsMissing`). A Store (directory or `.gstore`) lacking a model blob gets
//! it filled from `models/` (hard-linked afterwards; its model in `modelsResolved`, a note
//! says it was filled); a blob `models/` lacks too is left out of the copy and the database
//! is `needs-model` (`modelsMissing`), one `models/` holds damaged `500` naming the file. `role=base` makes a base database (read-only, frozen) of
//! a plain source; a layer source is `400`, a source naming a model it does not carry and
//! `models/` lacks `409`.
//!
//! A run (the run administration, [`crate::ops::Admin::runs`]): `database`, `id` (unique within
//! the database), `client` (`page:<id>`, `mcp:<session>`, ...), `alias` (the client's own run
//! id or `null`), `kind` (`query` | `layout` | `tool` | `other`), `state` (`queued` |
//! `running`), `cancelled`, `startedAtMs` (Unix milliseconds), `durationMs`, `text` (the first
//! `textHeadChars` characters of its query text, or `null`). Every client's runs are listed,
//! whoever asks: the query texts are other clients' data, readable through the admin surface
//! only (the databases' mounts keep graphersal-serve's own `/db/<name>/api/admin/runs` off).
//! Offline (the console on a stopped server) the routes are `409`.
//!
//! A promotion (`POST databases/<name>/promote`) turns an UNLOADED plain database into a base
//! (`role: base`, read-only) for good: a base is frozen forever and no route turns it back.
//! `409` while it is loaded (help: unload it), for a layer, a base, a `needs-model` or failed
//! database.
//!
//! A model (one blob of `models/blobs/`): `repo`, `identity`, `license` (from the model's own
//! manifest; `null` for a blob that is not a model archive), `blob` (SHA-256 hex), `bytes`,
//! `users` (databases whose Store holds it), `waiting` (`needs-model` databases waiting for its
//! repository), `copied` (bool) + `copiedIn` (users holding a COPY because the file system
//! refused the hard link) + `linkWarning` (text, when copied), `links` (unix: its hard links).
//! Adding a model resolves the `needs-model` databases whose model it is (state `unloaded`).
//!
//! The listing rows (`GET databases`, `GET databases/<name>`) add `sizeBytes` (the database's
//! directory on disk; a model blob linked from `models/` counts in full in every database
//! holding it, so the sum over databases is more than the disk use), `pooled: true` +
//! `pooledBy: [layer...]` (a base held in the loader's pool by its loaded layers or itself) and
//! `linkWarning` (some of its model blobs are copies, not links). Every entry with a readable
//! configuration carries `revision` (of its `database.toml`). `sizeBytes` is measured at most
//! once per 5 seconds per database (`usage::disk_bytes`). Round 3 B5: `memoryBytes` (every
//! row): the engine's memory footprint of a loaded database's graph (a plain database: its
//! graph; a layer: its own changes; a base: the frozen base graph), `null` when it is not
//! loaded (or was never measurable: a writer held it from the start); a layer adds
//! `sharedMemoryBytes`, its base's memory, shared with the base and the other layers
//! (`Loader::memory`, never waiting for a graph); `lastVerify` (B-VERIFY): `null` (never
//! verified) or `{"atMs", "verdict" (ok | needs-model | damaged | failed), "problemCount",
//! "modelsMissing"}`.
//!
//! An entry: `name`, `state` (`unloaded` | `loading` | `loaded` | `needs-model` | `failed` |
//! `maintenance`: loaded read-only because its Store is damaged, with `damage`, the report),
//! `kind` (`plain` | `layer`), `role` (`normal` | `base`), `readOnly` (a base is always
//! read-only), `autoLoad`, `description`, and when they apply `base` (a layer's base), `created`,
//! `reason` (`failed`: why; `needs-model`: the missing model), `model` (`needs-model`: the
//! model's repository), `aliasOf` (an alias row: the database it names; its other fields are
//! that database's). A database whose `database.toml` cannot be read has no `kind`, `role`,
//! `autoLoad` or `description`. The field names are those the web UI reads (`ui/databases.js`).

use std::collections::BTreeMap;
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
    Admin, BackupInfo, CreateSpec, DatabaseRun, ExportSpec, ImportFormat, ImportSource, InboxFile,
    ModelAddSpec, ModelAdded, TmpFile,
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
    // The revision a change is based on (crate::revision): the body's `revision`, else
    // `If-Match`; none = no check.
    let if_match = request
        .header("If-Match")
        .and_then(crate::revision::from_if_match);
    let expecting =
        |body: Option<String>| admin.clone().expecting(body.or_else(|| if_match.clone()));
    if matches!(segments.first(), Some(&("users" | "grants")))
        && let Some(answer) = users::route(admin, method, &segments, request, &expecting)
    {
        return answer;
    }
    if segments.first() == Some(&"mcp")
        && let Some(answer) = mcp::route(admin, method, &segments, request, &expecting)
    {
        return answer;
    }
    if let Some(answer) = server_config::route(admin, method, &segments, request, &expecting) {
        return answer;
    }
    if let Some(answer) = settings::route(admin, method, &segments, request, &expecting) {
        return answer;
    }
    if let Some(answer) = damage::route(admin, method, &segments, request) {
        return answer;
    }
    let answer = match (method, segments.as_slice()) {
        (_, ["databases"]) if read => Ok(Response::json(200, &list(admin))),
        (Method::Post, ["databases"]) => {
            let body: CreateBody = body_of(request)?;
            let name = body.name.clone();
            let spec = body.into_spec();
            admin
                .create_reported(&name, &spec)
                .map(|(entry, leftover)| {
                    let answer = with_leftover(entry_json(&entry, None), leftover);
                    Response::json(201, &with_mcp(admin, answer, &entry.name))
                })
                .map_err(|err| {
                    let help = name_taken_help(&err);
                    (err, help)
                })
        }
        (_, ["databases", name]) if read && !name.is_empty() => {
            return one(admin, name)
                .map(|entry| {
                    let etag = entry["revision"].as_str().map(crate::revision::etag);
                    let answer = Response::json(200, &entry);
                    match etag {
                        Some(etag) => answer.with_header("ETag", &etag),
                        None => answer,
                    }
                })
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
                        "grantsRemoved": deleted.grants,
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
                .clone()
                .exposing(body.mcp)
                .clone_reported(name, &body.name)
                .map(|(entry, leftover)| {
                    let answer = with_leftover(entry_json(&entry, None), leftover);
                    Response::json(201, &with_mcp(admin, answer, &entry.name))
                })
                .map_err(|err| {
                    let help = name_taken_help(&err);
                    (err, help)
                })
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
            .map_err(|err| {
                let help = unload_help(admin, name, &err);
                (err, help)
            }),
        (Method::Post, ["databases", name, "promote"]) => admin
            .promote(name)
            .map(|entry| Response::json(200, &entry_json(&entry, None)))
            .map_err(|err| {
                let help = promote_help(registry, name, &err);
                (err, help)
            }),
        (Method::Post, ["databases", name, "reset-layer"]) => {
            let body: ResetLayerBody = body_of(request)?;
            if !body.confirm {
                return Err(HttpError::bad_request(format!(
                    "a layer reset discards every change of the layer '{}' over its base (the \
                     old layer moves to trash/): confirm it with \"confirm\": true",
                    fold(name)
                )));
            }
            admin
                .reset_layer(name)
                .map(|done| Response::json(200, &reset_layer_json(registry, &done)))
                .map_err(|err| {
                    let help = reset_layer_help(registry, name, &err);
                    (err, help)
                })
        }
        (Method::Put, ["databases", name, "read-only"]) => {
            let body: ReadOnlyBody = body_of(request)?;
            expecting(body.revision.clone())
                .set_read_only(name, body.read_only)
                .map(|entry| Response::json(200, &entry_json(&entry, None)))
                .map_err(|err| {
                    let help = read_only_help(registry, name, body.read_only, &err);
                    (err, help)
                })
        }
        (Method::Put, ["databases", name, "auto-load"]) => {
            let body: AutoLoadBody = body_of(request)?;
            expecting(body.revision.clone())
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
                .clone()
                .exposing(body.mcp)
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
                    let answer = with_leftover(entry, restored.leftover_grants);
                    Response::json(201, &with_mcp(admin, answer, &restored.entry.name))
                })
                .map_err(|err| {
                    let target = body.name.as_deref().unwrap_or(name);
                    let help =
                        restore_help(registry, target, &err).or_else(|| name_taken_help(&err));
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
        (_, ["imports"]) if read => admin
            .inbox()
            .map(|files| Response::json(200, &inbox_json(registry, &files)))
            .map_err(|err| (err, None)),
        (_, ["models"]) if read => admin
            .models_index()
            .map(|index| Response::json(200, &models_json(&index)))
            .map_err(|err| (err, None)),
        (Method::Post, ["models"]) => {
            return if is_json(request) {
                model_add(admin, request)
            } else {
                model_upload(admin, request)
            };
        }
        (Method::Delete, ["models", rest @ ..])
            if rest.iter().all(|s| !s.is_empty()) && !rest.is_empty() =>
        {
            let model = rest.join("/");
            admin
                .model_remove(&model)
                .map(|removed| {
                    Response::json(
                        200,
                        &json!({
                            "model": removed.model,
                            "removed": removed.blobs,
                            "bytes": removed.bytes,
                        }),
                    )
                })
                .map_err(|err| {
                    let help = match &err {
                        ServerError::Refused { .. } => Some(
                            "a database keeps the models it uses: delete those databases first \
                             (DELETE /api/admin/databases/<name>), or remove the model from their \
                             Stores"
                                .to_string(),
                        ),
                        _ => None,
                    };
                    (err, help)
                })
        }
        (_, ["runs"]) if read => admin
            .runs(None)
            .map(|runs| Response::json(200, &runs_json(None, &runs)))
            .map_err(|err| (err, None)),
        (_, ["databases", name, "runs"]) if read => admin
            .runs(Some(name))
            .map(|runs| {
                let mut answer = runs_json(Some(&fold(name)), &runs);
                answer["loaded"] = Value::Bool(admin.is_loaded(name));
                Response::json(200, &answer)
            })
            .map_err(|err| (err, None)),
        (Method::Post, ["databases", name, "runs", "cancel"]) => {
            let body: CancelRunBody = body_of(request)?;
            admin
                .cancel_run(name, body.id)
                .map(|cancelled| {
                    Response::json(
                        200,
                        &json!({ "database": fold(name), "id": body.id, "cancelled": cancelled }),
                    )
                })
                .map_err(|err| (err, None))
        }
        (_, ["aliases"]) if read => {
            let (aliases, revision) = registry.aliases_with_revision();
            Ok(
                Response::json(200, &json!({ "aliases": aliases, "revision": revision }))
                    .with_header("ETag", &crate::revision::etag(&revision)),
            )
        }
        (Method::Put, ["aliases", alias]) if !alias.is_empty() => {
            let body: AliasBody = body_of(request)?;
            expecting(body.revision.clone())
                .exposing(body.mcp)
                .alias_set(alias, &body.target)
                .map(|leftover| {
                    let answer = with_leftover(
                        json!({ "alias": fold(alias), "target": fold(&body.target) }),
                        leftover,
                    );
                    Response::json(200, &with_mcp(admin, answer, &fold(alias)))
                })
                .map_err(|err| {
                    let help = name_taken_help(&err);
                    (err, help)
                })
        }
        (Method::Delete, ["aliases", alias]) if !alias.is_empty() => expecting(None)
            .alias_remove(alias)
            .map(|grants| {
                Response::json(
                    200,
                    &json!({ "alias": fold(alias), "removed": true, "grantsRemoved": grants }),
                )
            })
            .map_err(|err| (err, None)),
        (Method::Post, ["aliases", alias, "rename"]) if !alias.is_empty() => {
            let body: RenameBody = body_of(request)?;
            expecting(body.revision.clone())
                .alias_rename(alias, &body.name)
                .map(|renamed| {
                    Response::json(
                        200,
                        &with_leftover(
                            json!({
                                "alias": renamed.new,
                                "renamedFrom": renamed.old,
                                "target": renamed.target,
                                "mcpSessionsMoved": renamed.moved,
                                "grantsMoved": renamed.grants,
                            }),
                            renamed.leftover,
                        ),
                    )
                })
                .map_err(|err| {
                    let help = rename_help(&err);
                    (err, help)
                })
        }
        (_, ["databases"])
        | (_, ["databases", _])
        | (
            _,
            [
                "databases",
                _,
                "clone" | "load" | "unload" | "promote" | "read-only" | "auto-load" | "backup"
                | "backups" | "restore" | "export" | "runs" | "reset-layer",
            ],
        )
        | (_, ["databases", _, "runs", "cancel"])
        | (_, ["runs"])
        | (_, ["aliases"])
        | (_, ["aliases", _])
        | (_, ["aliases", _, "rename"])
        | (_, ["models", ..])
        | (_, ["import"])
        | (_, ["imports"]) => return Err(not_allowed()),
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

/// Whether `method path` with the content type `content_type` is an upload (an import or a
/// model: the listener lets its body pass up to the upload limit).
pub(crate) fn is_upload(method: &Method, path: &str, content_type: Option<&str>) -> bool {
    matches!(method, Method::Post)
        && matches!(path, "/api/admin/import" | "/api/admin/models")
        && !content_type
            .is_some_and(|value| value.to_ascii_lowercase().contains("application/json"))
}

/// `POST import {"path" | "inbox", ...}`: a file or directory on the server's machine, or an
/// entry of the import inbox.
fn import_path(admin: &Admin, request: &mut Request<'_>) -> Result<Response, HttpError> {
    let body: ImportBody = body_of(request)?;
    let path = match (&body.path, &body.inbox) {
        (Some(given), None) => {
            let path = PathBuf::from(given);
            if !path.is_absolute() {
                return Err(HttpError::bad_request(format!(
                    "import: the path {given:?} is not absolute (the server resolves it on its \
                     own machine)"
                )));
            }
            path
        }
        (None, Some(inbox)) => match admin.inbox_source(inbox) {
            Ok(path) => path,
            Err(err) => {
                let help = match &err {
                    ServerError::Refused { .. } => None,
                    _ => Some("GET /api/admin/imports lists the import inbox".to_string()),
                };
                return Ok(error_answer(&err, help));
            }
        },
        _ => {
            return Err(HttpError::bad_request(
                "import: name the source with either \"path\" (absolute, on the server's \
                 machine) or \"inbox\" (an entry of imports/), not both",
            ));
        }
    };
    let format = parse_format(body.format.as_deref())?;
    let role = parse_role(body.role.as_deref())?;
    let source = ImportSource::new(path).with_format(format);
    Ok(import_answer(
        &admin.clone().exposing(body.mcp),
        &source,
        &body.name,
        body.base.as_deref(),
        body.description.as_deref(),
        role,
    ))
}

/// `POST import?name=...` with the file as the body: streamed into `tmp/`, then imported.
fn import_upload(admin: &Admin, request: &mut Request<'_>) -> Result<Response, HttpError> {
    let mut params = query_params(
        request.url(),
        "import",
        &["name", "base", "format", "description", "role", "mcp"],
    )?;
    let Some(name) = params.remove("name").filter(|n| !n.is_empty()) else {
        return Err(HttpError::bad_request(
            "import: name the new database: POST /api/admin/import?name=<name> with the file \
             as the body",
        ));
    };
    let base = params.remove("base");
    let description = params.remove("description");
    let format = parse_format(params.remove("format").as_deref())?;
    let role = parse_role(params.remove("role").as_deref())?;
    let mcp = parse_mcp(params.remove("mcp").as_deref())?;
    let admin = &admin.clone().exposing(mcp);
    // The quick refusals before the body is read (the listener drains it).
    let key = fold(&name);
    if let Err(err) = crate::names::validate_new_name(&key) {
        return Ok(error_answer(&err, None));
    }
    if let Ok(entry) = admin.registry().get(&key) {
        let err = ServerError::NameInUse {
            name: entry.name,
            holder: "database",
        };
        let help = name_taken_help(&err);
        return Ok(error_answer(&err, help));
    }
    let (upload, received) = receive_upload(admin, request, "import")?;
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
        role,
    );
    // The upload is removed now (the import copied what it needs).
    drop(upload);
    Ok(answer)
}

/// The query of `url` as `key -> value` (`%XX`/`+` decoded); `400` for a key not in
/// `allowed`, a key given twice or a broken escape (`what` names the route).
fn query_params(
    url: &str,
    what: &str,
    allowed: &[&str],
) -> Result<BTreeMap<String, String>, HttpError> {
    let query = url
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or_default();
    let mut out = BTreeMap::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = query_decode(value)
            .ok_or_else(|| HttpError::bad_request(format!("{what}: {key}: a broken escape")))?;
        if !allowed.contains(&key) {
            return Err(HttpError::bad_request(format!(
                "{what}: unknown parameter {key:?} ({})",
                allowed.join(", ")
            )));
        }
        if out.insert(key.to_string(), value).is_some() {
            return Err(HttpError::bad_request(format!("{what}: {key} given twice")));
        }
    }
    Ok(out)
}

/// Streams the body of `request` into a new `tmp/upload-*.bin` (at most `server.toml`'s
/// `[limits] upload_bytes`, else `413`; an empty body `400`): the file (removed on drop) and
/// its length. `what` names the route in the messages.
fn receive_upload(
    admin: &Admin,
    request: &mut Request<'_>,
    what: &str,
) -> Result<(TmpFile, u64), HttpError> {
    let registry = admin.registry();
    let server_error = |err: ServerError| HttpError::new(status_of(&err), err.to_string());
    let limit = registry
        .dir()
        .load_config()
        .map_err(server_error)?
        .limits
        .upload_bytes;
    let upload = TmpFile::new(&registry.dir().tmp(), "upload", "bin").map_err(server_error)?;
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
        return Err(HttpError::bad_request(format!(
            "{what}: the body is empty; send the file as the request body"
        )));
    }
    Ok((upload, received))
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

/// The `mcp` parameter of an import upload: `true`/`false` (`400` for another value; none:
/// the server's default, `mcp.toml`'s `expose_new`).
fn parse_mcp(value: Option<&str>) -> Result<Option<bool>, HttpError> {
    match value.filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some("true" | "1" | "on" | "yes") => Ok(Some(true)),
        Some("false" | "0" | "off" | "no") => Ok(Some(false)),
        Some(other) => Err(HttpError::bad_request(format!(
            "import: mcp is true or false, not {other:?}"
        ))),
    }
}

/// `answer` with `"mcpExposed"`: whether MCP offers the new name `name` by its own settings
/// (round 4 B-MCPDEF: a new name is hidden unless the request or the server's default said
/// otherwise); absent when `mcp.toml` cannot be read.
fn with_mcp(admin: &Admin, mut answer: Value, name: &str) -> Value {
    if let Some(exposed) = admin.mcp_exposed(name) {
        answer["mcpExposed"] = Value::Bool(exposed);
    }
    answer
}

/// The role of an import (`400` for an unknown one; default `normal`).
fn parse_role(role: Option<&str>) -> Result<DatabaseRole, HttpError> {
    match role.filter(|r| !r.is_empty()) {
        None => Ok(DatabaseRole::Normal),
        Some(role) => DatabaseRole::parse(role).ok_or_else(|| {
            HttpError::bad_request(format!("import: unknown role {role:?} (normal, base)"))
        }),
    }
}

/// `GET imports`: the import inbox.
fn inbox_json(registry: &Registry, files: &[InboxFile]) -> Value {
    let rows: Vec<Value> = files
        .iter()
        .map(|file| {
            json!({
                "name": file.name,
                "kind": file.kind.map(ImportFormat::as_str),
                "directory": file.directory,
                "bytes": file.bytes,
                "modified": file.modified,
            })
        })
        .collect();
    json!({
        "dir": registry.dir().imports().to_string_lossy(),
        "imports": rows,
    })
}

/// `answer` with `"leftoverGrantsRemoved": n` when a new database or alias took a name that
/// still had grants (from a deleted database or alias) and they were removed first (`n` > 0;
/// the field is absent otherwise).
fn with_leftover(mut answer: Value, leftover: usize) -> Value {
    if leftover > 0 {
        answer["leftoverGrantsRemoved"] = json!(leftover);
    }
    answer
}

/// The answer of an import: `201` entry + `"import"`, or the error.
fn import_answer(
    admin: &Admin,
    source: &ImportSource,
    name: &str,
    base: Option<&str>,
    description: Option<&str>,
    role: DatabaseRole,
) -> Response {
    match admin.import_as(source, name, base, description, role) {
        Ok(imported) => {
            let mut entry = entry_json(&imported.entry, None);
            let mut details = json!({
                "format": imported.format.as_str(),
                "modelsMissing": imported.models_missing,
                "modelsResolved": imported.models_resolved,
                "skipped": imported.skipped,
                "notes": imported.notes,
            });
            if let Some(layer_base) = &imported.layer_base {
                details["layerBase"] = json!(layer_base);
            }
            entry["import"] = details;
            let answer = with_leftover(entry, imported.leftover_grants);
            Response::json(201, &with_mcp(admin, answer, &imported.entry.name))
        }
        Err(err) => {
            let help = match &err {
                ServerError::Refused { .. } if role == DatabaseRole::Base => Some(
                    "add its model to the shared models (POST /api/admin/models) and import \
                     again with role=base; or import it as an ordinary database (without \
                     role=base), add its model, then promote it: POST \
                     /api/admin/databases/<name>/promote"
                        .to_string(),
                ),
                ServerError::Refused { .. } => Some(
                    "a layer is imported over a base database with its base identity: import \
                     or restore that base first (role = base), or name it with base=<name>"
                        .to_string(),
                ),
                _ => name_taken_help(&err),
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
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    inbox: Option<String>,
    name: String,
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    role: Option<String>,
    /// Offer the new database to MCP (`None`: the server's default).
    #[serde(default)]
    mcp: Option<bool>,
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
    /// `None`: `server.toml`'s `[defaults] auto_load`.
    #[serde(default)]
    auto_load: Option<bool>,
    /// `None`: `server.toml`'s `[defaults] read_only`.
    #[serde(default)]
    read_only: Option<bool>,
    /// Offer the new database to MCP (`None`: the server's default).
    #[serde(default)]
    mcp: Option<bool>,
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
        .with_auto_load_opt(self.auto_load)
        .with_read_only_opt(self.read_only)
        .with_mcp(self.mcp);
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
    /// Offer a database restored into a NEW name to MCP (`None`: the server's default; a
    /// restore over an existing database keeps its MCP settings).
    #[serde(default)]
    mcp: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CloneBody {
    name: String,
    /// Offer the clone to MCP (`None`: the server's default).
    #[serde(default)]
    mcp: Option<bool>,
}

/// `POST databases/<name>/reset-layer`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ResetLayerBody {
    #[serde(default)]
    confirm: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ReadOnlyBody {
    read_only: bool,
    /// The database's revision the change is based on (crate::revision).
    #[serde(default)]
    revision: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct AutoLoadBody {
    auto_load: bool,
    /// The database's revision the change is based on (crate::revision).
    #[serde(default)]
    revision: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelRunBody {
    id: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AliasBody {
    target: String,
    /// Offer a NEW alias to MCP (`None`: the server's default; a flip keeps its settings).
    #[serde(default)]
    mcp: Option<bool>,
    /// The aliases' revision the change is based on (crate::revision).
    #[serde(default)]
    revision: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenameBody {
    name: String,
    /// The aliases' revision the change is based on (crate::revision).
    #[serde(default)]
    revision: Option<String>,
}

/// The status of an admin core error.
pub(crate) fn status_of(err: &ServerError) -> u16 {
    match err {
        ServerError::InvalidName { .. } | ServerError::InvalidInput { .. } => 400,
        ServerError::NotFound { .. }
        | ServerError::UserNotFound { .. }
        | ServerError::ModelNotFound { .. }
        | ServerError::BackupNotFound { .. }
        | ServerError::SourceNotFound { .. } => 404,
        ServerError::UnsupportedSource { .. } => 415,
        ServerError::NameInUse { .. }
        | ServerError::Refused { .. }
        | ServerError::DatabaseFailed { .. }
        | ServerError::ModelUnavailable { .. }
        | ServerError::LastAdmin { .. }
        | ServerError::Conflict { .. }
        | ServerError::Locked { .. } => 409,
        ServerError::Remote { status, .. } => *status,
        _ => 500,
    }
}

/// `{"error", "help"?}` for an admin core error.
fn error_answer(err: &ServerError, help: Option<String>) -> Response {
    let help = help.or_else(|| match err {
        ServerError::InvalidName { name, .. } if crate::names::is_reserved(name) => {
            Some(crate::names::RESERVED_HELP.to_string())
        }
        ServerError::NotFound { .. } => {
            Some("GET /api/admin/databases lists the databases and aliases".to_string())
        }
        ServerError::ModelNotFound { .. } => Some(
            "GET /api/admin/models lists the shared models; add one: POST /api/admin/models \
             {\"path\": \"<model directory or .gsnap>\"}"
                .to_string(),
        ),
        ServerError::BackupNotFound { database, .. } => Some(format!(
            "GET /api/admin/databases/{database}/backups lists its backups"
        )),
        ServerError::ModelUnavailable { .. } => Some(
            "add the model to the shared models, then import again: POST /api/admin/models \
             {\"path\": \"<model directory, .tar or .gsnap>\"} (or upload it in the Models tab \
             of the web UI)"
                .to_string(),
        ),
        ServerError::ModelDamaged { blob, .. } => {
            let file = blob
                .as_ref()
                .and_then(|b| b.file_name())
                .map(|hash| hash.to_string_lossy().into_owned())
                .unwrap_or_else(|| "<hash>".to_string());
            Some(format!(
                "replace the damaged file in the shared models with an intact copy: restore it \
                 from a backup of the server directory, or remove it (DELETE \
                 /api/admin/models/{file}, refused while a database uses it) and add the model \
                 again (POST /api/admin/models), then import again"
            ))
        }
        ServerError::Conflict { .. } => Some(crate::revision::CONFLICT_HELP.to_string()),
        _ => None,
    });
    let mut body = json!({ "error": err.to_string() });
    if let Some(help) = help {
        body["help"] = Value::String(help);
    }
    if let ServerError::Conflict { revision, .. } = err {
        body["revision"] = Value::String(revision.clone());
    }
    Response::json(status_of(err), &body)
}

/// The help of a refused delete: what to do first.
fn delete_help(registry: &Registry, name: &str) -> Option<String> {
    let key = fold(name);
    let entry = registry.get(&key).ok()?;
    if entry.state.is_busy() {
        return Some(format!(
            "unload it first: POST /api/admin/databases/{key}/unload"
        ));
    }
    if let Some(alias) = registry
        .aliases()
        .into_iter()
        .find(|(_, target)| *target == key)
    {
        return Some(format!(
            "remove the alias first: DELETE /api/admin/aliases/{}, or point it at another \
             database: PUT /api/admin/aliases/{} {{\"target\": \"<database>\"}}",
            alias.0, alias.0
        ));
    }
    let layers = layers_of(registry, &key);
    (!layers.is_empty()).then(|| {
        let steps: Vec<String> = layers
            .iter()
            .map(|l| format!("DELETE /api/admin/databases/{l}"))
            .collect();
        format!(
            "it is the base of the layers {}: delete them first ({})",
            layers.join(", "),
            steps.join(", ")
        )
    })
}

/// The layers whose base is the database `key`.
fn layers_of(registry: &Registry, key: &str) -> Vec<String> {
    registry
        .list()
        .into_iter()
        .filter(|e| {
            e.config
                .as_ref()
                .is_some_and(|c| c.kind == DatabaseKind::Layer && c.base.as_deref() == Some(key))
        })
        .map(|e| e.name)
        .collect()
}

/// The help of a name already taken (create, clone, restore, import, alias).
fn name_taken_help(err: &ServerError) -> Option<String> {
    matches!(err, ServerError::NameInUse { .. }).then(|| {
        "choose another name: GET /api/admin/databases lists the taken names (databases and \
         aliases share one namespace, case-insensitive)"
            .to_string()
    })
}

/// The help of a refused alias rename: a taken name, or a database named instead of an alias.
fn rename_help(err: &ServerError) -> Option<String> {
    match err {
        ServerError::NameInUse { .. } => name_taken_help(err),
        ServerError::Refused { name, .. } if name != "aliases.toml" => Some(format!(
            "databases have no rename; point an alias at it instead: PUT \
             /api/admin/aliases/<alias> {{\"target\": \"{name}\"}}"
        )),
        _ => None,
    }
}

/// The help of a refused unload: a base that loaded layers use.
fn unload_help(admin: &Admin, name: &str, err: &ServerError) -> Option<String> {
    if !matches!(err, ServerError::Refused { .. }) {
        return None;
    }
    let key = fold(name);
    let layers: Vec<String> = admin
        .pooled_users(&key)
        .into_iter()
        .filter(|user| *user != key)
        .collect();
    (!layers.is_empty()).then(|| {
        let steps: Vec<String> = layers
            .iter()
            .map(|l| format!("POST /api/admin/databases/{l}/unload"))
            .collect();
        format!(
            "the loaded layers {} use it as their base: unload them first ({})",
            layers.join(", "),
            steps.join(", ")
        )
    })
}

/// The help of a refused promotion: unload it first, or what a layer can do instead.
/// The answer of a layer reset: the entry + `"base"`, `"trash"`, `"discarded"`
/// (`{"added", "modified", "removed"}`, each `{"vertices", "edges"}`), `"jobsKept"`,
/// `"reloaded"`.
fn reset_layer_json(registry: &Registry, done: &crate::ops::LayerResetDone) -> Value {
    let counts = |(vertices, edges): (u64, u64)| json!({ "vertices": vertices, "edges": edges });
    let mut entry = entry_json(&done.entry, None);
    entry["base"] = Value::String(done.base.clone());
    entry["trash"] = Value::String(relative(registry.dir().root(), &done.trash));
    entry["discarded"] = json!({
        "added": counts(done.discarded.added),
        "modified": counts(done.discarded.modified),
        "removed": counts(done.discarded.removed),
    });
    entry["jobsKept"] = json!(done.jobs_kept);
    entry["reloaded"] = Value::Bool(done.reloaded);
    entry
}

/// The help of a refused layer reset.
fn reset_layer_help(registry: &Registry, name: &str, err: &ServerError) -> Option<String> {
    if !matches!(err, ServerError::Refused { .. }) {
        return None;
    }
    let key = fold(name);
    let entry = registry.get(&key).ok()?;
    let config = entry.config.as_ref()?;
    match (config.kind, config.role) {
        (DatabaseKind::Layer, _) => None,
        (_, DatabaseRole::Base) => {
            let layers: Vec<String> = registry
                .list()
                .into_iter()
                .filter(|e| {
                    e.config.as_ref().is_some_and(|c| {
                        c.kind == DatabaseKind::Layer && c.base.as_deref() == Some(key.as_str())
                    })
                })
                .map(|e| e.name)
                .collect();
            Some(match layers.first() {
                Some(layer) => format!(
                    "reset a layer over it instead (layers: {}): POST \
                     /api/admin/databases/{layer}/reset-layer {{\"confirm\": true}}",
                    layers.join(", ")
                ),
                None => "no layer uses it as its base; there is nothing to reset".to_string(),
            })
        }
        _ => Some(format!(
            "restore a backup instead: GET /api/admin/databases/{key}/backups, then POST \
             /api/admin/databases/{key}/restore {{\"backup\": \"<backup>\"}}"
        )),
    }
}

fn promote_help(registry: &Registry, name: &str, err: &ServerError) -> Option<String> {
    if !matches!(err, ServerError::Refused { .. }) {
        return None;
    }
    let key = fold(name);
    let entry = registry.get(&key).ok()?;
    if entry.state.is_busy() {
        return Some(format!(
            "unload it first: POST /api/admin/databases/{key}/unload, then promote it again"
        ));
    }
    let config = entry.config.as_ref()?;
    if config.kind == DatabaseKind::Layer {
        return Some(format!(
            "export it materialized (GET /api/admin/databases/{key}/export?materialize=true) and \
             import the file as a base (POST /api/admin/import?name=<new>&role=base)"
        ));
    }
    if let DbState::NeedsModel { repo } = &entry.state {
        return Some(format!(
            "add the model first: POST /api/admin/models {{\"repo\": \"{repo}\"}} (or a \
             \"path\")"
        ));
    }
    None
}

/// The help of a refused read-only change: a base is always read-only.
fn read_only_help(
    registry: &Registry,
    name: &str,
    read_only: bool,
    err: &ServerError,
) -> Option<String> {
    let entry = registry.get(name).ok()?;
    let base = entry
        .config
        .as_ref()
        .is_some_and(|c| c.role == DatabaseRole::Base);
    (!read_only && base && matches!(err, ServerError::Refused { .. })).then(|| {
        format!(
            "write into a layer over it instead: POST /api/admin/databases {{\"name\": \
             \"<new>\", \"base\": \"{}\"}}",
            entry.name
        )
    })
}

/// The help of a refused restore: unload the target first.
fn restore_help(registry: &Registry, target: &str, err: &ServerError) -> Option<String> {
    let key = fold(target);
    let entry = registry.get(&key).ok()?;
    (matches!(err, ServerError::Refused { .. }) && entry.state.is_busy())
        .then(|| format!("unload it first: POST /api/admin/databases/{key}/unload"))
}

/// A backup as JSON (module docs).
/// `{"runs": [run...], "textHeadChars"}` (+ `"database"` for one database's list).
fn runs_json(database: Option<&str>, runs: &[DatabaseRun]) -> Value {
    let rows: Vec<Value> = runs
        .iter()
        .map(|run| {
            json!({
                "database": run.database,
                "id": run.id,
                "client": run.client,
                "alias": run.alias,
                "kind": run.kind,
                "state": run.state,
                "cancelled": run.cancelled,
                "startedAtMs": run
                    .started_at
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
                "durationMs": u64::try_from(run.duration.as_millis()).unwrap_or(u64::MAX),
                "text": run.text,
            })
        })
        .collect();
    let mut answer = json!({
        "runs": rows,
        "textHeadChars": graphersal_serve::RUN_TEXT_HEAD_CHARS,
    });
    if let Some(database) = database {
        answer["database"] = Value::String(database.to_string());
    }
    answer
}

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

/// A download: a file written into `tmp/` (an export's `.gsnap`, a server backup's ZIP),
/// streamed by the listener and removed when this (its guard) is dropped.
pub(crate) struct Download {
    headers: Vec<(String, String)>,
    file: File,
    length: u64,
    guard: TmpFile,
}

impl Download {
    /// The download of the file `guard` (opened now) with `headers`; a file that cannot be
    /// read answers `500` (`what` names it).
    pub(crate) fn of(
        guard: TmpFile,
        headers: Vec<(String, String)>,
        what: &str,
    ) -> Result<Download, Response> {
        let opened = File::open(guard.path()).and_then(|file| {
            let length = file.metadata()?.len();
            Ok((file, length))
        });
        match opened {
            Ok((file, length)) => Ok(Download {
                headers,
                file,
                length,
                guard,
            }),
            Err(err) => Err(Response::json(
                500,
                &json!({ "error": format!("cannot read the {what}: {err}") }),
            )),
        }
    }

    /// The answer's headers (module docs).
    pub(crate) fn headers(&self) -> Vec<(String, String)> {
        self.headers.clone()
    }

    /// The file's length in bytes.
    pub(crate) fn length(&self) -> u64 {
        self.length
    }

    /// The open file to stream and the guard that removes it (drop the guard after the file).
    pub(crate) fn into_parts(self) -> (File, TmpFile) {
        (self.file, self.guard)
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
                &json!({ "error": format!("cannot read the download: {err}") }),
            );
        }
        headers
            .iter()
            .fold(Response::new(200, bytes), |r, (name, value)| {
                r.with_header(name, value)
            })
    }
}

/// The headers every download carries: the type, the file name, no caching, no sniffing.
fn download_headers(file_name: &str) -> Vec<(String, String)> {
    vec![
        ("Content-Type".into(), "application/octet-stream".into()),
        (
            "Content-Disposition".into(),
            format!("attachment; filename=\"{file_name}\""),
        ),
        ("Cache-Control".into(), "no-store".into()),
        ("X-Content-Type-Options".into(), "nosniff".into()),
    ]
}

/// `GET databases/<name>/export?...`: the export written into `tmp/`, as a download; an error
/// as its JSON answer.
pub(crate) fn export(admin: &Admin, name: &str, query: Option<&str>) -> Result<Download, Response> {
    let spec = export_spec(query)
        .map_err(|err| Response::json(err.status(), &json!({ "error": err.message() })))?;
    let export = admin
        .export_to_tmp(name, spec)
        .map_err(|err| error_answer(&err, None))?;
    let (exported, guard) = export.into_parts();
    let mut headers = download_headers(&exported.file_name);
    headers.push((
        "X-Graphersal-Snapshot".into(),
        if exported.layer { "layer" } else { "full" }.into(),
    ));
    headers.push((
        "X-Graphersal-Models".into(),
        if exported.models {
            "included"
        } else {
            "excluded"
        }
        .into(),
    ));
    Download::of(guard, headers, "export")
}

/// Whether the request is `GET server-backup` (the router answers it as a download).
pub(crate) fn is_server_backup(method: &Method, endpoint: &str) -> bool {
    matches!(method, Method::Get) && endpoint == "server-backup"
}

/// `GET server-backup?withDatabases=1`: the server backup ([`crate::server_backup`]) written
/// into `tmp/`, as a download; an error as its JSON answer. The router has refused every
/// principal but the operator and admin users (`everyone` of a server without access control
/// included: the archive holds the token hashes).
pub(crate) fn server_backup(admin: &Admin, query: Option<&str>) -> Result<Download, Response> {
    let mut with_databases = false;
    for pair in query
        .unwrap_or_default()
        .split('&')
        .filter(|p| !p.is_empty())
    {
        let (key, value) = pair.split_once('=').unwrap_or((pair, "1"));
        let flag = match value {
            "true" | "1" | "yes" => true,
            "false" | "0" | "no" => false,
            other => {
                return Err(Response::json(
                    400,
                    &json!({ "error": format!("server-backup: {key}={other} is not 1 or 0") }),
                ));
            }
        };
        match key {
            "withDatabases" => with_databases = flag,
            other => {
                return Err(Response::json(
                    400,
                    &json!({
                        "error": format!("server-backup: unknown parameter {other:?} (withDatabases)")
                    }),
                ));
            }
        }
    }
    let (guard, made) = admin
        .server_backup_to_tmp(with_databases)
        .map_err(|err| error_answer(&err, None))?;
    let mut headers = download_headers(&made.file_name);
    headers.push((
        "X-Graphersal-Server-Backup".into(),
        if with_databases {
            "with-databases"
        } else {
            "config"
        }
        .into(),
    ));
    let manifest = &made.manifest;
    headers.push((
        "X-Graphersal-Databases".into(),
        manifest
            .databases
            .iter()
            .filter(|d| d.included)
            .count()
            .to_string(),
    ));
    headers.push((
        "X-Graphersal-Skipped".into(),
        manifest
            .databases
            .iter()
            .filter(|d| !d.included && with_databases)
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>()
            .join(","),
    ));
    Download::of(guard, headers, "server backup")
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
fn list(admin: &Admin) -> Value {
    let registry = admin.registry();
    let entries = registry.list();
    let (aliases, aliases_revision) = registry.aliases_with_revision();
    let extras = Extras::of(admin);
    let mut databases: Vec<Value> = entries.iter().map(|e| extras.row(e, None)).collect();
    for (alias, target) in &aliases {
        if let Some(entry) = entries.iter().find(|e| &e.name == target) {
            databases.push(extras.row(entry, Some(alias)));
        }
    }
    databases.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    json!({
        "databases": databases,
        "aliases": aliases,
        "aliasesRevision": aliases_revision,
        "problems": registry.problems(),
        "newDatabases": new_database_defaults(admin),
    })
}

/// What a new database gets when its creator does not say (round 4 B-SETTINGS: the create
/// form starts from it): `autoLoad`, `readOnly` (`server.toml [defaults]`) and `mcp`
/// (`mcp.toml` `expose_new`); `null` for a file that cannot be read.
fn new_database_defaults(admin: &Admin) -> Value {
    let defaults = admin
        .registry()
        .dir()
        .load_config()
        .ok()
        .map(|c| c.defaults);
    json!({
        "autoLoad": defaults.as_ref().map(|d| d.auto_load),
        "readOnly": defaults.as_ref().map(|d| d.read_only),
        "mcp": admin.mcp_control().ok().map(|c| c.settings().expose_new),
    })
}

/// `GET /api/admin/databases/<name>`: the database or alias `name`, `None` when unknown.
fn one(admin: &Admin, name: &str) -> Option<Value> {
    let registry = admin.registry();
    let extras = Extras::of(admin);
    if let Ok(entry) = registry.get(name) {
        return Some(extras.row(&entry, None));
    }
    let alias = fold(name);
    registry.aliases().get(&alias)?;
    let entry = registry.resolve(&alias).ok()?;
    Some(extras.row(&entry, Some(&alias)))
}

/// The listing's fields beyond the entry (module docs): disk size, pool membership, model
/// copies.
struct Extras {
    pooled: BTreeMap<String, Vec<String>>,
    models: crate::models::ModelsDir,
    memory: crate::loader::LoadedMemory,
}

impl Extras {
    fn of(admin: &Admin) -> Self {
        Self {
            pooled: admin.pooled_bases(),
            models: admin.models(),
            memory: admin.loaded_memory(),
        }
    }

    /// A listing row: [`entry_json`] + `sizeBytes`, `memoryBytes` (+ `sharedMemoryBytes`),
    /// `pooled`/`pooledBy`, `linkWarning`.
    fn row(&self, entry: &DatabaseEntry, alias: Option<&str>) -> Value {
        let mut row = entry_json(entry, alias);
        row["sizeBytes"] = json!(crate::usage::disk_bytes(&entry.dir));
        row["lastVerify"] = crate::recovery::LastVerify::read(&entry.dir)
            .map_or(Value::Null, |last| last_verify_brief(&last));
        let memory = self.memory.databases.get(&entry.name);
        row["memoryBytes"] = json!(memory.and_then(|m| m.bytes));
        if let Some(shared) = memory.and_then(|m| m.shared_bytes) {
            row["sharedMemoryBytes"] = json!(shared);
        }
        if let Some(users) = self.pooled.get(&entry.name) {
            row["pooled"] = Value::Bool(true);
            let layers: Vec<&String> = users.iter().filter(|u| **u != entry.name).collect();
            row["pooledBy"] = json!(layers);
        }
        let copied = self.models.copied_blobs(&entry.store_dir());
        if !copied.is_empty() {
            row["linkWarning"] = Value::String(format!(
                "{} model blob(s) are copied, not linked to models/ (the file system has no \
                 hard links here, no disk saving)",
                copied.len()
            ));
        }
        row
    }
}

/// `GET /api/admin/models` (module docs).
fn models_json(index: &crate::models::ModelsIndex) -> Value {
    let rows: Vec<Value> = index.models.iter().map(model_json).collect();
    let missing: Vec<Value> = index
        .missing
        .iter()
        .map(|(database, model)| json!({ "database": database, "model": model }))
        .collect();
    json!({ "models": rows, "missing": missing })
}

/// One model blob as JSON (module docs).
fn model_json(model: &crate::models::ModelBlob) -> Value {
    let mut row = json!({
        "repo": model.repo,
        "identity": model.identity,
        "blob": model.blob,
        "bytes": model.bytes,
        "license": model.license,
        "users": model.users,
        "waiting": model.waiting,
        "copied": !model.copied.is_empty(),
        "copiedIn": model.copied,
    });
    if let Some(links) = model.links {
        row["links"] = json!(links);
    }
    if !model.copied.is_empty() {
        row["linkWarning"] = Value::String(format!(
            "copied, not linked, in {}: the file system has no hard links there, no disk saving",
            model.copied.join(", ")
        ));
    }
    row
}

/// `POST models?repo=&license=&format=` with the file as the body (a `.gsnap` carrying model
/// blobs, or a tar archive of a Model2Vec model directory): streamed into `tmp/`, then added
/// like a path.
fn model_upload(admin: &Admin, request: &mut Request<'_>) -> Result<Response, HttpError> {
    let mut params = query_params(request.url(), "models", &["repo", "license", "format"])?;
    let non_empty = |value: Option<String>| value.filter(|v| !v.trim().is_empty());
    let repo = non_empty(params.remove("repo"));
    let license = non_empty(params.remove("license"));
    let format = match non_empty(params.remove("format")).as_deref() {
        None => None,
        Some(hint) => Some(ModelFileFormat::parse(hint).ok_or_else(|| {
            HttpError::bad_request(format!(
                "models: unknown format {hint:?} (gsnap, tar: also a .tar.gz)"
            ))
        })?),
    };
    let (upload, received) = receive_upload(admin, request, "models")?;
    tracing::info!(
        target: "graphersal_server",
        "model upload: {received} bytes received"
    );
    let found = ModelFileFormat::of(upload.path());
    match (format, found) {
        (_, None) => {
            return Err(HttpError::bad_request(
                "models: the file is neither a packed snapshot (.gsnap) nor a tar archive \
                 (.tar, .tar.gz) of a Model2Vec model directory",
            ));
        }
        (Some(hint), Some(found)) if !hint.names(found) => {
            return Err(HttpError::bad_request(format!(
                "models: the file is a {}, not a {}",
                found.describe(),
                hint.describe()
            )));
        }
        _ => {}
    }
    let spec = ModelAddSpec::path(upload.path())
        .with_repo(repo)
        .with_license(license);
    let answer = model_added_answer(admin.model_add(&spec), 201);
    // The upload is removed now (the model is in `models/`).
    drop(upload);
    Ok(answer)
}

/// The format of an uploaded model file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelFileFormat {
    Snapshot,
    Tar,
    /// A gzip-compressed tar archive (the hint `tar` names it too: compression is told by the
    /// content).
    TarGz,
}

impl ModelFileFormat {
    /// The format a hint names: `gsnap` / `snapshot`, `tar`.
    fn parse(hint: &str) -> Option<Self> {
        match hint.trim().to_ascii_lowercase().as_str() {
            "gsnap" | "snapshot" => Some(Self::Snapshot),
            "tar" => Some(Self::Tar),
            _ => None,
        }
    }

    /// Whether the hint `self` names a file of the format `found`.
    fn names(self, found: Self) -> bool {
        self == found || (self == Self::Tar && found == Self::TarGz)
    }

    /// The format of the file at `path`, read from its content.
    fn of(path: &Path) -> Option<Self> {
        if crate::model_archive::is_tar(path) {
            return Some(Self::Tar);
        }
        if crate::model_archive::is_gzip(path) {
            return Some(Self::TarGz);
        }
        let file = File::open(path).ok()?;
        graphersal::persist::read_snapshot_info(std::io::BufReader::new(file))
            .is_ok()
            .then_some(Self::Snapshot)
    }

    fn describe(self) -> &'static str {
        match self {
            Self::Snapshot => "packed snapshot (gsnap)",
            Self::Tar => "tar archive (tar)",
            Self::TarGz => "gzip-compressed tar archive (tar.gz)",
        }
    }
}

/// `POST models {"path"?, "repo"?, "license"?}`.
fn model_add(admin: &Admin, request: &mut Request<'_>) -> Result<Response, HttpError> {
    let body: ModelAddBody = body_of(request)?;
    let path = body.path.as_ref().map(PathBuf::from);
    if let Some(path) = &path
        && !path.is_absolute()
    {
        return Err(HttpError::bad_request(format!(
            "models: the path {:?} is not absolute (the server resolves it on its own machine)",
            path
        )));
    }
    let from_path = path.is_some();
    let spec = match path {
        Some(path) => ModelAddSpec::path(path),
        None => ModelAddSpec::default(),
    }
    .with_repo(body.repo)
    .with_license(body.license);
    Ok(model_added_answer(
        admin.model_add(&spec),
        if from_path { 201 } else { 200 },
    ))
}

/// The answer of a model add (module docs), `status` when it succeeded.
fn model_added_answer(added: Result<ModelAdded, ServerError>, status: u16) -> Response {
    match added {
        Ok(added) => {
            let missing: Vec<Value> = added
                .missing
                .iter()
                .map(|(database, model)| json!({ "database": database, "model": model }))
                .collect();
            Response::json(
                status,
                &json!({
                    "added": added.added.iter().map(model_json).collect::<Vec<_>>(),
                    "resolved": added.resolved,
                    "stillMissing": missing,
                    "notes": added.notes,
                }),
            )
        }
        Err(err) => error_answer(&err, None),
    }
}

/// `POST /api/admin/models`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelAddBody {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    license: Option<String>,
}

/// One entry of the listing (module docs); `alias`: the row of that alias of `entry`.
/// A database's last verify in a list row: `{"atMs", "verdict", "problemCount",
/// "modelsMissing"}` (the settings show it whole).
pub(crate) fn last_verify_brief(last: &crate::recovery::LastVerify) -> Value {
    json!({
        "atMs": last.at_ms,
        "verdict": last.verdict,
        "problemCount": last.problem_count,
        "modelsMissing": last.models_missing,
    })
}

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
        DbState::Maintenance { damage } => {
            out.insert(
                "reason".into(),
                Value::String(
                    "its Store is damaged: served read-only in maintenance mode".to_string(),
                ),
            );
            out.insert("damage".into(), Value::String(damage.clone()));
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
    if let Some(revision) = entry.revision() {
        out.insert("revision".into(), Value::String(revision));
    }
    Value::Object(out)
}

#[path = "admin_api_users.rs"]
mod users;

#[path = "admin_api_mcp.rs"]
mod mcp;

#[path = "admin_api_settings.rs"]
mod settings;

#[path = "admin_api_config.rs"]
mod server_config;

#[path = "admin_api_damage.rs"]
mod damage;
pub(crate) use damage::{damage_help, unclean_json};

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

    /// The route over an offline admin of `registry` with the request `url` and `body`.
    fn route_with(
        registry: &std::sync::Arc<Registry>,
        method: &Method,
        endpoint: &str,
        url: &str,
        content_type: &str,
        body: &[u8],
    ) -> Result<Response, HttpError> {
        let mut request = Request::new(method.clone(), url, body)
            .with_header("Content-Type", content_type)
            .with_body_length(Some(body.len()));
        super::route(
            &Admin::offline(registry.clone()),
            method,
            endpoint,
            &mut request,
        )
    }

    /// `jobs` is reserved (the default backup root `backups/jobs/` would hold the admin
    /// backups of a database of that name): create, clone, import, alias and alias rename
    /// refuse it with help; nothing is made.
    #[test]
    fn the_reserved_name_jobs_is_refused_with_help() {
        let temp = TempDir::new("admin-api-reserved");
        let dir = server(&temp);
        add_db(&dir, "acme", Some(""), true);
        let registry = Registry::scan(&dir).unwrap();
        registry.alias_set("cur", "acme").unwrap();
        let cases: [(Method, &str, &str, &str, &[u8]); 5] = [
            (
                Method::Post,
                "databases",
                "/api/admin/databases",
                "application/json",
                br#"{"name": "jobs"}"#,
            ),
            (
                Method::Post,
                "databases/acme/clone",
                "/api/admin/databases/acme/clone",
                "application/json",
                br#"{"name": "jobs"}"#,
            ),
            (
                Method::Post,
                "import",
                "/api/admin/import?name=jobs",
                "application/octet-stream",
                b"not read",
            ),
            (
                Method::Put,
                "aliases/jobs",
                "/api/admin/aliases/jobs",
                "application/json",
                br#"{"target": "acme"}"#,
            ),
            (
                Method::Post,
                "aliases/cur/rename",
                "/api/admin/aliases/cur/rename",
                "application/json",
                br#"{"name": "jobs"}"#,
            ),
        ];
        for (method, endpoint, url, content_type, body) in cases {
            let answer = route_with(&registry, &method, endpoint, url, content_type, body)
                .unwrap_or_else(|err| panic!("{endpoint}: {}", err.message()));
            assert_eq!(answer.status(), 400, "{endpoint}");
            let answer = json_of(&answer);
            let error = answer["error"].as_str().unwrap();
            assert!(error.contains("reserved"), "{endpoint}: {error}");
            assert!(error.contains("backups/jobs/"), "{endpoint}: {error}");
            assert_eq!(answer["help"], crate::names::RESERVED_HELP, "{endpoint}");
        }
        let names: Vec<String> = registry.list().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["acme"]);
        assert_eq!(registry.aliases().len(), 1);
        assert_eq!(registry.aliases()["cur"], "acme");
        // A similar name is fine.
        let created = route_with(
            &registry,
            &Method::Post,
            "databases",
            "/api/admin/databases",
            "application/json",
            br#"{"name": "jobs-db"}"#,
        )
        .unwrap();
        assert_eq!(created.status(), 201);
    }

    /// The rule of NEW names (0.3.0: at most 32 characters, a letter or digit last, no `..`):
    /// create, clone, import, alias and alias rename refuse a name breaking it with `400`
    /// naming the reason; nothing is made. Databases named before (longer, a trailing '-')
    /// stay listed, looked up and aliased.
    #[test]
    fn new_names_follow_the_stricter_rule_and_old_names_keep_working() {
        let temp = TempDir::new("admin-api-new-names");
        let dir = server(&temp);
        let long = "a-database-name-of-forty-characters-0040";
        assert_eq!(long.len(), 40);
        add_db(&dir, "acme", Some(""), true);
        add_db(&dir, long, Some(""), true);
        add_db(&dir, "legacy-", Some(""), true);
        let registry = Registry::scan(&dir).unwrap();
        registry.alias_set("cur", "acme").unwrap();
        let too_long = "x".repeat(33);
        for (bad, reason) in [
            (too_long.as_str(), "at most 32 characters"),
            ("acme-", "ends with a lower-case letter or a digit"),
            ("acme_", "ends with a lower-case letter or a digit"),
            ("a..b", "cannot hold '..'"),
        ] {
            let body = serde_json::json!({ "name": bad }).to_string();
            let alias_url = format!("/api/admin/aliases/{bad}");
            let import_url = format!("/api/admin/import?name={bad}");
            let cases: [(Method, String, &str, &str, &[u8]); 5] = [
                (
                    Method::Post,
                    "databases".into(),
                    "/api/admin/databases",
                    "application/json",
                    body.as_bytes(),
                ),
                (
                    Method::Post,
                    "databases/acme/clone".into(),
                    "/api/admin/databases/acme/clone",
                    "application/json",
                    body.as_bytes(),
                ),
                (
                    Method::Post,
                    "import".into(),
                    &import_url,
                    "application/octet-stream",
                    b"not read",
                ),
                (
                    Method::Put,
                    format!("aliases/{bad}"),
                    &alias_url,
                    "application/json",
                    br#"{"target": "acme"}"#,
                ),
                (
                    Method::Post,
                    "aliases/cur/rename".into(),
                    "/api/admin/aliases/cur/rename",
                    "application/json",
                    body.as_bytes(),
                ),
            ];
            for (method, endpoint, url, content_type, body) in cases {
                let answer = route_with(&registry, &method, &endpoint, url, content_type, body)
                    .unwrap_or_else(|err| panic!("{endpoint}: {}", err.message()));
                assert_eq!(answer.status(), 400, "{endpoint} {bad}");
                let answer = json_of(&answer);
                let error = answer["error"].as_str().unwrap();
                assert!(error.contains("invalid name"), "{endpoint}: {error}");
                assert!(error.contains(reason), "{endpoint}: {error}");
            }
        }
        let mut names: Vec<String> = registry.list().into_iter().map(|e| e.name).collect();
        names.sort();
        assert_eq!(
            names,
            [
                "a-database-name-of-forty-characters-0040",
                "acme",
                "legacy-"
            ]
        );
        assert_eq!(registry.aliases().len(), 1);
        assert_eq!(registry.aliases()["cur"], "acme");

        // The old names: looked up (also folded) and aliased.
        for name in [long, "legacy-", "LEGACY-"] {
            let answer = route(&registry, &Method::Get, &format!("databases/{name}")).unwrap();
            assert_eq!(answer.status(), 200, "{name}");
        }
        let answer = route_with(
            &registry,
            &Method::Put,
            "aliases/old",
            "/api/admin/aliases/old",
            "application/json",
            format!(r#"{{"target": "{long}"}}"#).as_bytes(),
        )
        .unwrap();
        assert_eq!(answer.status(), 200, "{}", json_of(&answer));
        assert_eq!(registry.aliases()["old"], long);
        let answer = route_with(
            &registry,
            &Method::Put,
            "aliases/old",
            "/api/admin/aliases/old",
            "application/json",
            br#"{"target": "legacy-"}"#,
        )
        .unwrap();
        assert_eq!(answer.status(), 200, "{}", json_of(&answer));
        assert_eq!(registry.aliases()["old"], "legacy-");
        // A 32-character new name is the longest.
        let longest = "y".repeat(32);
        let created = route_with(
            &registry,
            &Method::Post,
            "databases",
            "/api/admin/databases",
            "application/json",
            format!(r#"{{"name": "{longest}"}}"#).as_bytes(),
        )
        .unwrap();
        assert_eq!(created.status(), 201, "{}", json_of(&created));
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
