//! The admin API's MCP routes (`/api/admin/mcp...`, round 3 B6; [`crate::mcp_control`]): the
//! switches of `mcp.toml` and the usage statistics; the web UI's MCP tab and the console's
//! `mcp` commands use them.
//!
//! | Method | Path | Body | Answer |
//! |---|---|---|---|
//! | GET | `mcp` | | `200` the MCP state (below; + `ETag`) |
//! | PUT | `mcp` | `{"enabled"?: bool, "exposeNew"?: bool, "revision"?}` (at least one) | `200` the MCP state |
//! | PUT | `mcp/databases/<name>` | `{"exposed"?: bool, "readOnly"?: bool, "revision"?}` | `200` the MCP state + `"changed": {"name", "exposed", "readOnly"}` |
//!
//! The MCP state: `enabled` (the switch of `mcp.toml`), `exposeNew` (whether a new database
//! or alias is offered to MCP when its creator does not say; `mcp.toml`'s `expose_new`,
//! default `false`), `endpointBuilt` (`server.toml [mcp]
//! enabled`: `false` = no MCP endpoint at all until a restart with it on), `serving`
//! (`enabled && endpointBuilt`), `online` (a running server answered; offline the statistics
//! are `null`), `endpoint` (`/mcp`), `maxSessions`, `revision` (of the settings), `databases`
//! (every database and alias, and every name with settings that names nothing now: `name`,
//! `kind` (`database` | `alias` | `missing`), `target` (an alias's database), `exposed` and
//! `readOnly` (the name's own settings), `effectiveExposed` (the name and its database
//! exposed), `effectiveReadOnly` (MCP read-only on the name or its database, or the
//! database's own mode: its read-only flag, a base), `sessions` (open MCP sessions working on
//! the name: its selection on `/mcp` or its `/db/<name>/mcp`), `calls`, `errors` (tool calls
//! since the start)) and `usage` ([`crate::mcp_control::McpUsage::to_json`]: `startedAtMs`,
//! `sessions {open, byUser, byDatabase, withoutDatabase}`, `calls {total, errors, refused}`,
//! `tools`, `users`, `databases` (`[{name, calls, errors}]`, most calls first),
//! `recentErrors` (`[{atMs, tool, user, database, message}]`, newest first), or `null`
//! offline).
//!
//! Errors: `400` (a body without `exposed` or `readOnly`; a `PUT mcp` without `enabled` or
//! `exposeNew`), `404` (no such database or alias),
//! `409` (a stale revision, with the current `revision`), `500` (`mcp.toml` cannot be read or
//! written).

use graphersal_serve::{HttpError, Method, Request, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{body_of, error_answer};
use crate::database::DatabaseRole;
use crate::error::ServerError;
use crate::mcp_control::McpSettings;
use crate::names::fold;
use crate::ops::Admin;

/// `PUT mcp`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SwitchBody {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    expose_new: Option<bool>,
    #[serde(default)]
    revision: Option<String>,
}

/// `PUT mcp/databases/<name>`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct NameBody {
    #[serde(default)]
    exposed: Option<bool>,
    #[serde(default)]
    read_only: Option<bool>,
    #[serde(default)]
    revision: Option<String>,
}

