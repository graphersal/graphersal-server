//! The databases under `/db/<name>/`: which loaded database a request goes to.
//!
//! The router ([`crate::router`]) parses `/db/<name>/<rest>`, resolves `<name>` through the
//! registry on EVERY request ([`Registry::resolve`]: an alias flip takes effect at once), and
//! answers `404` for a name that is neither a database nor an alias and `409` for a database
//! that is not loaded ([`not_loaded`]; never an implicit load). Otherwise it forwards the
//! request with the prefix stripped ([`forward`]: `/db/<name>/<rest>` becomes `/<rest>`) and
//! the database's real name in [`GRAPH_HEADER`] (a client's own copy of that header is dropped)
//! to `graphersal_serve::Server::handle`, whose graph resolver [`Databases`] picks the loaded
//! context by that header. The request holds the context (`Arc<Context>`) for its duration and
//! nothing longer: [`crate::Loader::unload`] waits for it.
//!
//! The client id (SEC M3): graphersal-serve keys a page's runs (`POST /api/cancel`) and names
//! its changes (`GET /api/changes`, `recent[].by`) by the client's `X-Graphersal-Client` header
//! TOGETHER with the request's identity, which the router sets on every forwarded request
//! (`Request::set_identity`): two principals sending the same client id get two run keys, and
//! the change feed shows another identity's change as `"another identity"` without its client
//! id or detail (graphersal-serve 0.9.0). The header is therefore forwarded as the client sent
//! it; the server keeps no rewrite of its own.

use std::sync::Arc;

use graphersal_serve::{Context, GraphResolver, Hooks, HttpError, Request, Response};
use serde_json::{Value, json};

use crate::loader::Loader;
use crate::registry::{DatabaseEntry, DbState};

/// The header that names the database of a forwarded request (set by the router only).
pub(crate) const GRAPH_HEADER: &str = "X-Graphersal-Graph";

/// The headers of `request` to forward: every header but the client's [`GRAPH_HEADER`], plus
/// the router's [`GRAPH_HEADER`] naming `real`.
fn forwarded_headers(request: &Request<'_>, real: &str) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = request
        .headers()
        .iter()
        .filter(|(name, _)| !name.eq_ignore_ascii_case(GRAPH_HEADER))
        .cloned()
        .collect();
    headers.push((GRAPH_HEADER.to_string(), real.to_string()));
    headers
}

/// The graph resolver of the server: the loaded context named by [`GRAPH_HEADER`].
pub(crate) struct Databases {
    pub(crate) loader: Arc<Loader>,
}

impl GraphResolver for Databases {
    fn resolve(&self, request: &Request<'_>) -> Result<Arc<Context>, HttpError> {
        let Some(name) = request.header(GRAPH_HEADER) else {
            return Err(HttpError::new(404, "not found: no database named"));
        };
        // The router saw the context a moment ago; an unload in between gets the 409 text.
        self.loader.context(name).ok_or_else(|| {
            let (error, help) = match self.loader.registry().get(name) {
                Ok(entry) => not_loaded_text(&entry),
                Err(_) => (format!("database '{name}' is not loaded"), String::new()),
            };
            HttpError::new(409, format!("{error}; {help}"))
        })
    }
}

/// The host's additions to a database's `GET /db/<name>/api/info`: `database` (its real name,
/// shown in the playground's header) and `home: "/"` (the server's own UI, the `server` profile
/// at the root: the playground links the database name back to it).
pub(crate) struct DatabaseInfo {
    /// The database's real name.
    pub(crate) name: String,
}

impl Hooks for DatabaseInfo {
    fn add_info(&self, _context: &Context, info: &mut Value) {
        info["database"] = Value::String(self.name.clone());
        info["home"] = Value::String("/".to_string());
    }
}

/// `/db/<name>` or `/db/<name>/<rest>` split into the name and the rest (`None` without the
/// slash after the name); `None` for any other path or an empty name.
pub(crate) fn split_db_path(path: &str) -> Option<(&str, Option<&str>)> {
    let tail = path.strip_prefix("/db/")?;
    let (name, rest) = match tail.split_once('/') {
        Some((name, rest)) => (name, Some(rest)),
        None => (tail, None),
    };
    (!name.is_empty()).then_some((name, rest))
}

/// The request to forward to the database `real`: `url` (the request target without the
/// `/db/<name>` prefix), the same method, headers and body, minus the client's
/// [`GRAPH_HEADER`], plus the router's (the router sets the principal's identity on it: module
/// docs).
pub(crate) fn forward<'b>(request: &'b mut Request<'_>, url: &str, real: &str) -> Request<'b> {
    let method = request.method().clone();
    let length = request.body_length();
    let headers = forwarded_headers(request, real);
    let mut forwarded = Request::new(method, url, request.body()).with_body_length(length);
    for (name, value) in &headers {
        forwarded = forwarded.with_header(name, value);
    }
    forwarded
}

/// [`forward`] with the body already read into `body` (the router checked it first): the same
/// method and headers, the length of `body`.
pub(crate) fn forward_with_body<'b>(
    request: &Request<'_>,
    url: &str,
    real: &str,
    body: &'b [u8],
) -> Request<'b> {
    let mut forwarded =
        Request::new(request.method().clone(), url, body).with_body_length(Some(body.len()));
    for (name, value) in &forwarded_headers(request, real) {
        forwarded = forwarded.with_header(name, value);
    }
    forwarded
}

