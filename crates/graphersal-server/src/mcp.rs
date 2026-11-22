//! The server's MCP endpoint: ONE `graphersal_mcp::Mcp` for the process (its MCP sessions,
//! token, statistics), built at start from `server.toml` `[mcp]` ([`endpoint`]).
//!
//! - `/mcp` serves every database of the registry through the endpoint's directory mode
//!   (`Mcp::handle_directory` over [`Directory`]): `list_graphs` lists the databases and aliases
//!   with their states, `use_graph` selects one for the MCP session (an alias is resolved at
//!   that moment and the session keeps the real database, also after the alias moves), every
//!   data tool takes an optional `graph` for one call. A database that is not loaded is listed
//!   with its state; using it is a tool error naming the state ("ask an operator to load it"):
//!   the MCP endpoint never loads or unloads (admin only).
//! - `/db/<name>/mcp` is the single-graph endpoint of a loaded database (today's tools), served
//!   by its context (`graphersal_serve`'s `/mcp` route behind the mount), on the same `Mcp`.
//!
//! Both run after the router's `Host`/`Origin` checks; the token (if any) is the endpoint's.

use std::sync::Arc;

use graphersal_mcp::{
    GraphDirectory, GraphEntry, GraphKind, GraphState, Host, Mcp, McpError, McpRequest, Profile,
};
use graphersal_serve::{Request, Response};

use crate::config::McpConfig;
use crate::database::{DatabaseKind, DatabaseRole};
use crate::loader::Loader;
use crate::registry::{DatabaseEntry, DbState};

/// The path of the server-wide endpoint.
pub(crate) const ENDPOINT: &str = "/mcp";

/// The server's MCP endpoint for `config`, `None` when MCP is off.
pub(crate) fn endpoint(config: &McpConfig) -> Option<Arc<Mcp>> {
    config.enabled.then(|| {
        Arc::new(
            Mcp::new(false, config.token.clone(), false)
                .with_endpoint(ENDPOINT)
                .with_max_sessions(config.max_sessions),
        )
    })
}

/// The databases of the registry as the directory of `/mcp` (module docs). Every method
/// answers from the registry and the loader's map: no session job.
pub(crate) struct Directory<'a> {
    pub(crate) loader: &'a Loader,
}

impl GraphDirectory for Directory<'_> {
    fn list(&self) -> Result<Vec<GraphEntry>, McpError> {
        let registry = self.loader.registry();
        let entries = registry.list();
        let mut graphs: Vec<GraphEntry> = entries.iter().map(|e| graph_entry(&e.name, e)).collect();
        for (alias, target) in registry.aliases() {
            if let Some(entry) = entries.iter().find(|e| e.name == target) {
                graphs.push(graph_entry(&alias, entry).with_alias_of(&target));
            }
        }
        Ok(graphs)
    }

    fn resolve(&self, name: &str) -> Result<String, McpError> {
        self.loader
            .registry()
            .resolve(name)
            .map(|entry| entry.name)
            .map_err(|_| McpError::Failed(format!("no database or alias '{name}'")))
    }

    fn host(&self, route: &str) -> Result<(Arc<dyn Host>, Profile), McpError> {
        if let Some(context) = self.loader.context(route) {
            let profile = context.mcp_profile().cloned().unwrap_or_default();
            let host: Arc<dyn Host> = context;
            return Ok((host, profile));
        }
        match self.loader.registry().get(route) {
            Ok(entry) => Err(McpError::Unavailable(not_loaded_text(&entry))),
            Err(_) => Err(McpError::Failed(format!(
                "no database '{route}' (it was deleted)"
            ))),
        }
    }
}

/// The `list_graphs` entry `name` (the database's own name, or an alias of it) for `entry`.
fn graph_entry(name: &str, entry: &DatabaseEntry) -> GraphEntry {
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
        DbState::Loaded => GraphState::Loaded,
        DbState::NeedsModel { .. } => GraphState::NeedsModel,
        DbState::Failed { reason } => GraphState::Failed(reason.clone()),
    };
    let mut graph = GraphEntry::new(name, state)
        .with_kind(kind)
        .with_read_only(base || config.is_some_and(|c| c.read_only));
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
        DbState::Unloaded | DbState::Loaded => {
            format!("database '{name}' is not loaded; ask an operator to load it")
        }
    }
}

/// Answers a request to `/mcp` (after the `Host`/`Origin` checks) on the endpoint `mcp` over
/// the databases of `loader`.
pub(crate) fn route(mcp: &Mcp, loader: &Loader, request: &mut Request<'_>) -> Response {
    let headers = request.headers().to_vec();
    let method = request.method().as_str().to_string();
    let identity = request.identity().map(str::to_string);
    let mut mcp_request = McpRequest::new(&method, request.body());
    for (name, value) in &headers {
        mcp_request = mcp_request.header(name, value);
    }
    if let Some(identity) = &identity {
        mcp_request = mcp_request.with_identity(identity);
    }
    let (status, headers, body) = mcp
        .handle_directory(&Directory { loader }, mcp_request)
        .into_parts();
    let mut answer = Response::new(status, body);
    for (name, value) in &headers {
        answer = answer.with_header(name, value);
    }
    answer
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
        let directory = Directory { loader: &loader };
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

    #[test]
    fn the_endpoint_follows_the_config() {
        let mut config = McpConfig::default();
        let mcp = endpoint(&config).unwrap();
        assert_eq!(mcp.status()["endpoint"], ENDPOINT);
        assert!(!mcp.has_token());
        config.enabled = false;
        assert!(endpoint(&config).is_none());
    }
}
