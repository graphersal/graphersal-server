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
//! | GET | `aliases` | | `200 {"aliases": {alias: database}}` |
//! | PUT | `aliases/<alias>` | `{"target"}` | `200 {"alias", "target"}` |
//! | DELETE | `aliases/<alias>` | | `200 {"alias", "removed": true}` |
//!
//! The actions take real database names (not aliases). Errors are `{"error", "help"?}` with
//! `400` (invalid name, body or schema), `404` (no such database, alias or endpoint), `405`,
//! `409` (a rule refuses it: name taken, loaded, named by an alias, base of layers, offline,
//! failed database), `413` (body too large), `500` (a Store or file failed). The repository's
//! `docs/admin-api.md` documents the routes for clients.
//!
//! An entry: `name`, `state` (`unloaded` | `loading` | `loaded` | `needs-model` | `failed`),
//! `kind` (`plain` | `layer`), `role` (`normal` | `base`), `readOnly` (a base is always
//! read-only), `autoLoad`, `description`, and when they apply `base` (a layer's base), `created`,
//! `reason` (`failed`: why; `needs-model`: the missing model), `model` (`needs-model`: the
//! model's repository), `aliasOf` (an alias row: the database it names; its other fields are
//! that database's). A database whose `database.toml` cannot be read has no `kind`, `role`,
//! `autoLoad` or `description`. The field names are those of the playground's landing view.

use std::path::Path;

use graphersal_serve::{HttpError, Method, Request, Response};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::database::{DatabaseKind, DatabaseRole};
use crate::error::ServerError;
use crate::loader::LoadStart;
use crate::names::fold;
use crate::ops::{Admin, CreateSpec};
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
                "clone" | "load" | "unload" | "read-only" | "auto-load",
            ],
        )
        | (_, ["aliases"])
        | (_, ["aliases", _]) => return Err(not_allowed()),
        _ => {
            return Err(HttpError::new(
                404,
                format!("unknown admin endpoint /api/admin/{endpoint}"),
            ));
        }
    };
    Ok(answer.unwrap_or_else(|(err, help)| error_answer(&err, help)))
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
        ServerError::NotFound { .. } => 404,
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
