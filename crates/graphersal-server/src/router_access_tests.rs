//! Access control on every route class of the router (B3): the admin API and Runs, the
//! database routes (write, read, none), both MCP endpoints, the browser login and logout, and
//! `/api/info`, for the operator, an admin, a writer/reader, a user without grants and
//! anonymous. In process: a [`Router`] over a server directory with loaded databases.

use std::sync::Arc;
use std::time::Duration;

use graphersal::persist::{Store, StoreOptions};
use graphersal::prelude::TraversalGraph;
use graphersal_serve::{Exposure, HostCheck, Method, Request, Response};
use serde_json::{Value, json};

use super::Router;
use crate::access::{Access, Grant, Level, Principal, Subject, TokenHash};
use crate::audit::{Audit, MemoryAudit};
use crate::loader::Loader;
use crate::registry::DbState;
use crate::registry::Registry;
use crate::registry::tests::server;
use crate::test_support::TempDir;

const HOST: &str = "127.0.0.1:8484";
const OPERATOR: &str = "access-tests-operator-token";
const WAIT: Duration = Duration::from_secs(120);
const WRITE: &str = r#"g.add_v("person").iterate()"#;

/// Who sends a request.
#[derive(Clone, Copy)]
enum Who<'a> {
    /// No credentials.
    Anonymous,
    /// `Authorization: Bearer <token>`.
    Bearer(&'a str),
    /// The browser session cookie `gs_session=<id>`.
    Cookie(&'a str),
}

/// A ready router over the databases `w`, `r` and `h` (loaded; aliases `cur` -> `r`, `hid` ->
/// `h`), with the users `alice` (admin), `bob` (`w` write, `r` read) and `carol` (no grant).
struct Fixture {
    router: Router,
    access: Arc<Access>,
    loader: Arc<Loader>,
    audit: Arc<MemoryAudit>,
    alice: String,
    bob: String,
    carol: String,
    _temp: TempDir,
}

fn fixture(name: &str) -> Fixture {
    let temp = TempDir::new(name);
    let dir = server(&temp);
    for db in ["w", "r", "h"] {
        let path = dir.databases().join(db);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("database.toml"), "").unwrap();
        Store::create_with(
            path.join("store"),
            TraversalGraph::tinkerpop_modern(),
            StoreOptions::new(),
        )
        .unwrap()
        .close()
        .unwrap();
    }
    let host_check = HostCheck::new(Exposure::Loopback, 8484, "http://127.0.0.1:8484/");
    let registry = Registry::scan(&dir).unwrap();
    registry.alias_set("cur", "r").unwrap();
    registry.alias_set("hid", "h").unwrap();
    let mcp = crate::mcp::endpoint(&crate::config::McpConfig::default());
    let loader = Loader::with_mcp(registry, host_check.clone(), mcp);
    let audit = Arc::new(MemoryAudit::default());
    let (access, _) = Access::open(
        &dir,
        Some(TokenHash::of(OPERATOR)),
        Audit::new(audit.clone()),
    )
    .unwrap();
    let operator = Principal::operator();
    let alice = access
        .add_user(&operator, "alice", vec!["admin".to_string()])
        .unwrap();
    let bob = access.add_user(&operator, "bob", Vec::new()).unwrap();
    let carol = access.add_user(&operator, "carol", Vec::new()).unwrap();
    for (database, level) in [("w", Level::Write), ("r", Level::Read)] {
        let grant = Grant::new(Subject::parse("user:bob").unwrap(), database, level).unwrap();
        access.grant(&operator, grant).unwrap();
    }
    let access = Arc::new(access);
    let router = Router::new(host_check);
    router.ready(loader.clone(), access.clone()).unwrap();
    for db in ["w", "r", "h"] {
        loader.load(db).unwrap();
        assert_eq!(loader.wait(db, WAIT).unwrap(), DbState::Loaded);
    }
    Fixture {
        router,
        access,
        loader,
        audit,
        alice: alice.expose().to_string(),
        bob: bob.expose().to_string(),
        carol: carol.expose().to_string(),
        _temp: temp,
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for db in ["w", "r", "h"] {
            let _ = self.loader.unload(db);
        }
    }
}

