//! The OpenAPI descriptions the server hands to HTTP clients (round 3 B-OPENAPI; the repository's
//! `docs/http-clients.md`):
//!
//! - `GET /openapi.json`: the server's own routes (the probes, login/logout, `/api/info`,
//!   `/api/databases`, the whole admin API), the hand-written `openapi/server.json` with
//!   `info.version` = this crate's version and `servers` = the URL the request came to. Public
//!   (no credentials: it carries no data). A test walks the route tables of the router and the
//!   admin API, so a route cannot stay undescribed.
//! - `GET /db/<name>/openapi.json` (also `/db/<name>/api/openapi.json`): one database's own HTTP
//!   API, built from the engine's description (graphersal-serve's `openapi.json`, taken from
//!   the pinned crate by asking a throwaway context once: [`engine_spec`]), patched with
//!   `openapi/engine-patch.json` (what graphersal-serve's description gained after the pinned
//!   version: the `409` of a request marked read-only on every writing route, the file root's
//!   `403`/`500`) and then for this server ([`database_spec`]): `servers` = the request's URL
//!   followed by `/db/<name>` (an alias's name stays the alias's), the security schemes `bearerAuth` and
//!   `sessionCookie` (plus `{}` when the name is open to anonymous: an anonymous grant, or
//!   access control off), the access layer's answers on every operation (`401` anonymous, `404`
//!   hidden or unknown, `409` not loaded, a read grant on a writing route), and
//!   `x-graphersal-access` (`read` | `query` | `write`) on every operation. The routes the
//!   server does not serve below a mount are left out (the AI Chat, the engine's run
//!   administration, the probes: the server's own are in its description). Every principal
//!   that reaches the name gets the SAME document (no filtering by level: a reader sees the
//!   writing operations, marked `write`); it is answered also while the database is not loaded.
//!
//! The base URL is `<scheme>://<Host>` of the request (`server.toml [http] public_scheme`; the
//! `Host` passed the Host check, so it is the listen address's or an `[http] allowed_hosts`
//! name). Offline (`graphersal-server openapi`), [`offline_base_url`]: the `--server` URL, else
//! the first `allowed_hosts` name, else the listen address.

use std::sync::OnceLock;

use serde_json::{Map, Value, json};

use crate::config::{PublicScheme, ServerConfig};
use crate::error::ServerError;

/// The path of the server's own description.
pub const SERVER_SPEC_PATH: &str = "/openapi.json";

/// The path template of a database's description (`{name}`: a database's name or an alias).
pub const DATABASE_SPEC_TEMPLATE: &str = "/db/{name}/openapi.json";

/// The paths below a mount that answer the database's description (relative to `/db/<name>/`).
pub(crate) const DATABASE_SPEC_PATHS: [&str; 2] = ["openapi.json", "api/openapi.json"];

/// The hand-written description of the server's own routes.
const SERVER_SPEC: &str = include_str!("../openapi/server.json");

/// What graphersal-serve's description gained after the pinned version (module docs).
const ENGINE_PATCH: &str = include_str!("../openapi/engine-patch.json");

/// The paths of the engine's description the server does not serve below `/db/<name>/`: the
/// probes (`/healthz`, `/readyz`: the server's own, in its description), the engine's run
/// administration (off on every mount: the admin API's runs instead) and the AI Chat (a dev
/// server's; the server has none). Prefixes end with `/`.
const NOT_MOUNTED: [&str; 5] = [
    "/healthz",
    "/readyz",
    "/api/admin/runs",
    "/api/admin/runs/cancel",
    "/api/ai/",
];

/// The engine routes that run a script or a saved query: on a read grant they run under the
/// reading authorizer (a write fails and changes nothing).
const QUERY_OPERATIONS: [(&str, &str); 2] = [("post", "/api/execute"), ("post", "/api/query")];

