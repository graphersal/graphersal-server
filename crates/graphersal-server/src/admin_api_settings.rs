//! The admin API's settings route of one database (`/api/admin/databases/<name>/settings`,
//! round 3 B7; [`crate::settings`]): everything about a database in one answer, and the change
//! of its runtime-editable settings; the web UI's Settings tab and the console's `db info` /
//! `db set` use it.
//!
//! | Method | Path | Body | Answer |
//! |---|---|---|---|
//! | GET | `databases/<name>/settings` | | `200` the settings (below; `<name>` may be an alias; `ETag` = `revision`) |
//! | PUT | `databases/<name>/settings` | `{"description"?: text, "readOnly"?: bool, "autoLoad"?: bool, "limits"?: {key: n \| null}, "revision"?}` | `200` the settings + `"changed": [field...]` |
//!
//! The settings: `name` (the real name), `requested` (the alias asked for, else absent),
//! `revision` (of its `database.toml`), `database` (the listing row: state, kind, role, base,
//! readOnly, autoLoad, description, created, sizeBytes, memoryBytes, ...), `aliases` (the
//! aliases naming it), `layers` (a base: the layers over it), `limits` (`loaded`,
//! `reloadNeeded` (loaded and the limits in effect differ from what the next load reads),
//! `applies` (when a change applies), `serverError` (a `server.toml` that cannot be read, else
//! `null`), `keys`: one row per key of `[limits.query]`: `key`, `override` (the database's
//! own value or `null`), `server` (`server.toml`'s or `null`), `effective` (what the next load
//! uses), `source` (`database` | `server` | `none`), `inEffect` (only when loaded: what the
//! running session uses)), `mcp` (the MCP state's `enabled`, `endpointBuilt`, `serving`,
//! `revision` and `names`: the MCP rows of the database and its aliases; or `{"error"}`),
//! `grants` (`{"subject", "database", "level", "via": "database" | "alias" | "all"}`: the
//! grants on its name, on an alias naming it and on `*`), `admins` (the users with the admin
//! role: they reach every database), `backups` (`count`, `bytes`, `latest`: a backup row or
//! `null`), `store` (the Store's creation parameters and position from the engine's
//! `Store::inspect`: `storeId`, `graphId`, `damagePolicy`, `chunkBytes`, `createdAt`,
//! `formatVersion`, `backend`, `commitSeq`, `lastCommitAt`, `layerBase`, `snapshots`,
//! `walBytes`, `clean`; read-only) or `storeError`.
//!
//! A change takes the real name (an alias is `404` with a help naming the database). Each
//! field is optional, at least one is given; `limits` sets a key to a whole number
//! `0..=9223372036854775807` or clears it with `null` (the server's default applies again);
//! the change is ONE write of `database.toml`, based on `revision` (else `If-Match`; a stale
//! one is `409` with the current `revision`), and audited field by field. Description and
//! auto-load apply at once, read-only at once (a loaded database is flipped), a limit at the
//! database's next load. Errors: `400` (an empty change, an unknown key, a bad value or
//! description), `404`, `409` (stale revision, read-only off for a base, a failed database),
//! `500`.

use graphersal_serve::{HttpError, Method, Request, Response};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{Extras, backup_json, body_of, error_answer, layers_of, read_only_help};
use crate::database::DatabaseConfig;
use crate::error::ServerError;
use crate::limits::QUERY_LIMIT_KEYS;
use crate::names::fold;
use crate::ops::Admin;
use crate::registry::DatabaseEntry;
use crate::settings::{LIMITS_APPLY_TEXT, SettingsChange, limit_value_of_json};

/// `PUT databases/<name>/settings`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ChangeBody {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    read_only: Option<bool>,
    #[serde(default)]
    auto_load: Option<bool>,
    #[serde(default)]
    limits: Option<Map<String, Value>>,
    #[serde(default)]
    revision: Option<String>,
}

/// Answers `method databases/<name>/settings`; `None` when the path is not this route.
pub(super) fn route(
    admin: &Admin,
    method: &Method,
    segments: &[&str],
    request: &mut Request<'_>,
    expecting: &dyn Fn(Option<String>) -> Admin,
) -> Option<Result<Response, HttpError>> {
    let ["databases", name, "settings"] = segments else {
        return None;
    };
    if name.is_empty() {
        return None;
    }
    let name = *name;
    Some(match method {
        Method::Get | Method::Head => view(admin, name)
            .map(answer)
            .ok_or_else(|| HttpError::new(404, format!("no database or alias '{name}'"))),
        Method::Put => change(admin, name, request, expecting),
        _ => Err(HttpError::new(
            405,
            format!("method {method} is not allowed on /api/admin/databases/{name}/settings"),
        )),
    })
}