/// Sends `method url` (with `body` as JSON and `extra` headers) as `who`.
fn send(
    fx: &Fixture,
    who: Who<'_>,
    method: Method,
    url: &str,
    body: Option<Value>,
    extra: &[(&str, &str)],
) -> Response {
    let bytes = body.map(|b| b.to_string().into_bytes()).unwrap_or_default();
    let length = bytes.len();
    let mut request = Request::new(method, url, std::io::Cursor::new(bytes))
        .with_body_length(Some(length))
        .with_header("Host", HOST)
        .with_header("Content-Type", "application/json");
    match who {
        Who::Anonymous => {}
        Who::Bearer(token) => {
            request = request.with_header("Authorization", &format!("Bearer {token}"));
        }
        Who::Cookie(id) => request = request.with_header("Cookie", &format!("gs_session={id}")),
    }
    for (name, value) in extra {
        request = request.with_header(name, value);
    }
    fx.router.route(request)
}

fn get(fx: &Fixture, who: Who<'_>, url: &str) -> Response {
    send(fx, who, Method::Get, url, None, &[])
}

fn post(fx: &Fixture, who: Who<'_>, url: &str, body: Value) -> Response {
    send(fx, who, Method::Post, url, Some(body), &[])
}

fn json_of(response: &Response) -> Value {
    serde_json::from_slice(response.body())
        .unwrap_or_else(|err| panic!("{err}: {}", String::from_utf8_lossy(response.body())))
}

fn header<'r>(response: &'r Response, name: &str) -> Option<&'r str> {
    response
        .headers()
        .iter()
        .find(|(field, _)| field.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// A `401` that names the login page.
fn assert_unauthorized(response: &Response, what: &str) {
    assert_eq!(response.status(), 401, "{what}");
    assert_eq!(json_of(response)["login"], "/", "{what}");
    assert_eq!(
        header(response, "WWW-Authenticate"),
        Some("Bearer"),
        "{what}"
    );
}

/// The first result of `script` run on `/db/<db>/` as `who`.
fn execute(fx: &Fixture, who: Who<'_>, db: &str, script: &str) -> Value {
    let answer = post(
        fx,
        who,
        &format!("/db/{db}/api/execute"),
        json!({ "script": script }),
    );
    assert_eq!(
        answer.status(),
        200,
        "{}",
        String::from_utf8_lossy(answer.body())
    );
    json_of(&answer)["execution"].clone()
}

fn person_count(fx: &Fixture, db: &str) -> i64 {
    let execution = execute(
        fx,
        Who::Bearer(OPERATOR),
        db,
        r#"g.v().has_label("person").count()"#,
    );
    execution["results"][0]
        .as_i64()
        .or_else(|| execution["results"][0]["value"].as_i64())
        .unwrap_or_else(|| panic!("no count: {execution}"))
}

#[test]
fn the_admin_api_and_runs_are_for_admins_only() {
    let fx = fixture("access-admin");
    let (alice, bob, carol) = (fx.alice.as_str(), fx.bob.as_str(), fx.carol.as_str());
    for url in [
        "/api/admin/databases",
        "/api/admin/runs",
        "/api/admin/databases/w/runs",
        "/api/admin/aliases",
        "/api/admin/models",
        "/api/admin/databases/w/export",
        "/api/admin/nothing-here",
    ] {
        assert_unauthorized(&get(&fx, Who::Anonymous, url), url);
        for user in [bob, carol] {
            let refused = get(&fx, Who::Bearer(user), url);
            assert_eq!(refused.status(), 404, "{url}");
            assert_eq!(
                json_of(&refused)["error"],
                format!("unknown endpoint {url}"),
                "as if the API did not exist: {url}"
            );
        }
    }
    for who in [Who::Bearer(alice), Who::Bearer(OPERATOR)] {
        assert_eq!(get(&fx, who, "/api/admin/databases").status(), 200);
        assert_eq!(get(&fx, who, "/api/admin/runs").status(), 200);
    }
    // A writer cannot unload, cancel runs, or change flags.
    for (method, url, body) in [
        (Method::Post, "/api/admin/databases/w/unload", None),
        (
            Method::Post,
            "/api/admin/databases/w/runs/cancel",
            Some(json!({"id": 1})),
        ),
        (
            Method::Put,
            "/api/admin/databases/w/auto-load",
            Some(json!({"autoLoad": true})),
        ),
    ] {
        let answer = send(
            &fx,
            Who::Bearer(bob),
            method.clone(),
            url,
            body.clone(),
            &[],
        );
        assert_eq!(answer.status(), 404, "{url}");
        let answer = send(&fx, Who::Anonymous, method, url, body, &[]);
        assert_unauthorized(&answer, url);
    }
    assert!(fx.loader.context("w").is_some(), "still loaded");
    // Wrong credentials are refused, never treated as anonymous.
    assert_unauthorized(
        &get(&fx, Who::Bearer("nope"), "/api/admin/databases"),
        "bad",
    );
    assert_unauthorized(&get(&fx, Who::Bearer("nope"), "/api/info"), "bad info");
    // The admin's operations carry the admin as the actor.
    let answer = send(
        &fx,
        Who::Bearer(alice),
        Method::Put,
        "/api/admin/databases/w/auto-load",
        Some(json!({"autoLoad": true})),
        &[],
    );
    assert_eq!(
        answer.status(),
        200,
        "{}",
        String::from_utf8_lossy(answer.body())
    );
    let lines = fx.audit.lines();
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("user:alice: ") && l.contains("auto-load")),
        "{lines:?}"
    );
}

