//! The server's MCP endpoint: ONE `graphersal_mcp::Mcp` for the process (its MCP sessions and
//! statistics), built at start from `server.toml` `[mcp]` ([`endpoint`]).
//!
//! - `/mcp` serves the databases of the registry the caller may see through the endpoint's
//!   directory mode (`Mcp::handle_directory` over a [`Directory`] built per request for the
//!   request's principal): `list_graphs` lists the visible databases and aliases with their
//!   states, `use_graph` selects one for the MCP session (by name, resolved on every call),
//!   every data tool takes an optional `graph` for one call. A database the caller may not see
//!   answers exactly like an unknown name; one it may only read is served with the reading
//!   profile. A database that is not loaded is listed with its state; using it is a tool error
//!   naming the state ("ask an operator to load it"): the MCP endpoint never loads or unloads
//!   (admin only).
//! - `/db/<name>/mcp` is the single-graph endpoint of a loaded database (today's tools), served
//!   by its context (`graphersal_serve`'s `/mcp` route behind the mount), on the same `Mcp`;
//!   the router refuses it like every other route of the database and marks it read-only for a
//!   reader.
//!
//! The runtime switches of `mcp.toml` ([`crate::mcp_control`]) apply to both: off = `503`
//! ([`switched_off`], checked by the router), a hidden name is unknown to the [`Directory`]
//! (and `404` on its own endpoint, [`not_exposed`]), an MCP read-only name gets the reading
//! profile; every tool call is counted ([`route`], the router for `/db/<name>/mcp`).
//!
//! MCP sessions survive a restart: `serve` builds the endpoint with [`persistent_endpoint`]
//! (`sessions/mcp.json`, [`crate::sessions`]), so a client keeps its session id, its identity
//! and its selected name across a restart.
//!
//! Both run after the router's `Host`/`Origin` checks and authentication; the request carries
//! the principal's identity, so an MCP session belongs to the principal that initialized it
//! (another principal, anonymous included, gets the unknown-session `404`). No MCP route of
//! this server resolves a session id to a graph (no session routes), so
//! `Mcp::verified_session_route_for` has no caller here.

use std::sync::Arc;

use graphersal_mcp::{
    GraphDirectory, GraphEntry, GraphKind, GraphState, Host, Mcp, McpError, McpRequest, Profile,
};
use graphersal_serve::{Request, Response};

use crate::access::{Access, AccessFile, Level, Principal};
use crate::config::McpConfig;
use crate::database::{DatabaseKind, DatabaseRole};
use crate::layout::ServerDir;
use crate::loader::Loader;
use crate::mcp_control::{McpUsage, ObservedRequest, ObservedSessions};
use crate::registry::{DatabaseEntry, DbState};
use crate::sessions::McpSessionFile;

/// The path of the server-wide endpoint.
pub(crate) const ENDPOINT: &str = "/mcp";

/// The server's MCP endpoint for `config` with its sessions in memory only (reported to
/// `usage`), `None` when MCP is off (the unit tests; `serve` uses [`persistent_endpoint`]).
#[cfg(test)]
pub(crate) fn endpoint(config: &McpConfig, usage: Arc<McpUsage>) -> Option<Arc<Mcp>> {
    config.enabled.then(|| Arc::new(in_memory(config, usage)))
}

/// The endpoint of `config` whose sessions live in memory only, reported to `usage`.
fn in_memory(config: &McpConfig, usage: Arc<McpUsage>) -> Mcp {
    let store = Arc::new(ObservedSessions::new(None, usage));
    match bare_endpoint(config).with_session_store(store) {
        Ok(mcp) => mcp,
        // An empty store cannot fail to load.
        Err(_) => bare_endpoint(config),
    }
}

/// The endpoint of `config` without a session store.
fn bare_endpoint(config: &McpConfig) -> Mcp {
    Mcp::new(false, None, false)
        .with_endpoint(ENDPOINT)
        .with_max_sessions(config.max_sessions)
}

