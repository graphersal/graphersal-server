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
//! its changes (`GET /api/changes`, `recent[].by`) by the `X-Graphersal-Client` header, which
//! the client chooses. A forwarded request never carries the client's own header: [`forward`]
//! replaces it with [`CLIENT_HEADER`] `<tag>-<client id>`, the tag being a keyed hash of the
//! principal's identity ([`client_tag`]: HMAC-SHA256 with a key made at start, never the
//! identity itself; anonymous has a tag of its own). Two principals sending the same client id
//! therefore get two run keys, and one cannot cancel the other's queries. The change feed is
//! answered as the reader may see it ([`own_changes`]): the reader's own changes with its
//! client ids as it sent them, every change of another identity as `"another identity"`
//! without its detail.

use std::sync::Arc;

use graphersal_serve::{Context, GraphResolver, Hooks, HttpError, Request, Response};
use serde_json::{Value, json};

use crate::loader::Loader;
use crate::registry::{DatabaseEntry, DbState};

/// The header that names the database of a forwarded request (set by the router only).
pub(crate) const GRAPH_HEADER: &str = "X-Graphersal-Graph";

/// The header of a page's client id (graphersal-serve's run key and change feed name it);
/// rewritten on every forwarded request ([`forwarded_client`]).
pub(crate) const CLIENT_HEADER: &str = "X-Graphersal-Client";

/// The `by` of a change made under another identity than the reader's ([`own_changes`]): it
/// cannot be a client id (those keep letters, digits, `-` and `_` only), so no page takes it
/// for its own.
pub(crate) const OTHER_IDENTITY: &str = "another identity";

/// The hex digits of a client tag ([`client_tag`]).
const TAG_HEX: usize = 12;

/// The longest client id graphersal-serve keeps (letters, digits, `-`, `_`).
const MAX_CLIENT_ID: usize = 64;

/// The key of [`client_tag`]: 256 random bits made once per process.
fn client_key() -> &'static ring::hmac::Key {
    static KEY: std::sync::OnceLock<ring::hmac::Key> = std::sync::OnceLock::new();
    KEY.get_or_init(|| {
        let rng = ring::rand::SystemRandom::new();
        ring::hmac::Key::generate(ring::hmac::HMAC_SHA256, &rng).unwrap_or_else(|_| {
            // The OS generator failed: a key from what this process knows, still never the
            // identity itself (the tags only have to differ between identities).
            tracing::warn!(
                target: "graphersal_server",
                "the random generator failed: the client tags use a weaker key"
            );
            let seed = format!(
                "{}-{:?}-{:p}",
                std::process::id(),
                std::time::SystemTime::now(),
                &rng
            );
            ring::hmac::Key::new(ring::hmac::HMAC_SHA256, seed.as_bytes())
        })
    })
}

