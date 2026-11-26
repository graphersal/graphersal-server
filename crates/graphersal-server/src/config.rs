//! `server.toml`: the configuration of a server directory.
//!
//! ```toml
//! listen = "127.0.0.1:8484"   # IP:PORT (or localhost:PORT); port 0 picks a free port
//!
//! [mcp]
//! enabled = true              # the endpoints exist: /mcp, /db/<name>/mcp (the runtime switch
//!                             # is mcp.toml's `enabled`, written off by init)
//! max_sessions = 256          # MCP sessions remembered (the oldest forgotten first)
//! max_sessions_per_identity = 16 # MCP sessions one principal holds (its oldest ended first)
//!
//! [defaults]                  # what a new database starts with (create, import)
//! chunk_bytes = 1048576       # creation parameters: fixed for the database's life
//! damage_policy = "maintenance"
//! auto_load = false           # its first auto-load and read-only flags (changeable later)
//! read_only = false
//!
//! [limits]
//! upload_bytes = 4294967296   # the largest import upload (POST /api/admin/import), 4 GiB
//! max_connections = 256       # open connections served at once (more: 503 + Retry-After)
//! header_timeout_secs = 30    # a request's head must arrive within this (slow clients)
//! idle_timeout_secs = 60      # a connection without a running request is closed after this
//! memory_limit = "auto"       # the process memory ceiling: bytes, "auto" or "none" (at start)
//! memory_headroom = 67108864  # what "auto" keeps free below a cgroup limit, in bytes
//!
//! [limits.query]              # query limits of every database (crate::limits); none by default
//! timeout_ms = 30000          # a database.toml's [limits.query] overrides single keys
//!
//! [sessions]                  # browser sessions (they survive a restart); 0 = no limit
//! lifetime_secs = 2592000     # the longest a session lasts after its login (default 30 days)
//! idle_timeout_secs = 604800  # the longest after its last request (default 7 days)
//!
//! [access]
//! enabled = true              # false: no access control, every request is an admin
//!
//! [http]
//! public_scheme = "http"      # "https" behind a TLS proxy: the login cookie gets `Secure`
//! allowed_hosts = []          # public names behind a proxy ("db.example.com", port optional)
//!
//! [jobs]
//! backup_root = "backups/jobs" # where backup jobs write (relative: to the server directory)
//! ```
//!
//! Every key is optional (its default applies); an unknown key is an error (a typo must not be
//! ignored silently).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use graphersal::exec::MemoryLimit;
use graphersal::persist::{DEFAULT_CHUNK_BYTES, DamagePolicy};
use serde::Deserialize;

use crate::access::SessionLimits;
use crate::error::ServerError;
use crate::limits::QueryLimits;

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
    /// Whether access control is on.
    #[serde(default)]
    pub access: AccessConfig,
    /// How clients reach the server (scheme, public host names).
    #[serde(default)]
    pub http: HttpConfig,
    /// The scheduled jobs of the databases (the backup root).
    #[serde(default)]
    pub jobs: JobsConfig,
}

/// The default backup root of backup jobs, relative to the server directory, as path
/// components ([`JobsConfig::backup_root`]).
pub const DEFAULT_BACKUP_ROOT: &[&str] = &["backups", "jobs"];

/// The scheduled jobs of the databases (`[jobs]` of `server.toml`), read at start.
///
/// `#[non_exhaustive]`: further settings may be added.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct JobsConfig {
    /// The **backup root**: the one directory backup jobs (`POST /db/<name>/api/jobs/define`,
    /// action `backup`) and the Store panel's fork/backup `dir` may write into. A relative path
    /// is relative to the server directory; an absolute one may name a mounted storage.
    /// Missing: `<server-dir>/backups/jobs` ([`DEFAULT_BACKUP_ROOT`]). `serve` (and `init`)
    /// create it. A backup job's `path` is resolved relative to it; an absolute path, or one
    /// leaving it (`..`, a symbolic link pointing out), is refused when the job is defined.
    #[serde(default)]
    pub backup_root: Option<PathBuf>,
}

