//! The admin API's MCP routes (`/api/admin/mcp...`, round 3 B6; [`crate::mcp_control`]): the
//! switches of `mcp.toml` and the usage statistics; the web UI's MCP tab and the console's
//! `mcp` commands use them.
//!
//! | Method | Path | Body | Answer |
//! |---|---|---|---|
//! | GET | `mcp` | | `200` the MCP state (below; + `ETag`) |
//! | PUT | `mcp` | `{"enabled"?: bool, "exposeNew"?: bool, "revision"?}` (at least one) | `200` the MCP state |
//! | PUT | `mcp/databases/<name>` | `{"exposed"?: bool, "readOnly"?: bool, "revision"?}` | `200` the MCP state + `"changed": {"name", "exposed", "readOnly"}` |
//! | GET | `mcp/builtin` | | `200` the built-in texts a profile starts from ([`crate::mcp_profiles::builtin`]) |
//! | GET | `mcp/profile` | | `200` the profile view of the server default profile (below; + `ETag`) |
//! | PUT | `mcp/profile` | `{"profile": {...}, "revision"?}` | `200` the profile view + `"changed": true` |
//! | DELETE | `mcp/profile` | (`If-Match`) | `200` the profile view + `"changed": true` |
//! | GET, PUT, DELETE | `mcp/databases/<name>/profile` | as for `mcp/profile` | the profile view of the name |
//! | POST | `mcp/preview` | `{"profile"?: {...} \| null, "name"?, "endpoint"?: "database" \| "server", "level"?: "read" \| "write"}` | `200` what an agent sees (below) |
//!
//! MCP profiles (MCPPROF, [`crate::mcp_profiles`]): the profile view is `name` (null for the
//! server default), `kind` (`default` | `database` | `alias` | `missing`), `target` (an alias's
//! database), `profile` (the name's own, or null), `source` (`own` | `database` | `default` |
//! `builtin`: where the profile that applies comes from), `effective` (that profile, null =
//! built-in), `endpoint`, `readOnly` (MCP serves it read-only whatever the grant: a base, the
//! read-only flag, MCP read-only), `loaded`, `savedQueries` (`[{name, folder, description,
//! writes}]` of a loaded database, else null), `warnings` (writing tools a reader never gets,
//! `saved_query_tools` on `/mcp`), `revision` (of `mcp.toml`, shared with the switches). A
//! refused profile is `400 {"error": "the MCP profile is not valid: <graphersal-mcp's
//! message>", "help"}`. The preview runs graphersal-mcp's own `initialize`, `tools/list` and
//! `resources/list` for the profile (absent: the name's effective one; null: built-in) on the
//! database's context when it is loaded (else an empty graph), at `level` (default `write`;
//! a read-only database is read for everyone) or for `/mcp` (`endpoint` `server`, the default
//! without `name`): `{instructions, serverInfo, tools, resources, endpoint, name, level,
//! readOnly, loaded, source, notes, warnings, directoryBlock}`. `directoryBlock` (with a
//! `name`, else null) is the text "Instructions for this database" that `/mcp` gives a caller
//! at `level` for that name after `use_graph` ([`crate::mcp_guidance`]), with the previewed
//! profile in place: the name's own (`endpoint` `database`) or the server default (`server`).
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
//! database's own mode: its read-only flag, a base), `readOnlyReason` (`null`, or why the name
//! is read-only for MCP whatever its own setting: `base` (a base is always read-only), `flag`
//! (the database's read-only flag), `database` (an alias whose database is read-only for
//! MCP)), `sessions` (open MCP sessions working on
//! the name: its selection on `/mcp` or its `/db/<name>/mcp`), `calls`, `errors` (tool calls
//! since the start), `profile` (where its MCP profile comes from: `own` | `database` | `default`
//! | `builtin`)), `defaultProfile` (whether a server default profile is set) and `usage` ([`crate::mcp_control::McpUsage::to_json`]: `startedAtMs`,
//! `sessions {open, byUser, byDatabase, withoutDatabase}`, `calls {total, errors, refused}`,
//! `tools`, `users`, `databases` (`[{name, calls, errors}]`, most calls first),
//! `recentErrors` (`[{atMs, tool, user, database, message}]`, newest first), or `null`
//! offline).
//!
//! Errors: `400` (a body without `exposed` or `readOnly`; a `PUT mcp` without `enabled` or
//! `exposeNew`; a profile graphersal-mcp refuses), `404` (no such database or alias),
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