/// `PUT databases/<name>/settings`.
fn change(
    admin: &Admin,
    name: &str,
    request: &mut Request<'_>,
    expecting: &dyn Fn(Option<String>) -> Admin,
) -> Result<Response, HttpError> {
    let body: ChangeBody = body_of(request)?;
    let mut limits = Vec::new();
    for (key, value) in body.limits.iter().flatten() {
        crate::settings::check_limit_key(key).map_err(HttpError::bad_request)?;
        let value = limit_value_of_json(key, value).map_err(HttpError::bad_request)?;
        limits.push((key.clone(), value));
    }
    let change = SettingsChange {
        description: body.description,
        read_only: body.read_only,
        auto_load: body.auto_load,
        limits,
    };
    let registry = admin.registry();
    match expecting(body.revision).change_settings(name, &change) {
        Ok(entry) => {
            let mut out = view(admin, &entry.name).unwrap_or_else(|| json!({}));
            out["changed"] = json!(changed_fields(&change));
            Ok(answer(out))
        }
        Err(err) => {
            let help = match &err {
                ServerError::NotFound { .. } => match registry.aliases().get(&fold(name)) {
                    Some(target) => Some(format!(
                        "'{}' is an alias of '{target}': change the settings of the database \
                         itself: PUT /api/admin/databases/{target}/settings",
                        fold(name)
                    )),
                    None => {
                        Some("GET /api/admin/databases lists the databases and aliases".to_string())
                    }
                },
                ServerError::InvalidInput { .. } => Some(format!(
                    "the editable settings: description, readOnly, autoLoad, limits {{{}}}",
                    QUERY_LIMIT_KEYS.join(", ")
                )),
                _ => change
                    .read_only
                    .and_then(|read_only| read_only_help(registry, name, read_only, &err)),
            };
            Ok(error_answer(&err, help))
        }
    }
}

/// The fields `change` names, in the order of the body.
fn changed_fields(change: &SettingsChange) -> Vec<String> {
    let mut out = Vec::new();
    if change.description.is_some() {
        out.push("description".to_string());
    }
    if change.read_only.is_some() {
        out.push("readOnly".to_string());
    }
    if change.auto_load.is_some() {
        out.push("autoLoad".to_string());
    }
    out.extend(change.limits.iter().map(|(key, _)| format!("limits.{key}")));
    out
}

/// `200` of the settings with its revision as `ETag`.
fn answer(view: Value) -> Response {
    let etag = view["revision"].as_str().map(crate::revision::etag);
    let response = Response::json(200, &view);
    match etag {
        Some(etag) => response.with_header("ETag", &etag),
        None => response,
    }
}

/// The settings of the database or alias `name` (module docs); `None` when it names nothing.
pub(crate) fn view(admin: &Admin, name: &str) -> Option<Value> {
    let registry = admin.registry();
    let (entry, requested) = match registry.get(name) {
        Ok(entry) => (entry, None),
        Err(_) => {
            let alias = fold(name);
            registry.aliases().get(&alias)?;
            (registry.resolve(&alias).ok()?, Some(alias))
        }
    };
    let real = entry.name.clone();
    let aliases: Vec<String> = registry
        .aliases()
        .into_iter()
        .filter(|(_, target)| *target == real)
        .map(|(alias, _)| alias)
        .collect();
    let mut out = json!({
        "name": real,
        "revision": entry.revision(),
        "database": Extras::of(admin).row(&entry, None),
        "aliases": aliases,
        "layers": layers_of(registry, &real),
        "limits": limits_json(admin, &entry),
        "mcp": mcp_json(admin, &real, &aliases),
        "grants": grants_json(admin, &real, &aliases),
        "admins": admins_json(admin),
        "backups": backups_json(admin, &real),
    });
    if let Some(requested) = requested {
        out["requested"] = json!(requested);
    }
    match admin.store_facts(&entry) {
        Ok(facts) => {
            out["store"] = json!({
                "storeId": facts.store_id,
                "graphId": facts.graph_id,
                "damagePolicy": facts.damage_policy,
                "chunkBytes": facts.chunk_bytes,
                "createdAt": facts.created_at,
                "formatVersion": facts.format_version,
                "backend": facts.backend,
                "commitSeq": facts.commit_seq,
                "lastCommitAt": facts.last_commit_at,
                "layerBase": facts.layer_base,
                "snapshots": facts.snapshots,
                "walBytes": facts.wal_bytes,
                "clean": facts.clean,
            });
        }
        Err(reason) => out["storeError"] = json!(reason),
    }
    Some(out)
}