impl JobsConfig {
    /// The backup root of the server directory `server_dir` ([`Self::backup_root`]): the
    /// configured path (relative to `server_dir`), else the default.
    pub fn backup_root(&self, server_dir: &Path) -> PathBuf {
        match &self.backup_root {
            Some(path) if path.is_absolute() => path.clone(),
            Some(path) => server_dir.join(path),
            None => DEFAULT_BACKUP_ROOT
                .iter()
                .fold(server_dir.to_path_buf(), |path, part| path.join(part)),
        }
    }
}

/// Access control (`[access]` of `server.toml`), read at start.
///
/// `#[non_exhaustive]`: further settings may be added.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct AccessConfig {
    /// Whether requests are authenticated and refused per principal (default `true`). `false`:
    /// no access control, every request runs as an admin (the operator or an admin user when
    /// its credentials are valid, else the principal `everyone`); the start block warns.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for AccessConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// How clients reach the server: the scheme of its public URL.
///
/// `#[non_exhaustive]`: another scheme is not planned, but the set is not closed by definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum PublicScheme {
    /// Plain HTTP (the default): the login cookie has no `Secure` (a browser drops a `Secure`
    /// cookie over plain HTTP except on localhost); tokens travel in clear text.
    #[default]
    Http,
    /// HTTPS, TLS ended by a reverse proxy: the login cookie gets `Secure`.
    Https,
}

impl PublicScheme {
    /// The scheme as `server.toml` writes it: `http`, `https`.
    pub fn as_str(self) -> &'static str {
        match self {
            PublicScheme::Http => "http",
            PublicScheme::Https => "https",
        }
    }
}

/// How clients reach the server (`[http]` of `server.toml`), read at start.
///
/// `#[non_exhaustive]`: further settings may be added.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct HttpConfig {
    /// The scheme clients use (default [`PublicScheme::Http`]).
    #[serde(default)]
    pub public_scheme: PublicScheme,
    /// Further host names a request's `Host` (and an `http(s)://` `Origin`) may name besides
    /// the listen address's own names: the public names behind a reverse proxy. A name with an
    /// optional port (`db.example.com`, `db.example.com:443`, an IP address, `[::1]`); the port
    /// is not checked (a name is admitted on any port). [`ServerConfig::parse`] normalizes
    /// them: lower case, no port, no trailing dot, IPv6 in brackets, no duplicates.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
}

/// How long browser sessions last (`[sessions]` of `server.toml`), read at start. Both keys
/// are optional: a missing key is its default (30 days since the login, 7 days since the last
/// request), `0` is no limit. Sessions survive a restart either
/// way (there is no setting for that: `crate::sessions`).
///
/// `#[non_exhaustive]`: further settings may be added.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct SessionsConfig {
    /// The longest a browser session lasts after its login, in seconds (missing:
    /// [`DEFAULT_SESSION_LIFETIME_SECS`], 30 days; `0`: no limit). The browser logs in again
    /// afterwards.
    #[serde(default)]
    pub lifetime_secs: Option<u64>,
    /// The longest a browser session lasts after its last request, in seconds (missing:
    /// [`DEFAULT_SESSION_IDLE_TIMEOUT_SECS`], 7 days; `0`: no limit). Counted coarsely: the last
    /// request is recorded at most every minute.
    #[serde(default)]
    pub idle_timeout_secs: Option<u64>,
}

/// The default lifetime of a browser session ([`SessionsConfig::lifetime_secs`]): 30 days.
pub const DEFAULT_SESSION_LIFETIME_SECS: u64 = 30 * 24 * 60 * 60;

/// The default idle timeout of a browser session ([`SessionsConfig::idle_timeout_secs`]): 7
/// days.
pub const DEFAULT_SESSION_IDLE_TIMEOUT_SECS: u64 = 7 * 24 * 60 * 60;

/// A session limit in seconds as it applies: missing = `default`, `0` = no limit (`None`).
fn session_limit(value: Option<u64>, default: u64) -> Option<u64> {
    match value.unwrap_or(default) {
        0 => None,
        secs => Some(secs),
    }
}