/// `PUT mcp/profile`, `PUT mcp/databases/<name>/profile`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileBody {
    profile: Value,
    #[serde(default)]
    revision: Option<String>,
}

/// The `400` of a profile graphersal-mcp refuses (its message names the key).
fn invalid_profile(message: &str) -> Response {
    Response::json(
        400,
        &json!({
            "error": format!("the MCP profile is not valid: {message}"),
            "help": crate::mcp_profiles::PROFILE_HELP,
        }),
    )
}

/// What the name `key` is: `database`, `alias` (with its database) or `missing` (only
/// settings left on it); `None`: nothing of that name.
fn name_kind(
    admin: &Admin,
    settings: &McpSettings,
    key: &str,
) -> Option<(&'static str, Option<String>)> {
    let registry = admin.registry();
    if registry.list().iter().any(|e| e.name == key) {
        return Some(("database", None));
    }
    if let Some(target) = registry.aliases().get(key) {
        return Some(("alias", Some(target.clone())));
    }
    settings
        .databases
        .contains_key(key)
        .then_some(("missing", None))
}

/// Whether MCP serves the request naming `key` (resolving to `real`) read-only whatever the
/// caller's grant: MCP read-only on either name, or the database's own mode (a base, its
/// read-only flag).
fn mcp_read_only_of(admin: &Admin, settings: &McpSettings, key: &str, real: &str) -> bool {
    let mode = admin
        .registry()
        .list()
        .iter()
        .find(|e| e.name == real)
        .and_then(|e| e.config.clone())
        .is_some_and(|c| c.role == DatabaseRole::Base || c.read_only);
    mode || settings.read_only(key, real)
}

/// The profile view of `GET mcp/profile` (`name` `None`: the server default) or `GET
/// mcp/databases/<name>/profile` (module docs).
fn profile_view(admin: &Admin, name: Option<&str>) -> Result<Value, ServerError> {
    let control = admin.mcp_control()?;
    let settings = control.settings();
    let revision = settings.revision();
    let Some(name) = name else {
        let profile = settings.default_profile.clone();
        let warnings = profile
            .as_ref()
            .map(|p| crate::mcp_profiles::warnings(p, false, true))
            .unwrap_or_default();
        return Ok(json!({
            "name": null,
            "kind": "default",
            "profile": profile,
            "source": if profile.is_some() { "own" } else { "builtin" },
            "effective": profile,
            "endpoint": crate::mcp::ENDPOINT,
            "readOnly": false,
            "loaded": null,
            "savedQueries": null,
            "warnings": warnings,
            "revision": revision,
        }));
    };
    let key = fold(name);
    let Some((kind, target)) = name_kind(admin, &settings, &key) else {
        return Err(ServerError::NotFound { name: key });
    };
    let real = target.clone().unwrap_or_else(|| key.clone());
    let (effective, source) = settings.effective_profile(&key, &real);
    let read_only = mcp_read_only_of(admin, &settings, &key, &real);
    let context = admin.loaded_context(&real);
    let saved = context
        .as_ref()
        .and_then(|c| crate::mcp_profiles::saved_queries(c).ok());
    let warnings = effective
        .as_ref()
        .map(|p| crate::mcp_profiles::warnings(p, read_only, false))
        .unwrap_or_default();
    Ok(json!({
        "name": key,
        "kind": kind,
        "target": target,
        "profile": settings.of(&key).profile,
        "source": source.as_str(),
        "effective": effective,
        "endpoint": format!("/db/{key}/mcp"),
        "readOnly": read_only,
        "loaded": context.is_some(),
        "savedQueries": saved,
        "warnings": warnings,
        "revision": revision,
    }))
}