#[test]
fn database_routes_follow_the_grant_and_hide_the_rest() {
    let fx = fixture("access-db");
    let (alice, bob, carol) = (fx.alice.as_str(), fx.bob.as_str(), fx.carol.as_str());
    // Anonymous: 401 for every name (known, hidden, unknown); a page goes to the login page.
    for name in ["w", "r", "h", "cur", "nope"] {
        let url = format!("/db/{name}/api/info");
        assert_unauthorized(&get(&fx, Who::Anonymous, &url), &url);
        let url = format!("/db/{name}/mcp");
        assert_unauthorized(&post(&fx, Who::Anonymous, &url, json!({})), &url);
        for page in [
            format!("/db/{name}/"),
            format!("/db/{name}"),
            format!("/db/{name}/js/main.js"),
        ] {
            let answer = get(&fx, Who::Anonymous, &page);
            assert_eq!(answer.status(), 303, "{page}");
            assert_eq!(
                header(&answer, "Location"),
                Some(format!("/?next={page}").as_str()),
                "{page}"
            );
        }
    }
    let answer = get(&fx, Who::Anonymous, "/db/w/?a=1&b=2");
    assert_eq!(
        header(&answer, "Location"),
        Some("/?next=/db/w/%3Fa%3D1%26b%3D2")
    );
    // Logged in without a grant: exactly the unknown-name answer.
    let unknown = get(&fx, Who::Bearer(bob), "/db/nope/api/info");
    assert_eq!(unknown.status(), 404);
    for (who, name) in [
        (bob, "h"),
        (bob, "hid"),
        (carol, "w"),
        (carol, "r"),
        (carol, "cur"),
    ] {
        for rest in ["api/info", "", "mcp"] {
            let url = format!("/db/{name}/{rest}");
            let hidden = get(&fx, Who::Bearer(who), &url);
            assert_eq!(hidden.status(), 404, "{url}");
            assert_eq!(
                json_of(&hidden),
                json!({
                    "error": format!("no database or alias '{name}'"),
                    "help": "GET /api/admin/databases lists the databases and aliases",
                }),
                "{url}"
            );
        }
        let short = get(&fx, Who::Bearer(who), &format!("/db/{name}"));
        assert_eq!(short.status(), 404, "no redirect leaks the name");
    }
    // Write: the writer and the admin change the data.
    let before = person_count(&fx, "w");
    execute(&fx, Who::Bearer(bob), "w", WRITE);
    execute(&fx, Who::Bearer(alice), "w", WRITE);
    assert_eq!(person_count(&fx, "w"), before + 2);
    let info = json_of(&get(&fx, Who::Bearer(bob), "/db/w/api/info"));
    assert_eq!(info["readOnly"], false, "{info}");
    // Read: the reader reads, and every write is refused (script, commit routes).
    let before = person_count(&fx, "r");
    for name in ["r", "cur"] {
        let info = json_of(&get(&fx, Who::Bearer(bob), &format!("/db/{name}/api/info")));
        assert_eq!(info["readOnly"], true, "{name}: {info}");
        assert_eq!(info["database"], "r", "{info}");
        let read = execute(&fx, Who::Bearer(bob), name, "g.v().count()");
        assert!(read.to_string().contains('6'), "{read}");
        let write = execute(&fx, Who::Bearer(bob), name, WRITE);
        assert!(
            write.to_string().to_lowercase().contains("denied"),
            "{name}: {write}"
        );
        let define = post(
            &fx,
            Who::Bearer(bob),
            &format!("/db/{name}/api/queries/define"),
            json!({"name": "q", "script": "g.v().count()"}),
        );
        assert_eq!(
            define.status(),
            409,
            "{}",
            String::from_utf8_lossy(define.body())
        );
    }
    assert_eq!(person_count(&fx, "r"), before, "nothing changed");
    let info = json_of(&get(&fx, Who::Bearer(alice), "/db/r/api/info"));
    assert_eq!(
        info["readOnly"], false,
        "the admin writes everywhere: {info}"
    );
    // An anonymous grant opens a database to everyone (read here), logged in or not.
    let grant = Grant::new(Subject::parse("anonymous").unwrap(), "h", Level::Read).unwrap();
    fx.access.grant(&Principal::operator(), grant).unwrap();
    for who in [Who::Anonymous, Who::Bearer(carol)] {
        let info = get(&fx, who, "/db/h/api/info");
        assert_eq!(info.status(), 200);
        assert_eq!(json_of(&info)["readOnly"], true);
    }
    assert_eq!(get(&fx, Who::Anonymous, "/db/h/").status(), 200, "its page");
}