/// The HTTP methods of an OpenAPI path item.
const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// The server's own description with `servers` = `base_url` (no trailing `/`).
pub fn server_spec(base_url: &str) -> Result<Value, ServerError> {
    let mut spec: Value =
        serde_json::from_str(SERVER_SPEC).map_err(|err| invalid("server", err))?;
    spec["info"]["version"] = json!(env!("CARGO_PKG_VERSION"));
    spec["servers"] = json!([{
        "url": trim_base(base_url),
        "description": "this graphersal-server",
    }]);
    Ok(spec)
}

/// The engine's description of one database's HTTP API (graphersal-serve's `openapi.json` of
/// the pinned crate, asked once from a throwaway context over an empty graph and kept), with
/// `openapi/engine-patch.json` applied.
pub fn engine_spec() -> Result<Value, ServerError> {
    static ENGINE: OnceLock<Result<Value, String>> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let raw = served_engine_spec()?;
            apply_engine_patch(raw)
        })
        .clone()
        .map_err(|message| ServerError::Serve {
            name: "openapi.json".to_string(),
            message,
        })
}

/// graphersal-serve's own `GET /openapi.json` of a context over the empty sample graph.
pub(crate) fn served_engine_spec() -> Result<Value, String> {
    let (launched, ()) = graphersal_serve::launch(
        graphersal_serve::ServedSource::Sample("empty".to_string()),
        graphersal_session::SessionPresets::default(),
        |_| Ok::<(), String>(()),
        None,
    )
    .map_err(|err| format!("the engine's API description cannot be built: {err}"))?;
    let check = graphersal_serve::HostCheck::new(
        graphersal_serve::Exposure::Loopback,
        0,
        "http://127.0.0.1:0/",
    );
    let context = launched.context_builder(check).build();
    let request = graphersal_serve::Request::new(
        graphersal_serve::Method::Get,
        "/openapi.json",
        std::io::empty(),
    )
    .with_header("Host", "127.0.0.1:0");
    let answer = graphersal_serve::handle(&context, request);
    if answer.status() != 200 {
        return Err(format!(
            "the engine answered {} for its API description",
            answer.status()
        ));
    }
    serde_json::from_slice(answer.body())
        .map_err(|err| format!("the engine's API description is not JSON: {err}"))
}

/// `raw` (the engine's description) with `openapi/engine-patch.json` applied: its responses
/// added to `components.responses`, its operations replacing the engine's.
pub(crate) fn apply_engine_patch(mut raw: Value) -> Result<Value, String> {
    let patch: Value =
        serde_json::from_str(ENGINE_PATCH).map_err(|err| format!("engine-patch.json: {err}"))?;
    if let Some(responses) = patch["responses"].as_object() {
        let target = raw["components"]["responses"]
            .as_object_mut()
            .ok_or("the engine's description has no components.responses")?;
        for (name, response) in responses {
            target.insert(name.clone(), response.clone());
        }
    }
    if let Some(operations) = patch["operations"].as_object() {
        for (key, operation) in operations {
            let (method, path) = key
                .split_once(' ')
                .ok_or_else(|| format!("engine-patch.json: bad operation key {key:?}"))?;
            let slot = raw["paths"][path]
                .get_mut(method.to_ascii_lowercase())
                .ok_or_else(|| format!("engine-patch.json: the engine has no {key}"))?;
            *slot = operation.clone();
        }
    }
    Ok(raw)
}