/// `POST mcp/preview` (module docs): `Ok(Ok(answer))`, `Ok(Err(400))` for a bad body or
/// profile.
fn preview(admin: &Admin, body: &Value) -> Result<Result<Value, Response>, ServerError> {
    let bad = |message: String| Ok(Err(Response::json(400, &json!({"error": message}))));
    let Some(fields) = body.as_object() else {
        return bad(
            "the body is a JSON object: {\"profile\"?, \"name\"?, \"endpoint\"?, \"level\"?}"
                .into(),
        );
    };
    if let Some(key) = fields
        .keys()
        .find(|k| !matches!(k.as_str(), "profile" | "name" | "endpoint" | "level"))
    {
        return bad(format!(
            "unknown field {key:?}: the body takes profile, name, endpoint and level"
        ));
    }
    let name = match fields.get("name") {
        None | Some(Value::Null) => None,
        Some(Value::String(name)) if !name.trim().is_empty() => Some(fold(name.trim())),
        Some(_) => return bad("\"name\" is a database's or alias's name".into()),
    };
    let level = match fields.get("level").and_then(Value::as_str) {
        None | Some("write") => "write",
        Some("read") => "read",
        Some(other) => return bad(format!("\"level\" is \"read\" or \"write\", not {other:?}")),
    };
    let endpoint = match fields.get("endpoint").and_then(Value::as_str) {
        None if name.is_some() => "database",
        None | Some("server") => "server",
        Some("database") => "database",
        Some(other) => {
            return bad(format!(
                "\"endpoint\" is \"database\" (/db/<name>/mcp) or \"server\" (/mcp), not {other:?}"
            ));
        }
    };
    let control = admin.mcp_control()?;
    let settings = control.settings();
    let (key, kind_target) = match &name {
        Some(key) => match name_kind(admin, &settings, key) {
            Some(found) => (Some(key.clone()), Some(found)),
            None => return Err(ServerError::NotFound { name: key.clone() }),
        },
        None => (None, None),
    };
    let real = kind_target
        .as_ref()
        .and_then(|(_, target)| target.clone())
        .or_else(|| key.clone());
    // The profile previewed: the body's (null: the built-in one), else the effective one.
    let (profile, source) = match fields.get("profile") {
        Some(Value::Null) => (None, "builtin"),
        Some(profile) => (Some(profile.clone()), "body"),
        None => match (endpoint, &key, &real) {
            ("database", Some(key), Some(real)) => {
                let (profile, source) = settings.effective_profile(key, real);
                (profile, source.as_str())
            }
            _ => match &settings.default_profile {
                Some(profile) => (Some(profile.clone()), "default"),
                None => (None, "builtin"),
            },
        },
    };
    let config = match &profile {
        Some(profile) => match crate::mcp_profiles::check(profile.clone()) {
            Ok((_, config)) => Some(config),
            Err(message) => return Ok(Err(invalid_profile(&message))),
        },
        None => None,
    };
    let mut notes: Vec<String> = Vec::new();
    let failed = |message: String| ServerError::Serve {
        name: "MCP preview".to_string(),
        message,
    };
    let mode_read_only = match (&key, &real) {
        (Some(key), Some(real)) if endpoint == "database" => {
            mcp_read_only_of(admin, &settings, key, real)
        }
        _ => false,
    };
    let (seen, read_only, loaded) = if endpoint == "server" {
        notes.push(
            "on /mcp every caller sees this list; a call runs under the profile of the database \
             it names and the caller's level (a tool not offered there is refused): \
             directoryBlock is what /mcp tells about a chosen database (name, level)"
                .to_string(),
        );
        (
            crate::mcp_profiles::preview_directory(config.clone()).map_err(failed)?,
            false,
            None,
        )
    } else {
        let read_only = level == "read" || mode_read_only;
        let context = real.as_deref().and_then(|real| admin.loaded_context(real));
        let seen = match &context {
            Some(context) => {
                crate::mcp_profiles::preview_database(config.clone(), context.as_ref(), read_only)
            }
            None => {
                if key.is_some() {
                    notes.push(
                        "the database is not loaded: this preview runs over an empty graph, so \
                         its saved queries are not listed as tools (load it to see them)"
                            .to_string(),
                    );
                }
                let host = crate::mcp_profiles::empty_host().map_err(failed)?;
                crate::mcp_profiles::preview_database(config.clone(), host.as_ref(), read_only)
            }
        }
        .map_err(failed)?;
        if mode_read_only && level == "write" {
            notes.push(
                "the database is read-only for MCP (a base, its read-only flag or MCP \
                 read-only): every caller gets the reading tools only"
                    .to_string(),
            );
        }
        (seen, read_only, Some(context.is_some()))
    };
    let warnings = profile
        .as_ref()
        .map(|p| crate::mcp_profiles::warnings(p, mode_read_only, endpoint == "server"))
        .unwrap_or_default();
    let mut out = seen;
    out["directoryBlock"] = match (&key, &real) {
        (Some(key), Some(real)) => {
            let previewed = match (fields.get("profile"), &profile, &config) {
                (None, _, _) => Previewed::Settings,
                (Some(_), Some(profile), Some(config)) => {
                    Previewed::Body(Some(Box::new((profile.clone(), config.clone()))))
                }
                (Some(_), _, _) => Previewed::Body(None),
            };
            json!(directory_block(
                admin,
                &settings,
                key,
                real,
                level == "read",
                endpoint == "server",
                previewed,
            )?)
        }
        _ => Value::Null,
    };
    out["endpoint"] = json!(if endpoint == "server" {
        crate::mcp::ENDPOINT.to_string()
    } else {
        format!("/db/{}/mcp", key.as_deref().unwrap_or("<name>"))
    });
    out["name"] = json!(key);
    out["level"] = json!(level);
    out["readOnly"] = json!(read_only);
    out["loaded"] = json!(loaded);
    out["source"] = json!(source);
    out["notes"] = json!(notes);
    out["warnings"] = json!(warnings);
    Ok(Ok(out))
}

