//! `server.toml`: the configuration of a server directory.
//!
//! ```toml
//! listen = "127.0.0.1:8484"   # IP:PORT (or localhost:PORT); port 0 picks a free port
//!
//! [mcp]
//! enabled = true              # /mcp over every database, /db/<name>/mcp per loaded database
//! max_sessions = 256          # MCP sessions remembered (the oldest forgotten first)
//!
//! [defaults]                  # the creation parameters of new databases
//! chunk_bytes = 1048576
//! damage_policy = "maintenance"
//!
//! [limits]
//! upload_bytes = 4294967296   # the largest import upload (POST /api/admin/import), 4 GiB
//! max_connections = 256       # open connections served at once (more: 503 + Retry-After)
//! header_timeout_secs = 30    # a request's head must arrive within this (slow clients)
//! idle_timeout_secs = 60      # a connection without a running request is closed after this
//!
//! [sessions]                  # browser sessions (they survive a restart); unlimited by default
//! lifetime_secs = 2592000     # the longest a session lasts after its login
//! idle_timeout_secs = 604800  # the longest a session lasts after its last request
//! ```
//!
//! Every key is optional (its default applies); an unknown key is an error (a typo must not be
//! ignored silently).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use graphersal::persist::{DEFAULT_CHUNK_BYTES, DamagePolicy};
use serde::Deserialize;

use crate::access::SessionLimits;
use crate::error::ServerError;

/// The default listen address (loopback only: tokens travel in clear text over plain HTTP).
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8484";

/// The contents of `server.toml`.
///
/// `#[non_exhaustive]`: further settings will be added.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ServerConfig {
    /// The address the server listens on (`IP:PORT`, or `localhost:PORT`).
    #[serde(default = "default_listen")]
    pub listen: String,
    /// The MCP endpoint's options.
    #[serde(default)]
    pub mcp: McpConfig,
    /// The creation parameters of new databases.
    #[serde(default)]
    pub defaults: DatabaseDefaults,
    /// The request limits.
    #[serde(default)]
    pub limits: Limits,
    /// How long browser sessions last.
    #[serde(default)]
    pub sessions: SessionsConfig,
}

/// How long browser sessions last (`[sessions]` of `server.toml`), read at start. Both keys
/// are optional: a missing key is no limit (the default). Sessions survive a restart either
/// way (there is no setting for that: `crate::sessions`).
///
/// `#[non_exhaustive]`: further settings may be added.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct SessionsConfig {
    /// The longest a browser session lasts after its login, in seconds (at least 1; missing:
    /// no limit). The browser logs in again afterwards.
    #[serde(default)]
    pub lifetime_secs: Option<u64>,
    /// The longest a browser session lasts after its last request, in seconds (at least 1;
    /// missing: no limit). Counted coarsely: the last request is recorded at most every minute.
    #[serde(default)]
    pub idle_timeout_secs: Option<u64>,
}

impl SessionsConfig {
    /// The limits the browser sessions run with.
    pub fn limits(&self) -> SessionLimits {
        SessionLimits::unlimited()
            .with_lifetime(self.lifetime_secs.map(Duration::from_secs))
            .with_idle_timeout(self.idle_timeout_secs.map(Duration::from_secs))
    }
}

/// The default largest import upload: 4 GiB.
pub const DEFAULT_UPLOAD_BYTES: u64 = 4 << 30;

/// The default largest number of open connections served at once.
pub const DEFAULT_MAX_CONNECTIONS: usize = 256;

/// The default time a request's head may take to arrive, in seconds.
pub const DEFAULT_HEADER_TIMEOUT_SECS: u64 = 30;

/// The default time a connection without a running request stays open, in seconds.
pub const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 60;