/// The description of the database (or alias) `name` for clients of the server at `base_url`
/// (module docs). `anonymous`: the name is open without credentials (an anonymous grant
/// reaching it, or access control off): the security alternatives then include `{}`.
pub fn database_spec(engine: &Value, base_url: &str, name: &str, anonymous: bool) -> Value {
    let mut spec = engine.clone();
    let engine_version = spec["info"]["version"].clone();
    let conventions = spec["info"]["description"]
        .as_str()
        .and_then(|text| text.split_once("Conventions:"))
        .map(|(_, rest)| format!("\n\nConventions of the engine's API:{rest}"))
        .unwrap_or_default();
    let anonymous_text = if anonymous {
        " This database is open to anonymous requests too (an anonymous grant, or access \
         control off): the security alternatives include none."
    } else {
        ""
    };
    spec["info"] = json!({
        "title": format!("graphersal-server database '{name}' (EXPERIMENTAL)"),
        "version": env!("CARGO_PKG_VERSION"),
        "summary": format!("The HTTP API of the database '{name}' on a graphersal-server: \
                            EXPERIMENTAL, may change in any release."),
        "description": format!(
            "**EXPERIMENTAL.** The HTTP API of the database `{name}` served by graphersal-server \
             below `/db/{name}/` (the engine's API, graphersal-serve {engine_version}; `info.version` \
             is the server's). Every path below is relative to the server URL in `servers`.\n\n\
             **Security.** Send a user token as `Authorization: Bearer <64 hex digits>` \
             (`bearerAuth`), or the browser session cookie `gs_session` of `POST /api/login` \
             (`sessionCookie`). The access layer answers before the database: anonymous \
             without a grant gets `401` for every name; a caller without a grant on the name \
             gets `404` as if the database did not exist; a database that is not loaded \
             answers `409`. A read grant reaches every operation, but the writing ones \
             (`x-graphersal-access: write`) answer `409` and the queries \
             (`x-graphersal-access: query`) run under the reading authorizer. Wrong \
             credentials are `401`, never anonymous.{anonymous_text}\n\n\
             The server's own routes (login, the caller's databases, the admin API) are \
             described at `/openapi.json` of the server.{conventions}",
            engine_version = engine_version.as_str().unwrap_or("?"),
        ),
        "license": {"name": "Apache-2.0", "identifier": "Apache-2.0"},
        "x-experimental": true,
        "x-graphersal-database": name,
        "x-graphersal-engine-version": engine_version,
    });
    spec["servers"] = json!([{
        "url": format!("{}/db/{name}", trim_base(base_url)),
        "description": format!("the database '{name}' on this graphersal-server"),
    }]);
    let mut security = vec![json!({"bearerAuth": []}), json!({"sessionCookie": []})];
    if anonymous {
        security.push(json!({}));
    }
    spec["security"] = Value::Array(security);
    if !spec["components"].is_object() {
        spec["components"] = json!({});
    }
    if !spec["components"]["responses"].is_object() {
        spec["components"]["responses"] = json!({});
    }
    spec["components"]["securitySchemes"] = security_schemes();
    if let Some(responses) = spec["components"]["responses"].as_object_mut() {
        for (key, response) in access_responses() {
            responses.insert(key.to_string(), response);
        }
    }
    let shared = spec["components"]["responses"].clone();
    if let Some(paths) = spec["paths"].as_object_mut() {
        paths.retain(|path, _| !is_not_mounted(path));
        for (path, item) in paths.iter_mut() {
            let Some(item) = item.as_object_mut() else {
                continue;
            };
            for (method, operation) in item.iter_mut() {
                if !METHODS.contains(&method.as_str()) {
                    continue;
                }
                patch_operation(operation, method, path, &shared);
            }
        }
    }
    spec
}

/// One operation of a database's description, patched (module docs).
fn patch_operation(operation: &mut Value, method: &str, path: &str, shared: &Value) {
    let access = access_class(operation, method, path);
    let Some(op) = operation.as_object_mut() else {
        return;
    };
    op.insert("x-graphersal-access".to_string(), json!(access));
    let note = match access {
        "write" => " **Writes:** needs a write grant; a read grant answers `409`.",
        "query" => {
            " **Query:** a read grant runs it under the reading authorizer (a write fails and \
             changes nothing); a write grant may write."
        }
        _ => " Needs a read grant.",
    };
    let description = op
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    op.insert(
        "description".to_string(),
        json!(format!("{description}{note}")),
    );
    let spec_route = path == "/openapi.json" || path == "/api/openapi.json";
    let responses = op
        .entry("responses")
        .or_insert_with(|| json!({}))
        .as_object_mut();
    let Some(responses) = responses else {
        return;
    };
    merge_response(responses, "401", "AccessUnauthorized", shared);
    merge_response(responses, "404", "AccessNotFound", shared);
    if !spec_route {
        let conflict = if access == "write" {
            "AccessConflictWrite"
        } else {
            "AccessConflict"
        };
        merge_response(responses, "409", conflict, shared);
    }
    if path == "/mcp" {
        merge_response(responses, "503", "McpOff", shared);
        let text = op
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        op.insert(
            "description".to_string(),
            json!(format!(
                "{text} On graphersal-server: `404` when `server.toml [mcp] enabled = false` or \
                 `mcp.toml` hides this name from MCP, `503` while `mcp.toml` switches MCP off; \
                 a name MCP serves read-only is marked read-only whatever the grant."
            )),
        );
    }
}