/// Answers `method mcp...` (`segments`: the endpoint split at `/`); `None` when the path is
/// none of these routes.
pub(super) fn route(
    admin: &Admin,
    method: &Method,
    segments: &[&str],
    request: &mut Request<'_>,
    expecting: &dyn Fn(Option<String>) -> Admin,
) -> Option<Result<Response, HttpError>> {
    let read = matches!(method, Method::Get | Method::Head);
    let decoded: Vec<String> = match segments
        .iter()
        .map(|s| crate::router::percent_decode(s))
        .collect::<Option<Vec<String>>>()
    {
        Some(decoded) => decoded,
        None => return Some(Err(HttpError::bad_request("a broken %-escape in the path"))),
    };
    let parts: Vec<&str> = decoded.iter().map(String::as_str).collect();
    let answer: Result<Response, ServerError> = match (method, parts.as_slice()) {
        (_, ["mcp"]) if read => state(admin).map(answer),
        (Method::Put, ["mcp"]) => {
            let body: SwitchBody = match body_of(request) {
                Ok(body) => body,
                Err(err) => return Some(Err(err)),
            };
            if body.enabled.is_none() && body.expose_new.is_none() {
                return Some(Err(HttpError::bad_request(
                    "send \"enabled\" and/or \"exposeNew\" (true or false)",
                )));
            }
            // Both in one request: the second change is based on the first one's result.
            let mut expected = expecting(body.revision);
            let mut changed = Ok(());
            if let Some(on) = body.expose_new {
                changed = expected.mcp_set_expose_new(on).map(|settings| {
                    expected = admin.clone().expecting(Some(settings.revision()));
                });
            }
            if let (Ok(()), Some(on)) = (&changed, body.enabled) {
                changed = expected.mcp_set_enabled(on).map(drop);
            }
            changed.and_then(|()| state(admin)).map(answer)
        }
        (Method::Put, ["mcp", "databases", name]) if !name.is_empty() => {
            let body: NameBody = match body_of(request) {
                Ok(body) => body,
                Err(err) => return Some(Err(err)),
            };
            if body.exposed.is_none() && body.read_only.is_none() {
                return Some(Err(HttpError::bad_request(
                    "send \"exposed\" and/or \"readOnly\" (true or false)",
                )));
            }
            expecting(body.revision)
                .mcp_set_name(name, body.exposed, body.read_only)
                .and_then(|(name, value, _)| {
                    let mut out = state(admin)?;
                    out["changed"] = json!({
                        "name": name,
                        "exposed": value.exposed,
                        "readOnly": value.read_only,
                    });
                    Ok(answer(out))
                })
        }
        (_, ["mcp"]) | (_, ["mcp", "databases", _]) => {
            return Some(Err(HttpError::new(
                405,
                format!(
                    "method {method} is not allowed on /api/admin/{}",
                    segments.join("/")
                ),
            )));
        }
        (_, ["mcp", ..]) => {
            return Some(Err(HttpError::new(
                404,
                format!("unknown admin endpoint /api/admin/{}", segments.join("/")),
            )));
        }
        _ => return None,
    };
    Some(Ok(answer.unwrap_or_else(|err| {
        let help = match &err {
            ServerError::NotFound { .. } => Some(
                "GET /api/admin/mcp lists the databases and aliases with their MCP settings"
                    .to_string(),
            ),
            _ => None,
        };
        error_answer(&err, help)
    })))
}

/// `200` of the MCP state with its revision as `ETag`.
fn answer(state: Value) -> Response {
    let etag = state["revision"]
        .as_str()
        .map(crate::revision::etag)
        .unwrap_or_default();
    Response::json(200, &state).with_header("ETag", &etag)
}

/// The MCP state (module docs).
pub(crate) fn state(admin: &Admin) -> Result<Value, ServerError> {
    let control = admin.mcp_control()?;
    let settings = control.settings();
    let built = admin.mcp_endpoint_built();
    let usage = control.usage();
    let max_sessions = admin
        .registry()
        .dir()
        .load_config()
        .map(|config| config.mcp.max_sessions)
        .ok();
    Ok(json!({
        "enabled": settings.enabled,
        "exposeNew": settings.expose_new,
        "endpointBuilt": built,
        "serving": settings.enabled && built,
        "online": admin.is_online(),
        "endpoint": crate::mcp::ENDPOINT,
        "maxSessions": max_sessions,
        "revision": settings.revision(),
        "databases": rows(admin, &settings, |name| usage.of_name(name)),
        "usage": admin.is_online().then(|| usage.to_json()),
    }))
}

/// The `databases` rows of the MCP state: every database, every alias, every name with
/// settings that names nothing now.
fn rows(
    admin: &Admin,
    settings: &McpSettings,
    counts: impl Fn(&str) -> (u64, u64, u64),
) -> Vec<Value> {
    let registry = admin.registry();
    let entries = registry.list();
    let aliases = registry.aliases();
    let mode_read_only = |real: &str| {
        entries
            .iter()
            .find(|e| e.name == real)
            .and_then(|e| e.config.as_ref())
            .is_some_and(|c| c.read_only || c.role == DatabaseRole::Base)
    };
    let row = |name: &str, kind: &str, target: Option<&str>| {
        let real = target.unwrap_or(name);
        let own = settings.of(name);
        let (sessions, calls, errors) = counts(name);
        json!({
            "name": name,
            "kind": kind,
            "target": target,
            "exposed": own.exposed,
            "readOnly": own.read_only,
            "effectiveExposed": settings.exposed(name, real),
            "effectiveReadOnly": settings.read_only(name, real) || mode_read_only(real),
            "sessions": sessions,
            "calls": calls,
            "errors": errors,
        })
    };
    let mut out: Vec<Value> = entries
        .iter()
        .map(|e| row(&e.name, "database", None))
        .collect();
    out.extend(
        aliases
            .iter()
            .map(|(alias, target)| row(alias, "alias", Some(target))),
    );
    for name in settings.databases.keys() {
        let known = entries.iter().any(|e| &e.name == name) || aliases.contains_key(name);
        if !known {
            out.push(row(&fold(name), "missing", None));
        }
    }
    out
}