/// Posts one JSON-RPC message to `path` as `who` (in the MCP session `session`).
fn mcp(fx: &Fixture, who: Who<'_>, path: &str, session: Option<&str>, message: Value) -> Response {
    let mut extra = vec![("Accept", "application/json, text/event-stream")];
    if let Some(session) = session {
        extra.push(("Mcp-Session-Id", session));
    }
    send(fx, who, Method::Post, path, Some(message), &extra)
}

fn mcp_initialize(fx: &Fixture, who: Who<'_>, path: &str) -> String {
    let answer = mcp(
        fx,
        who,
        path,
        None,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "clientInfo": {"name": "t", "version": "1"}}}),
    );
    assert_eq!(
        answer.status(),
        200,
        "{}",
        String::from_utf8_lossy(answer.body())
    );
    header(&answer, "Mcp-Session-Id").unwrap().to_string()
}

/// Calls the tool `name` with `args`: (text, isError).
fn mcp_tool(
    fx: &Fixture,
    who: Who<'_>,
    path: &str,
    session: &str,
    name: &str,
    args: Value,
) -> (String, bool) {
    let answer = mcp(
        fx,
        who,
        path,
        Some(session),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": name, "arguments": args}}),
    );
    assert_eq!(
        answer.status(),
        200,
        "{}",
        String::from_utf8_lossy(answer.body())
    );
    let result = json_of(&answer)["result"].clone();
    (
        result["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        result["isError"].as_bool().unwrap_or(false),
    )
}

fn mcp_tool_names(fx: &Fixture, who: Who<'_>, path: &str, session: &str) -> Vec<String> {
    let answer = mcp(
        fx,
        who,
        path,
        Some(session),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    );
    assert_eq!(answer.status(), 200);
    json_of(&answer)["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn mcp_lists_uses_and_calls_per_principal() {
    let fx = fixture("access-mcp");
    let (alice, bob) = (fx.alice.as_str(), fx.bob.as_str());
    let session = mcp_initialize(&fx, Who::Bearer(bob), "/mcp");
    // list_graphs: only what bob may see.
    let (listing, error) = mcp_tool(
        &fx,
        Who::Bearer(bob),
        "/mcp",
        &session,
        "list_graphs",
        json!({}),
    );
    assert!(!error, "{listing}");
    for name in ["\"w\"", "\"r\"", "\"cur\""] {
        assert!(listing.contains(name), "{name}: {listing}");
    }
    for name in ["\"h\"", "\"hid\""] {
        assert!(!listing.contains(name), "{name}: {listing}");
    }
    // use_graph / a call on a hidden graph: the unknown-name error, word for word.
    let (hidden, error) = mcp_tool(
        &fx,
        Who::Bearer(bob),
        "/mcp",
        &session,
        "use_graph",
        json!({"name": "h"}),
    );
    assert!(error, "{hidden}");
    let (unknown, _) = mcp_tool(
        &fx,
        Who::Bearer(bob),
        "/mcp",
        &session,
        "use_graph",
        json!({"name": "zz"}),
    );
    assert_eq!(hidden.replace("'h'", "'zz'"), unknown);
    let query = json!({"query": "MATCH (p:person) RETURN count(p) AS n", "graph": "h"});
    let (hidden, error) = mcp_tool(&fx, Who::Bearer(bob), "/mcp", &session, "gql", query);
    assert!(error, "{hidden}");
    let query = json!({"query": "MATCH (p:person) RETURN count(p) AS n", "graph": "zz"});
    let (unknown, _) = mcp_tool(&fx, Who::Bearer(bob), "/mcp", &session, "gql", query);
    assert_eq!(hidden.replace("'h'", "'zz'"), unknown);
    // A reader cannot write through MCP; a writer can.
    let before = person_count(&fx, "r");
    let (text, error) = mcp_tool(
        &fx,
        Who::Bearer(bob),
        "/mcp",
        &session,
        "query",
        json!({"script": WRITE, "graph": "r"}),
    );
    assert!(error || text.to_lowercase().contains("denied"), "{text}");
    assert_eq!(person_count(&fx, "r"), before);
    let before = person_count(&fx, "w");
    let (text, error) = mcp_tool(
        &fx,
        Who::Bearer(bob),
        "/mcp",
        &session,
        "query",
        json!({"script": WRITE, "graph": "w"}),
    );
    assert!(!error, "{text}");
    assert_eq!(person_count(&fx, "w"), before + 1);
    // The session is bob's: nobody else can borrow it (anonymous included).
    for who in [Who::Bearer(alice), Who::Anonymous, Who::Bearer(OPERATOR)] {
        let borrowed = mcp(
            &fx,
            who,
            "/mcp",
            Some(&session),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        );
        assert_eq!(borrowed.status(), 404);
    }
    // Anonymous MCP sees nothing without anonymous grants; wrong credentials are 401.
    let anonymous = mcp_initialize(&fx, Who::Anonymous, "/mcp");
    let (listing, _) = mcp_tool(
        &fx,
        Who::Anonymous,
        "/mcp",
        &anonymous,
        "list_graphs",
        json!({}),
    );
    for name in ["\"w\"", "\"r\"", "\"h\""] {
        assert!(!listing.contains(name), "{name}: {listing}");
    }
    assert_unauthorized(
        &mcp(&fx, Who::Bearer("nope"), "/mcp", None, json!({})),
        "mcp",
    );
    // /db/<name>/mcp: the reader gets the reading tools, the admin all of them.
    let reader = mcp_initialize(&fx, Who::Bearer(bob), "/db/r/mcp");
    let tools = mcp_tool_names(&fx, Who::Bearer(bob), "/db/r/mcp", &reader);
    assert!(!tools.iter().any(|t| t == "set_schema"), "{tools:?}");
    let admin = mcp_initialize(&fx, Who::Bearer(alice), "/db/r/mcp");
    let tools = mcp_tool_names(&fx, Who::Bearer(alice), "/db/r/mcp", &admin);
    assert!(tools.iter().any(|t| t == "set_schema"), "{tools:?}");
    let (text, error) = mcp_tool(
        &fx,
        Who::Bearer(bob),
        "/db/r/mcp",
        &reader,
        "query",
        json!({"script": WRITE}),
    );
    assert!(error || text.to_lowercase().contains("denied"), "{text}");
    assert_eq!(person_count(&fx, "r"), before);
    // A session of /db/r/mcp is not the reader's on another principal either.
    let borrowed = mcp(
        &fx,
        Who::Bearer(alice),
        "/db/r/mcp",
        Some(&reader),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    );
    assert_eq!(borrowed.status(), 404);
}

#[test]
fn browser_login_and_logout() {
    let fx = fixture("access-login");
    let bob = fx.bob.as_str();
    // Wrong token, wrong method, wrong body.
    assert_unauthorized(
        &post(&fx, Who::Anonymous, "/api/login", json!({"token": "nope"})),
        "login",
    );
    assert_eq!(get(&fx, Who::Anonymous, "/api/login").status(), 405);
    assert_eq!(
        post(&fx, Who::Anonymous, "/api/login", json!({"secret": bob})).status(),
        400
    );
    let answer = post(&fx, Who::Anonymous, "/api/login", json!({"token": bob}));
    assert_eq!(
        answer.status(),
        200,
        "{}",
        String::from_utf8_lossy(answer.body())
    );
    assert_eq!(json_of(&answer)["principal"]["id"], "bob");
    let cookie = header(&answer, "Set-Cookie").unwrap().to_string();
    assert!(
        cookie.ends_with("; HttpOnly; SameSite=Strict; Path=/"),
        "{cookie}"
    );
    assert!(!cookie.contains("Secure"), "{cookie}");
    let id = cookie
        .strip_prefix("gs_session=")
        .and_then(|rest| rest.split(';').next())
        .unwrap()
        .to_string();
    assert_eq!(id.len(), 64);
    // The cookie is bob's credentials everywhere.
    let info = json_of(&get(&fx, Who::Cookie(&id), "/api/info"));
    assert_eq!(info["principal"]["id"], "bob", "{info}");
    assert_eq!(get(&fx, Who::Cookie(&id), "/db/w/api/info").status(), 200);
    assert_eq!(get(&fx, Who::Cookie(&id), "/db/h/api/info").status(), 404);
    assert_eq!(
        get(&fx, Who::Cookie(&id), "/api/admin/databases").status(),
        404
    );
    // A stale cookie does not lock the browser out of the login.
    let again = send(
        &fx,
        Who::Cookie("stale"),
        Method::Post,
        "/api/login",
        Some(json!({"token": bob})),
        &[],
    );
    assert_eq!(again.status(), 200);
    // Logout ends the session and clears the cookie.
    let out = send(
        &fx,
        Who::Cookie(&id),
        Method::Post,
        "/api/logout",
        None,
        &[],
    );
    assert_eq!(out.status(), 200);
    assert_eq!(json_of(&out)["loggedOut"], true);
    let cleared = header(&out, "Set-Cookie").unwrap();
    assert!(
        cleared.starts_with("gs_session=;") && cleared.contains("Max-Age=0"),
        "{cleared}"
    );
    // The ended session: 401 with the cookie cleared; a page goes to the login page.
    let refused = get(&fx, Who::Cookie(&id), "/db/w/api/info");
    assert_unauthorized(&refused, "ended");
    assert!(
        header(&refused, "Set-Cookie")
            .unwrap()
            .contains("Max-Age=0")
    );
    let page = get(&fx, Who::Cookie(&id), "/db/w/");
    assert_eq!(page.status(), 303);
    assert_eq!(header(&page, "Location"), Some("/?next=/db/w/"));
    assert!(header(&page, "Set-Cookie").unwrap().contains("Max-Age=0"));
    let root = get(&fx, Who::Cookie(&id), "/");
    assert_eq!(root.status(), 303);
    assert_eq!(header(&root, "Location"), Some("/"));
    let out = send(
        &fx,
        Who::Cookie(&id),
        Method::Post,
        "/api/logout",
        None,
        &[],
    );
    assert_eq!(json_of(&out)["loggedOut"], false);
    assert_eq!(get(&fx, Who::Anonymous, "/api/logout").status(), 405);
    // The login events are audited.
    let lines = fx.audit.lines();
    for needle in ["user:bob: ", "login"] {
        assert!(
            lines.iter().any(|l| l.contains(needle)),
            "{needle}: {lines:?}"
        );
    }
    assert!(
        lines.iter().filter(|l| l.starts_with("user:bob: ")).count() >= 3,
        "two logins and a logout: {lines:?}"
    );
}

#[test]
fn the_root_info_is_minimal_for_anonymous_and_names_the_principal() {
    let fx = fixture("access-info");
    let anonymous = json_of(&get(&fx, Who::Anonymous, "/api/info"));
    assert_eq!(anonymous["protocol"], "graphersal/1");
    assert_eq!(anonymous["uiProfile"]["name"], "server");
    assert_eq!(anonymous["login"], "/");
    assert_eq!(
        anonymous["principal"],
        json!({"id": "anonymous", "kind": "anonymous", "admin": false})
    );
    for key in ["uploadBytes", "mcp", "version"] {
        assert!(anonymous.get(key).is_none(), "{key}: {anonymous}");
    }
    let bob = json_of(&get(&fx, Who::Bearer(&fx.bob), "/api/info"));
    assert_eq!(
        bob["principal"],
        json!({"id": "bob", "kind": "user", "admin": false})
    );
    assert!(bob["uploadBytes"].is_u64(), "{bob}");
    assert_eq!(bob["protocol"], "graphersal/1");
    let alice = json_of(&get(&fx, Who::Bearer(&fx.alice), "/api/info"));
    assert_eq!(alice["principal"]["admin"], true);
    let operator = json_of(&get(&fx, Who::Bearer(OPERATOR), "/api/info"));
    assert_eq!(
        operator["principal"],
        json!({"id": "operator", "kind": "operator", "admin": true})
    );
    // The UI files are public: the login page is one of them.
    assert_eq!(get(&fx, Who::Anonymous, "/").status(), 200);
    assert_eq!(
        get(&fx, Who::Anonymous, "/server-ui/databases-tab.js").status(),
        200
    );
    assert_eq!(get(&fx, Who::Anonymous, "/api/other").status(), 404);
}