impl SessionsConfig {
    /// The lifetime that applies, in seconds (`None`: no limit).
    pub fn effective_lifetime_secs(&self) -> Option<u64> {
        session_limit(self.lifetime_secs, DEFAULT_SESSION_LIFETIME_SECS)
    }

    /// The idle timeout that applies, in seconds (`None`: no limit).
    pub fn effective_idle_timeout_secs(&self) -> Option<u64> {
        session_limit(self.idle_timeout_secs, DEFAULT_SESSION_IDLE_TIMEOUT_SECS)
    }

    /// The limits the browser sessions run with.
    pub fn limits(&self) -> SessionLimits {
        SessionLimits::unlimited()
            .with_lifetime(self.effective_lifetime_secs().map(Duration::from_secs))
            .with_idle_timeout(self.effective_idle_timeout_secs().map(Duration::from_secs))
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
    /// The process memory ceiling (`graphersal::exec::set_memory_limit`): a number of bytes
    /// (`0` is no ceiling), `"auto"` or `"none"`, as the graphersal command line's
    /// `--memory-limit`. One setting for the whole process (every database), set at start;
    /// missing: the library's default (`auto`: the cgroup limit minus the headroom, else 80 %
    /// of the physical memory). A query, import or write that would take the process over it
    /// fails and rolls back.
    #[serde(default, deserialize_with = "crate::limits::memory_limit")]
    pub memory_limit: Option<MemoryLimit>,
    /// What `memory_limit = "auto"` keeps free below a cgroup memory limit, in bytes (at least
    /// 1), as `--memory-headroom`; missing: a tenth of the limit, at least 64 MiB. Set at start.
    #[serde(default)]
    pub memory_headroom: Option<u64>,
    /// The query limits of every database (`[limits.query]`, [`crate::limits`]); a database's
    /// `database.toml` overrides single keys. Read at every load; default: no limit.
    #[serde(default)]
    pub query: QueryLimits,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            upload_bytes: DEFAULT_UPLOAD_BYTES,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            header_timeout_secs: DEFAULT_HEADER_TIMEOUT_SECS,
            idle_timeout_secs: DEFAULT_IDLE_TIMEOUT_SECS,
            memory_limit: None,
            memory_headroom: None,
            query: QueryLimits::default(),
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
    /// Whether the server builds its MCP endpoints: `/mcp` over every database and
    /// `/db/<name>/mcp` per loaded database (default `true`). The hard switch: MCP answers
    /// only when this AND `mcp.toml`'s runtime `enabled` are on; `init` leaves this `true` and
    /// writes the runtime switch off (round 4 B-MCPDEF), so an admin switches MCP on without a
    /// restart.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// How many MCP sessions the server remembers, the oldest forgotten first (its client
    /// initializes again and selects its graph again); default [`DEFAULT_MCP_SESSIONS`], at
    /// least 1. One table for `/mcp` and every `/db/<name>/mcp`.
    #[serde(default = "default_mcp_sessions")]
    pub max_sessions: usize,
    /// How many MCP sessions one principal (a user, the operator, anonymous) holds at once
    /// (SEC M2); default [`DEFAULT_MCP_SESSIONS_PER_IDENTITY`], at least 1. A new session over
    /// it first ends that principal's oldest session, so one principal's flood of `initialize`
    /// requests never evicts another principal's sessions.
    #[serde(default = "default_mcp_sessions_per_identity")]
    pub max_sessions_per_identity: usize,
}

fn default_mcp_sessions() -> usize {
    DEFAULT_MCP_SESSIONS
}

fn default_mcp_sessions_per_identity() -> usize {
    DEFAULT_MCP_SESSIONS_PER_IDENTITY
}

/// The default number of MCP sessions one principal holds
/// ([`McpConfig::max_sessions_per_identity`]).
pub const DEFAULT_MCP_SESSIONS_PER_IDENTITY: usize = 16;

/// What a new database starts with (`[defaults]` of `server.toml`), read at every create and
/// import (round 4 B-SETTINGS): the Store's creation parameters (fixed for the database's life)
/// and the first value of its own `auto_load` and `read_only` flags (copied into its
/// `database.toml`, changeable per database afterwards; a change here never touches an
/// existing database).
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
    /// Whether a new database (create, import) loads when the server starts (default
    /// `false`); a create request or `db create --auto-load`/`--no-auto-load` wins.
    #[serde(default)]
    pub auto_load: bool,
    /// Whether a new database (create, import) is served read-only (default `false`); a
    /// create request or `db create --read-only`/`--no-read-only` wins. A base is read-only
    /// whatever this says.
    #[serde(default)]
    pub read_only: bool,
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
            access: AccessConfig::default(),
            http: HttpConfig::default(),
            jobs: JobsConfig::default(),
        }
    }
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_sessions: DEFAULT_MCP_SESSIONS,
            max_sessions_per_identity: DEFAULT_MCP_SESSIONS_PER_IDENTITY,
        }
    }
}

