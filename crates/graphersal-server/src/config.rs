//! `server.toml`: the configuration of a server directory.
//!
//! ```toml
//! listen = "127.0.0.1:8484"   # IP:PORT (or localhost:PORT); port 0 picks a free port
//!
//! [mcp]
//! enabled = true              # /mcp over every database, /db/<name>/mcp per loaded database
//! # token = "secret"          # optional: requests need `Authorization: Bearer secret`
//! max_sessions = 256          # MCP sessions remembered (the oldest forgotten first)
//!
//! [defaults]                  # the creation parameters of new databases
//! chunk_bytes = 1048576
//! damage_policy = "maintenance"
//!
//! [limits]
//! upload_bytes = 4294967296   # the largest import upload (POST /api/admin/import), 4 GiB
//! ```
//!
//! Every key is optional (its default applies); an unknown key is an error (a typo must not be
//! ignored silently).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;

use graphersal::persist::{DEFAULT_CHUNK_BYTES, DamagePolicy};
use serde::Deserialize;

use crate::error::ServerError;

/// The default listen address (loopback only: the server has no access control).
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
}

/// The default largest import upload: 4 GiB.
pub const DEFAULT_UPLOAD_BYTES: u64 = 4 << 30;

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
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            upload_bytes: DEFAULT_UPLOAD_BYTES,
        }
    }
}

fn default_upload_bytes() -> u64 {
    DEFAULT_UPLOAD_BYTES
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
    /// The bearer token every MCP request must carry (`Authorization: Bearer <token>`), on
    /// `/mcp` and on every `/db/<name>/mcp`; `None` (the default): no token. It is the only
    /// check of an MCP request besides the `Host`/`Origin` checks: no access control.
    #[serde(default)]
    pub token: Option<String>,
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
        }
    }
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            token: None,
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
        if config
            .mcp
            .token
            .as_deref()
            .is_some_and(|t| t.trim().is_empty())
        {
            return Err(invalid(
                "mcp.token: must not be empty (remove the key for no token)".to_string(),
            ));
        }
        if config.limits.upload_bytes == 0 {
            return Err(invalid(format!(
                "limits.upload_bytes: must be at least 1 (default {DEFAULT_UPLOAD_BYTES})"
            )));
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
         # port. The server has NO access control: keep it on a loopback address (127.0.0.1,\n\
         # [::1]); any other address is reachable from the network and logs a warning.\n\
         # `graphersal-server serve --listen ADDR` overrides it.\n\
         listen = \"{DEFAULT_LISTEN}\"\n\
         \n\
         [mcp]\n\
         # The MCP endpoints: /mcp over every database (list_graphs, use_graph) and\n\
         # /db/<name>/mcp per loaded database. Read when the server starts.\n\
         enabled = true\n\
         # A bearer token every MCP request must carry (no token by default):\n\
         # token = \"<secret>\"\n\
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
         upload_bytes = {}\n",
        DEFAULT_MCP_SESSIONS, defaults.chunk_bytes, defaults.damage_policy, DEFAULT_UPLOAD_BYTES
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
        assert_eq!(config.mcp.token, None);
        assert_eq!(config.mcp.max_sessions, DEFAULT_MCP_SESSIONS);
        let config = parse("[mcp]\ntoken = \"s3cret\"\nmax_sessions = 3").unwrap();
        assert_eq!(config.mcp.token.as_deref(), Some("s3cret"));
        assert_eq!(config.mcp.max_sessions, 3);
        for (text, needle) in [
            ("[mcp]\nmax_sessions = 0", "max_sessions"),
            ("[mcp]\ntoken = \" \"", "token"),
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
}
