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

use super::{Modes, Router};
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
    fixture_with(name, Vec::new(), Modes::default())
}

/// [`fixture`] with the allowed host names `allowed` and the router's `modes`.
fn fixture_with(name: &str, allowed: Vec<String>, modes: Modes) -> Fixture {
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
    let host_check = HostCheck::new(Exposure::Loopback, 8484, "http://127.0.0.1:8484/")
        .with_allowed_hosts(allowed);
    let registry = Registry::scan(&dir).unwrap();
    registry.alias_set("cur", "r").unwrap();
    registry.alias_set("hid", "h").unwrap();
    let control = Arc::new(crate::mcp_control::McpControl::open(&dir).unwrap());
    let mcp = crate::mcp::endpoint(
        &crate::config::McpConfig::default(),
        control.usage().clone(),
    );
    let loader = Loader::with_mcp(registry, host_check.clone(), mcp);
    loader.set_mcp_control(control);
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
    let router = Router::new(host_check).with_modes(modes);
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

// ---------------------------------------------------------------------------------------------
// Users and grants over the admin API (B5)
// ---------------------------------------------------------------------------------------------

/// A browser login with `token`: the session id of its cookie.
fn login(fx: &Fixture, token: &str) -> String {
    let answer = post(fx, Who::Anonymous, "/api/login", json!({ "token": token }));
    assert_eq!(answer.status(), 200, "login");
    header(&answer, "Set-Cookie")
        .and_then(|c| c.strip_prefix("gs_session="))
        .and_then(|rest| rest.split(';').next())
        .unwrap()
        .to_string()
}

fn delete(fx: &Fixture, who: Who<'_>, url: &str) -> Response {
    send(fx, who, Method::Delete, url, None, &[])
}

fn put(fx: &Fixture, who: Who<'_>, url: &str, body: Value) -> Response {
    send(fx, who, Method::Put, url, Some(body), &[])
}

/// `response`'s JSON, asserting `status`.
fn expect(response: Response, status: u16) -> Value {
    assert_eq!(
        response.status(),
        status,
        "{}",
        String::from_utf8_lossy(response.body())
    );
    json_of(&response)
}

#[test]
fn the_users_api_is_for_admins_and_manages_users_and_grants() {
    let fx = fixture("access-users-api");
    let (alice, bob) = (fx.alice.as_str(), fx.bob.as_str());
    for url in [
        "/api/admin/users",
        "/api/admin/users/bob",
        "/api/admin/grants",
    ] {
        assert_unauthorized(&get(&fx, Who::Anonymous, url), url);
        let refused = expect(get(&fx, Who::Bearer(bob), url), 404);
        assert_eq!(refused["error"], format!("unknown endpoint {url}"));
    }
    assert_unauthorized(
        &post(
            &fx,
            Who::Anonymous,
            "/api/admin/users",
            json!({"name": "eve"}),
        ),
        "anonymous add",
    );
    expect(
        post(
            &fx,
            Who::Bearer(bob),
            "/api/admin/users",
            json!({"name": "eve"}),
        ),
        404,
    );
    assert!(fx.access.file().user("eve").is_none());

    // The listing: roles, the token's last 4 characters (never a hash), the grants.
    let list = expect(get(&fx, Who::Bearer(alice), "/api/admin/users"), 200);
    let row = |list: &Value, name: &str| {
        list["users"]
            .as_array()
            .unwrap()
            .iter()
            .find(|u| u["name"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("no {name} in {list}"))
    };
    let alice_row = row(&list, "alice");
    assert_eq!(alice_row["admin"], true);
    assert_eq!(alice_row["tokenLast4"], alice[alice.len() - 4..]);
    assert_eq!(alice_row["hasToken"], true);
    let bob_row = row(&list, "bob");
    assert_eq!(bob_row["roles"], json!([]));
    assert_eq!(
        bob_row["grants"],
        json!([
            {"database": "w", "level": "write", "kind": "database", "target": null},
            {"database": "r", "level": "read", "kind": "database", "target": null}
        ])
    );
    assert!(!list.to_string().contains(&TokenHash::of(bob).to_hex()));
    assert_eq!(list["grants"].as_array().unwrap().len(), 2, "{list}");

    // Add: the token once; it works at once.
    let dave = expect(
        post(
            &fx,
            Who::Bearer(alice),
            "/api/admin/users",
            json!({"name": "Dave"}),
        ),
        201,
    );
    assert_eq!(dave["name"], "dave");
    let token = dave["token"].as_str().unwrap().to_string();
    assert_eq!(token.len(), 64);
    assert_eq!(dave["tokenLast4"], token[60..]);
    let info = expect(get(&fx, Who::Bearer(&token), "/api/info"), 200);
    assert_eq!(info["principal"]["id"], "dave");
    assert_eq!(
        get(&fx, Who::Bearer(&token), "/db/w/api/info").status(),
        404
    );
    let again = expect(
        post(
            &fx,
            Who::Bearer(alice),
            "/api/admin/users",
            json!({"name": "dave"}),
        ),
        409,
    );
    assert!(
        again["error"].as_str().unwrap().contains("exists"),
        "{again}"
    );
    expect(
        post(
            &fx,
            Who::Bearer(alice),
            "/api/admin/users",
            json!({"name": "anonymous"}),
        ),
        409,
    );
    expect(
        post(
            &fx,
            Who::Bearer(alice),
            "/api/admin/users",
            json!({"name": "no way"}),
        ),
        400,
    );

    // Grants: a bare user name, read; unknown names are refused (an alias is granted: see
    // `a_grant_on_an_alias_*`).
    let grant = expect(
        post(
            &fx,
            Who::Bearer(alice),
            "/api/admin/grants",
            json!({"subject": "dave", "database": "W", "level": "read"}),
        ),
        200,
    );
    assert_eq!(
        grant,
        json!({"subject": "user:dave", "database": "w", "level": "read", "kind": "database",
               "target": null})
    );
    assert_eq!(
        get(&fx, Who::Bearer(&token), "/db/w/api/info").status(),
        200
    );
    for (body, status) in [
        (
            json!({"subject": "dave", "database": "nope", "level": "read"}),
            404,
        ),
        (
            json!({"subject": "nobody", "database": "w", "level": "read"}),
            404,
        ),
        (
            json!({"subject": "dave", "database": "w", "level": "admin"}),
            400,
        ),
        (
            json!({"subject": "x:y", "database": "w", "level": "read"}),
            400,
        ),
    ] {
        expect(
            post(&fx, Who::Bearer(alice), "/api/admin/grants", body.clone()),
            status,
        );
    }
    // Anonymous grants: everyone reaches `r`, until revoked.
    expect(
        post(
            &fx,
            Who::Bearer(alice),
            "/api/admin/grants",
            json!({"subject": "anonymous", "database": "r", "level": "read"}),
        ),
        200,
    );
    assert_eq!(get(&fx, Who::Anonymous, "/db/r/api/info").status(), 200);
    let revoked = expect(
        delete(&fx, Who::Bearer(alice), "/api/admin/grants/anonymous/r"),
        200,
    );
    assert_eq!(
        revoked,
        json!({"subject": "anonymous", "database": "r", "revoked": true})
    );
    assert_unauthorized(&get(&fx, Who::Anonymous, "/db/r/api/info"), "revoked");
    let none = expect(
        delete(&fx, Who::Bearer(alice), "/api/admin/grants/anonymous/r"),
        200,
    );
    assert_eq!(none["revoked"], false);
    // A percent-encoded subject.
    let revoked = expect(
        delete(&fx, Who::Bearer(alice), "/api/admin/grants/user%3Adave/w"),
        200,
    );
    assert_eq!(revoked["revoked"], true, "{revoked}");
    assert_eq!(
        get(&fx, Who::Bearer(&token), "/db/w/api/info").status(),
        404
    );

    // The admin flag and roles.
    let user = expect(
        put(
            &fx,
            Who::Bearer(alice),
            "/api/admin/users/dave/admin",
            json!({"admin": true}),
        ),
        200,
    );
    assert_eq!(user["admin"], true);
    assert_eq!(
        get(&fx, Who::Bearer(&token), "/api/admin/users").status(),
        200
    );
    let user = expect(
        put(
            &fx,
            Who::Bearer(&token),
            "/api/admin/users/dave/roles",
            json!({"roles": ["analyst"]}),
        ),
        200,
    );
    assert_eq!(user["roles"], json!(["analyst"]));
    assert_eq!(user["admin"], false);
    assert_eq!(
        get(&fx, Who::Bearer(&token), "/api/admin/users").status(),
        404
    );
    expect(
        put(
            &fx,
            Who::Bearer(alice),
            "/api/admin/users/nobody/admin",
            json!({"admin": true}),
        ),
        404,
    );
    expect(
        send(
            &fx,
            Who::Bearer(alice),
            Method::Patch,
            "/api/admin/users/dave",
            None,
            &[],
        ),
        405,
    );

    // Every change is audited with the actor.
    let lines = fx.audit.lines();
    for wanted in [
        "user:alice: user 'dave' added",
        "user:alice: grant user:dave read on 'w'",
        "user:alice: grant of anonymous on 'r' revoked",
        "user:alice: user 'dave' roles set to [admin]",
        "user:dave: user 'dave' roles set to [analyst]",
    ] {
        assert!(
            lines.iter().any(|l| l.starts_with(wanted)),
            "{wanted}: {lines:#?}"
        );
    }
}

#[test]
fn a_token_change_or_removal_ends_browser_and_mcp_sessions() {
    let fx = fixture("access-users-sessions");
    let (alice, bob) = (fx.alice.as_str(), fx.bob.as_str());
    let cookie = login(&fx, bob);
    let session = mcp_initialize(&fx, Who::Bearer(bob), "/mcp");
    assert_eq!(
        get(&fx, Who::Cookie(&cookie), "/db/w/api/info").status(),
        200
    );

    // A new token: the old token, the browser session and the MCP session end.
    let answer = expect(
        post(
            &fx,
            Who::Bearer(alice),
            "/api/admin/users/bob/token",
            json!({}),
        ),
        200,
    );
    assert_eq!(answer["mcpSessionsEnded"], 1, "{answer}");
    let new_token = answer["token"].as_str().unwrap().to_string();
    assert_ne!(new_token, bob);
    assert_eq!(answer["tokenLast4"], new_token[60..]);
    assert_unauthorized(&get(&fx, Who::Bearer(bob), "/api/info"), "old token");
    assert_eq!(
        get(&fx, Who::Cookie(&cookie), "/db/w/api/info").status(),
        401,
        "the browser session ended"
    );
    // The MCP session is gone even for bob's new token (same identity).
    let gone = mcp(
        &fx,
        Who::Bearer(&new_token),
        "/mcp",
        Some(&session),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    );
    assert_eq!(
        gone.status(),
        404,
        "{}",
        String::from_utf8_lossy(gone.body())
    );

    // A revoked token: the same.
    let cookie = login(&fx, &new_token);
    let session = mcp_initialize(&fx, Who::Bearer(&new_token), "/mcp");
    let answer = expect(
        delete(&fx, Who::Bearer(alice), "/api/admin/users/bob/token"),
        200,
    );
    assert_eq!(answer["hasToken"], false);
    assert_eq!(answer["tokenLast4"], Value::Null);
    assert_eq!(answer["mcpSessionsEnded"], 1);
    assert_unauthorized(&get(&fx, Who::Bearer(&new_token), "/api/info"), "revoked");
    assert_eq!(get(&fx, Who::Cookie(&cookie), "/api/info").status(), 401);
    assert_eq!(
        fx.loader
            .mcp()
            .unwrap()
            .forget_sessions_of(Some("user:bob")),
        0
    );
    let _ = session;

    // A removed user: its sessions and grants go with it.
    let newest = expect(
        post(
            &fx,
            Who::Bearer(alice),
            "/api/admin/users/bob/token",
            json!({}),
        ),
        200,
    )["token"]
        .as_str()
        .unwrap()
        .to_string();
    let cookie = login(&fx, &newest);
    mcp_initialize(&fx, Who::Bearer(&newest), "/mcp");
    let removed = expect(delete(&fx, Who::Bearer(alice), "/api/admin/users/bob"), 200);
    assert_eq!(
        removed,
        json!({"name": "bob", "removed": true, "grantsRemoved": 2, "mcpSessionsEnded": 1})
    );
    assert_eq!(get(&fx, Who::Cookie(&cookie), "/api/info").status(), 401);
    assert!(fx.access.file().grants().is_empty());
    expect(delete(&fx, Who::Bearer(alice), "/api/admin/users/bob"), 404);
    // Other users' sessions are untouched.
    let carol = login(&fx, &fx.carol);
    assert_eq!(get(&fx, Who::Cookie(&carol), "/api/info").status(), 200);
}

#[test]
fn the_last_admin_is_never_removed_demoted_or_locked_out() {
    let fx = fixture("access-users-last-admin");
    let alice = fx.alice.as_str();
    // Two admins (init's `admin` and alice): one may go.
    expect(
        delete(&fx, Who::Bearer(alice), "/api/admin/users/admin"),
        200,
    );
    for (method, url, body) in [
        (Method::Delete, "/api/admin/users/alice", None),
        (Method::Delete, "/api/admin/users/alice/token", None),
        (
            Method::Put,
            "/api/admin/users/alice/admin",
            Some(json!({"admin": false})),
        ),
        (
            Method::Put,
            "/api/admin/users/alice/roles",
            Some(json!({"roles": []})),
        ),
    ] {
        for who in [Who::Bearer(alice), Who::Bearer(OPERATOR)] {
            let refused = expect(send(&fx, who, method.clone(), url, body.clone(), &[]), 409);
            assert!(
                refused["error"]
                    .as_str()
                    .unwrap()
                    .contains("'alice' is the last admin"),
                "{refused}"
            );
            assert!(
                refused["help"]
                    .as_str()
                    .unwrap()
                    .contains("POST /api/admin/users/<name>/token"),
                "{refused}"
            );
        }
    }
    let file = fx.access.file();
    assert!(
        file.user("alice")
            .unwrap()
            .roles
            .contains(&"admin".to_string())
    );
    assert_eq!(file.usable_admins(), 1);
    // A new token is fine (it replaces, it does not lock out); a second admin frees alice.
    let token = expect(
        post(
            &fx,
            Who::Bearer(alice),
            "/api/admin/users/alice/token",
            json!({}),
        ),
        200,
    )["token"]
        .as_str()
        .unwrap()
        .to_string();
    expect(
        put(
            &fx,
            Who::Bearer(&token),
            "/api/admin/users/carol/admin",
            json!({"admin": true}),
        ),
        200,
    );
    expect(
        delete(&fx, Who::Bearer(&token), "/api/admin/users/alice"),
        200,
    );
}

// ---------------------------------------------------------------------------------------------
// "My databases" (B4): `GET /api/databases` and the profile per principal
// ---------------------------------------------------------------------------------------------

/// The `(name, level, aliasOf)` of each row of `GET /api/databases` as `who`.
fn listed(fx: &Fixture, who: Who<'_>) -> Vec<(String, String, Option<String>)> {
    let answer = expect(get(fx, who, "/api/databases"), 200);
    answer["databases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let name = row["name"].as_str().unwrap().to_string();
            assert_eq!(row["playground"], format!("/db/{name}/"), "{row}");
            assert_eq!(row["state"], "loaded", "{row}");
            assert_eq!(row["readOnly"], row["level"] == "read", "{row}");
            assert!(row["description"].is_string(), "{row}");
            assert!(row.get("reason").is_none(), "no admin detail: {row}");
            (
                name,
                row["level"].as_str().unwrap().to_string(),
                row["aliasOf"].as_str().map(str::to_string),
            )
        })
        .collect()
}

fn row(name: &str, level: &str, alias_of: Option<&str>) -> (String, String, Option<String>) {
    (
        name.to_string(),
        level.to_string(),
        alias_of.map(str::to_string),
    )
}

#[test]
fn my_databases_lists_only_what_the_principal_may_reach() {
    let fx = fixture("access-mine");
    // bob: `w` write, `r` read (and its alias `cur`); `h` and its alias `hid` are hidden.
    assert_eq!(
        listed(&fx, Who::Bearer(&fx.bob)),
        [
            row("cur", "read", Some("r")),
            row("r", "read", None),
            row("w", "write", None),
        ]
    );
    // The same through the browser session.
    let session = login(&fx, &fx.bob);
    assert_eq!(listed(&fx, Who::Cookie(&session)).len(), 3);
    // carol has no grant: an empty list, never a hidden name.
    assert!(listed(&fx, Who::Bearer(&fx.carol)).is_empty());
    let carol = get(&fx, Who::Bearer(&fx.carol), "/api/databases");
    let text = String::from_utf8_lossy(carol.body()).into_owned();
    for hidden in ["\"h\"", "\"hid\"", "\"w\"", "\"r\""] {
        assert!(!text.contains(hidden), "{hidden} leaks: {text}");
    }
    // Anonymous is a principal: an empty list (200, not 401) until an anonymous grant opens one.
    assert!(listed(&fx, Who::Anonymous).is_empty());
    // An admin and the operator reach everything, at write.
    let all = [
        row("cur", "write", Some("r")),
        row("h", "write", None),
        row("hid", "write", Some("h")),
        row("r", "write", None),
        row("w", "write", None),
    ];
    assert_eq!(listed(&fx, Who::Bearer(&fx.alice)), all);
    assert_eq!(listed(&fx, Who::Bearer(OPERATOR)), all);
    // An anonymous grant opens `h` (and its alias) to everyone, logged in or not.
    let operator = Principal::operator();
    let grant = Grant::new(Subject::parse("anonymous").unwrap(), "h", Level::Read).unwrap();
    fx.access.grant(&operator, grant).unwrap();
    assert_eq!(
        listed(&fx, Who::Anonymous),
        [row("h", "read", None), row("hid", "read", Some("h"))]
    );
    assert_eq!(
        listed(&fx, Who::Bearer(&fx.carol)),
        [row("h", "read", None), row("hid", "read", Some("h"))]
    );
    // The level is capped by the database's mode: a read-only `w` is `read` for bob.
    fx.loader.set_read_only("w", true).unwrap();
    assert!(listed(&fx, Who::Bearer(&fx.bob)).contains(&row("w", "read", None)));
    // Wrong credentials are 401, never anonymous; a write method is 405.
    assert_unauthorized(
        &get(&fx, Who::Bearer("not-a-token"), "/api/databases"),
        "wrong token",
    );
    let post = post(&fx, Who::Bearer(&fx.bob), "/api/databases", json!({}));
    assert_eq!(post.status(), 405);
}

#[test]
fn the_root_profile_follows_the_principal() {
    let fx = fixture("access-profile");
    let tabs = |who| -> (Vec<String>, Value) {
        let info = expect(get(&fx, who, "/api/info"), 200);
        let profile = info["uiProfile"].clone();
        assert_eq!(profile["name"], "server");
        assert_eq!(profile["header"]["module"], "header.js");
        let ids = profile["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap().to_string())
            .collect();
        (ids, profile["startTab"].clone())
    };
    assert_eq!(
        tabs(Who::Anonymous),
        (vec!["login".to_string()], json!("login"))
    );
    assert_eq!(
        tabs(Who::Bearer(&fx.bob)),
        (vec!["mine".to_string()], json!("mine"))
    );
    let (admin, start) = tabs(Who::Bearer(&fx.alice));
    assert_eq!(start, "databases");
    assert!(admin.contains(&"users".to_string()), "{admin:?}");
    assert_eq!(tabs(Who::Bearer(OPERATOR)).0, admin);
    // The modules of every view are public files (the login page loads before any login).
    for module in [
        "login-tab.js",
        "mine-tab.js",
        "header.js",
        "access-api.js",
        "users-tab.js",
    ] {
        assert_eq!(
            get(&fx, Who::Anonymous, &format!("/server-ui/{module}")).status(),
            200,
            "{module}"
        );
    }
}

// B6: the modes of server.toml (`[access] enabled`, `[http] public_scheme`, `[http]
// allowed_hosts`, the login cookie's `Max-Age`).

fn modes(access_enabled: bool, secure_cookies: bool, cookie_max_age_secs: u64) -> Modes {
    Modes {
        access_enabled,
        secure_cookies,
        cookie_max_age_secs,
    }
}

#[test]
fn access_off_makes_every_request_an_admin() {
    let fx = fixture_with(
        "modes-access-off",
        Vec::new(),
        modes(false, false, super::DEFAULT_COOKIE_MAX_AGE_SECS),
    );
    let principal = |who| expect(get(&fx, who, "/api/info"), 200)["principal"].clone();
    let everyone = json!({"id": "everyone", "kind": "everyone", "admin": true});
    // No credentials, a user without the admin role, wrong credentials: `everyone`, an admin.
    for who in [
        Who::Anonymous,
        Who::Bearer(&fx.carol),
        Who::Bearer(&fx.bob),
        Who::Bearer("nope"),
        Who::Cookie("stale"),
    ] {
        assert_eq!(principal(who), everyone);
    }
    // Valid admin credentials keep their identity (the audit names them).
    assert_eq!(principal(Who::Bearer(&fx.alice))["id"], "alice");
    assert_eq!(principal(Who::Bearer(OPERATOR))["kind"], "operator");
    // The admin view of the UI, the admin API, every database read-write.
    let info = expect(get(&fx, Who::Anonymous, "/api/info"), 200);
    assert_eq!(info["uiProfile"]["startTab"], "databases", "{info}");
    assert!(info.get("uploadBytes").is_some(), "{info}");
    expect(get(&fx, Who::Anonymous, "/api/admin/databases"), 200);
    expect(get(&fx, Who::Anonymous, "/api/admin/users"), 200);
    for db in ["w", "r", "h", "hid"] {
        expect(get(&fx, Who::Anonymous, &format!("/db/{db}/api/info")), 200);
    }
    let before = person_count(&fx, "h");
    execute(&fx, Who::Anonymous, "h", WRITE);
    execute(&fx, Who::Bearer(&fx.carol), "h", WRITE);
    assert_eq!(person_count(&fx, "h"), before + 2);
    let listed = expect(get(&fx, Who::Anonymous, "/api/databases"), 200);
    assert_eq!(listed["databases"].as_array().unwrap().len(), 5, "{listed}");
    // The audit still records, with `everyone` as the actor.
    let answer = send(
        &fx,
        Who::Anonymous,
        Method::Put,
        "/api/admin/databases/w/auto-load",
        Some(json!({"autoLoad": true})),
        &[],
    );
    assert_eq!(answer.status(), 200);
    let lines = fx.audit.lines();
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("everyone: ") && l.contains("auto-load")),
        "{lines:?}"
    );
    // MCP: every graph, every tool, writes allowed.
    let session = mcp_initialize(&fx, Who::Anonymous, "/mcp");
    let (listing, error) = mcp_tool(
        &fx,
        Who::Anonymous,
        "/mcp",
        &session,
        "list_graphs",
        json!({}),
    );
    assert!(!error, "{listing}");
    for name in ["\"w\"", "\"r\"", "\"h\"", "\"cur\"", "\"hid\""] {
        assert!(listing.contains(name), "{name}: {listing}");
    }
    let before = person_count(&fx, "r");
    let (text, error) = mcp_tool(
        &fx,
        Who::Anonymous,
        "/mcp",
        &session,
        "query",
        json!({"script": WRITE, "graph": "r"}),
    );
    assert!(!error, "{text}");
    assert_eq!(person_count(&fx, "r"), before + 1);
    let db = mcp_initialize(&fx, Who::Anonymous, "/db/h/mcp");
    let tools = mcp_tool_names(&fx, Who::Anonymous, "/db/h/mcp", &db);
    assert!(tools.iter().any(|t| t == "set_schema"), "{tools:?}");
    // Login still works (nobody needs it) and answers the user it logged in.
    let answer = post(&fx, Who::Anonymous, "/api/login", json!({"token": fx.bob}));
    assert_eq!(expect(answer, 200)["principal"]["id"], "bob");
}