/// The server's MCP endpoint for `config` whose sessions survive a restart: kept in `dir`'s
/// `sessions/mcp.json` ([`McpSessionFile`], restored now, written through on every change).
/// The stored sessions of a user that is gone or has no token are dropped at this load
/// (revoked while the server was down); every saved table is reported to `usage` (the open
/// sessions of the statistics); `None` when MCP is off.
pub(crate) fn persistent_endpoint(
    config: &McpConfig,
    dir: &ServerDir,
    usage: Arc<McpUsage>,
) -> Option<Arc<Mcp>> {
    if !config.enabled {
        return None;
    }
    let file = match AccessFile::read(dir) {
        Ok(access) => {
            let access = access.unwrap_or_default();
            McpSessionFile::new(dir).keeping(move |identity| {
                match identity.and_then(|id| id.strip_prefix("user:")) {
                    Some(name) => access.user(name).is_some_and(|user| user.token.is_some()),
                    None => true,
                }
            })
        }
        // A broken access.toml stops the start anyway (`Access::open`): keep every session.
        Err(_) => McpSessionFile::new(dir),
    };
    let store = Arc::new(ObservedSessions::new(Some(file), usage.clone()));
    match bare_endpoint(config).with_session_store(store) {
        Ok(mcp) => Some(Arc::new(mcp)),
        Err(err) => {
            tracing::warn!(
                target: "graphersal_server",
                "MCP sessions: {err}; they are kept in memory only"
            );
            Some(Arc::new(in_memory(config, usage)))
        }
    }
}

/// The databases of the registry as the directory of `/mcp` for one caller (module docs),
/// built per request. Every method answers from the registry, the loader's map and the access
/// state: no session job.
pub(crate) struct Directory<'a> {
    pub(crate) loader: &'a Loader,
    pub(crate) access: &'a Access,
    pub(crate) principal: &'a Principal,
}

impl Directory<'_> {
    /// The level granted to the caller on the real database `name`, asked by that name.
    fn granted(&self, name: &str) -> Level {
        self.access.granted_level(self.principal, name)
    }

    /// The error of a name that is unknown or hidden from the caller (the same text for both).
    fn unknown(name: &str) -> McpError {
        McpError::Failed(format!("no database or alias '{name}'"))
    }

    /// Whether MCP offers `requested` (resolving to the database `real`): `mcp.toml`'s
    /// exposure ([`crate::mcp_control`]; everything without switches).
    fn exposed(&self, requested: &str, real: &str) -> bool {
        self.loader
            .mcp_control()
            .is_none_or(|control| control.exposed(requested, real))
    }

    /// Whether MCP serves `requested` (resolving to `real`) read-only by `mcp.toml`.
    fn mcp_read_only(&self, requested: &str, real: &str) -> bool {
        self.loader
            .mcp_control()
            .is_some_and(|control| control.read_only(requested, real))
    }

    /// Whether the alias name `requested` has MCP settings of its own (then a session that
    /// selected it is routed by that name, so they apply on every call).
    fn own_settings(&self, requested: &str) -> bool {
        self.loader
            .mcp_control()
            .is_some_and(|control| control.has_own(requested))
    }
}

impl GraphDirectory for Directory<'_> {
    fn list(&self) -> Result<Vec<GraphEntry>, McpError> {
        let registry = self.loader.registry();
        let mut graphs: Vec<GraphEntry> = registry
            .list()
            .into_iter()
            .filter_map(|e| {
                let level = self.granted(&e.name);
                (level != Level::None && self.exposed(&e.name, &e.name)).then(|| {
                    let read_only = self.mcp_read_only(&e.name, &e.name);
                    graph_entry(&e.name, &e, level, read_only)
                })
            })
            .collect();
        // An alias is listed when it reaches its target for the caller: a grant on the target
        // or on the alias itself (`Access::reach`); both names exposed to MCP.
        for (alias, _) in registry.aliases() {
            if let Some((entry, level)) = self.access.reach(registry, self.principal, &alias)
                && self.exposed(&alias, &entry.name)
            {
                let read_only = self.mcp_read_only(&alias, &entry.name);
                graphs
                    .push(graph_entry(&alias, &entry, level, read_only).with_alias_of(&entry.name));
            }
        }
        Ok(graphs)
    }

    /// The route of `name`: the real database's name when the caller may reach it by that
    /// name, else (a grant on the alias only) the alias's own name, so the real name stays
    /// hidden and [`GraphDirectory::host`] resolves the alias again on every call (a flip
    /// moves the session along).
    fn resolve(&self, name: &str) -> Result<String, McpError> {
        let registry = self.loader.registry();
        match self.access.reach(registry, self.principal, name) {
            Some((entry, _)) if !self.exposed(name, &entry.name) => Err(Self::unknown(name)),
            Some((entry, _))
                if self.granted(&entry.name) != Level::None
                    && (crate::names::fold(name) == entry.name || !self.own_settings(name)) =>
            {
                Ok(entry.name)
            }
            Some(_) => Ok(crate::names::fold(name)),
            None => Err(Self::unknown(name)),
        }
    }

    fn host(&self, route: &str) -> Result<(Arc<dyn Host>, Profile), McpError> {
        let Some((entry, level)) = self
            .access
            .reach(self.loader.registry(), self.principal, route)
        else {
            return Err(Self::unknown(route));
        };
        if !self.exposed(route, &entry.name) {
            return Err(Self::unknown(route));
        }
        if let Some(context) = self.loader.context(&entry.name) {
            let profile = context.mcp_profile().cloned().unwrap_or_default();
            let profile = if level == Level::Read || self.mcp_read_only(route, &entry.name) {
                profile.with_read_only(true)
            } else {
                profile
            };
            let host: Arc<dyn Host> = context;
            return Ok((host, profile));
        }
        Err(McpError::Unavailable(not_loaded_text(&entry)))
    }
}