/// The profile a preview previews.
enum Previewed {
    /// The one the settings give (the body has no `profile`).
    Settings,
    /// The body's (`None`: `"profile": null`, the built-in one).
    Body(Option<Box<(Value, graphersal_mcp::McpConfig)>>),
}

/// The block "Instructions for this database" ([`crate::mcp_guidance`]) that `/mcp` gives a
/// caller (`read`: one that may only read) for the name `key` (resolving to `real`), with the
/// previewed profile in place of the server default (`server_wide`) or of the name's own.
///
/// `Err`: the preview's empty graph cannot be served (a database that is not loaded).
fn directory_block(
    admin: &Admin,
    settings: &McpSettings,
    key: &str,
    real: &str,
    read: bool,
    server_wide: bool,
    previewed: Previewed,
) -> Result<String, ServerError> {
    use crate::mcp_profiles::ProfileSource;
    let parse = |profile: &Value| crate::mcp_profiles::config_of(profile).ok();
    let saved_default = settings.default_profile.as_ref().and_then(parse);
    let (effective, source) = settings.effective_profile(key, real);
    let has_own = matches!(source, ProfileSource::Own | ProfileSource::Database);
    // (the database's own profile, the configuration its calls run under, the session list's)
    let (own, config, session) = match previewed {
        Previewed::Settings => {
            let config = effective.as_ref().and_then(parse);
            (effective.filter(|_| has_own), config, saved_default)
        }
        // The name's own profile previewed (null: none of its own, the default applies).
        Previewed::Body(Some(body)) if !server_wide => {
            let (profile, config) = *body;
            (Some(profile), Some(config), saved_default)
        }
        Previewed::Body(None) if !server_wide => (None, saved_default.clone(), saved_default),
        // The server default previewed: it applies to a name without a profile of its own.
        Previewed::Body(body) => {
            let body = body.map(|body| body.1);
            if has_own {
                let config = effective.as_ref().and_then(parse);
                (effective, config, body)
            } else {
                (None, body.clone(), body)
            }
        }
    };
    let entry = admin.registry().list().into_iter().find(|e| e.name == real);
    let mode = entry.as_ref().and_then(|e| e.config.as_ref());
    let read_only = crate::mcp_guidance::ReadOnlyWhy::first(
        read,
        mode.is_some_and(|c| c.role == DatabaseRole::Base),
        settings.read_only(key, real),
    );
    let failed = |message: String| ServerError::Serve {
        name: "MCP preview".to_string(),
        message,
    };
    let context = admin.loaded_context(real);
    let host: std::sync::Arc<dyn graphersal_mcp::Host> = match context {
        Some(context) => context,
        None => crate::mcp_profiles::empty_host().map_err(failed)?,
    };
    Ok(crate::mcp_guidance::block(&crate::mcp_guidance::Facts {
        requested: key,
        real,
        own_profile: own.as_ref(),
        config: config.as_ref(),
        read_only,
        flag: mode.is_some_and(|c| c.read_only),
        session_config: session.as_ref(),
        host: host.as_ref(),
    }))
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
        (_, ["mcp", "builtin"]) if read => Ok(Response::json(200, &crate::mcp_profiles::builtin())),
        (_, ["mcp", "profile"]) if read => profile_view(admin, None).map(answer),
        (_, ["mcp", "databases", name, "profile"]) if read && !name.is_empty() => {
            profile_view(admin, Some(name)).map(answer)
        }
        (Method::Put | Method::Delete, ["mcp", "profile"])
        | (Method::Put | Method::Delete, ["mcp", "databases", _, "profile"]) => {
            let name = match parts.as_slice() {
                ["mcp", "databases", name, "profile"] if !name.is_empty() => Some(*name),
                ["mcp", "profile"] => None,
                _ => return Some(Err(HttpError::bad_request("the path names no database"))),
            };
            let (profile, revision) = if method == &Method::Put {
                let body: ProfileBody = match body_of(request) {
                    Ok(body) => body,
                    Err(err) => return Some(Err(err)),
                };
                if let Err(message) = crate::mcp_profiles::check(body.profile.clone()) {
                    return Some(Ok(invalid_profile(&message)));
                }
                (Some(body.profile), body.revision)
            } else {
                (None, None)
            };
            expecting(revision)
                .mcp_profile_set(name, profile)
                .and_then(|(name, _)| {
                    let mut out = profile_view(admin, name.as_deref())?;
                    out["changed"] = json!(true);
                    Ok(answer(out))
                })
        }
        (Method::Post, ["mcp", "preview"]) => {
            let body = match request.json_body() {
                Ok(body) => body,
                Err(err) => return Some(Err(err)),
            };
            match preview(admin, &body) {
                Ok(Ok(value)) => Ok(Response::json(200, &value)),
                Ok(Err(refused)) => return Some(Ok(refused)),
                Err(err) => Err(err),
            }
        }
        (_, ["mcp"])
        | (_, ["mcp", "databases", _])
        | (_, ["mcp", "builtin" | "profile" | "preview"])
        | (_, ["mcp", "databases", _, "profile"]) => {
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
        "defaultProfile": settings.default_profile.is_some(),
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
    // Why the database's own mode makes it read-only: a base, or its read-only flag.
    let mode_reason = |real: &str| {
        entries
            .iter()
            .find(|e| e.name == real)
            .and_then(|e| e.config.as_ref())
            .and_then(|c| {
                if c.role == DatabaseRole::Base {
                    Some("base")
                } else if c.read_only {
                    Some("flag")
                } else {
                    None
                }
            })
    };
    let row = |name: &str, kind: &str, target: Option<&str>| {
        let real = target.unwrap_or(name);
        let own = settings.of(name);
        let (sessions, calls, errors) = counts(name);
        // Read-only for MCP whatever the name's own setting says.
        let forced = mode_reason(real)
            .or_else(|| (target.is_some() && settings.of(real).read_only).then_some("database"));
        json!({
            "name": name,
            "kind": kind,
            "target": target,
            "exposed": own.exposed,
            "readOnly": own.read_only,
            "effectiveExposed": settings.exposed(name, real),
            "effectiveReadOnly": settings.read_only(name, real) || forced.is_some(),
            "readOnlyReason": forced,
            "profile": settings.effective_profile(name, real).1.as_str(),
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