#[test]
fn access_on_is_the_default_mode() {
    assert_eq!(Modes::default(), modes(true, false, 400 * 24 * 60 * 60));
    let config = crate::config::ServerConfig::default();
    assert_eq!(Modes::of(&config), Modes::default());
    let config = crate::config::ServerConfig::parse(
        "[access]\nenabled = false\n[http]\npublic_scheme = \"https\"\n\
         [sessions]\nlifetime_secs = 3600",
        std::path::Path::new("server.toml"),
    )
    .unwrap();
    assert_eq!(Modes::of(&config), modes(false, true, 3600));
}

/// The `Set-Cookie` of a login of `token` and of a logout with the cookie it set.
fn login_and_logout_cookies(fx: &Fixture, token: &str) -> (String, String) {
    let answer = post(fx, Who::Anonymous, "/api/login", json!({"token": token}));
    assert_eq!(answer.status(), 200);
    let set = header(&answer, "Set-Cookie").unwrap().to_string();
    let id = set
        .strip_prefix("gs_session=")
        .and_then(|rest| rest.split(';').next())
        .unwrap()
        .to_string();
    let answer = post(fx, Who::Cookie(&id), "/api/logout", json!({}));
    assert_eq!(answer.status(), 200);
    assert_eq!(json_of(&answer)["loggedOut"], true);
    (set, header(&answer, "Set-Cookie").unwrap().to_string())
}