/// The `list_graphs` entry `name` (the database's own name, or an alias of it) for `entry`;
/// read-only also when the caller's `level` is only [`Level::Read`] or `mcp_read_only`
/// (`mcp.toml`).
fn graph_entry(name: &str, entry: &DatabaseEntry, level: Level, mcp_read_only: bool) -> GraphEntry {
    let config = entry.config.as_ref();
    let base = config.is_some_and(|c| c.role == DatabaseRole::Base);
    let kind = if base {
        GraphKind::Base
    } else if config.is_some_and(|c| c.kind == DatabaseKind::Layer) {
        GraphKind::Layer
    } else {
        GraphKind::Plain
    };
    let state = match &entry.state {
        DbState::Unloaded => GraphState::Unloaded,
        DbState::Loading => GraphState::Loading,
        DbState::Loaded | DbState::Maintenance { .. } => GraphState::Loaded,
        DbState::NeedsModel { .. } => GraphState::NeedsModel,
        DbState::Failed { reason } => GraphState::Failed(reason.clone()),
    };
    let mut graph = GraphEntry::new(name, state).with_kind(kind).with_read_only(
        base || config.is_some_and(|c| c.read_only) || level == Level::Read || mcp_read_only,
    );
    if let Some(description) = config
        .map(|c| c.description.as_str())
        .filter(|d| !d.is_empty())
    {
        graph = graph.with_description(description);
    }
    graph
}

/// The tool error of a call to the database `entry` that is not loaded, naming its state and
/// what to do (an agent cannot load it: only an operator can).
fn not_loaded_text(entry: &DatabaseEntry) -> String {
    let name = &entry.name;
    match &entry.state {
        DbState::Loading => {
            format!("database '{name}' is not loaded: it is loading; retry in a moment")
        }
        DbState::Failed { reason } => format!(
            "database '{name}' is not loaded: it failed ({reason}); ask an operator to fix it \
             and load it"
        ),
        DbState::NeedsModel { repo } => format!(
            "database '{name}' is not loaded: it needs the embedding model '{repo}'; ask an \
             operator to add the model and load it"
        ),
        // `loaded`: it was unloaded a moment ago.
        DbState::Unloaded | DbState::Loaded | DbState::Maintenance { .. } => {
            format!("database '{name}' is not loaded; ask an operator to load it")
        }
    }
}