/// The tag of the principal `identity` (`None`: anonymous) in forwarded client ids:
/// [`TAG_HEX`] hex digits of HMAC-SHA256 under the process's key, so it tells identities apart
/// without naming them.
pub(crate) fn client_tag(identity: Option<&str>) -> String {
    let message = match identity {
        Some(identity) => format!("identity:{identity}"),
        None => "anonymous".to_string(),
    };
    let tag = ring::hmac::sign(client_key(), message.as_bytes());
    tag.as_ref()
        .iter()
        .take(TAG_HEX / 2)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The client id graphersal-serve reads from `raw` (letters, digits, `-` and `_`, at most
/// [`MAX_CLIENT_ID`]).
fn clean_client_id(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(MAX_CLIENT_ID)
        .collect()
}

/// The client id a request of `identity` with the client's `X-Graphersal-Client` `raw` is
/// forwarded with: `<tag>` alone without a client id, else `<tag>-<client id>` (the client id
/// shortened so the whole stays within [`MAX_CLIENT_ID`]).
pub(crate) fn forwarded_client(identity: Option<&str>, raw: Option<&str>) -> String {
    let tag = client_tag(identity);
    let id = clean_client_id(raw.unwrap_or_default());
    if id.is_empty() {
        return tag;
    }
    let room = MAX_CLIENT_ID - TAG_HEX - 1;
    format!("{tag}-{}", &id[..id.len().min(room)])
}

/// The headers of `request` to forward: every header but the client's [`GRAPH_HEADER`] and
/// [`CLIENT_HEADER`], plus the rewritten client id ([`forwarded_client`]) and the router's
/// [`GRAPH_HEADER`] naming `real`.
fn forwarded_headers(request: &Request<'_>, real: &str) -> Vec<(String, String)> {
    let client = forwarded_client(request.identity(), request.header(CLIENT_HEADER));
    let mut headers: Vec<(String, String)> = request
        .headers()
        .iter()
        .filter(|(name, _)| {
            !name.eq_ignore_ascii_case(GRAPH_HEADER) && !name.eq_ignore_ascii_case(CLIENT_HEADER)
        })
        .cloned()
        .collect();
    headers.push((CLIENT_HEADER.to_string(), client));
    headers.push((GRAPH_HEADER.to_string(), real.to_string()));
    headers
}

/// `answer` (the database's `GET /api/changes`) as the reader `identity` may see it (module
/// docs): its own changes name its client ids as it sent them, a change of another identity is
/// `by: "another identity"` without `detail`; a change without a page client (`mcp`, or the
/// server's own, empty) is kept. An answer that is not a JSON object is passed on unchanged.
pub(crate) fn own_changes(answer: Response, identity: Option<&str>) -> Response {
    let Ok(mut body) = serde_json::from_slice::<Value>(answer.body()) else {
        return answer;
    };
    let Some(recent) = body.get_mut("recent").and_then(Value::as_array_mut) else {
        return answer;
    };
    let tag = client_tag(identity);
    let own_prefix = format!("{tag}-");
    for change in recent.iter_mut() {
        let Some(by) = change.get("by").and_then(Value::as_str).map(str::to_string) else {
            continue;
        };
        let shown = if by == tag {
            String::new()
        } else if let Some(id) = by.strip_prefix(&own_prefix) {
            id.to_string()
        } else if by.is_empty() || by == "mcp" {
            continue;
        } else {
            if let Some(change) = change.as_object_mut() {
                change.remove("detail");
            }
            OTHER_IDENTITY.to_string()
        };
        change["by"] = Value::String(shown);
    }
    let mut rebuilt = Response::new(answer.status(), body.to_string().into_bytes());
    for (name, value) in answer.headers() {
        if !name.eq_ignore_ascii_case("Content-Length") {
            rebuilt = rebuilt.with_header(name, value);
        }
    }
    rebuilt
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
/// [`GRAPH_HEADER`] and [`CLIENT_HEADER`], plus the router's and the rewritten client id
/// (module docs; the request carries the principal's identity already).
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

    /// SEC M3: the client's `X-Graphersal-Client` never reaches the database; the forwarded id
    /// carries a tag of the identity (never the identity itself), so two principals with the
    /// same client id get different ids.
    #[test]
    fn the_client_id_is_rewritten_per_identity() {
        let forwarded_of = |identity: Option<&str>, raw: Option<&str>| {
            let mut request = Request::new(Method::Get, "/db/x/api/changes", std::io::empty());
            if let Some(raw) = raw {
                request = request
                    .with_header("x-graphersal-client", raw)
                    .with_header(CLIENT_HEADER, "second-copy");
            }
            if let Some(identity) = identity {
                request.set_identity(identity);
            }
            let forwarded = forward(&mut request, "/api/changes", "x");
            let clients: Vec<String> = forwarded
                .headers()
                .iter()
                .filter(|(n, _)| n.eq_ignore_ascii_case(CLIENT_HEADER))
                .map(|(_, v)| v.clone())
                .collect();
            assert_eq!(clients.len(), 1, "{clients:?}");
            clients[0].clone()
        };
        let victim = forwarded_of(Some("operator"), Some("victim-page-1"));
        let bob = forwarded_of(Some("user:bob"), Some("victim-page-1"));
        let anonymous = forwarded_of(None, Some("victim-page-1"));
        assert_ne!(victim, bob);
        assert_ne!(victim, anonymous);
        assert_ne!(bob, anonymous);
        assert!(victim.ends_with("-victim-page-1"), "{victim}");
        assert!(
            !victim.contains("operator") && !bob.contains("bob"),
            "{bob}"
        );
        // Stable per identity; no header: the tag alone (curl clients no longer share a key).
        assert_eq!(
            victim,
            forwarded_of(Some("operator"), Some("victim-page-1"))
        );
        assert_eq!(
            forwarded_of(Some("user:bob"), None),
            client_tag(Some("user:bob"))
        );
        assert_ne!(
            forwarded_of(None, None),
            forwarded_of(Some("user:bob"), None)
        );
        // Within graphersal-serve's 64 characters, its alphabet only.
        let long = forwarded_of(Some("user:bob"), Some(&"x!".repeat(100)));
        assert_eq!(long.len(), 64, "{long}");
        assert!(long.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }

    /// SEC M3: the change feed names the reader's own client ids as it sent them and hides
    /// every other identity's.
    #[test]
    fn the_change_feed_shows_only_the_readers_own_client_ids() {
        let mine = forwarded_client(Some("user:bob"), Some("bob-tab"));
        let bare = forwarded_client(Some("user:bob"), None);
        let other = forwarded_client(Some("operator"), Some("victim-page-1"));
        let feed = json!({"version": 4, "recent": [
            {"version": 1, "by": mine, "commitSeq": 1},
            {"version": 2, "by": other, "commitSeq": 2, "detail": "secret"},
            {"version": 3, "by": "mcp", "commitSeq": 3},
            {"version": 4, "by": bare, "commitSeq": 4},
        ]});
        let answer = Response::json(200, &feed);
        let shown = own_changes(answer, Some("user:bob"));
        assert_eq!(shown.status(), 200);
        assert!(
            shown
                .headers()
                .iter()
                .any(|(n, v)| n.eq_ignore_ascii_case("Content-Type") && v.contains("json"))
        );
        let shown: Value = serde_json::from_slice(shown.body()).unwrap();
        let by: Vec<&str> = shown["recent"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["by"].as_str().unwrap())
            .collect();
        assert_eq!(by, ["bob-tab", OTHER_IDENTITY, "mcp", ""]);
        assert!(shown["recent"][1].get("detail").is_none());
        assert!(!shown.to_string().contains("victim-page-1"));
        // Not a feed: unchanged.
        let other = Response::new(404, b"nope".to_vec());
        assert_eq!(own_changes(other.clone(), None), other);
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