/// The request limits of `server.toml` (`[limits]`).
///
/// `#[non_exhaustive]`: further limits will be added.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Limits {
    /// The largest body of an import upload (`POST /api/admin/import` with the file as the
    /// body), in bytes (default [`DEFAULT_UPLOAD_BYTES`], 4 GiB); at least 1. The body is
    /// streamed into `tmp/`, never held in memory; a larger one is refused with `413`.
    #[serde(default = "default_upload_bytes")]
    pub upload_bytes: u64,
    /// The largest number of open connections the server serves at once (default
    /// [`DEFAULT_MAX_CONNECTIONS`]); at least 1. A connection over it is answered `503` with
    /// `Retry-After` and closed. Read at start; `serve --max-connections N` overrides it.
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    /// How long a request's head (request line and headers) may take to arrive, in seconds,
    /// counted from the moment the server waits for it (a new connection, or the end of the
    /// previous answer on a kept-alive HTTP/1.1 connection); default
    /// [`DEFAULT_HEADER_TIMEOUT_SECS`], at least 1. A slower client is disconnected. A body is
    /// never cut by it (a body read waits at most 120 s for each next piece instead). Read at
    /// start; `serve --header-timeout SECS` overrides it.
    #[serde(default = "default_header_timeout_secs")]
    pub header_timeout_secs: u64,
    /// How long a connection without a running request stays open, in seconds (default
    /// [`DEFAULT_IDLE_TIMEOUT_SECS`]); at least 1. HTTP/1.1 waits for the next request's head
    /// at most the shorter of this and `header_timeout_secs`. Read at start;
    /// `serve --idle-timeout SECS` overrides it.
    #[serde(default = "default_idle_timeout_secs")]
    pub idle_timeout_secs: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            upload_bytes: DEFAULT_UPLOAD_BYTES,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            header_timeout_secs: DEFAULT_HEADER_TIMEOUT_SECS,
            idle_timeout_secs: DEFAULT_IDLE_TIMEOUT_SECS,
        }
    }
}

fn default_upload_bytes() -> u64 {
    DEFAULT_UPLOAD_BYTES
}

fn default_max_connections() -> usize {
    DEFAULT_MAX_CONNECTIONS
}

fn default_header_timeout_secs() -> u64 {
    DEFAULT_HEADER_TIMEOUT_SECS
}

fn default_idle_timeout_secs() -> u64 {
    DEFAULT_IDLE_TIMEOUT_SECS
}

/// The default number of MCP sessions the server remembers ([`McpConfig::max_sessions`]).
pub const DEFAULT_MCP_SESSIONS: usize = 256;

/// The MCP options of `server.toml` (`[mcp]`), read when the server starts.
///
/// `#[non_exhaustive]`: further options will be added.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct McpConfig {
    /// Whether the server offers its MCP endpoints: `/mcp` over every database and
    /// `/db/<name>/mcp` per loaded database (default `true`).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// How many MCP sessions the server remembers, the oldest forgotten first (its client
    /// initializes again and selects its graph again); default [`DEFAULT_MCP_SESSIONS`], at
    /// least 1. One table for `/mcp` and every `/db/<name>/mcp`.
    #[serde(default = "default_mcp_sessions")]
    pub max_sessions: usize,
}

fn default_mcp_sessions() -> usize {
    DEFAULT_MCP_SESSIONS
}

/// The creation parameters of new databases (`[defaults]` of `server.toml`).
///
/// `#[non_exhaustive]`: further parameters will be added.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct DatabaseDefaults {
    /// The uncompressed snapshot chunk size of a new Store (default
    /// [`graphersal::persist::DEFAULT_CHUNK_BYTES`], 1 MiB); at least 1.
    #[serde(default = "default_chunk_bytes")]
    pub chunk_bytes: u32,
    /// The damage policy of a new Store: `"maintenance"` (default) or `"continue"`.
    #[serde(default = "default_damage_policy")]
    pub damage_policy: String,
}

fn default_listen() -> String {
    DEFAULT_LISTEN.to_string()
}

fn default_true() -> bool {
    true
}

fn default_chunk_bytes() -> u32 {
    DEFAULT_CHUNK_BYTES
}

fn default_damage_policy() -> String {
    DamagePolicy::default().name().to_string()
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            mcp: McpConfig::default(),
            defaults: DatabaseDefaults::default(),
            limits: Limits::default(),
            sessions: SessionsConfig::default(),
        }
    }
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_sessions: DEFAULT_MCP_SESSIONS,
        }
    }
}

impl Default for DatabaseDefaults {
    fn default() -> Self {
        Self {
            chunk_bytes: default_chunk_bytes(),
            damage_policy: default_damage_policy(),
        }
    }
}