/// Answers a request to `/mcp` (after the `Host`/`Origin` checks and authentication; the
/// request carries the principal's identity) on the endpoint `mcp` over `directory`; its tool
/// calls are counted in the loader's MCP statistics ([`crate::mcp_control::McpUsage`]).
pub(crate) fn route(mcp: &Mcp, directory: &Directory<'_>, request: &mut Request<'_>) -> Response {
    let headers = request.headers().to_vec();
    let method = request.method().as_str().to_string();
    let identity = request.identity().map(str::to_string);
    let usage = directory.loader.mcp_control().map(|c| c.usage().clone());
    let buffered = match &usage {
        Some(_) => match buffer_body(request) {
            Ok(body) => body,
            Err(answer) => return answer,
        },
        None => None,
    };
    let mut mcp_request = match &buffered {
        Some(body) => McpRequest::new(&method, body.as_slice()),
        None => McpRequest::new(&method, request.body()),
    };
    for (name, value) in &headers {
        mcp_request = mcp_request.header(name, value);
    }
    if let Some(identity) = &identity {
        mcp_request = mcp_request.with_identity(identity);
    }
    let (status, answer_headers, body) = mcp.handle_directory(directory, mcp_request).into_parts();
    if let (Some(usage), Some(sent)) = (&usage, &buffered) {
        let session = header_of(&headers, SESSION_HEADER);
        usage.observe(
            &ObservedRequest {
                identity: identity.as_deref(),
                mount: None,
                session: session.as_deref(),
                new_session: None,
                body: sent,
            },
            status,
            &body,
        );
    }
    let mut answer = Response::new(status, body);
    for (name, value) in &answer_headers {
        answer = answer.with_header(name, value);
    }
    answer
}

/// The MCP session header.
pub(crate) const SESSION_HEADER: &str = "Mcp-Session-Id";

/// The largest body read to count its tool calls (the endpoint's own body limit, 16 MiB): a
/// larger declared body is passed on unread (the endpoint refuses it).
const MAX_OBSERVED_BODY: usize = 16 * 1024 * 1024;