/// The error text and the help of a request to the database `entry` that is not loaded,
/// naming its state.
pub(crate) fn not_loaded_text(entry: &DatabaseEntry) -> (String, String) {
    let name = &entry.name;
    let load = format!("POST /api/admin/databases/{name}/load");
    match &entry.state {
        DbState::Loading => (
            format!("database '{name}' is not loaded: it is loading"),
            format!("retry in a moment; GET /api/admin/databases/{name} shows its state"),
        ),
        DbState::Failed { reason } => (
            format!("database '{name}' is not loaded: it failed ({reason})"),
            format!("fix the reason, then load it: {load}"),
        ),
        DbState::NeedsModel { repo } => (
            format!("database '{name}' is not loaded: it needs the embedding model '{repo}'"),
            format!("add the model '{repo}' to the server, then load it: {load}"),
        ),
        DbState::Unloaded | DbState::Loaded | DbState::Maintenance { .. } => (
            format!("database '{name}' is not loaded"),
            format!("load it: {load}"),
        ),
    }
}

/// `409` for a request to the database `entry` that is not loaded: `{"error", "help",
/// "state"}`; while it loads also `Retry-After: 1`.
pub(crate) fn not_loaded(entry: &DatabaseEntry) -> Response {
    let (error, help) = not_loaded_text(entry);
    let response = Response::json(
        409,
        &json!({ "error": error, "help": help, "state": entry.state.as_str() }),
    );
    if entry.state == DbState::Loading {
        response.with_header("Retry-After", "1")
    } else {
        response
    }
}

/// `404` for a request to `/db/<name>/` whose name is neither a database nor an alias.
pub(crate) fn unknown_database(name: &str) -> Response {
    Response::json(
        404,
        &json!({
            "error": format!("no database or alias '{name}'"),
            "help": "GET /api/admin/databases lists the databases and aliases",
        }),
    )
}

/// `308` from `/db/<name>` to `/db/<name>/` (the playground resolves `api/` relative to its
/// page), the query string kept.
pub(crate) fn redirect_to_slash(path: &str, query: Option<&str>) -> Response {
    let location = match query {
        Some(query) => format!("{path}/?{query}"),
        None => format!("{path}/"),
    };
    Response::new(308, Vec::new())
        .with_header("Location", &location)
        .with_header("Cache-Control", "no-store")
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphersal_serve::Method;

    #[test]
    fn db_paths_split_into_name_and_rest() {
        assert_eq!(
            split_db_path("/db/a/api/info"),
            Some(("a", Some("api/info")))
        );
        assert_eq!(split_db_path("/db/a/"), Some(("a", Some(""))));
        assert_eq!(split_db_path("/db/a"), Some(("a", None)));
        assert_eq!(split_db_path("/db/"), None);
        assert_eq!(split_db_path("/db"), None);
        assert_eq!(split_db_path("/dba/x"), None);
        assert_eq!(split_db_path("/api/info"), None);
    }

    #[test]
    fn a_forwarded_request_names_the_database_and_keeps_its_body() {
        let body = b"{\"script\": \"1\"}".to_vec();
        let mut request = Request::new(Method::Post, "/db/x/api/execute", &body[..])
            .with_body_length(Some(body.len()))
            .with_header("Host", "127.0.0.1:8484")
            .with_header("x-graphersal-graph", "other")
            .with_header("Content-Type", "application/json");
        let mut forwarded = forward(&mut request, "/api/execute?a=1", "real");
        assert_eq!(forwarded.url(), "/api/execute?a=1");
        assert_eq!(forwarded.method(), &Method::Post);
        assert_eq!(forwarded.header(GRAPH_HEADER), Some("real"));
        assert_eq!(
            forwarded
                .headers()
                .iter()
                .filter(|(n, _)| n.eq_ignore_ascii_case(GRAPH_HEADER))
                .count(),
            1
        );
        assert_eq!(forwarded.header("Host"), Some("127.0.0.1:8484"));
        assert_eq!(forwarded.body_length(), Some(body.len()));
        let mut read = Vec::new();
        forwarded.body().read_to_end(&mut read).unwrap();
        assert_eq!(read, body);
    }

    #[test]
    fn not_loaded_texts_name_the_state() {
        let entry = |state: DbState| DatabaseEntry {
            name: "x".to_string(),
            dir: std::path::PathBuf::from("x"),
            config: None,
            state,
        };
        assert_eq!(
            not_loaded_text(&entry(DbState::Unloaded)),
            (
                "database 'x' is not loaded".to_string(),
                "load it: POST /api/admin/databases/x/load".to_string()
            )
        );
        assert!(
            not_loaded_text(&entry(DbState::Loading))
                .0
                .ends_with("it is loading")
        );
        assert!(
            not_loaded_text(&entry(DbState::failed("bad store")))
                .0
                .contains("failed (bad store)")
        );
        assert!(
            not_loaded_text(&entry(DbState::needs_model("org/model")))
                .0
                .contains("'org/model'")
        );
        let loading = not_loaded(&entry(DbState::Loading));
        assert_eq!(loading.status(), 409);
        assert!(loading.headers().iter().any(|(n, _)| n == "Retry-After"));
    }

    #[test]
    fn a_short_db_path_redirects_with_its_query() {
        let answer = redirect_to_slash("/db/a", Some("x=1"));
        assert_eq!(answer.status(), 308);
        assert!(
            answer
                .headers()
                .iter()
                .any(|(n, v)| n == "Location" && v == "/db/a/?x=1")
        );
    }
}