impl ServerConfig {
    /// Parses and checks the text of `server.toml` (`path` names the file in errors).
    pub fn parse(text: &str, path: &Path) -> Result<Self, ServerError> {
        let config: ServerConfig = toml::from_str(text).map_err(|err| ServerError::Config {
            path: path.to_path_buf(),
            message: err.message().to_string(),
        })?;
        let invalid = |message: String| ServerError::Config {
            path: path.to_path_buf(),
            message,
        };
        parse_listen(&config.listen).map_err(|err| invalid(format!("listen: {err}")))?;
        if config.defaults.chunk_bytes == 0 {
            return Err(invalid(
                "defaults.chunk_bytes: must be at least 1 (default 1048576)".to_string(),
            ));
        }
        DamagePolicy::parse(&config.defaults.damage_policy)
            .map_err(|err| invalid(format!("defaults.damage_policy: {err}")))?;
        if config.mcp.max_sessions == 0 {
            return Err(invalid(format!(
                "mcp.max_sessions: must be at least 1 (default {DEFAULT_MCP_SESSIONS})"
            )));
        }
        if config.limits.upload_bytes == 0 {
            return Err(invalid(format!(
                "limits.upload_bytes: must be at least 1 (default {DEFAULT_UPLOAD_BYTES})"
            )));
        }
        for (key, value, default) in [
            (
                "max_connections",
                config.limits.max_connections as u64,
                DEFAULT_MAX_CONNECTIONS as u64,
            ),
            (
                "header_timeout_secs",
                config.limits.header_timeout_secs,
                DEFAULT_HEADER_TIMEOUT_SECS,
            ),
            (
                "idle_timeout_secs",
                config.limits.idle_timeout_secs,
                DEFAULT_IDLE_TIMEOUT_SECS,
            ),
        ] {
            if value == 0 {
                return Err(invalid(format!(
                    "limits.{key}: must be at least 1 (default {default})"
                )));
            }
        }
        for (key, value) in [
            ("lifetime_secs", config.sessions.lifetime_secs),
            ("idle_timeout_secs", config.sessions.idle_timeout_secs),
        ] {
            if value == Some(0) {
                return Err(invalid(format!(
                    "sessions.{key}: must be at least 1 (leave it out for no limit)"
                )));
            }
        }
        Ok(config)
    }

    /// The listen address (checked by [`Self::parse`]).
    pub fn listen_addr(&self) -> Result<SocketAddr, ServerError> {
        parse_listen(&self.listen)
    }

    /// The damage policy of new databases (checked by [`Self::parse`]).
    pub fn damage_policy(&self) -> Result<DamagePolicy, ServerError> {
        DamagePolicy::parse(&self.defaults.damage_policy).map_err(|message| ServerError::Config {
            path: Path::new(crate::layout::CONFIG_FILE).to_path_buf(),
            message,
        })
    }
}

/// Parses a listen address: `IP:PORT` (IPv6 in brackets) or `localhost:PORT` (the IPv4
/// loopback). No other host names: the address must not depend on name resolution.
pub fn parse_listen(value: &str) -> Result<SocketAddr, ServerError> {
    let trimmed = value.trim();
    if let Ok(addr) = trimmed.parse::<SocketAddr>() {
        return Ok(addr);
    }
    let refused = |message: &str| ServerError::InvalidListen {
        value: value.to_string(),
        message: message.to_string(),
    };
    match trimmed.rsplit_once(':') {
        Some((host, port)) if host.eq_ignore_ascii_case("localhost") => port
            .parse::<u16>()
            .map(|port| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port))
            .map_err(|_| refused("the port is not a number from 0 to 65535")),
        Some(_) => Err(refused("not an IP address with a port")),
        None => Err(refused("no port")),
    }
}