#[test]
fn the_login_cookie_is_secure_only_on_https_and_persistent() {
    // http (the default): no Secure, Max-Age 400 days; the clear cookie Max-Age=0.
    let fx = fixture_with("modes-http", Vec::new(), Modes::default());
    let (set, cleared) = login_and_logout_cookies(&fx, &fx.bob.clone());
    assert!(set.contains("; Max-Age=34560000;"), "{set}");
    assert!(!set.contains("Secure"), "{set}");
    assert!(cleared.starts_with("gs_session=;") && cleared.contains("; Max-Age=0;"));
    assert!(!cleared.contains("Secure"), "{cleared}");
    // A refused cookie is cleared the same way.
    let refused = get(&fx, Who::Cookie("stale"), "/api/info");
    let cleared = header(&refused, "Set-Cookie").unwrap();
    assert!(
        cleared.contains("Max-Age=0") && !cleared.contains("Secure"),
        "{cleared}"
    );
    drop(fx);
    // https: Secure on the set and the clear cookie; Max-Age = [sessions] lifetime_secs.
    let fx = fixture_with("modes-https", Vec::new(), modes(true, true, 3600));
    let (set, cleared) = login_and_logout_cookies(&fx, &fx.bob.clone());
    assert!(set.contains("; Max-Age=3600;"), "{set}");
    assert!(
        set.ends_with("; HttpOnly; SameSite=Strict; Path=/; Secure"),
        "{set}"
    );
    assert!(
        cleared.contains("; Max-Age=0;") && cleared.ends_with("; Secure"),
        "{cleared}"
    );
    let refused = get(&fx, Who::Cookie("stale"), "/api/info");
    let cleared = header(&refused, "Set-Cookie").unwrap();
    assert!(
        cleared.contains("Max-Age=0") && cleared.ends_with("; Secure"),
        "{cleared}"
    );
    let page = get(&fx, Who::Cookie("stale"), "/db/w/");
    assert_eq!(page.status(), 303);
    assert!(header(&page, "Set-Cookie").unwrap().ends_with("; Secure"));
}

/// `GET url` as the operator with `Host: host` (and `Origin: origin`).
fn get_from_host(fx: &Fixture, url: &str, host: &str, origin: Option<&str>) -> Response {
    let mut request = Request::new(Method::Get, url, std::io::Cursor::new(Vec::new()))
        .with_header("Host", host)
        .with_header("Authorization", &format!("Bearer {OPERATOR}"));
    if let Some(origin) = origin {
        request = request.with_header("Origin", origin);
    }
    fx.router.route(request)
}

#[test]
fn allowed_hosts_admit_a_proxy_name_and_refuse_others() {
    let fx = fixture_with(
        "modes-allowed-hosts",
        vec!["db.example.com".to_string()],
        Modes::default(),
    );
    for host in [
        "db.example.com",
        "db.example.com:443",
        "DB.Example.com:8080",
        HOST,
    ] {
        let answer = get_from_host(&fx, "/api/info", host, None);
        assert_eq!(answer.status(), 200, "{host}");
    }
    let answer = get_from_host(
        &fx,
        "/api/info",
        "db.example.com",
        Some("https://db.example.com"),
    );
    assert_eq!(answer.status(), 200);
    for host in [
        "other.example.com",
        "evil.db.example.com",
        "example.com:443",
    ] {
        let answer = get_from_host(&fx, "/api/info", host, None);
        assert_eq!(answer.status(), 403, "{host}");
    }
    let answer = get_from_host(
        &fx,
        "/api/info",
        "db.example.com",
        Some("https://evil.example.com"),
    );
    assert_eq!(answer.status(), 403);
    // Without allowed names the proxy name is refused (as before).
    drop(fx);
    let fx = fixture("modes-no-allowed-hosts");
    assert_eq!(
        get_from_host(&fx, "/api/info", "db.example.com", None).status(),
        403
    );
}

#[test]
fn the_import_inbox_is_listed_and_imported_by_admins_only_and_stays_inside() {
    let fx = fixture("access-inbox");
    let imports = fx.loader.registry().dir().imports();
    use graphersal::prelude::GraphExport;
    graphersal::prelude::Graph::new(TraversalGraph::tinkerpop_modern())
        .export_graphson_writer(std::fs::File::create(imports.join("people.json")).unwrap())
        .unwrap();
    std::fs::write(imports.join(".hidden.json"), "{}").unwrap();
    let outside = fx.loader.registry().dir().root().join("outside.json");
    std::fs::write(&outside, "{}").unwrap();
    let (alice, bob) = (fx.alice.as_str(), fx.bob.as_str());

    // Admins only: anonymous 401, a user without the admin role 404, the listing and the
    // import alike.
    assert_unauthorized(&get(&fx, Who::Anonymous, "/api/admin/imports"), "list");
    assert_eq!(
        get(&fx, Who::Bearer(bob), "/api/admin/imports").status(),
        404
    );
    let by_bob = post(
        &fx,
        Who::Bearer(bob),
        "/api/admin/import",
        json!({"inbox": "people.json", "name": "bobs"}),
    );
    assert_eq!(by_bob.status(), 404);
    assert!(!fx.loader.registry().dir().databases().join("bobs").exists());

    // The listing.
    let listing = expect(get(&fx, Who::Bearer(alice), "/api/admin/imports"), 200);
    assert_eq!(listing["dir"], imports.to_string_lossy().as_ref());
    let rows = listing["imports"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{listing}");
    assert_eq!(rows[0]["name"], "people.json");
    assert_eq!(rows[0]["kind"], "graphson");
    assert_eq!(rows[0]["directory"], false);
    assert!(rows[0]["bytes"].as_u64().unwrap() > 0);
    assert!(rows[0]["modified"].is_string());
    let refused = send(
        &fx,
        Who::Bearer(alice),
        Method::Post,
        "/api/admin/imports",
        None,
        &[],
    );
    assert_eq!(refused.status(), 405);

    // An import by inbox name.
    let imported = expect(
        post(
            &fx,
            Who::Bearer(OPERATOR),
            "/api/admin/import",
            json!({"inbox": "people.json", "name": "people"}),
        ),
        201,
    );
    assert_eq!(imported["name"], "people");
    assert_eq!(imported["import"]["format"], "graphson");
    assert!(
        imports.join("people.json").is_file(),
        "the inbox file stays"
    );

    // Traversal and other names that are not one inbox entry: 400; missing: 404.
    for name in [
        "../outside.json",
        "..",
        ".",
        "",
        ".hidden.json",
        "sub/people.json",
        "..\\outside.json",
        "/etc/passwd",
    ] {
        let answer = post(
            &fx,
            Who::Bearer(alice),
            "/api/admin/import",
            json!({"inbox": name, "name": "evil"}),
        );
        assert_eq!(answer.status(), 400, "{name:?}: {:?}", json_of(&answer));
    }
    let missing = expect(
        post(
            &fx,
            Who::Bearer(alice),
            "/api/admin/import",
            json!({"inbox": "nope.json", "name": "evil"}),
        ),
        404,
    );
    assert!(
        missing["help"]
            .as_str()
            .unwrap()
            .contains("GET /api/admin/imports")
    );
    // Both or neither of path and inbox: 400.
    for body in [
        json!({"inbox": "people.json", "path": outside.to_str().unwrap(), "name": "evil"}),
        json!({"name": "evil"}),
    ] {
        let answer = post(&fx, Who::Bearer(alice), "/api/admin/import", body);
        assert_eq!(answer.status(), 400);
    }
    // A link leading outside the inbox: not listed, refused (409).
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside, imports.join("escape.json")).unwrap();
        let listing = expect(get(&fx, Who::Bearer(alice), "/api/admin/imports"), 200);
        assert_eq!(listing["imports"].as_array().unwrap().len(), 1, "{listing}");
        let answer = expect(
            post(
                &fx,
                Who::Bearer(alice),
                "/api/admin/import",
                json!({"inbox": "escape.json", "name": "evil"}),
            ),
            409,
        );
        assert!(
            answer["error"]
                .as_str()
                .unwrap()
                .contains("outside imports/"),
            "{answer}"
        );
    }
    assert!(!fx.loader.registry().dir().databases().join("evil").exists());
}