/// `read`, `query` or `write`: a writing route is one whose `409` is the engine's refusal of a
/// request marked read-only (`E409ReadOnlyRequest`, `E409Write`); a query runs a script.
fn access_class(operation: &Value, method: &str, path: &str) -> &'static str {
    if QUERY_OPERATIONS.contains(&(method, path)) {
        return "query";
    }
    let conflict = operation["responses"]["409"]["$ref"]
        .as_str()
        .unwrap_or_default();
    if conflict.ends_with("/E409ReadOnlyRequest") || conflict.ends_with("/E409Write") {
        "write"
    } else {
        "read"
    }
}

/// Puts the shared response `name` at `code`; an answer the engine already describes at that
/// code is kept beside it in one description (both can happen).
fn merge_response(responses: &mut Map<String, Value>, code: &str, name: &str, shared: &Value) {
    let ours = json!({ "$ref": format!("#/components/responses/{name}") });
    let Some(existing) = responses.get(code) else {
        responses.insert(code.to_string(), ours);
        return;
    };
    let theirs = describe(existing, shared);
    let mine = describe(&ours, shared);
    responses.insert(
        code.to_string(),
        json!({
            "description": format!("{mine} Or, from the database's engine: {theirs}"),
            "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Error"}}},
        }),
    );
}

/// The description of a response object (a `$ref` into `components.responses` resolved).
fn describe(response: &Value, shared: &Value) -> String {
    let resolved = match response["$ref"].as_str() {
        Some(target) => target
            .strip_prefix("#/components/responses/")
            .map_or(&Value::Null, |name| &shared[name]),
        None => response,
    };
    resolved["description"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The security schemes of both descriptions (the server's own carries the same).
fn security_schemes() -> Value {
    json!({
        "bearerAuth": {
            "type": "http",
            "scheme": "bearer",
            "description": "A user token: 64 hex digits (256 random bits), shown once by \
                            `graphersal-server user add` / `user token` or the admin API. The \
                            console's operator token (`server.run`) works the same.",
        },
        "sessionCookie": {
            "type": "apiKey",
            "in": "cookie",
            "name": crate::access::SESSION_COOKIE,
            "description": "The browser session cookie `POST /api/login` sets (HttpOnly, \
                            SameSite=Strict).",
        },
    })
}

/// The access layer's answers added to every operation of a database's description.
fn access_responses() -> [(&'static str, Value); 5] {
    let error = |description: &str| {
        json!({
            "description": description,
            "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Error"}}},
        })
    };
    [
        (
            "AccessUnauthorized",
            json!({
                "description": "graphersal-server: no credentials and no anonymous grant on \
                                this name (`401` for EVERY name, so nothing about existence \
                                leaks; `{\"error\", \"login\": \"/\"}` with `WWW-Authenticate: \
                                Bearer`), or a token nobody holds, or an ended session cookie.",
                "headers": {"WWW-Authenticate": {"schema": {"type": "string"}}},
                "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Error"}}},
            }),
        ),
        (
            "AccessNotFound",
            error(
                "graphersal-server: no database or alias of this name, or one the caller has \
                 no grant on (it does not exist for the caller).",
            ),
        ),
        (
            "AccessConflict",
            error(
                "graphersal-server: the database is not loaded (an admin loads it: `POST \
                 /api/admin/databases/<name>/load`).",
            ),
        ),
        (
            "AccessConflictWrite",
            error(
                "graphersal-server: the database is not loaded, or the caller has only a read \
                 grant (`the graph is served read-only: writes are refused`; ask for write \
                 access), or the database is read-only.",
            ),
        ),
        (
            "McpOff",
            error("graphersal-server: `mcp.toml` switches MCP off (`Retry-After` absent)."),
        ),
    ]
}

