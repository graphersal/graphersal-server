//! The admin API's server settings route (`/api/admin/config`, round 4 B-SETTINGS;
//! [`crate::server_settings`]): every key of `server.toml`, and its change at runtime; the web
//! UI's Settings tab ("Server" and "Server defaults") and the console's `config get|set|unset`
//! use it.
//!
//! | Method | Path | Body | Answer |
//! |---|---|---|---|
//! | GET | `config` | | `200` the settings (below; `ETag` = `revision`) |
//! | PUT | `config` | `{"set": {key: value \| null}, "revision"?}` | `200` the settings + `"changed": [key...]`, `"applied": [key...]`, `"rewritten": bool` |
//!
//! The settings: `path` (the file), `revision` (of its text), `running` (`true` when a running
//! server answered; offline the console reads the file), `restartPending` (the keys whose
//! change waits for a restart), `keys` (one row per key in file order: `key`, `section`,
//! `value` (the file's value, `null` = not set: the default applies), `default`, `set`,
//! `kind` (`bool` | `count` | `text` | `memory` | `hosts`), `choices` (a text key's values or
//! `null`), `applies` (`now` | `load` | `create` | `restart`), `appliesText`, `running` (what the running
//! server applies, for `now` and `restart` keys), `pending`, `help`), `queryLimitKeys`, and
//! `mcpExposeNew` (`mcp.toml`'s `expose_new`, the MCP default of new databases and aliases:
//! changed by `PUT mcp {"exposeNew"}`, shown here for the "Server defaults"; `null` when
//! `mcp.toml` cannot be read).
//!
//! A change sets each named key to a value (JSON of the key's type: `true`/`false`, a whole
//! number, a text, `memory_limit` a number or `"auto"`/`"none"`, `allowed_hosts` a list of
//! names) or back to its default with `null`; ONE write of `server.toml` (atomic, its comments
//! kept where the line patch can; `rewritten` says when they could not be), based on
//! `revision` (else `If-Match`; a stale one is `409` with the current `revision`), checked by
//! the same parser as at start, audited key by key. `changed`: the keys whose value changed;
//! `applied`: the keys the running server applied at once. Errors: `400` (an empty or unknown
//! change, a refused value: the parser's words), `409` (stale revision), `500` (the file cannot
//! be read or written).

use graphersal_serve::{HttpError, Method, Request, Response};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{body_of, error_answer};
use crate::error::ServerError;
use crate::ops::Admin;
use crate::server_settings::toml_of_json;

/// `PUT config`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ChangeBody {
    #[serde(default)]
    set: Option<Map<String, Value>>,
    #[serde(default)]
    revision: Option<String>,
}

/// Answers `method config`; `None` when the path is not this route.
pub(super) fn route(
    admin: &Admin,
    method: &Method,
    segments: &[&str],
    request: &mut Request<'_>,
    expecting: &dyn Fn(Option<String>) -> Admin,
) -> Option<Result<Response, HttpError>> {
    let ["config"] = segments else {
        return None;
    };
    Some(match method {
        Method::Get | Method::Head => Ok(match view(admin) {
            Ok(view) => answer(view),
            Err(err) => error_answer(&err, None),
        }),
        Method::Put => change(admin, request, expecting),
        _ => Err(HttpError::new(
            405,
            format!("method {method} is not allowed on /api/admin/config"),
        )),
    })
}

/// The settings view with the MCP default of new names.
fn view(admin: &Admin) -> Result<Value, ServerError> {
    let mut view = admin.server_settings()?;
    view["mcpExposeNew"] = admin
        .mcp_control()
        .map(|control| json!(control.settings().expose_new))
        .unwrap_or(Value::Null);
    Ok(view)
}

/// `PUT config`.
fn change(
    admin: &Admin,
    request: &mut Request<'_>,
    expecting: &dyn Fn(Option<String>) -> Admin,
) -> Result<Response, HttpError> {
    let body: ChangeBody = body_of(request)?;
    let set = body.set.unwrap_or_default();
    if set.is_empty() {
        return Err(HttpError::bad_request(
            "send \"set\": {key: value | null} with at least one key of server.toml (GET \
             /api/admin/config lists them)",
        ));
    }
    let mut changes = Vec::new();
    for (key, value) in &set {
        match toml_of_json(key, value) {
            Ok(value) => changes.push((key.clone(), value)),
            Err(err) => return Ok(error_answer(&err, None)),
        }
    }
    let result = expecting(body.revision)
        .change_server_settings(&changes)
        .and_then(|(edited, applied)| {
            let mut out = view(admin)?;
            out["changed"] = json!(edited.changed);
            out["applied"] = json!(applied);
            out["rewritten"] = json!(edited.rewritten);
            Ok(out)
        });
    Ok(match result {
        Ok(out) => answer(out),
        Err(err) => error_answer(&err, None),
    })
}

/// `200` with the view and its `ETag`.
fn answer(view: Value) -> Response {
    let etag = view["revision"].as_str().map(crate::revision::etag);
    let response = Response::json(200, &view);
    match etag {
        Some(etag) => response.with_header("ETag", &etag),
        None => response,
    }
}