/// The text `init` writes into a new `server.toml`: every setting with its default and a
/// comment (it parses to [`ServerConfig::default`]).
pub fn default_config_text() -> String {
    let defaults = DatabaseDefaults::default();
    format!(
        "# graphersal-server configuration (EXPERIMENTAL).\n\
         \n\
         # The address the server listens on: IP:PORT (or localhost:PORT); port 0 picks a free\n\
         # port. Requests need a user token or a browser login (access.toml); over plain HTTP\n\
         # tokens travel in clear text: keep it on a loopback address (127.0.0.1, [::1]) or\n\
         # behind a TLS proxy; any other address is reachable from the network (serve warns).\n\
         # `graphersal-server serve --listen ADDR` overrides it.\n\
         listen = \"{DEFAULT_LISTEN}\"\n\
         \n\
         [mcp]\n\
         # The MCP endpoints: /mcp over every database (list_graphs, use_graph) and\n\
         # /db/<name>/mcp per loaded database. Read when the server starts.\n\
         enabled = true\n\
         # How many MCP sessions are remembered (the oldest forgotten first).\n\
         max_sessions = {}\n\
         \n\
         [defaults]\n\
         # The creation parameters of new databases (fixed for each database's life).\n\
         # Uncompressed snapshot chunk size in bytes.\n\
         chunk_bytes = {}\n\
         # What a Store does on damage: \"maintenance\" (read-only until repaired) or \"continue\".\n\
         damage_policy = \"{}\"\n\
         \n\
         [limits]\n\
         # The largest import upload (POST /api/admin/import, the file as the body) in bytes;\n\
         # it is streamed into tmp/, never held in memory.\n\
         upload_bytes = {}\n\
         # Open connections served at once; one more is answered 503 with Retry-After.\n\
         max_connections = {}\n\
         # Seconds a request's head (request line and headers) may take to arrive.\n\
         header_timeout_secs = {}\n\
         # Seconds a connection without a running request stays open.\n\
         idle_timeout_secs = {}\n\
         \n\
         [sessions]\n\
         # Browser sessions (the login cookie) and MCP sessions survive a restart (kept in\n\
         # sessions/, hashes only). How long a browser session lasts, in seconds; no limit\n\
         # when left out. Read when the server starts.\n\
         # Since its login (30 days here):\n\
         # lifetime_secs = 2592000\n\
         # Since its last request (7 days here; counted coarsely, up to a minute):\n\
         # idle_timeout_secs = 604800\n",
        DEFAULT_MCP_SESSIONS,
        defaults.chunk_bytes,
        defaults.damage_policy,
        DEFAULT_UPLOAD_BYTES,
        DEFAULT_MAX_CONNECTIONS,
        DEFAULT_HEADER_TIMEOUT_SECS,
        DEFAULT_IDLE_TIMEOUT_SECS
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<ServerConfig, ServerError> {
        ServerConfig::parse(text, Path::new("server.toml"))
    }

    #[test]
    fn the_default_text_parses_to_the_defaults() {
        let config = parse(&default_config_text()).unwrap();
        assert_eq!(config, ServerConfig::default());
        assert_eq!(config.listen_addr().unwrap().to_string(), "127.0.0.1:8484");
        assert_eq!(config.damage_policy().unwrap(), DamagePolicy::Maintenance);
        assert_eq!(config.defaults.chunk_bytes, 1 << 20);
        assert!(config.mcp.enabled);
    }

    #[test]
    fn the_upload_limit_defaults_to_4_gib_and_is_at_least_1() {
        assert_eq!(parse("").unwrap().limits.upload_bytes, 4 << 30);
        assert_eq!(
            parse(&default_config_text()).unwrap().limits.upload_bytes,
            DEFAULT_UPLOAD_BYTES
        );
        assert_eq!(
            parse("[limits]\nupload_bytes = 10")
                .unwrap()
                .limits
                .upload_bytes,
            10
        );
        let err = parse("[limits]\nupload_bytes = 0").unwrap_err().to_string();
        assert!(err.contains("upload_bytes"), "{err}");
        assert!(parse("[limits]\nupload = 1").is_err());
    }

    #[test]
    fn the_connection_limits_default_and_are_at_least_1() {
        let limits = parse(&default_config_text()).unwrap().limits;
        assert_eq!(limits.max_connections, DEFAULT_MAX_CONNECTIONS);
        assert_eq!(limits.header_timeout_secs, DEFAULT_HEADER_TIMEOUT_SECS);
        assert_eq!(limits.idle_timeout_secs, DEFAULT_IDLE_TIMEOUT_SECS);
        let limits =
            parse("[limits]\nmax_connections = 2\nheader_timeout_secs = 3\nidle_timeout_secs = 4")
                .unwrap()
                .limits;
        assert_eq!(
            (
                limits.max_connections,
                limits.header_timeout_secs,
                limits.idle_timeout_secs
            ),
            (2, 3, 4)
        );
        for key in [
            "max_connections",
            "header_timeout_secs",
            "idle_timeout_secs",
        ] {
            let err = parse(&format!("[limits]\n{key} = 0"))
                .unwrap_err()
                .to_string();
            assert!(err.contains(key) && err.contains("at least 1"), "{err}");
        }
    }

    #[test]
    fn every_key_is_optional() {
        assert_eq!(parse("").unwrap(), ServerConfig::default());
        let config =
            parse("listen = \"[::1]:0\"\n[defaults]\ndamage_policy = \"Continue\"\n").unwrap();
        assert_eq!(config.listen_addr().unwrap().to_string(), "[::1]:0");
        assert_eq!(config.damage_policy().unwrap(), DamagePolicy::Continue);
    }

    #[test]
    fn bad_values_and_unknown_keys_are_refused() {
        for (text, needle) in [
            ("listen = \"nowhere\"", "listen"),
            ("listen = \"example.com:80\"", "listen"),
            ("lisen = \"127.0.0.1:1\"", "lisen"),
            ("[defaults]\nchunk_bytes = 0", "chunk_bytes"),
            ("[defaults]\ndamage_policy = \"panic\"", "damage_policy"),
            ("[mcp]\nenable = true", "enable"),
            ("listen = ", "server.toml"),
        ] {
            let err = parse(text).unwrap_err().to_string();
            assert!(err.contains(needle), "{text:?}: {err}");
        }
    }

    #[test]
    fn mcp_options() {
        let config = parse("").unwrap();
        assert!(config.mcp.enabled);
        assert_eq!(config.mcp.max_sessions, DEFAULT_MCP_SESSIONS);
        let config = parse("[mcp]\nmax_sessions = 3").unwrap();
        assert_eq!(config.mcp.max_sessions, 3);
        for (text, needle) in [
            ("[mcp]\nmax_sessions = 0", "max_sessions"),
            // The MCP token is gone (access.toml's users and grants replace it).
            ("[mcp]\ntoken = \"s3cret\"", "token"),
        ] {
            let err = parse(text).unwrap_err().to_string();
            assert!(err.contains(needle), "{text:?}: {err}");
        }
    }

    #[test]
    fn listen_addresses() {
        assert_eq!(
            parse_listen("localhost:9000").unwrap().to_string(),
            "127.0.0.1:9000"
        );
        assert_eq!(
            parse_listen(" 0.0.0.0:80 ").unwrap().to_string(),
            "0.0.0.0:80"
        );
        for bad in ["", "127.0.0.1", "localhost:x", "localhost:70000", "::1:80"] {
            assert!(parse_listen(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn session_limits_are_unlimited_by_default_and_at_least_1() {
        let config = parse(&default_config_text()).unwrap();
        assert_eq!(config.sessions, SessionsConfig::default());
        assert_eq!(config.sessions.limits(), SessionLimits::unlimited());
        assert!(default_config_text().contains("[sessions]"));
        let limits = parse("[sessions]\nlifetime_secs = 3600\nidle_timeout_secs = 60")
            .unwrap()
            .sessions
            .limits();
        assert_eq!(limits.lifetime, Some(Duration::from_secs(3600)));
        assert_eq!(limits.idle_timeout, Some(Duration::from_secs(60)));
        for (text, key) in [
            ("[sessions]\nlifetime_secs = 0", "sessions.lifetime_secs"),
            (
                "[sessions]\nidle_timeout_secs = 0",
                "sessions.idle_timeout_secs",
            ),
        ] {
            let err = parse(text).unwrap_err().to_string();
            assert!(err.contains(key) && err.contains("no limit"), "{err}");
        }
        assert!(parse("[sessions]\nenabled = false").is_err());
    }
}