/// The first header `name` of `headers` (case-insensitive).
pub(crate) fn header_of(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(field, _)| field.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

/// The body of a `POST` to an MCP endpoint, read into memory (at most one byte over the
/// endpoint's limit, so the endpoint still sees an oversize body as one) to count its tool
/// calls; `None` for another method or a declared body over the limit; `Err`: the answer when
/// the body cannot be read.
pub(crate) fn buffer_body(request: &mut Request<'_>) -> Result<Option<Vec<u8>>, Response> {
    use std::io::Read;
    if request.method().as_str() != "POST"
        || request
            .body_length()
            .is_some_and(|len| len > MAX_OBSERVED_BODY)
    {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    match request
        .body()
        .take(MAX_OBSERVED_BODY as u64 + 1)
        .read_to_end(&mut bytes)
    {
        Ok(_) => Ok(Some(bytes)),
        Err(err) => Err(Response::json(
            400,
            &serde_json::json!({ "error": format!("cannot read the body: {err}") }),
        )),
    }
}

/// The answer of an MCP endpoint while MCP is switched off (`mcp.toml` `enabled = false`).
pub(crate) fn switched_off() -> Response {
    Response::json(
        503,
        &serde_json::json!({
            "error": "MCP is switched off on this server",
            "help": crate::mcp_control::SWITCHED_OFF_HELP,
        }),
    )
}

/// The answer of `/db/<name>/mcp` for a name MCP does not offer (`mcp.toml` `exposed =
/// false` on the name or its database).
pub(crate) fn not_exposed(name: &str) -> Response {
    Response::json(
        404,
        &serde_json::json!({
            "error": format!("MCP is switched off for '{name}'"),
            "help": "an admin offers it again in the web UI's MCP tab, with PUT \
                     /api/admin/mcp/databases/<name> {\"exposed\": true}, or with \
                     graphersal-server mcp db NAME on --dir DIR",
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Registry;
    use crate::registry::tests::{add_db, server};
    use crate::test_support::TempDir;
    use graphersal_serve::{Exposure, HostCheck};

    #[test]
    fn the_directory_lists_resolves_and_refuses_without_a_session_job() {
        let temp = TempDir::new("mcp-directory");
        let dir = server(&temp);
        add_db(&dir, "a", Some("description = \"first\"\n"), true);
        add_db(
            &dir,
            "base1",
            Some("role = \"base\"\nread_only = true\n"),
            true,
        );
        add_db(&dir, "broken", None, false);
        let registry = Registry::scan(&dir).unwrap();
        registry.alias_set("cur", "a").unwrap();
        let loader = Loader::new(
            registry,
            HostCheck::new(Exposure::Loopback, 8484, "http://127.0.0.1:8484/"),
        );
        let (access, _) = Access::open(&dir, None, crate::audit::Audit::default()).unwrap();
        let operator = Principal::operator();
        let directory = Directory {
            loader: &loader,
            access: &access,
            principal: &operator,
        };
        let graphs = directory.list().unwrap();
        let find = |name: &str| graphs.iter().find(|g| g.name() == name).unwrap();
        assert_eq!(find("a").state(), &GraphState::Unloaded);
        assert_eq!(find("a").description(), Some("first"));
        assert_eq!(find("base1").kind(), GraphKind::Base);
        assert!(find("base1").read_only());
        assert_eq!(find("broken").state().as_str(), "failed");
        assert_eq!(find("cur").alias_of(), Some("a"));
        assert_eq!(directory.resolve("CUR").unwrap(), "a");
        assert!(directory.resolve("none").is_err());
        let err = directory.host("a").err().unwrap().to_string();
        assert_eq!(
            err,
            "database 'a' is not loaded; ask an operator to load it"
        );
        let err = directory.host("broken").err().unwrap().to_string();
        assert!(err.contains("it failed"), "{err}");
        assert!(directory.host("gone").is_err());
    }

    /// The directory of one caller: only what it may see, hidden = unknown, the reading
    /// profile for a reader.
    #[test]
    fn the_directory_shows_each_principal_its_own_databases() {
        let temp = TempDir::new("mcp-directory-access");
        let dir = server(&temp);
        add_db(&dir, "a", None, true);
        add_db(&dir, "b", None, true);
        add_db(&dir, "c", None, true);
        let registry = Registry::scan(&dir).unwrap();
        registry.alias_set("cur", "b").unwrap();
        registry.alias_set("hid", "c").unwrap();
        let loader = Loader::new(
            registry,
            HostCheck::new(Exposure::Loopback, 8484, "http://127.0.0.1:8484/"),
        );
        let (access, _) = Access::open(&dir, None, crate::audit::Audit::default()).unwrap();
        let operator = Principal::operator();
        access.add_user(&operator, "bob", Vec::new()).unwrap();
        for (database, level) in [("a", Level::Write), ("b", Level::Read)] {
            let grant = crate::access::Grant::new(
                crate::access::Subject::parse("user:bob").unwrap(),
                database,
                level,
            )
            .unwrap();
            access.grant(&operator, grant).unwrap();
        }
        let bob = access.file().principal_of("bob").unwrap();
        let directory = Directory {
            loader: &loader,
            access: &access,
            principal: &bob,
        };
        let graphs = directory.list().unwrap();
        let mut names: Vec<&str> = graphs.iter().map(GraphEntry::name).collect();
        names.sort_unstable();
        assert_eq!(names, ["a", "b", "cur"]);
        let find = |name: &str| graphs.iter().find(|g| g.name() == name).unwrap();
        assert!(!find("a").read_only());
        assert!(find("b").read_only(), "a reader sees the graph read-only");
        assert!(find("cur").read_only());
        assert_eq!(directory.resolve("cur").unwrap(), "b");
        let hidden = directory.resolve("c").err().unwrap().to_string();
        let unknown = directory.resolve("zz").err().unwrap().to_string();
        assert_eq!(hidden, "no database or alias 'c'");
        assert_eq!(unknown, "no database or alias 'zz'");
        assert_eq!(
            directory.resolve("hid").err().unwrap().to_string(),
            "no database or alias 'hid'"
        );
        assert_eq!(
            directory.host("c").err().unwrap().to_string(),
            "no database or alias 'c'"
        );
        // Not loaded: the state text, for a visible database only.
        assert!(
            directory
                .host("a")
                .err()
                .unwrap()
                .to_string()
                .contains("not loaded")
        );
        let anonymous = Principal::anonymous();
        let nobody = Directory {
            loader: &loader,
            access: &access,
            principal: &anonymous,
        };
        assert!(nobody.list().unwrap().is_empty());
        assert!(nobody.resolve("a").is_err());
    }

    #[test]
    fn the_endpoint_follows_the_config() {
        let mut config = McpConfig::default();
        let usage = Arc::new(McpUsage::new());
        let mcp = endpoint(&config, usage.clone()).unwrap();
        assert_eq!(mcp.status()["endpoint"], ENDPOINT);
        assert!(!mcp.has_token());
        config.enabled = false;
        assert!(endpoint(&config, usage).is_none());
    }
}