/// The `limits` of the settings (module docs).
fn limits_json(admin: &Admin, entry: &DatabaseEntry) -> Value {
    // The file now (a hand edit since the last load shows), else the registry's copy.
    let own = DatabaseConfig::read(&entry.dir)
        .ok()
        .or_else(|| entry.config.clone())
        .map(|config| config.limits.query)
        .unwrap_or_default();
    let (server, server_error) = match admin.server_query_limits() {
        Ok(limits) => (limits, None),
        Err(err) => (Default::default(), Some(err.to_string())),
    };
    let effective = own.over(&server);
    let in_effect = admin.limits_in_effect(&entry.name);
    let keys: Vec<Value> = QUERY_LIMIT_KEYS
        .iter()
        .map(|key| {
            let own = own.get(key).flatten();
            let server = server.get(key).flatten();
            let source = match (own, server) {
                (Some(_), _) => "database",
                (None, Some(_)) => "server",
                (None, None) => "none",
            };
            let mut row = json!({
                "key": key,
                "override": own,
                "server": server,
                "effective": own.or(server),
                "source": source,
            });
            if let Some(in_effect) = &in_effect {
                row["inEffect"] = json!(in_effect.get(key).flatten());
            }
            row
        })
        .collect();
    json!({
        "loaded": in_effect.is_some(),
        "reloadNeeded": in_effect.as_ref().is_some_and(|now| *now != effective),
        "applies": LIMITS_APPLY_TEXT,
        "serverError": server_error,
        "keys": keys,
    })
}

/// The `mcp` of the settings: the MCP state reduced to the database and its aliases.
fn mcp_json(admin: &Admin, real: &str, aliases: &[String]) -> Value {
    match super::mcp::state(admin) {
        Ok(state) => {
            let names: Vec<Value> = state["databases"]
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .filter(|row| {
                            let name = row["name"].as_str().unwrap_or_default();
                            name == real || aliases.iter().any(|a| a == name)
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            json!({
                "enabled": state["enabled"],
                "endpointBuilt": state["endpointBuilt"],
                "serving": state["serving"],
                "revision": state["revision"],
                "names": names,
            })
        }
        Err(err) => json!({ "error": err.to_string() }),
    }
}

/// The `grants` of the settings: on the name, on an alias naming it, on `*`.
fn grants_json(admin: &Admin, real: &str, aliases: &[String]) -> Value {
    let Ok(file) = admin.users() else {
        return json!([]);
    };
    let rows: Vec<Value> = file
        .grants()
        .iter()
        .filter_map(|grant| {
            let via = if grant.database == real {
                "database"
            } else if aliases.contains(&grant.database) {
                "alias"
            } else if grant.database == "*" {
                "all"
            } else {
                return None;
            };
            Some(json!({
                "subject": grant.subject.to_string(),
                "database": grant.database,
                "level": grant.level.as_str(),
                "via": via,
            }))
        })
        .collect();
    json!(rows)
}

/// The users with the admin role (they reach every database).
fn admins_json(admin: &Admin) -> Value {
    let Ok(file) = admin.users() else {
        return json!([]);
    };
    let names: Vec<&String> = file
        .users()
        .iter()
        .filter(|(_, user)| user.roles.iter().any(|r| r == crate::access::ADMIN_ROLE))
        .map(|(name, _)| name)
        .collect();
    json!(names)
}

/// The `backups` of the settings.
fn backups_json(admin: &Admin, real: &str) -> Value {
    let list = admin.backups(real).unwrap_or_default();
    let latest = list
        .iter()
        .max_by(|a, b| a.backup.cmp(&b.backup))
        .map(|b| backup_json(admin.registry(), b));
    json!({
        "count": list.len(),
        "bytes": list.iter().map(|b| b.bytes).sum::<u64>(),
        "latest": latest,
    })
}