/// Whether the engine's `path` is one the server does not serve below a mount.
fn is_not_mounted(path: &str) -> bool {
    NOT_MOUNTED.iter().any(|not| {
        if not.ends_with('/') {
            path.starts_with(not)
        } else {
            path == *not
        }
    })
}

/// `base` without trailing `/`.
fn trim_base(base: &str) -> &str {
    base.trim_end_matches('/')
}

/// The scheme clients use (`[http] public_scheme`).
pub(crate) fn scheme(public: PublicScheme) -> &'static str {
    match public {
        PublicScheme::Https => "https",
        PublicScheme::Http => "http",
    }
}

/// The base URL of a request: `<scheme>://<Host>`; `None` without a `Host` (the Host check
/// refuses that before).
pub(crate) fn request_base_url(https: bool, host: Option<&str>) -> Option<String> {
    let host = host?.trim();
    if host.is_empty() {
        return None;
    }
    Some(format!("{}://{host}", if https { "https" } else { "http" }))
}

/// The base URL `graphersal-server openapi` writes (module docs): `server` (a URL, or a bare
/// `host[:port]` that gets the public scheme), else the first `[http] allowed_hosts` name,
/// else the listen address (`0.0.0.0` named as `127.0.0.1`).
pub fn offline_base_url(config: &ServerConfig, server: Option<&str>) -> String {
    let scheme = scheme(config.http.public_scheme);
    if let Some(server) = server.map(str::trim).filter(|s| !s.is_empty()) {
        if server.contains("://") {
            return trim_base(server).to_string();
        }
        return format!("{scheme}://{}", trim_base(server));
    }
    if let Some(host) = config.http.allowed_hosts.first() {
        return format!("{scheme}://{host}");
    }
    let listen = config.listen.trim();
    let listen = listen
        .strip_prefix("0.0.0.0:")
        .map(|port| format!("127.0.0.1:{port}"))
        .or_else(|| {
            listen
                .strip_prefix("[::]:")
                .map(|port| format!("[::1]:{port}"))
        })
        .unwrap_or_else(|| listen.to_string());
    format!("{scheme}://{listen}")
}

/// The description `graphersal-server openapi --dir DIR [--db NAME] [--server URL]` writes,
/// read from the directory without a running server: the server's, or the database (or alias)
/// `db`'s (unknown name: [`ServerError::NotFound`]), with the base URL of
/// [`offline_base_url`].
pub fn offline_spec(
    dir: &crate::layout::ServerDir,
    db: Option<&str>,
    server: Option<&str>,
) -> Result<Value, ServerError> {
    let config = dir.load_config()?;
    let base = offline_base_url(&config, server);
    let Some(name) = db else {
        return server_spec(&base);
    };
    let registry = crate::registry::Registry::peek(dir)?;
    let entry = registry.resolve(name)?;
    let name = crate::names::fold(name);
    let anonymous = !config.access.enabled
        || crate::access::AccessFile::read(dir)?.is_some_and(|file| {
            file.granted_level_via(&crate::access::Principal::anonymous(), &name, &entry.name)
                != crate::access::Level::None
        });
    Ok(database_spec(&engine_spec()?, &base, &name, anonymous))
}

/// The error of a hand-written description that does not parse (a build bug; the tests catch
/// it first).
fn invalid(which: &str, err: serde_json::Error) -> ServerError {
    ServerError::Serve {
        name: format!("openapi/{which}.json"),
        message: format!("the {which} API description does not parse: {err}"),
    }
}

#[cfg(test)]
#[path = "openapi_tests.rs"]
pub(crate) mod tests;