// ---------------------------------------------------------------------------------------------
// Grants on aliases (round 3 B3)
// ---------------------------------------------------------------------------------------------

/// The person count of the database `name` names, asked by `who` through that name.
fn person_count_as(fx: &Fixture, who: Who<'_>, name: &str) -> i64 {
    let execution = execute(fx, who, name, r#"g.v().has_label("person").count()"#);
    execution["results"][0]
        .as_i64()
        .or_else(|| execution["results"][0]["value"].as_i64())
        .unwrap_or_else(|| panic!("no count: {execution}"))
}

/// The person count an MCP call in `session` answers (on the session's graph): the result
/// line of the answer (the first line carries the timing), and whether it is an error.
fn mcp_person_count(fx: &Fixture, who: Who<'_>, session: &str) -> (String, bool) {
    let (text, error) = mcp_tool(
        fx,
        who,
        "/mcp",
        session,
        "query",
        json!({"script": r#"g.v().has_label("person").count()"#}),
    );
    (result_line(&text), error)
}

/// The last line of a tool answer (the result, without the timing line).
fn result_line(text: &str) -> String {
    text.lines().last().unwrap_or_default().to_string()
}

#[test]
fn a_grant_on_an_alias_reaches_its_current_target_only_through_the_alias() {
    let fx = fixture("access-alias-grant");
    let (alice, carol) = (fx.alice.as_str(), fx.carol.as_str());
    // `w` gets one more person than `h`, so the two are told apart.
    execute(&fx, Who::Bearer(OPERATOR), "w", WRITE);
    let (in_h, in_w) = (person_count(&fx, "h"), person_count(&fx, "w"));
    assert_eq!(in_w, in_h + 1);
    // An admin grants carol `read` on the alias `hid` (-> `h`): taken, marked as an alias.
    let grant = expect(
        post(
            &fx,
            Who::Bearer(alice),
            "/api/admin/grants",
            json!({"subject": "carol", "database": "HID", "level": "read"}),
        ),
        200,
    );
    assert_eq!(
        grant,
        json!({"subject": "user:carol", "database": "hid", "level": "read", "kind": "alias",
               "target": "h"})
    );
    // Through the alias: reachable at `read`; through the real name: the unknown-name 404,
    // byte for byte the answer of a name that does not exist.
    assert_eq!(person_count_as(&fx, Who::Bearer(carol), "hid"), in_h);
    let refused = post(
        &fx,
        Who::Bearer(carol),
        "/db/hid/api/execute",
        json!({ "script": WRITE }),
    );
    let text = String::from_utf8_lossy(refused.body()).to_lowercase();
    assert!(
        refused.status() != 200 || text.contains("denied") || text.contains("read-only"),
        "a read grant does not write: {text}"
    );
    assert_eq!(person_count(&fx, "h"), in_h);
    let real = get(&fx, Who::Bearer(carol), "/db/h/api/info");
    assert_eq!(real.status(), 404);
    let unknown = get(&fx, Who::Bearer(carol), "/db/zz/api/info");
    assert_eq!(
        String::from_utf8_lossy(real.body()).replace("'h'", "'zz'"),
        String::from_utf8_lossy(unknown.body())
    );
    // Anonymous has no grant on the alias: 401 as for every name.
    assert_unauthorized(&get(&fx, Who::Anonymous, "/db/hid/api/info"), "no grant");
    // "My databases": the alias only, marked, never the real database's own row.
    assert_eq!(
        listed(&fx, Who::Bearer(carol)),
        [row("hid", "read", Some("h"))]
    );
    let mine = expect(get(&fx, Who::Bearer(carol), "/api/databases"), 200);
    assert_eq!(mine["databases"][0]["kind"], "alias", "{mine}");
    let bob_mine = expect(get(&fx, Who::Bearer(&fx.bob), "/api/databases"), 200);
    let kinds: Vec<(&str, &str)> = bob_mine["databases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["name"].as_str().unwrap(), r["kind"].as_str().unwrap()))
        .collect();
    assert_eq!(
        kinds,
        [("cur", "alias"), ("r", "database"), ("w", "database")]
    );

    // MCP: list_graphs shows the alias (not the real database as a graph of its own),
    // use_graph takes the alias and refuses the real name like an unknown one.
    let session = mcp_initialize(&fx, Who::Bearer(carol), "/mcp");
    let (listing, error) = mcp_tool(
        &fx,
        Who::Bearer(carol),
        "/mcp",
        &session,
        "list_graphs",
        json!({}),
    );
    assert!(!error, "{listing}");
    assert!(listing.contains("\"name\":\"hid\""), "{listing}");
    for hidden in ["\"name\":\"h\"", "\"name\":\"w\"", "\"name\":\"r\""] {
        assert!(!listing.contains(hidden), "{hidden}: {listing}");
    }
    let (used, error) = mcp_tool(
        &fx,
        Who::Bearer(carol),
        "/mcp",
        &session,
        "use_graph",
        json!({"name": "hid"}),
    );
    assert!(!error, "{used}");
    assert!(
        used.contains("\"graph\":\"hid\""),
        "the real name stays out: {used}"
    );
    assert!(used.contains("\"readOnly\":true"), "{used}");
    let (real, error) = mcp_tool(
        &fx,
        Who::Bearer(carol),
        "/mcp",
        &session,
        "use_graph",
        json!({"name": "h"}),
    );
    assert!(error, "{real}");
    let (unknown, _) = mcp_tool(
        &fx,
        Who::Bearer(carol),
        "/mcp",
        &session,
        "use_graph",
        json!({"name": "zz"}),
    );
    assert_eq!(real.replace("'h'", "'zz'"), unknown);
    let (before, error) = mcp_person_count(&fx, Who::Bearer(carol), &session);
    assert!(!error, "{before}");
    let (on_h, _) = mcp_tool(
        &fx,
        Who::Bearer(alice),
        "/mcp",
        &mcp_initialize(&fx, Who::Bearer(alice), "/mcp"),
        "query",
        json!({"script": r#"g.v().has_label("person").count()"#, "graph": "h"}),
    );
    assert_eq!(before, result_line(&on_h));
    assert_eq!(before, format!("[{in_h}]"));

    // A flip moves the access at once: HTTP and the open MCP session follow the alias; the
    // old target is hidden again, the new one only through the alias.
    expect(
        put(
            &fx,
            Who::Bearer(alice),
            "/api/admin/aliases/hid",
            json!({"target": "w"}),
        ),
        200,
    );
    assert_eq!(person_count_as(&fx, Who::Bearer(carol), "hid"), in_w);
    assert_eq!(get(&fx, Who::Bearer(carol), "/db/w/api/info").status(), 404);
    assert_eq!(get(&fx, Who::Bearer(carol), "/db/h/api/info").status(), 404);
    let (after, error) = mcp_person_count(&fx, Who::Bearer(carol), &session);
    assert!(!error, "{after}");
    assert_eq!(after, format!("[{in_w}]"), "the session follows the flip");
    let users = expect(get(&fx, Who::Bearer(alice), "/api/admin/users"), 200);
    let marked: Vec<&Value> = users["grants"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|g| g["subject"] == "user:carol")
        .collect();
    assert_eq!(
        marked,
        [
            &json!({"subject": "user:carol", "database": "hid", "level": "read",
                 "kind": "alias", "target": "w"})
        ]
    );
    let carol_row = users["users"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["name"] == "carol")
        .unwrap()
        .clone();
    assert_eq!(
        carol_row["grants"],
        json!([{"database": "hid", "level": "read", "kind": "alias", "target": "w"}])
    );
    let bob_row = users["users"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["name"] == "bob")
        .unwrap()
        .clone();
    assert_eq!(bob_row["grants"][0]["kind"], "database", "{bob_row}");
    assert_eq!(bob_row["grants"][0]["target"], Value::Null, "{bob_row}");

    // A rename carries the grant (and the MCP session) to the new name.
    let renamed = expect(
        post(
            &fx,
            Who::Bearer(alice),
            "/api/admin/aliases/hid/rename",
            json!({"name": "seen"}),
        ),
        200,
    );
    assert_eq!(renamed["grantsMoved"], 1, "{renamed}");
    assert_eq!(person_count_as(&fx, Who::Bearer(carol), "seen"), in_w);
    assert_eq!(
        get(&fx, Who::Bearer(carol), "/db/hid/api/info").status(),
        404
    );
    let (moved, error) = mcp_person_count(&fx, Who::Bearer(carol), &session);
    assert!(!error, "{moved}");
    assert_eq!(moved, after);

    // A removal drops the grant: a later alias of that name gives carol nothing.
    let removed = expect(
        delete(&fx, Who::Bearer(alice), "/api/admin/aliases/seen"),
        200,
    );
    assert_eq!(removed["grantsRemoved"], 1, "{removed}");
    assert_eq!(fx.access.file().grants_on("seen"), 0);
    expect(
        put(
            &fx,
            Who::Bearer(alice),
            "/api/admin/aliases/seen",
            json!({"target": "w"}),
        ),
        200,
    );
    assert_eq!(
        get(&fx, Who::Bearer(carol), "/db/seen/api/info").status(),
        404
    );
    assert!(listed(&fx, Who::Bearer(carol)).is_empty());
}

#[test]
fn an_alias_grant_and_a_real_grant_add_up_capped_by_the_database_mode() {
    let fx = fixture("access-alias-levels");
    let operator = Principal::operator();
    // bob: `r` read (fixture); a write grant on its alias `cur` raises it through `cur` only.
    let grant = Grant::new(Subject::parse("user:bob").unwrap(), "cur", Level::Write).unwrap();
    fx.access.grant(&operator, grant).unwrap();
    let registry = fx.loader.registry();
    let bob = fx.access.file().principal_of("bob").unwrap();
    assert_eq!(fx.access.level_in(registry, &bob, "cur"), Level::Write);
    assert_eq!(fx.access.level_in(registry, &bob, "r"), Level::Read);
    // The database's mode still caps it.
    fx.loader.set_read_only("r", true).unwrap();
    assert_eq!(fx.access.level_in(registry, &bob, "cur"), Level::Read);
    // A `*` grant reaches every database by its name and through every alias, as before.
    let carol = fx.access.file().principal_of("carol").unwrap();
    let all = Grant::new(Subject::parse("user:carol").unwrap(), "*", Level::Read).unwrap();
    fx.access.grant(&operator, all).unwrap();
    for name in ["h", "hid", "w", "cur"] {
        assert_eq!(
            fx.access.level_in(registry, &carol, name),
            Level::Read,
            "{name}"
        );
    }
    let users = expect(get(&fx, Who::Bearer(&fx.alice), "/api/admin/grants"), 200);
    let star = users["grants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["database"] == "*")
        .unwrap()
        .clone();
    assert_eq!(star["kind"], "all", "{star}");
}

/// Unloads `name` through the admin API and waits until it is.
fn unload_as_admin(fx: &Fixture, name: &str) {
    expect(
        post(
            fx,
            Who::Bearer(&fx.alice),
            &format!("/api/admin/databases/{name}/unload"),
            json!({}),
        ),
        200,
    );
}

/// Loads `name` through the admin API and waits until it is.
fn load_as_admin(fx: &Fixture, name: &str) {
    let answer = post(
        fx,
        Who::Bearer(&fx.alice),
        &format!("/api/admin/databases/{name}/load"),
        json!({}),
    );
    assert!(
        matches!(answer.status(), 200 | 202),
        "{}",
        String::from_utf8_lossy(answer.body())
    );
    assert_eq!(fx.loader.wait(name, WAIT).unwrap(), DbState::Loaded);
}

/// Whether `who` reaches `name` over MCP: `use_graph` succeeds and `list_graphs` names it.
fn mcp_reaches(fx: &Fixture, who: Who<'_>, name: &str) -> bool {
    let session = mcp_initialize(fx, who, "/mcp");
    let (listing, error) = mcp_tool(fx, who, "/mcp", &session, "list_graphs", json!({}));
    assert!(!error, "{listing}");
    let listed = listing.contains(&format!("\"name\":\"{name}\""));
    let (used, error) = mcp_tool(
        fx,
        who,
        "/mcp",
        &session,
        "use_graph",
        json!({"name": name}),
    );
    assert_eq!(
        listed, !error,
        "list_graphs and use_graph agree: {listing} / {used}"
    );
    !error
}

#[test]
fn deleting_a_database_drops_its_grants_and_neither_a_new_one_nor_a_restore_revives_them() {
    let fx = fixture("access-delete-grants");
    let (alice, carol) = (fx.alice.clone(), fx.carol.clone());
    expect(
        post(
            &fx,
            Who::Bearer(&alice),
            "/api/admin/grants",
            json!({"subject": "carol", "database": "h", "level": "write"}),
        ),
        200,
    );
    assert_eq!(
        person_count_as(&fx, Who::Bearer(&carol), "h"),
        person_count(&fx, "h")
    );
    assert!(mcp_reaches(&fx, Who::Bearer(&carol), "h"));
    // A backup to restore later; the alias `hid` names `h`, so it goes first.
    let backup = expect(
        post(
            &fx,
            Who::Bearer(&alice),
            "/api/admin/databases/h/backup",
            json!({}),
        ),
        201,
    );
    let backup = backup["backup"].as_str().unwrap().to_string();
    expect(
        delete(&fx, Who::Bearer(&alice), "/api/admin/aliases/hid"),
        200,
    );
    unload_as_admin(&fx, "h");

    // The delete drops carol's grant (audited) and says so.
    let deleted = expect(
        delete(&fx, Who::Bearer(&alice), "/api/admin/databases/h"),
        200,
    );
    assert_eq!(deleted["grantsRemoved"], 1, "{deleted}");
    assert_eq!(fx.access.file().grants_on("h"), 0);
    let lines = fx.audit.lines();
    assert!(
        lines
            .iter()
            .any(|l| l.ends_with("grant of user:carol on 'h' revoked")),
        "{lines:?}"
    );

    // A new database of that name: no leftover to remove, and carol does not reach it.
    let created = expect(
        post(
            &fx,
            Who::Bearer(&alice),
            "/api/admin/databases",
            json!({"name": "h"}),
        ),
        201,
    );
    assert!(created.get("leftoverGrantsRemoved").is_none(), "{created}");
    load_as_admin(&fx, "h");
    assert_eq!(
        get(&fx, Who::Bearer(&carol), "/db/h/api/info").status(),
        404
    );
    assert!(!mcp_reaches(&fx, Who::Bearer(&carol), "h"));

    // Deleted again and restored from the old backup: the grant does not come back.
    unload_as_admin(&fx, "h");
    let deleted = expect(
        delete(&fx, Who::Bearer(&alice), "/api/admin/databases/h"),
        200,
    );
    assert_eq!(deleted["grantsRemoved"], 0, "{deleted}");
    let restored = expect(
        post(
            &fx,
            Who::Bearer(&alice),
            "/api/admin/databases/h/restore",
            json!({"backup": backup}),
        ),
        201,
    );
    assert!(
        restored.get("leftoverGrantsRemoved").is_none(),
        "{restored}"
    );
    assert_eq!(fx.access.file().grants_on("h"), 0);
    load_as_admin(&fx, "h");
    assert_eq!(
        get(&fx, Who::Bearer(&carol), "/db/h/api/info").status(),
        404
    );
    assert!(!mcp_reaches(&fx, Who::Bearer(&carol), "h"));
    // The admin still reaches it, and it holds the backed-up data.
    assert_eq!(person_count(&fx, "h"), person_count(&fx, "r"));
}

#[test]
fn grants_left_on_a_name_are_listed_revocable_and_removed_when_the_name_is_taken() {
    let fx = fixture("access-leftover-grants");
    let (alice, carol) = (fx.alice.clone(), fx.carol.clone());
    // Grants left on names nothing holds (as an older version left them on deleted names).
    let operator = Principal::operator();
    for name in ["gone", "gone-alias", "gone-rename", "kept"] {
        let grant = Grant::new(Subject::parse("user:carol").unwrap(), name, Level::Write).unwrap();
        fx.access.grant(&operator, grant).unwrap();
    }
    // Listed as `missing`, and revocable.
    let grants = expect(get(&fx, Who::Bearer(&alice), "/api/admin/grants"), 200);
    let missing: Vec<&str> = grants["grants"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|g| g["kind"] == "missing")
        .map(|g| g["database"].as_str().unwrap())
        .collect();
    assert_eq!(missing, ["gone", "gone-alias", "gone-rename", "kept"]);
    let revoked = expect(
        delete(
            &fx,
            Who::Bearer(&alice),
            "/api/admin/grants/user:carol/kept",
        ),
        200,
    );
    assert_eq!(revoked["revoked"], true, "{revoked}");

    // A new database takes `gone`: the grant goes first, the answer warns.
    let created = expect(
        post(
            &fx,
            Who::Bearer(&alice),
            "/api/admin/databases",
            json!({"name": "gone"}),
        ),
        201,
    );
    assert_eq!(created["leftoverGrantsRemoved"], 1, "{created}");
    assert_eq!(fx.access.file().grants_on("gone"), 0);
    load_as_admin(&fx, "gone");
    assert_eq!(
        get(&fx, Who::Bearer(&carol), "/db/gone/api/info").status(),
        404
    );
    assert!(!mcp_reaches(&fx, Who::Bearer(&carol), "gone"));

    // A new alias takes `gone-alias`: the same.
    let set = expect(
        put(
            &fx,
            Who::Bearer(&alice),
            "/api/admin/aliases/gone-alias",
            json!({"target": "w"}),
        ),
        200,
    );
    assert_eq!(set["leftoverGrantsRemoved"], 1, "{set}");
    assert_eq!(
        get(&fx, Who::Bearer(&carol), "/db/gone-alias/api/info").status(),
        404
    );
    // A flip of an existing alias removes nothing.
    let flipped = expect(
        put(
            &fx,
            Who::Bearer(&alice),
            "/api/admin/aliases/gone-alias",
            json!({"target": "r"}),
        ),
        200,
    );
    assert!(flipped.get("leftoverGrantsRemoved").is_none(), "{flipped}");

    // A rename onto `gone-rename`: the leftover goes, the alias's own grants (none) move.
    let renamed = expect(
        post(
            &fx,
            Who::Bearer(&alice),
            "/api/admin/aliases/gone-alias/rename",
            json!({"name": "gone-rename"}),
        ),
        200,
    );
    assert_eq!(renamed["leftoverGrantsRemoved"], 1, "{renamed}");
    assert_eq!(renamed["grantsMoved"], 0, "{renamed}");
    assert_eq!(
        get(&fx, Who::Bearer(&carol), "/db/gone-rename/api/info").status(),
        404
    );
    assert!(!mcp_reaches(&fx, Who::Bearer(&carol), "gone-rename"));

    // carol has no grant left; each removal was audited.
    assert!(
        fx.access
            .file()
            .grants()
            .iter()
            .all(|g| g.subject.to_string() != "user:carol")
    );
    let lines = fx.audit.lines();
    for name in ["gone", "gone-alias", "gone-rename", "kept"] {
        let revoked = format!("grant of user:carol on '{name}' revoked");
        assert!(
            lines.iter().any(|l| l.ends_with(&revoked)),
            "{name}: {lines:?}"
        );
    }
    let _ = fx.loader.unload("gone");
}

/// Every answer says `Cache-Control: no-store` exactly once (round 3 #1: a tab shows the
/// current state on every visit, never an answer a browser kept): the root, user and admin API
/// (reads, writes, refusals, unknown routes), each database's API, the UI files, a redirect.
#[test]
fn every_answer_says_no_store_exactly_once() {
    let fx = fixture("no-store");
    let alice = fx.alice.clone();
    let bob = fx.bob.clone();
    let mut answers: Vec<(String, Response)> = Vec::new();
    for (who, url) in [
        (Who::Bearer(OPERATOR), "/api/info"),
        (Who::Anonymous, "/api/info"),
        (Who::Bearer(&bob), "/api/databases"),
        (Who::Anonymous, "/api/databases"),
        (Who::Bearer(&alice), "/api/admin/databases"),
        (Who::Bearer(&alice), "/api/admin/databases/w"),
        (Who::Bearer(&alice), "/api/admin/aliases"),
        (Who::Bearer(&alice), "/api/admin/users"),
        (Who::Bearer(&alice), "/api/admin/grants"),
        (Who::Bearer(&alice), "/api/admin/models"),
        (Who::Bearer(&alice), "/api/admin/imports"),
        (Who::Bearer(&alice), "/api/admin/runs"),
        (Who::Bearer(&alice), "/api/admin/databases/w/backups"),
        (Who::Bearer(&alice), "/api/admin/nope"),
        (Who::Bearer(&bob), "/api/admin/users"),
        (Who::Anonymous, "/api/admin/users"),
        (Who::Bearer(&bob), "/db/w/api/info"),
        (Who::Bearer(&bob), "/db/cur/api/info"),
        (Who::Anonymous, "/db/w/api/info"),
        (Who::Bearer(&bob), "/db/w"),
        (Who::Bearer(&bob), "/api/nope"),
        (Who::Anonymous, "/"),
        (Who::Anonymous, "/server-ui/users-tab.js"),
    ] {
        answers.push((format!("GET {url}"), get(&fx, who, url)));
    }
    answers.push((
        "POST /api/admin/users".to_string(),
        post(
            &fx,
            Who::Bearer(&alice),
            "/api/admin/users",
            json!({ "name": "dave" }),
        ),
    ));
    answers.push((
        "POST /api/info".to_string(),
        post(&fx, Who::Bearer(&alice), "/api/info", json!({})),
    ));
    answers.push((
        "POST /api/login (wrong token)".to_string(),
        post(
            &fx,
            Who::Anonymous,
            "/api/login",
            json!({ "token": "nope" }),
        ),
    ));
    answers.push((
        "PUT /db/w/ (bad credentials)".to_string(),
        send(
            &fx,
            Who::Bearer("wrong"),
            Method::Put,
            "/db/w/api/info",
            None,
            &[],
        ),
    ));
    for (what, answer) in &answers {
        let values: Vec<&str> = answer
            .headers()
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("cache-control"))
            .map(|(_, value)| value.as_str())
            .collect();
        assert_eq!(values, ["no-store"], "{what} ({})", answer.status());
    }
    // The answers above are what the UI reads: a user added meanwhile shows on the next read.
    let users = json_of(&get(&fx, Who::Bearer(&alice), "/api/admin/users"));
    assert!(users.to_string().contains("\"dave\""), "{users}");
}

/// Round 3 B6: the MCP switches of `mcp.toml` through the admin API, enforced on `/mcp` and
/// `/db/<name>/mcp`: off globally (503, sessions kept, back on), a name hidden (unknown on
/// `/mcp`, 404 on its endpoint, HTTP untouched), MCP read-only (writes refused through MCP,
/// the HTTP API with a write grant still writes), revisions (409), admins only.
#[test]
fn mcp_switches_turn_off_hide_and_force_read_only() {
    let fx = fixture("access-mcp-switches");
    let (alice, bob) = (Who::Bearer(fx.alice.as_str()), Who::Bearer(fx.bob.as_str()));
    let state = expect(get(&fx, alice, "/api/admin/mcp"), 200);
    assert_eq!(state["enabled"], true);
    assert_eq!(state["serving"], true);
    assert_eq!(state["endpointBuilt"], true);
    assert_eq!(state["online"], true);
    let names: Vec<&str> = state["databases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["h", "r", "w", "cur", "hid"]);
    let r0 = state["revision"].as_str().unwrap().to_string();
    // Only admins see the routes.
    assert_eq!(get(&fx, bob, "/api/admin/mcp").status(), 404);
    let session = mcp_initialize(&fx, bob, "/mcp");
    let mounted = mcp_initialize(&fx, alice, "/db/w/mcp");
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});

    // Off: both endpoints 503 with help, a stale revision is refused, back on: the sessions go on.
    let off = expect(
        put(
            &fx,
            alice,
            "/api/admin/mcp",
            json!({"enabled": false, "revision": r0}),
        ),
        200,
    );
    assert_eq!(off["enabled"], false);
    assert_eq!(off["serving"], false);
    let stale = expect(
        put(
            &fx,
            alice,
            "/api/admin/mcp",
            json!({"enabled": true, "revision": r0}),
        ),
        409,
    );
    assert_eq!(stale["revision"], off["revision"]);
    for (path, id, who) in [("/mcp", &session, bob), ("/db/w/mcp", &mounted, alice)] {
        let refused = mcp(&fx, who, path, Some(id), list.clone());
        assert_eq!(refused.status(), 503, "{path}");
        let body = json_of(&refused);
        assert_eq!(body["error"], "MCP is switched off on this server");
        assert!(body["help"].as_str().unwrap().contains("mcp on"), "{body}");
    }
    let text = std::fs::read_to_string(fx.loader.registry().dir().mcp_settings_path()).unwrap();
    assert!(text.contains("enabled = false"), "{text}");
    expect(
        put(&fx, alice, "/api/admin/mcp", json!({"enabled": true})),
        200,
    );
    assert_eq!(
        mcp(&fx, bob, "/mcp", Some(&session), list.clone()).status(),
        200
    );
    assert_eq!(
        mcp(&fx, alice, "/db/w/mcp", Some(&mounted), list.clone()).status(),
        200
    );

    // A hidden name: not listed, unknown on use_graph and `graph`, its endpoint 404; HTTP as before.
    let hidden = expect(
        put(
            &fx,
            alice,
            "/api/admin/mcp/databases/W",
            json!({"exposed": false}),
        ),
        200,
    );
    assert_eq!(
        hidden["changed"],
        json!({"name": "w", "exposed": false, "readOnly": false})
    );
    let (listing, _) = mcp_tool(&fx, bob, "/mcp", &session, "list_graphs", json!({}));
    assert!(!listing.contains("\"w\""), "{listing}");
    assert!(listing.contains("\"r\""), "{listing}");
    let (used, error) = mcp_tool(
        &fx,
        bob,
        "/mcp",
        &session,
        "use_graph",
        json!({"name": "w"}),
    );
    assert!(error, "{used}");
    let (unknown, _) = mcp_tool(
        &fx,
        bob,
        "/mcp",
        &session,
        "use_graph",
        json!({"name": "zz"}),
    );
    assert_eq!(used.replace("'w'", "'zz'"), unknown);
    let query = json!({"query": "MATCH (p:person) RETURN count(p) AS n", "graph": "w"});
    let (text, error) = mcp_tool(&fx, bob, "/mcp", &session, "gql", query);
    assert!(error, "{text}");
    let refused = mcp(&fx, alice, "/db/w/mcp", Some(&mounted), list.clone());
    assert_eq!(refused.status(), 404);
    assert_eq!(json_of(&refused)["error"], "MCP is switched off for 'w'");
    let before = person_count(&fx, "w");
    execute(&fx, bob, "w", WRITE);
    assert_eq!(person_count(&fx, "w"), before + 1, "HTTP is not touched");
    // An alias hidden on its own: its database stays listed.
    expect(
        put(
            &fx,
            alice,
            "/api/admin/mcp/databases/cur",
            json!({"exposed": false}),
        ),
        200,
    );
    let (listing, _) = mcp_tool(&fx, bob, "/mcp", &session, "list_graphs", json!({}));
    assert!(!listing.contains("\"cur\""), "{listing}");
    assert!(listing.contains("\"r\""), "{listing}");

    // MCP read-only: bob's write grant on w no longer writes through MCP, still over HTTP.
    let state = expect(
        put(
            &fx,
            alice,
            "/api/admin/mcp/databases/w",
            json!({"exposed": true, "readOnly": true}),
        ),
        200,
    );
    let row = state["databases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "w")
        .unwrap()
        .clone();
    assert_eq!(row["effectiveExposed"], true);
    assert_eq!(row["effectiveReadOnly"], true);
    let (listing, _) = mcp_tool(&fx, bob, "/mcp", &session, "list_graphs", json!({}));
    let graphs: Value = serde_json::from_str(&listing).unwrap();
    let w = graphs["graphs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["name"] == "w")
        .unwrap()
        .clone();
    assert_eq!(w["readOnly"], true, "{listing}");
    let before = person_count(&fx, "w");
    let (text, error) = mcp_tool(
        &fx,
        bob,
        "/mcp",
        &session,
        "query",
        json!({"script": WRITE, "graph": "w"}),
    );
    assert!(error || text.to_lowercase().contains("denied"), "{text}");
    let bob_mounted = mcp_initialize(&fx, bob, "/db/w/mcp");
    let (text, error) = mcp_tool(
        &fx,
        bob,
        "/db/w/mcp",
        &bob_mounted,
        "query",
        json!({"script": WRITE}),
    );
    assert!(error || text.to_lowercase().contains("denied"), "{text}");
    assert_eq!(person_count(&fx, "w"), before, "MCP did not write");
    execute(&fx, bob, "w", WRITE);
    assert_eq!(
        person_count(&fx, "w"),
        before + 1,
        "the HTTP API still writes"
    );
    // Read-only on an alias narrows only the alias, also for a caller granted its database.
    expect(
        put(
            &fx,
            alice,
            "/api/admin/mcp/databases/w",
            json!({"readOnly": false}),
        ),
        200,
    );
    expect(
        put(&fx, alice, "/api/admin/aliases/ww", json!({"target": "w"})),
        200,
    );
    expect(
        put(
            &fx,
            alice,
            "/api/admin/mcp/databases/ww",
            json!({"readOnly": true}),
        ),
        200,
    );
    let admin_session = mcp_initialize(&fx, alice, "/mcp");
    let (_, error) = mcp_tool(
        &fx,
        alice,
        "/mcp",
        &admin_session,
        "use_graph",
        json!({"name": "ww"}),
    );
    assert!(!error);
    let before = person_count(&fx, "w");
    let (text, error) = mcp_tool(
        &fx,
        alice,
        "/mcp",
        &admin_session,
        "query",
        json!({"script": WRITE}),
    );
    assert!(error || text.to_lowercase().contains("denied"), "{text}");
    let (text, error) = mcp_tool(
        &fx,
        alice,
        "/mcp",
        &admin_session,
        "query",
        json!({"script": WRITE, "graph": "w"}),
    );
    assert!(!error, "{text}");
    assert_eq!(person_count(&fx, "w"), before + 1);
    // A rename carries the alias's settings, a removal drops them.
    expect(
        post(
            &fx,
            alice,
            "/api/admin/aliases/ww/rename",
            json!({"name": "wv"}),
        ),
        200,
    );
    let state = expect(get(&fx, alice, "/api/admin/mcp"), 200);
    let find = |name: &str| {
        state["databases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == name)
            .cloned()
    };
    assert_eq!(find("wv").unwrap()["readOnly"], true);
    assert!(find("ww").is_none());
    expect(delete(&fx, alice, "/api/admin/aliases/wv"), 200);
    let state = expect(get(&fx, alice, "/api/admin/mcp"), 200);
    assert!(
        !state["databases"].to_string().contains("\"wv\""),
        "{state}"
    );

    // Refusals: an unknown name, nothing to change, a stale revision of a name's change.
    let unknown = expect(
        put(
            &fx,
            alice,
            "/api/admin/mcp/databases/nope",
            json!({"exposed": false}),
        ),
        404,
    );
    assert!(
        unknown["help"]
            .as_str()
            .unwrap()
            .contains("GET /api/admin/mcp"),
        "{unknown}"
    );
    expect(
        put(&fx, alice, "/api/admin/mcp/databases/w", json!({})),
        400,
    );
    expect(
        put(
            &fx,
            alice,
            "/api/admin/mcp/databases/w",
            json!({"exposed": false, "revision": r0}),
        ),
        409,
    );
    assert_eq!(get(&fx, alice, "/api/admin/mcp/other").status(), 404);
    assert_eq!(post(&fx, alice, "/api/admin/mcp", json!({})).status(), 405);
}

/// Round 3 B6: the usage statistics count open sessions (by user, by database), tool calls per
/// tool, user and database, errors with the recent ones, and requests refused while off.
#[test]
fn mcp_usage_statistics_count_sessions_calls_and_errors() {
    let fx = fixture("access-mcp-usage");
    let (alice, bob) = (Who::Bearer(fx.alice.as_str()), Who::Bearer(fx.bob.as_str()));
    let session = mcp_initialize(&fx, bob, "/mcp");
    let (_, error) = mcp_tool(
        &fx,
        bob,
        "/mcp",
        &session,
        "use_graph",
        json!({"name": "w"}),
    );
    assert!(!error);
    let (_, error) = mcp_tool(
        &fx,
        bob,
        "/mcp",
        &session,
        "use_graph",
        json!({"name": "zz"}),
    );
    assert!(error);
    let mounted = mcp_initialize(&fx, alice, "/db/w/mcp");
    let query = json!({"query": "MATCH (p:person) RETURN count(p) AS n"});
    let (text, error) = mcp_tool(&fx, alice, "/db/w/mcp", &mounted, "gql", query);
    assert!(!error, "{text}");
    expect(
        put(&fx, alice, "/api/admin/mcp", json!({"enabled": false})),
        200,
    );
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    assert_eq!(mcp(&fx, bob, "/mcp", Some(&session), list).status(), 503);
    expect(
        put(&fx, alice, "/api/admin/mcp", json!({"enabled": true})),
        200,
    );

    let state = expect(get(&fx, alice, "/api/admin/mcp"), 200);
    let usage = &state["usage"];
    assert_eq!(
        usage["calls"],
        json!({"total": 3, "errors": 1, "refused": 1})
    );
    assert_eq!(usage["sessions"]["open"], 2);
    assert_eq!(
        usage["sessions"]["byDatabase"],
        json!([{"database": "w", "sessions": 2}])
    );
    assert_eq!(
        usage["sessions"]["byUser"],
        json!([{"user": "alice", "sessions": 1}, {"user": "bob", "sessions": 1}])
    );
    assert_eq!(
        usage["tools"],
        json!([
            {"tool": "use_graph", "calls": 2, "errors": 1},
            {"tool": "gql", "calls": 1, "errors": 0},
        ])
    );
    assert_eq!(
        usage["users"],
        json!([
            {"user": "bob", "calls": 2, "errors": 1},
            {"user": "alice", "calls": 1, "errors": 0},
        ])
    );
    assert_eq!(
        usage["databases"],
        json!([
            {"database": "w", "calls": 2, "errors": 0},
            {"database": "zz", "calls": 1, "errors": 1},
        ])
    );
    let recent = usage["recentErrors"].as_array().unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0]["tool"], "use_graph");
    assert_eq!(recent[0]["user"], "bob");
    assert_eq!(recent[0]["database"], "zz");
    assert!(
        recent[0]["message"].as_str().unwrap().contains("zz"),
        "{recent:?}"
    );
    assert!(recent[0]["atMs"].as_u64().unwrap() > 0);
    let w = state["databases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "w")
        .unwrap()
        .clone();
    assert_eq!(
        (w["sessions"].clone(), w["calls"].clone()),
        (json!(2), json!(2))
    );
    // An ended session leaves the count.
    let ended = send(
        &fx,
        bob,
        Method::Delete,
        "/mcp",
        None,
        &[("Mcp-Session-Id", session.as_str())],
    );
    assert!(ended.status() < 300, "{}", ended.status());
    let state = expect(get(&fx, alice, "/api/admin/mcp"), 200);
    assert_eq!(state["usage"]["sessions"]["open"], 1);
}

/// A script run on `/db/<db>/` as the operator: the answer's JSON, whatever its status.
fn run_as_operator(fx: &Fixture, db: &str, script: &str) -> Value {
    json_of(&post(
        fx,
        Who::Bearer(OPERATOR),
        &format!("/db/{db}/api/execute"),
        json!({ "script": script }),
    ))
}

/// Whether the answer of [`run_as_operator`] is a refusal naming `needle`.
fn refused_with(answer: &Value, needle: &str) -> bool {
    answer["ok"] == Value::Bool(false) && answer.to_string().contains(needle)
}

/// The row of the limit `key` in a settings answer.
fn limit_row(settings: &Value, key: &str) -> Value {
    settings["limits"]["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["key"] == key)
        .unwrap_or_else(|| panic!("no {key}: {settings}"))
        .clone()
}

const HEAVY: &str = "let n = 0; while n < 20000 { n += 1; } n";

#[test]
fn database_settings_show_everything_and_change_with_revisions_validation_and_audit() {
    let fx = fixture("access-settings");
    let alice = Who::Bearer(&fx.alice);
    let url = "/api/admin/databases/w/settings";
    // Admins only.
    assert_eq!(get(&fx, Who::Bearer(&fx.bob), url).status(), 404);
    assert_unauthorized(&get(&fx, Who::Anonymous, url), "anonymous settings");

    // The view of `r` (alias `cur`, bob reads it).
    let view = expect(get(&fx, alice, "/api/admin/databases/r/settings"), 200);
    assert_eq!(view["name"], "r");
    assert!(view.get("requested").is_none(), "{view}");
    assert_eq!(view["aliases"], json!(["cur"]));
    assert_eq!(view["layers"], json!([]));
    let db = &view["database"];
    assert_eq!(db["state"], "loaded");
    assert_eq!(db["kind"], "plain");
    assert_eq!(db["role"], "normal");
    assert_eq!(db["readOnly"], false);
    assert!(db["sizeBytes"].as_u64().unwrap() > 0, "{db}");
    assert!(
        db["memoryBytes"].is_u64() || db["memoryBytes"].is_null(),
        "{db}"
    );
    assert_eq!(view["limits"]["loaded"], true);
    assert_eq!(view["limits"]["reloadNeeded"], false);
    assert!(
        view["limits"]["applies"]
            .as_str()
            .unwrap()
            .contains("next load"),
        "{view}"
    );
    let keys: Vec<&str> = view["limits"]["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, crate::limits::QUERY_LIMIT_KEYS.to_vec());
    let row = limit_row(&view, "max_operations");
    assert_eq!(row["source"], "none");
    assert_eq!(row["override"], Value::Null);
    assert_eq!(row["inEffect"], Value::Null);
    let mcp_names: Vec<&str> = view["mcp"]["names"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .collect();
    assert_eq!(mcp_names, ["r", "cur"], "{view}");
    assert_eq!(view["mcp"]["serving"], true, "{view}");
    assert_eq!(
        view["grants"],
        json!([{"subject": "user:bob", "database": "r", "level": "read", "via": "database"}])
    );
    assert!(
        view["admins"].as_array().unwrap().contains(&json!("alice")),
        "{view}"
    );
    assert_eq!(view["backups"]["count"], 0);
    assert_eq!(view["backups"]["latest"], Value::Null);
    let store = &view["store"];
    assert_eq!(store["storeId"].as_str().unwrap().len(), 32, "{store}");
    assert_eq!(store["damagePolicy"], "maintenance");
    assert_eq!(store["backend"], "directory");
    assert!(store["chunkBytes"].as_u64().unwrap() > 0, "{store}");
    assert!(store["formatVersion"].as_u64().unwrap() > 0, "{store}");
    assert!(
        store["createdAt"].as_str().unwrap().ends_with('Z'),
        "{store}"
    );
    assert_eq!(store["layerBase"], Value::Null);

    // Through the alias; a grant on the alias shows as one; a backup is counted.
    expect(
        post(
            &fx,
            alice,
            "/api/admin/grants",
            json!({"subject": "carol", "database": "cur", "level": "read"}),
        ),
        200,
    );
    expect(
        post(&fx, alice, "/api/admin/databases/r/backup", json!({})),
        201,
    );
    let via = expect(get(&fx, alice, "/api/admin/databases/cur/settings"), 200);
    assert_eq!(via["name"], "r");
    assert_eq!(via["requested"], "cur");
    assert!(
        via["grants"].as_array().unwrap().contains(
            &json!({"subject": "user:carol", "database": "cur", "level": "read", "via": "alias"})
        ),
        "{via}"
    );
    assert_eq!(via["backups"]["count"], 1);
    assert!(via["backups"]["latest"]["backup"].is_string(), "{via}");
    assert_eq!(
        get(&fx, alice, "/api/admin/databases/nope/settings").status(),
        404
    );

    // ETag = the revision; a description change based on it.
    let answer = get(&fx, alice, url);
    let w = json_of(&answer);
    let revision = w["revision"].as_str().unwrap().to_string();
    assert_eq!(
        header(&answer, "ETag"),
        Some(crate::revision::etag(&revision).as_str())
    );
    let changed = expect(
        put(
            &fx,
            alice,
            url,
            json!({"description": "Acme's graph", "revision": revision}),
        ),
        200,
    );
    assert_eq!(changed["changed"], json!(["description"]));
    assert_eq!(changed["database"]["description"], "Acme's graph");
    let new_revision = changed["revision"].as_str().unwrap().to_string();
    assert_ne!(new_revision, revision);
    // The old revision is stale now: 409 with the current one, nothing changed.
    let stale = expect(
        put(
            &fx,
            alice,
            url,
            json!({"description": "lost", "revision": revision}),
        ),
        409,
    );
    assert_eq!(stale["revision"], new_revision.as_str());
    assert!(
        stale["help"].as_str().unwrap().contains("reload"),
        "{stale}"
    );
    let stale = send(
        &fx,
        alice,
        Method::Put,
        url,
        Some(json!({"autoLoad": true})),
        &[("If-Match", &crate::revision::etag(&revision))],
    );
    assert_eq!(stale.status(), 409);
    let now = expect(get(&fx, alice, url), 200);
    assert_eq!(now["database"]["description"], "Acme's graph");
    assert_eq!(now["database"]["autoLoad"], false);

    // Read-only and auto-load in one change: the loaded database is flipped at once.
    let before = person_count(&fx, "w");
    let changed = expect(
        put(
            &fx,
            alice,
            url,
            json!({"readOnly": true, "autoLoad": true, "revision": new_revision}),
        ),
        200,
    );
    assert_eq!(changed["changed"], json!(["readOnly", "autoLoad"]));
    assert_eq!(changed["database"]["readOnly"], true);
    assert_eq!(changed["database"]["autoLoad"], true);
    let _ = run_as_operator(&fx, "w", WRITE);
    assert_eq!(person_count(&fx, "w"), before, "read-only at once");
    let changed = expect(put(&fx, alice, url, json!({"readOnly": false})), 200);
    assert_eq!(changed["database"]["readOnly"], false);
    let _ = run_as_operator(&fx, "w", WRITE);
    assert_eq!(person_count(&fx, "w"), before + 1, "writable again");

    // A limit override: written at once, in effect at the next load.
    assert_eq!(run_as_operator(&fx, "w", HEAVY)["ok"], true);
    let changed = expect(
        put(&fx, alice, url, json!({"limits": {"max_operations": 1000}})),
        200,
    );
    assert_eq!(changed["changed"], json!(["limits.max_operations"]));
    let row = limit_row(&changed, "max_operations");
    assert_eq!(row["override"], 1000);
    assert_eq!(row["effective"], 1000);
    assert_eq!(row["source"], "database");
    assert_eq!(row["inEffect"], Value::Null, "the load's limits");
    assert_eq!(changed["limits"]["reloadNeeded"], true);
    let file = fx
        .loader
        .registry()
        .dir()
        .databases()
        .join("w")
        .join("database.toml");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("[limits.query]\nmax_operations = 1000"),
        "{text}"
    );
    assert!(
        text.contains("description = \"Acme's graph\""),
        "the other settings are kept: {text}"
    );
    assert_eq!(
        run_as_operator(&fx, "w", HEAVY)["ok"],
        true,
        "not before the next load"
    );
    unload_as_admin(&fx, "w");
    let unloaded = expect(get(&fx, alice, url), 200);
    assert_eq!(unloaded["limits"]["loaded"], false);
    assert!(
        limit_row(&unloaded, "max_operations")
            .get("inEffect")
            .is_none()
    );
    load_as_admin(&fx, "w");
    let answer = run_as_operator(&fx, "w", HEAVY);
    assert!(refused_with(&answer, "operations"), "{answer}");
    let loaded = expect(get(&fx, alice, url), 200);
    assert_eq!(loaded["limits"]["reloadNeeded"], false);
    assert_eq!(limit_row(&loaded, "max_operations")["inEffect"], 1000);
    // Cleared: the server's default (none) applies at the next load.
    let changed = expect(
        put(&fx, alice, url, json!({"limits": {"max_operations": null}})),
        200,
    );
    let row = limit_row(&changed, "max_operations");
    assert_eq!(row["override"], Value::Null);
    assert_eq!(row["source"], "none");
    assert_eq!(row["inEffect"], 1000);
    assert_eq!(changed["limits"]["reloadNeeded"], true);
    assert!(
        !std::fs::read_to_string(&file).unwrap().contains("limits"),
        "the empty table is not written"
    );

    // Validation: like server.toml's parser, before anything is written.
    let revision_before = expect(get(&fx, alice, url), 200)["revision"].clone();
    for (body, needle) in [
        (json!({}), "nothing to change"),
        (json!({"limits": {"max_operations": -1}}), "whole number"),
        (json!({"limits": {"max_operations": 1.5}}), "whole number"),
        (json!({"limits": {"timeout_ms": "5"}}), "whole number"),
        (
            json!({"limits": {"max_traverser": 1}}),
            "unknown query limit",
        ),
        (json!({"limits": {"memory_limit": 1}}), "server.toml"),
        (json!({"description": "two\nlines"}), "one line"),
        (json!({"readonly": true}), "unknown field"),
    ] {
        let refusal = expect(put(&fx, alice, url, body.clone()), 400);
        assert!(
            refusal["error"].as_str().unwrap().contains(needle),
            "{body}: {refusal}"
        );
    }
    assert_eq!(
        expect(get(&fx, alice, url), 200)["revision"],
        revision_before
    );
    // A change takes the real name; an alias names its database in the help.
    let refusal = expect(
        put(
            &fx,
            alice,
            "/api/admin/databases/cur/settings",
            json!({"description": "x"}),
        ),
        404,
    );
    assert!(
        refusal["help"]
            .as_str()
            .unwrap()
            .contains("PUT /api/admin/databases/r/settings"),
        "{refusal}"
    );
    assert_eq!(
        send(&fx, alice, Method::Post, url, Some(json!({})), &[]).status(),
        405
    );

    let lines = fx.audit.lines();
    for needle in [
        "database 'w' description set to \"Acme's graph\"",
        "database 'w' read-only on",
        "database 'w' auto-load on",
        "database 'w' query limit max_operations = 1000 (applies at its next load)",
        "database 'w' query limit max_operations cleared",
    ] {
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("user:alice: ") && l.contains(needle)),
            "{needle}: {lines:?}"
        );
    }
    assert!(
        !lines.iter().any(|l| l.contains("lost")),
        "a refused change is not audited: {lines:?}"
    );
}

#[test]
fn a_base_keeps_read_only_and_shows_its_layers_in_the_settings() {
    let fx = fixture("access-settings-base");
    let alice = Who::Bearer(&fx.alice);
    unload_as_admin(&fx, "h");
    expect(
        post(&fx, alice, "/api/admin/databases/h/promote", json!({})),
        200,
    );
    expect(
        post(
            &fx,
            alice,
            "/api/admin/databases",
            json!({"name": "l1", "kind": "layer", "base": "h"}),
        ),
        201,
    );
    let base = expect(get(&fx, alice, "/api/admin/databases/h/settings"), 200);
    assert_eq!(base["database"]["role"], "base");
    assert_eq!(base["layers"], json!(["l1"]));
    let refusal = expect(
        put(
            &fx,
            alice,
            "/api/admin/databases/h/settings",
            json!({"readOnly": false, "description": "not written"}),
        ),
        409,
    );
    assert!(
        refusal["help"].as_str().unwrap().contains("layer over it"),
        "{refusal}"
    );
    let base = expect(get(&fx, alice, "/api/admin/databases/h/settings"), 200);
    assert_eq!(base["database"]["description"], "");
    // Other settings of a base change freely.
    expect(
        put(
            &fx,
            alice,
            "/api/admin/databases/h/settings",
            json!({"description": "frozen", "limits": {"timeout_ms": 0}}),
        ),
        200,
    );
    let layer = expect(get(&fx, alice, "/api/admin/databases/l1/settings"), 200);
    assert_eq!(layer["database"]["kind"], "layer");
    assert_eq!(layer["database"]["base"], "h");
    assert!(layer["store"]["layerBase"].is_string(), "{layer}");
}

/// `GET /api/admin/server-backup`: the operator and admin users only (anonymous `401`, other
/// users the unknown-endpoint `404`, `everyone` of a server without access control `403`: the
/// archive holds the token hashes); a ZIP download, audited.
#[test]
fn the_server_backup_download_is_for_admins_only_and_audited() {
    let fx = fixture("server-backup-access");
    assert_unauthorized(
        &get(&fx, Who::Anonymous, "/api/admin/server-backup"),
        "anonymous",
    );
    for who in [Who::Bearer(&fx.bob), Who::Bearer(&fx.carol)] {
        assert_eq!(get(&fx, who, "/api/admin/server-backup").status(), 404);
    }
    for who in [Who::Bearer(OPERATOR), Who::Bearer(&fx.alice)] {
        let answer = get(&fx, who, "/api/admin/server-backup");
        assert_eq!(
            answer.status(),
            200,
            "{}",
            String::from_utf8_lossy(answer.body())
        );
        assert_eq!(
            header(&answer, "Content-Type"),
            Some("application/octet-stream")
        );
        let disposition = header(&answer, "Content-Disposition").unwrap_or_default();
        assert!(disposition.contains("-server-backup-"), "{disposition}");
        assert_eq!(
            header(&answer, "X-Graphersal-Server-Backup"),
            Some("config")
        );
        assert!(answer.body().starts_with(b"PK\x03\x04"));
    }
    // With the databases (all three loaded: backed up from their open Stores).
    let full = get(
        &fx,
        Who::Bearer(&fx.alice),
        "/api/admin/server-backup?withDatabases=1",
    );
    assert_eq!(
        full.status(),
        200,
        "{}",
        String::from_utf8_lossy(full.body())
    );
    assert_eq!(
        header(&full, "X-Graphersal-Server-Backup"),
        Some("with-databases")
    );
    assert_eq!(header(&full, "X-Graphersal-Databases"), Some("3"));
    assert_eq!(header(&full, "X-Graphersal-Skipped"), Some(""));
    assert_eq!(
        get(
            &fx,
            Who::Bearer(&fx.alice),
            "/api/admin/server-backup?full=1"
        )
        .status(),
        400
    );
    assert_eq!(
        send(
            &fx,
            Who::Bearer(&fx.alice),
            Method::Post,
            "/api/admin/server-backup",
            None,
            &[]
        )
        .status(),
        404
    );
    let lines = fx.audit.lines();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("server backup (configuration only") && l.contains("alice")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("with 3 database(s)")),
        "{lines:?}"
    );

    // Without access control: `everyone` is refused, an admin's token still works.
    let open = fixture_with(
        "server-backup-access-off",
        Vec::new(),
        modes(false, false, super::DEFAULT_COOKIE_MAX_AGE_SECS),
    );
    let refused = get(&open, Who::Anonymous, "/api/admin/server-backup");
    assert_eq!(refused.status(), 403);
    assert!(
        json_of(&refused)["error"]
            .as_str()
            .unwrap()
            .contains("token hashes")
    );
    assert_eq!(
        get(&open, Who::Bearer(&open.alice), "/api/admin/server-backup").status(),
        200
    );
}