impl Default for DatabaseDefaults {
    fn default() -> Self {
        Self {
            chunk_bytes: default_chunk_bytes(),
            damage_policy: default_damage_policy(),
            auto_load: false,
            read_only: false,
        }
    }
}

impl ServerConfig {
    /// Parses and checks the text of `server.toml` (`path` names the file in errors).
    pub fn parse(text: &str, path: &Path) -> Result<Self, ServerError> {
        let mut config: ServerConfig = toml::from_str(text).map_err(|err| ServerError::Config {
            path: path.to_path_buf(),
            message: toml_error_message(&err, text),
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
        if config.mcp.max_sessions_per_identity == 0 {
            return Err(invalid(format!(
                "mcp.max_sessions_per_identity: must be at least 1 (default \
                 {DEFAULT_MCP_SESSIONS_PER_IDENTITY})"
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
        if config.limits.memory_headroom == Some(0) {
            return Err(invalid(
                "limits.memory_headroom: must be at least 1 byte (leave it out for the default, \
                 a tenth of the cgroup limit)"
                    .to_string(),
            ));
        }
        config.http.allowed_hosts = allowed_hosts(&config.http.allowed_hosts)
            .map_err(|err| invalid(format!("http.allowed_hosts: {err}")))?;
        if config
            .jobs
            .backup_root
            .as_ref()
            .is_some_and(|root| root.as_os_str().is_empty())
        {
            return Err(invalid(
                "jobs.backup_root: must name a directory (leave it out for the default, \
                 backups/jobs in the server directory)"
                    .to_string(),
            ));
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

/// The message of a TOML error in `text`, with the line it points at (`at line 3: timeout_ms
/// = -1`): the parser's own message does not always name the key.
pub(crate) fn toml_error_message(err: &toml::de::Error, text: &str) -> String {
    let message = err.message().trim_end().to_string();
    let Some(span) = err.span() else {
        return message;
    };
    let start = span.start.min(text.len());
    if !text.is_char_boundary(start) {
        return message;
    }
    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[line_start..]
        .find('\n')
        .map_or(text.len(), |i| line_start + i);
    let line = text[line_start..line_end].trim();
    if line.is_empty() {
        return message;
    }
    let number = text[..line_start].matches('\n').count() + 1;
    format!("{message} (at line {number}: {line})")
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

/// Checks and normalizes the allowed host names ([`HttpConfig::allowed_hosts`]): each a host
/// name or an IP address with an optional port, without scheme or path; lower case, port and
/// trailing dot dropped, IPv6 in brackets, duplicates dropped.
fn allowed_hosts(raw: &[String]) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    for entry in raw {
        let value = entry.trim();
        let refused = || {
            format!(
                "{entry:?} is not a host name such as db.example.com (an optional port, no \
                 scheme or path): the public name of this server behind a reverse proxy"
            )
        };
        let name = match value.parse::<SocketAddr>() {
            Ok(addr) => normal_ip(addr.ip()),
            Err(_) => {
                let bare = value
                    .strip_prefix('[')
                    .and_then(|v| v.strip_suffix(']'))
                    .unwrap_or(value);
                match bare.parse::<IpAddr>() {
                    Ok(ip) => normal_ip(ip),
                    Err(_) => {
                        let host = match value.rsplit_once(':') {
                            Some((host, port)) if port.parse::<u16>().is_ok() => host,
                            Some(_) => return Err(refused()),
                            None => value,
                        };
                        let host = host.trim_end_matches('.');
                        let valid = !host.is_empty()
                            && host
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'));
                        if !valid {
                            return Err(refused());
                        }
                        host.to_ascii_lowercase()
                    }
                }
            }
        };
        if !names.contains(&name) {
            names.push(name);
        }
    }
    Ok(names)
}

/// An IP address as a `Host` header names it (IPv6 in brackets).
fn normal_ip(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
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
         # /db/<name>/mcp per loaded database. Read when the server starts. false: no\n\
         # endpoint at all. While true, the switch an admin uses is mcp.toml's enabled (the\n\
         # MCP tab, `graphersal-server mcp on|off`): a new server starts with it off, and new\n\
         # databases and aliases are not offered to MCP unless created with --mcp.\n\
         enabled = true\n\
         # How many MCP sessions are remembered (the oldest forgotten first).\n\
         max_sessions = {}\n\
         # How many MCP sessions one principal (a user, anonymous) holds; a new one over it\n\
         # ends that principal's oldest session, never another principal's.\n\
         max_sessions_per_identity = {}\n\
         \n\
         [defaults]\n\
         # What a new database (create, import) starts with; read at every create, so a\n\
         # change applies to the next new database and never to an existing one.\n\
         # Creation parameters (fixed for each database's life):\n\
         # Uncompressed snapshot chunk size in bytes.\n\
         chunk_bytes = {}\n\
         # What a Store does on damage: \"maintenance\" (read-only until repaired) or \"continue\".\n\
         damage_policy = \"{}\"\n\
         # The first value of its own flags (changeable per database afterwards):\n\
         # loaded when the server starts:\n\
         auto_load = false\n\
         # served read-only (every write refused):\n\
         read_only = false\n\
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
         # The process memory ceiling, one setting for every database: a number of bytes,\n\
         # \"auto\" (the default: the cgroup limit minus memory_headroom, else 80 % of the\n\
         # physical memory) or \"none\". A query, import or write that would take the process\n\
         # over it fails and rolls back. Read when the server starts.\n\
         # memory_limit = \"auto\"\n\
         # What \"auto\" keeps free below a cgroup limit, in bytes (default a tenth of it).\n\
         # memory_headroom = 268435456\n\
         \n\
         [limits.query]\n\
         # Query limits of every database; none when left out. A database's database.toml\n\
         # overrides single keys in its own [limits.query] (0 lifts a limit there). Read at\n\
         # every load: unload and load a database to apply a change. 0 is no limit\n\
         # (max_value_depth: 0 is the default, 128).\n\
         # evaluationTimeout of every traversal and the budget of a run, in milliseconds:\n\
         # timeout_ms = 30000\n\
         # Rhai operations one script may run:\n\
         # max_operations = 10000000\n\
         # Longest string a script or a step may build, in bytes:\n\
         # max_string_size = 16777216\n\
         # Largest array (items) and map (entries) a script may build:\n\
         # max_array_size = 100000\n\
         # max_map_size = 100000\n\
         # Traversers one step may leave alive (engine default 10000000):\n\
         # max_traversers = 10000000\n\
         # Memory one query holds at once, in bytes (engine default 1 GiB):\n\
         # max_materialized_bytes = 524288000\n\
         # Deepest nesting of arrays and maps in one value:\n\
         # max_value_depth = 128\n\
         \n\
         [sessions]\n\
         # Browser sessions (the login cookie) and MCP sessions survive a restart (kept in\n\
         # sessions/, hashes only). How long a browser session lasts, in seconds; 0 = no\n\
         # limit. Read when the server starts (the admin API applies a change at once).\n\
         # Since its login (default 30 days):\n\
         # lifetime_secs = 2592000\n\
         # Since its last request (default 7 days; counted coarsely, up to a minute):\n\
         # idle_timeout_secs = 604800\n\
         # The login cookie lasts as long as the lifetime (400 days, the longest a browser\n\
         # keeps a cookie, with lifetime_secs = 0).\n\
         \n\
         [access]\n\
         # Access control: requests need a user token or a browser login (access.toml).\n\
         # false = NO access control: every request is an admin (serve warns loudly). Read\n\
         # when the server starts.\n\
         enabled = true\n\
         \n\
         [http]\n\
         # How clients reach the server: \"http\", or \"https\" when a reverse proxy ends TLS\n\
         # (the login cookie then gets Secure; over http tokens travel in clear text).\n\
         public_scheme = \"http\"\n\
         # The public names behind a reverse proxy that requests may name in Host, besides\n\
         # this machine's loopback names and the listen address (a port is optional and not\n\
         # checked). Other names are refused (DNS rebinding).\n\
         # allowed_hosts = [\"db.example.com\"]\n\
         \n\
         [jobs]\n\
         # The backup root: the only directory backup jobs (and the Store panel's fork and\n\
         # backup) may write into; a job's path is relative to it (absolute paths and paths\n\
         # leaving it are refused). Relative to this directory, or an absolute path such as a\n\
         # mounted storage; created at start. Read when the server starts.\n\
         # backup_root = \"backups/jobs\"\n",
        DEFAULT_MCP_SESSIONS,
        DEFAULT_MCP_SESSIONS_PER_IDENTITY,
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

    /// SEC L4: browser sessions expire by default (30 days since the login, 7 days since the
    /// last request); `0` lifts a limit.
    #[test]
    fn session_limits_have_defaults_and_0_lifts_them() {
        let config = parse(&default_config_text()).unwrap();
        assert_eq!(config.sessions, SessionsConfig::default());
        assert_eq!(
            config.sessions.limits(),
            SessionLimits::unlimited()
                .with_lifetime(Some(Duration::from_secs(30 * 86_400)))
                .with_idle_timeout(Some(Duration::from_secs(7 * 86_400)))
        );
        assert!(default_config_text().contains("[sessions]"));
        let limits = parse("[sessions]\nlifetime_secs = 3600\nidle_timeout_secs = 60")
            .unwrap()
            .sessions
            .limits();
        assert_eq!(limits.lifetime, Some(Duration::from_secs(3600)));
        assert_eq!(limits.idle_timeout, Some(Duration::from_secs(60)));
        let lifted = parse("[sessions]\nlifetime_secs = 0\nidle_timeout_secs = 0").unwrap();
        assert_eq!(lifted.sessions.limits(), SessionLimits::unlimited());
        assert_eq!(lifted.sessions.effective_lifetime_secs(), None);
        assert!(parse("[sessions]\nenabled = false").is_err());
    }

    #[test]
    fn access_and_http_modes_default_and_parse() {
        let config = parse(&default_config_text()).unwrap();
        assert!(config.access.enabled);
        assert_eq!(config.http.public_scheme, PublicScheme::Http);
        assert!(config.http.allowed_hosts.is_empty());
        assert!(
            default_config_text().contains("\n[access]\n")
                && default_config_text().contains("\n[http]\n")
        );
        assert!(default_config_text().contains("public_scheme = \"http\""));
        let config = parse(
            "[access]\nenabled = false\n[http]\npublic_scheme = \"https\"\n\
             allowed_hosts = [\"DB.Example.com.\", \"db.example.com:443\", \"10.0.0.7:80\", \
             \"::1\", \"[fe80::1]:8484\", \"proxy\"]",
        )
        .unwrap();
        assert!(!config.access.enabled);
        assert_eq!(config.http.public_scheme, PublicScheme::Https);
        assert_eq!(config.http.public_scheme.as_str(), "https");
        assert_eq!(
            config.http.allowed_hosts,
            ["db.example.com", "10.0.0.7", "[::1]", "[fe80::1]", "proxy"]
        );
        for (text, needle) in [
            (
                "[http]\npublic_scheme = \"ftp\"",
                "expected `http` or `https`",
            ),
            (
                "[http]\nallowed_hosts = [\"https://db.example.com\"]",
                "allowed_hosts",
            ),
            (
                "[http]\nallowed_hosts = [\"db.example.com/x\"]",
                "allowed_hosts",
            ),
            (
                "[http]\nallowed_hosts = [\"db.example.com:x\"]",
                "allowed_hosts",
            ),
            ("[http]\nallowed_hosts = [\"\"]", "allowed_hosts"),
            ("[http]\nallowed_hosts = [\"a b\"]", "allowed_hosts"),
            ("[http]\nallowed_host = []", "allowed_host"),
            ("[access]\nenable = false", "enable"),
        ] {
            let err = parse(text).unwrap_err().to_string();
            assert!(err.contains(needle), "{text:?}: {err}");
        }
    }

    #[test]
    fn query_limits_and_the_memory_ceiling_default_to_none_and_parse() {
        let limits = parse(&default_config_text()).unwrap().limits;
        assert!(limits.query.is_empty());
        assert_eq!(limits.memory_limit, None);
        assert_eq!(limits.memory_headroom, None);
        assert!(default_config_text().contains("\n[limits.query]\n"));
        let limits = parse(
            "[limits]\nmemory_limit = \"none\"\nmemory_headroom = 1024\n\
             [limits.query]\ntimeout_ms = 5\nmax_operations = 0",
        )
        .unwrap()
        .limits;
        assert_eq!(limits.memory_limit, Some(MemoryLimit::Unlimited));
        assert_eq!(limits.memory_headroom, Some(1024));
        assert_eq!(limits.query.timeout_ms, Some(5));
        assert_eq!(limits.query.max_operations, Some(0));
        assert_eq!(
            parse("[limits]\nmemory_limit = 4096")
                .unwrap()
                .limits
                .memory_limit,
            Some(MemoryLimit::Bytes(4096))
        );
        for (text, needle) in [
            ("[limits]\nmemory_limit = \"lots\"", "\"auto\" or \"none\""),
            ("[limits]\nmemory_limit = -1", "memory_limit"),
            ("[limits]\nmemory_headroom = 0", "limits.memory_headroom"),
            ("[limits]\nmemory_headroom = -1", "memory_headroom"),
            ("[limits.query]\ntimeout_ms = -1", "timeout_ms"),
            ("[limits.query]\nmax_traversers = \"x\"", "max_traversers"),
            ("[limits.query]\nmemory_limit = 1", "unknown field"),
            ("[limits.query]\ntimeout = 1", "unknown field"),
        ] {
            let err = parse(text).unwrap_err().to_string();
            assert!(err.contains(needle), "{text:?}: {err}");
        }
    }

    #[test]
    fn the_backup_root_defaults_to_backups_jobs_and_resolves_relative_to_the_directory() {
        let dir = Path::new("srv");
        let config = parse("").unwrap();
        assert_eq!(
            config.jobs.backup_root(dir),
            dir.join("backups").join("jobs")
        );
        let config = parse("[jobs]\nbackup_root = \"vault\"\n").unwrap();
        assert_eq!(config.jobs.backup_root(dir), dir.join("vault"));
        let absolute = std::env::temp_dir().join("mounted");
        let text = format!(
            "[jobs]\nbackup_root = {:?}\n",
            absolute.display().to_string()
        );
        assert_eq!(parse(&text).unwrap().jobs.backup_root(dir), absolute);
        let err = parse("[jobs]\nbackup_root = \"\"\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("jobs.backup_root"), "{err}");
        let err = parse("[jobs]\nroot = \"x\"\n").unwrap_err().to_string();
        assert!(err.contains("unknown field"), "{err}");
    }
}
