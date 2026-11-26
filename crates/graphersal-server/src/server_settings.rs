//! `server.toml` edited at runtime (round 4 B-SETTINGS): every key by its dotted name, checked
//! with the same parser as at start ([`ServerConfig::parse`]), written atomically, applied at
//! once where that is safe, and otherwise marked "restart pending" until the next start.
//!
//! **Keys** ([`KEYS`]): `listen`, `mcp.enabled`, `mcp.max_sessions`, `defaults.chunk_bytes`,
//! `defaults.damage_policy`, `defaults.auto_load`, `defaults.read_only`,
//! `limits.upload_bytes`, `limits.max_connections`, `limits.header_timeout_secs`,
//! `limits.idle_timeout_secs`, `limits.memory_limit`, `limits.memory_headroom`,
//! `limits.query.<key>` (the eight query limits), `sessions.lifetime_secs`,
//! `sessions.idle_timeout_secs`, `access.enabled`, `http.public_scheme`,
//! `http.allowed_hosts`, `jobs.backup_root`. Each has an [`Applies`] class:
//!
//! - `now`: applied by the running server when the change is made through the admin API (the
//!   web UI's Settings, `graphersal-server config set`): the upload limit, the process memory
//!   ceiling and its headroom (`graphersal::exec::set_memory_limit` is a runtime setter), the
//!   browser session lifetimes (the access state's limits and the login cookie's `Max-Age`) and
//!   the public scheme (the login cookie's `Secure`, the API descriptions' URLs);
//! - `load`: the query limits: read at every load of a database (a loaded one keeps its limits
//!   until it is loaded again);
//! - `create`: `[defaults]`: read at every create and import (never touches an existing
//!   database);
//! - `restart`: the rest (the listener, the MCP endpoint, the connection limits, access
//!   control, the allowed host names, the backup root): written, and listed as **restart
//!   pending** until the server starts again.
//!
//! "Restart pending" ([`LiveConfig::pending`]) is derived, never stored: the `now` and
//! `restart` keys whose value in the file differs from the value the running server applies.
//! A hand edit of the file therefore shows as pending too (only the admin API applies a change
//! at once; `load` and `create` keys are read from the file when they are used).
//!
//! **Writing** ([`edit`]): the change is made on the file's TOML table and checked by
//! [`ServerConfig::parse`]; then the file's own text is patched line by line (a key's line
//! replaced, a commented `# key = ...` example taken over, a reset key commented out), so its
//! comments survive, and the patched text must parse to exactly the same configuration. When it
//! does not (a layout the patch does not know: dotted keys, inline tables, multi-line arrays),
//! the whole file is rewritten from the table with a header comment ([`Edited::rewritten`]: the
//! file's own comments are lost then). No `toml_edit`: it is not in the dependency tree.
//!
//! The writer holds [`LiveConfig`]'s mutex (online; a leaf lock) or the server lock (offline,
//! the console) across read, revision check and write; the revision is
//! [`crate::revision::of`] the file's text.

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use serde_json::{Value, json};

use crate::config::{PublicScheme, ServerConfig};
use crate::error::ServerError;
use crate::limits::QUERY_LIMIT_KEYS;

/// When a key's change takes effect (module docs).
///
/// `#[non_exhaustive]`: a finer class may be added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Applies {
    /// At once (through the admin API).
    Now,
    /// At each database's next load.
    NextLoad,
    /// At the next create or import of a database.
    NewDatabases,
    /// At the next start of the server.
    Restart,
}

impl Applies {
    /// The class as the API and the console write it: `now`, `load`, `create`, `restart`.
    pub fn as_str(self) -> &'static str {
        match self {
            Applies::Now => "now",
            Applies::NextLoad => "load",
            Applies::NewDatabases => "create",
            Applies::Restart => "restart",
        }
    }

    /// The class in words.
    pub fn text(self) -> &'static str {
        match self {
            Applies::Now => "applies now",
            Applies::NextLoad => "at each database's next load",
            Applies::NewDatabases => "for new databases",
            Applies::Restart => "after a restart",
        }
    }
}

/// The values a text key takes (`null`: any text).
fn choices_of(name: &str) -> Value {
    match name {
        "defaults.damage_policy" => json!(["maintenance", "continue"]),
        "http.public_scheme" => json!(["http", "https"]),
        _ => Value::Null,
    }
}

/// The value type of a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `true` / `false`.
    Bool,
    /// A whole number (its range is the parser's).
    Count,
    /// A text.
    Text,
    /// A number of bytes, `"auto"` or `"none"`.
    Memory,
    /// A list of host names.
    Hosts,
}

impl Kind {
    /// The type as the view names it: `bool`, `count`, `text`, `memory`, `hosts`.
    fn as_str(self) -> &'static str {
        match self {
            Kind::Bool => "bool",
            Kind::Count => "count",
            Kind::Text => "text",
            Kind::Memory => "memory",
            Kind::Hosts => "hosts",
        }
    }
}

/// One key of `server.toml` (module docs).
#[derive(Debug, Clone, Copy)]
pub struct Key {
    /// Its dotted name (`limits.upload_bytes`).
    pub name: &'static str,
    kind: Kind,
    /// When a change takes effect.
    pub applies: Applies,
    /// What it means, in one sentence.
    pub help: &'static str,
}

const fn key(name: &'static str, kind: Kind, applies: Applies, help: &'static str) -> Key {
    Key {
        name,
        kind,
        applies,
        help,
    }
}

/// Every key of `server.toml`, in file order (module docs).
pub const KEYS: &[Key] = &[
    key(
        "listen",
        Kind::Text,
        Applies::Restart,
        "The address the server listens on: IP:PORT or localhost:PORT (port 0 picks a free one).",
    ),
    key(
        "mcp.enabled",
        Kind::Bool,
        Applies::Restart,
        "Whether the MCP endpoints exist at all (the MCP tab's switch turns them on and off at runtime).",
    ),
    key(
        "mcp.max_sessions",
        Kind::Count,
        Applies::Restart,
        "How many MCP sessions are remembered (the oldest forgotten first).",
    ),
    key(
        "defaults.chunk_bytes",
        Kind::Count,
        Applies::NewDatabases,
        "The uncompressed snapshot chunk size of a new database's Store, in bytes (fixed for its life).",
    ),
    key(
        "defaults.damage_policy",
        Kind::Text,
        Applies::NewDatabases,
        "What a new database's Store does on damage: maintenance (read-only until repaired) or continue (fixed for its life).",
    ),
    key(
        "defaults.auto_load",
        Kind::Bool,
        Applies::NewDatabases,
        "Whether a new database loads when the server starts (its own setting afterwards).",
    ),
    key(
        "defaults.read_only",
        Kind::Bool,
        Applies::NewDatabases,
        "Whether a new database is served read-only (its own setting afterwards).",
    ),
    key(
        "limits.upload_bytes",
        Kind::Count,
        Applies::Now,
        "The largest upload (an import or a model file), in bytes.",
    ),
    key(
        "limits.max_connections",
        Kind::Count,
        Applies::Restart,
        "Open connections served at once; one more is answered 503 with Retry-After.",
    ),
    key(
        "limits.header_timeout_secs",
        Kind::Count,
        Applies::Restart,
        "Seconds a request's head may take to arrive.",
    ),
    key(
        "limits.idle_timeout_secs",
        Kind::Count,
        Applies::Restart,
        "Seconds a connection without a running request stays open.",
    ),
    key(
        "limits.memory_limit",
        Kind::Memory,
        Applies::Now,
        "The process memory ceiling: bytes, auto or none (unset: the engine's auto).",
    ),
    key(
        "limits.memory_headroom",
        Kind::Count,
        Applies::Now,
        "What auto keeps free below a cgroup memory limit, in bytes (unset: a tenth of it).",
    ),
    key(
        "limits.query.timeout_ms",
        Kind::Count,
        Applies::NextLoad,
        "Query limit of every database: the timeout of a traversal and of a run, in ms (0 = none).",
    ),
    key(
        "limits.query.max_operations",
        Kind::Count,
        Applies::NextLoad,
        "Query limit of every database: Rhai operations one script may run (0 = none).",
    ),
    key(
        "limits.query.max_string_size",
        Kind::Count,
        Applies::NextLoad,
        "Query limit of every database: the longest string, in bytes (0 = none).",
    ),
    key(
        "limits.query.max_array_size",
        Kind::Count,
        Applies::NextLoad,
        "Query limit of every database: the largest array, in items (0 = none).",
    ),
    key(
        "limits.query.max_map_size",
        Kind::Count,
        Applies::NextLoad,
        "Query limit of every database: the largest map, in entries (0 = none).",
    ),
    key(
        "limits.query.max_traversers",
        Kind::Count,
        Applies::NextLoad,
        "Query limit of every database: traversers one step may leave alive (0 = none).",
    ),
    key(
        "limits.query.max_materialized_bytes",
        Kind::Count,
        Applies::NextLoad,
        "Query limit of every database: memory one query holds at once, in bytes (0 = none).",
    ),
    key(
        "limits.query.max_value_depth",
        Kind::Count,
        Applies::NextLoad,
        "Query limit of every database: the deepest nesting in one value (0 = the default 128).",
    ),
    key(
        "sessions.lifetime_secs",
        Kind::Count,
        Applies::Now,
        "The longest a browser session lasts after its login, in seconds (unset: no limit).",
    ),
    key(
        "sessions.idle_timeout_secs",
        Kind::Count,
        Applies::Now,
        "The longest a browser session lasts after its last request, in seconds (unset: no limit).",
    ),
    key(
        "access.enabled",
        Kind::Bool,
        Applies::Restart,
        "Access control; false = every request is an admin.",
    ),
    key(
        "http.public_scheme",
        Kind::Text,
        Applies::Now,
        "How clients reach the server: http, or https behind a TLS proxy (the login cookie gets Secure).",
    ),
    key(
        "http.allowed_hosts",
        Kind::Hosts,
        Applies::Restart,
        "Public host names behind a reverse proxy that requests may name in Host.",
    ),
    key(
        "jobs.backup_root",
        Kind::Text,
        Applies::Restart,
        "The only directory backup jobs write into (relative to the server directory, or absolute).",
    ),
];

/// The key named `name`.
pub fn key_of(name: &str) -> Option<&'static Key> {
    KEYS.iter().find(|key| key.name == name)
}

/// The refusal of an unknown key name.
fn unknown_key(name: &str) -> ServerError {
    invalid(format!(
        "unknown setting '{name}': the keys are {}",
        KEYS.iter().map(|k| k.name).collect::<Vec<_>>().join(", ")
    ))
}

fn invalid(message: String) -> ServerError {
    ServerError::InvalidInput {
        name: crate::layout::CONFIG_FILE.to_string(),
        message,
    }
}

/// The value of the key `name` in `config`, as JSON (`null`: not set, its default applies);
/// `None` for an unknown name.
pub fn value_of(config: &ServerConfig, name: &str) -> Option<Value> {
    if let Some(limit) = name.strip_prefix("limits.query.") {
        return config.limits.query.get(limit).map(|v| json!(v));
    }
    let limits = &config.limits;
    Some(match name {
        "listen" => json!(config.listen),
        "mcp.enabled" => json!(config.mcp.enabled),
        "mcp.max_sessions" => json!(config.mcp.max_sessions),
        "defaults.chunk_bytes" => json!(config.defaults.chunk_bytes),
        "defaults.damage_policy" => match config.damage_policy() {
            Ok(policy) => json!(policy.name()),
            Err(_) => json!(config.defaults.damage_policy),
        },
        "defaults.auto_load" => json!(config.defaults.auto_load),
        "defaults.read_only" => json!(config.defaults.read_only),
        "limits.upload_bytes" => json!(limits.upload_bytes),
        "limits.max_connections" => json!(limits.max_connections),
        "limits.header_timeout_secs" => json!(limits.header_timeout_secs),
        "limits.idle_timeout_secs" => json!(limits.idle_timeout_secs),
        "limits.memory_limit" => crate::usage::configured_limit(limits.memory_limit),
        "limits.memory_headroom" => json!(limits.memory_headroom),
        "sessions.lifetime_secs" => json!(config.sessions.lifetime_secs),
        "sessions.idle_timeout_secs" => json!(config.sessions.idle_timeout_secs),
        "access.enabled" => json!(config.access.enabled),
        "http.public_scheme" => json!(config.http.public_scheme.as_str()),
        "http.allowed_hosts" => json!(config.http.allowed_hosts),
        "jobs.backup_root" => json!(
            config
                .jobs
                .backup_root
                .as_ref()
                .map(|p| p.display().to_string())
        ),
        _ => return None,
    })
}

/// The TOML value of the key `name` given as JSON (`None`: reset to its default, from `null`).
/// `Err`: an unknown key or a value of the wrong type (the parser checks the range).
pub fn toml_of_json(name: &str, value: &Value) -> Result<Option<toml::Value>, ServerError> {
    let key = key_of(name).ok_or_else(|| unknown_key(name))?;
    if value.is_null() {
        return Ok(None);
    }
    let wrong = |what: &str| invalid(format!("{name}: expected {what}, not {value}"));
    let number = |value: &Value| {
        value
            .as_u64()
            .and_then(|n| i64::try_from(n).ok())
            .map(toml::Value::Integer)
    };
    let toml = match key.kind {
        Kind::Bool => value.as_bool().map(toml::Value::Boolean),
        Kind::Count => number(value),
        Kind::Text => value.as_str().map(|s| toml::Value::String(s.to_string())),
        Kind::Memory => number(value).or_else(|| {
            value
                .as_str()
                .map(|s| toml::Value::String(s.trim().to_ascii_lowercase()))
        }),
        Kind::Hosts => match value {
            Value::String(text) => Some(hosts_of_text(text)),
            Value::Array(items) => items
                .iter()
                .map(|item| item.as_str().map(|s| toml::Value::String(s.to_string())))
                .collect::<Option<Vec<_>>>()
                .map(toml::Value::Array),
            _ => None,
        },
    };
    let what = match key.kind {
        Kind::Bool => "true or false",
        Kind::Count => "a whole number from 0 to 9223372036854775807",
        Kind::Text => "a text",
        Kind::Memory => "a number of bytes, \"auto\" or \"none\"",
        Kind::Hosts => "a list of host names",
    };
    toml.map(Some).ok_or_else(|| wrong(what))
}

/// The TOML value of the key `name` given as console text: `true`/`false` (also `on`/`off`,
/// `yes`/`no`), a whole number (`_` between digits allowed), a text, a comma-separated list of
/// host names (empty: none).
pub fn toml_of_text(name: &str, text: &str) -> Result<toml::Value, ServerError> {
    let key = key_of(name).ok_or_else(|| unknown_key(name))?;
    let trimmed = text.trim();
    let digits: String = trimmed.chars().filter(|c| *c != '_').collect();
    let number = || {
        (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
            .then(|| digits.parse::<i64>().ok())
            .flatten()
            .map(toml::Value::Integer)
    };
    let value = match key.kind {
        Kind::Bool => match trimmed.to_ascii_lowercase().as_str() {
            "true" | "on" | "yes" => Some(toml::Value::Boolean(true)),
            "false" | "off" | "no" => Some(toml::Value::Boolean(false)),
            _ => None,
        },
        Kind::Count => number(),
        Kind::Text => Some(toml::Value::String(trimmed.to_string())),
        Kind::Memory => {
            number().or_else(|| Some(toml::Value::String(trimmed.to_ascii_lowercase())))
        }
        Kind::Hosts => Some(hosts_of_text(trimmed)),
    };
    value.ok_or_else(|| {
        invalid(match key.kind {
            Kind::Bool => format!("{name}: expected true or false, not {trimmed:?}"),
            _ => format!("{name}: expected a whole number, not {trimmed:?}"),
        })
    })
}

/// A comma-separated list of host names as a TOML array (empty text: no names).
fn hosts_of_text(text: &str) -> toml::Value {
    toml::Value::Array(
        text.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| toml::Value::String(s.to_string()))
            .collect(),
    )
}

/// The result of [`edit`].
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Edited {
    /// The new text of `server.toml`.
    pub text: String,
    /// The configuration it parses to.
    pub config: ServerConfig,
    /// The keys whose value changed (file order).
    pub changed: Vec<&'static str>,
    /// Whether the file was rewritten from its table (its own comments lost) because the
    /// line patch could not keep its layout (module docs).
    pub rewritten: bool,
}

/// The header of a rewritten `server.toml` ([`Edited::rewritten`]).
const REWRITTEN_HEADER: &str = "# graphersal-server configuration (EXPERIMENTAL).\n\
     # Rewritten by a settings change (the file's own comments could not be kept); every key\n\
     # and its default: the configuration reference of the book.\n\n";

/// Applies `changes` (key, new TOML value or `None` = back to its default) to the text of
/// `server.toml` (module docs). `path` names the file in errors.
///
/// `Err`: [`ServerError::InvalidInput`] for an unknown key or a value the parser refuses (the
/// parser's message, naming the key); [`ServerError::Config`] when `text` itself does not
/// parse.
pub fn edit(
    text: &str,
    path: &Path,
    changes: &[(String, Option<toml::Value>)],
) -> Result<Edited, ServerError> {
    let before = ServerConfig::parse(text, path)?;
    let mut table: toml::Table = toml::from_str(text).map_err(|err| ServerError::Config {
        path: path.to_path_buf(),
        message: crate::config::toml_error_message(&err, text),
    })?;
    for (name, value) in changes {
        key_of(name).ok_or_else(|| unknown_key(name))?;
        set_in_table(&mut table, name, value.clone())?;
    }
    let canonical = toml::to_string(&table).map_err(|err| invalid(err.to_string()))?;
    let config = ServerConfig::parse(&canonical, path).map_err(|err| match err {
        ServerError::Config { message, .. } => invalid(message),
        other => other,
    })?;
    let changed: Vec<&'static str> = KEYS
        .iter()
        .filter(|key| value_of(&before, key.name) != value_of(&config, key.name))
        .map(|key| key.name)
        .collect();
    let patched = patch_text(text, changes)
        .filter(|patched| ServerConfig::parse(patched, path).ok().as_ref() == Some(&config));
    let (text, rewritten) = match patched {
        Some(patched) => (patched, false),
        None => (format!("{REWRITTEN_HEADER}{canonical}"), true),
    };
    Ok(Edited {
        text,
        config,
        changed,
        rewritten,
    })
}

/// Sets (or removes) the dotted key `name` in `table`, creating the tables on its path.
fn set_in_table(
    table: &mut toml::Table,
    name: &str,
    value: Option<toml::Value>,
) -> Result<(), ServerError> {
    let mut parts: Vec<&str> = name.split('.').collect();
    let last = parts.pop().unwrap_or(name);
    let mut current = table;
    for part in parts {
        if value.is_none() && !current.contains_key(part) {
            return Ok(());
        }
        let entry = current
            .entry(part.to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        current = match entry {
            toml::Value::Table(inner) => inner,
            _ => {
                return Err(invalid(format!(
                    "{name}: '{part}' is not a table in the file"
                )));
            }
        };
    }
    match value {
        Some(value) => {
            current.insert(last.to_string(), value);
        }
        None => {
            current.remove(last);
        }
    }
    Ok(())
}

/// A TOML literal of `value` for one line (`None`: a kind the patch does not write).
fn literal(value: &toml::Value) -> Option<String> {
    Some(match value {
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Integer(n) => n.to_string(),
        // A JSON string is a valid TOML basic string (the same escapes).
        toml::Value::String(s) => serde_json::to_string(s).ok()?,
        toml::Value::Array(items) => {
            let items: Option<Vec<String>> = items.iter().map(literal).collect();
            format!("[{}]", items?.join(", "))
        }
        _ => return None,
    })
}

/// The table header a line opens (`[limits.query]` -> `limits.query`), if any.
fn header_of(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    let inner = trimmed.strip_prefix('[')?;
    if inner.starts_with('[') {
        return Some(inner);
    }
    let end = inner.find(']')?;
    let rest = inner[end + 1..].trim();
    (rest.is_empty() || rest.starts_with('#')).then(|| inner[..end].trim())
}

/// Whether `line` (without leading `#` when `commented`) assigns the key `key`.
fn assigns(line: &str, key: &str, commented: bool) -> bool {
    let mut text = line.trim_start();
    if commented {
        let Some(rest) = text.strip_prefix('#') else {
            return false;
        };
        text = rest.trim_start();
    }
    text.strip_prefix(key)
        .map(str::trim_start)
        .is_some_and(|rest| rest.starts_with('='))
}

/// The text with `changes` patched in line by line (module docs); `None` when a change meets
/// a layout this patch does not handle (the caller checks the result by parsing it anyway).
fn patch_text(text: &str, changes: &[(String, Option<toml::Value>)]) -> Option<String> {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    for (name, value) in changes {
        let (section, key) = match name.rsplit_once('.') {
            Some((section, key)) => (section, key),
            None => ("", name.as_str()),
        };
        // The lines of the section: after its header up to the next header (the top level:
        // up to the first header).
        let header_at = if section.is_empty() {
            None
        } else {
            let found: Vec<usize> = lines
                .iter()
                .enumerate()
                .filter(|(_, line)| header_of(line) == Some(section))
                .map(|(i, _)| i)
                .collect();
            match found.as_slice() {
                [] => None,
                [one] => Some(*one),
                _ => return None,
            }
        };
        if !section.is_empty() && header_at.is_none() {
            match value {
                None => continue,
                Some(value) => {
                    if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                        lines.push(String::new());
                    }
                    lines.push(format!("[{section}]"));
                    lines.push(format!("{key} = {}", literal(value)?));
                    continue;
                }
            }
        }
        let start = header_at.map_or(0, |i| i + 1);
        let end = lines[start..]
            .iter()
            .position(|line| header_of(line).is_some())
            .map_or(lines.len(), |i| start + i);
        let active = (start..end).find(|&i| assigns(&lines[i], key, false));
        if let Some(i) = active {
            let line = lines[i].clone();
            // A value continued on the next lines (a multi-line array or string): not ours.
            let value_part = line.split_once('=').map_or("", |(_, v)| v);
            if (value_part.contains('[') && !value_part.contains(']'))
                || value_part.contains("\"\"\"")
                || value_part.contains("'''")
            {
                return None;
            }
            lines[i] = match value {
                Some(value) => format!("{key} = {}", literal(value)?),
                None => format!("# {}", line.trim_start()),
            };
            continue;
        }
        let Some(value) = value else {
            continue;
        };
        let line = format!("{key} = {}", literal(value)?);
        match (start..end).find(|&i| assigns(&lines[i], key, true)) {
            Some(i) => lines[i] = line,
            None => {
                // After the section's last key or comment (before its trailing blank lines).
                let mut at = end;
                while at > start && lines[at - 1].trim().is_empty() {
                    at -= 1;
                }
                lines.insert(at, line);
            }
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    Some(out)
}

/// The settings a running server applies (module docs): what it started with, and the `now`
/// keys changed through the admin API since. Set once on the loader by `serve`
/// (`Loader::set_live_config`).
#[derive(Debug)]
pub struct LiveConfig {
    applied: RwLock<ServerConfig>,
    /// The writer lock of `server.toml` (a leaf lock: read, check, write, apply).
    writer: Mutex<()>,
    /// The access state whose session limits follow `[sessions]`.
    access: OnceLock<Arc<crate::access::Access>>,
    /// The address the server listens on and whether the command line chose it (`--listen`):
    /// it may differ from the file's `listen` without a restart pending.
    listening: Option<(std::net::SocketAddr, bool)>,
}

impl LiveConfig {
    /// The live settings of a server that started with `started`.
    pub fn new(started: ServerConfig) -> Self {
        Self {
            applied: RwLock::new(started),
            writer: Mutex::new(()),
            access: OnceLock::new(),
            listening: None,
        }
    }

    /// The same, listening on `address` (bound; a port 0 became a real port), chosen by the
    /// command line (`--listen`) when `from_command_line`.
    pub fn with_listening(
        mut self,
        address: std::net::SocketAddr,
        from_command_line: bool,
    ) -> Self {
        self.listening = Some((address, from_command_line));
        self
    }

    /// The address the server listens on, when it differs from the `listen` it started with,
    /// and why: `command line` (`--listen`) or `picked port` (port 0 in the file).
    fn listen_override(&self) -> Option<(String, &'static str)> {
        let (address, from_command_line) = self.listening?;
        let started = self.applied().listen_addr().ok();
        if started == Some(address) {
            return None;
        }
        let source = if from_command_line {
            "command line"
        } else {
            "picked port"
        };
        Some((address.to_string(), source))
    }

    /// Links the access state (its browser session limits follow `[sessions]`).
    pub(crate) fn set_access(&self, access: Arc<crate::access::Access>) {
        let _ = self.access.set(access);
    }

    /// The configuration the server applies now.
    pub fn applied(&self) -> ServerConfig {
        self.applied
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Holds the writer lock of `server.toml` while `f` runs.
    pub(crate) fn writing<T>(&self, f: impl FnOnce() -> T) -> T {
        let _guard = self
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f()
    }

    /// Applies the `now` keys of `file` (the configuration just written): the memory ceiling,
    /// the browser session limits, the upload limit and the public scheme. Answers the keys
    /// whose applied value changed. A memory setter that refuses is logged and leaves that
    /// key pending.
    pub(crate) fn apply(&self, file: &ServerConfig) -> Vec<&'static str> {
        let mut applied = self
            .applied
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = applied.clone();
        applied.limits.upload_bytes = file.limits.upload_bytes;
        applied.http.public_scheme = file.http.public_scheme;
        if applied.sessions != file.sessions {
            applied.sessions = file.sessions.clone();
            if let Some(access) = self.access.get() {
                access.set_session_limits(file.sessions.limits());
            }
        }
        let memory = (file.limits.memory_limit, file.limits.memory_headroom);
        if (applied.limits.memory_limit, applied.limits.memory_headroom) != memory {
            match crate::limits::apply_memory_live(memory.0, memory.1) {
                Ok(()) => {
                    applied.limits.memory_limit = memory.0;
                    applied.limits.memory_headroom = memory.1;
                }
                Err(message) => tracing::warn!(target: "graphersal_server", "{message}"),
            }
        }
        KEYS.iter()
            .filter(|key| value_of(&before, key.name) != value_of(&applied, key.name))
            .map(|key| key.name)
            .collect()
    }

    /// The keys waiting for a restart: the `now` and `restart` keys whose value in `file`
    /// differs from the applied one (module docs).
    pub fn pending(&self, file: &ServerConfig) -> Vec<&'static str> {
        let applied = self.applied();
        KEYS.iter()
            .filter(|key| matches!(key.applies, Applies::Now | Applies::Restart))
            .filter(|key| value_of(file, key.name) != value_of(&applied, key.name))
            .map(|key| key.name)
            .collect()
    }

    /// Whether the login cookie gets `Secure` now.
    pub(crate) fn secure_cookies(&self) -> bool {
        self.applied
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .http
            .public_scheme
            == PublicScheme::Https
    }

    /// The login cookie's `Max-Age` now (`[sessions] lifetime_secs`, else the default).
    pub(crate) fn cookie_max_age_secs(&self) -> u64 {
        self.applied
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sessions
            .lifetime_secs
            .unwrap_or(crate::router::DEFAULT_COOKIE_MAX_AGE_SECS)
    }

    /// The upload limit now.
    pub(crate) fn upload_bytes(&self) -> u64 {
        self.applied
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .limits
            .upload_bytes
    }
}

/// The JSON of the settings of `text` (the file now; module docs): `path`, `revision`,
/// `running` (a running server answered), `restartPending` (keys), `keys` (one row per key:
/// `key`, `section`, `value` (the file's, `null` = not set), `default`, `set` (the file names
/// it), `kind` (`bool` | `count` | `text` | `memory` | `hosts`), `choices` (the values a text
/// key takes, else `null`), `applies`, `appliesText`, `running` (what the running server
/// applies: `now`/`restart` keys only, else `null`; `listen`: the address it listens on),
/// `runningSource` (`listen` only: `command line` or `picked port` when the server listens on
/// another address than the `listen` it started with, else `null`; not a restart pending),
/// `pending`, `help`) and `queryLimitKeys`.
pub(crate) fn view_json(
    path: &Path,
    text: &str,
    config: &ServerConfig,
    live: Option<&LiveConfig>,
) -> Value {
    let table: toml::Table = toml::from_str(text).unwrap_or_default();
    let defaults = ServerConfig::default();
    let applied = live.map(LiveConfig::applied);
    let pending = live.map(|live| live.pending(config)).unwrap_or_default();
    let listen_override = live.and_then(LiveConfig::listen_override);
    let keys: Vec<Value> = KEYS
        .iter()
        .map(|key| {
            let section = key.name.rsplit_once('.').map_or("", |(s, _)| s);
            let running = match (&applied, key.applies) {
                (Some(applied), Applies::Now | Applies::Restart) => {
                    value_of(applied, key.name).unwrap_or(Value::Null)
                }
                _ => Value::Null,
            };
            let (running, running_source) = match (&listen_override, key.name) {
                (Some((address, source)), "listen") => (json!(address), json!(source)),
                _ => (running, Value::Null),
            };
            json!({
                "key": key.name,
                "section": section,
                "value": value_of(config, key.name).unwrap_or(Value::Null),
                "default": value_of(&defaults, key.name).unwrap_or(Value::Null),
                "set": is_set(&table, key.name),
                "kind": key.kind.as_str(),
                "choices": choices_of(key.name),
                "applies": key.applies.as_str(),
                "appliesText": key.applies.text(),
                "running": running,
                "runningSource": running_source,
                "pending": pending.contains(&key.name),
                "help": key.help,
            })
        })
        .collect();
    json!({
        "path": path.display().to_string(),
        "revision": crate::revision::of(text.as_bytes()),
        "running": live.is_some(),
        "restartPending": pending,
        "keys": keys,
        "queryLimitKeys": QUERY_LIMIT_KEYS,
    })
}

/// Whether the file's table names the dotted key `name`.
fn is_set(table: &toml::Table, name: &str) -> bool {
    let mut parts = name.split('.').peekable();
    let mut current = table;
    while let Some(part) = parts.next() {
        match current.get(part) {
            None => return false,
            Some(value) if parts.peek().is_none() => {
                return !matches!(value, toml::Value::Table(_));
            }
            Some(toml::Value::Table(inner)) => current = inner,
            Some(_) => return false,
        }
    }
    false
}

/// The text of the `server.toml` (or a file meant to become one) at `path`.
///
/// `Err`: [`ServerError::Io`].
pub fn read_text(path: &Path) -> Result<String, ServerError> {
    std::fs::read_to_string(path).map_err(|err| ServerError::io("read", path, err))
}

/// Checks the text of a `server.toml` (`graphersal-server config check`, `init --config`):
/// the configuration it parses to, or the parser's error.
pub fn check_text(text: &str, path: &Path) -> Result<ServerConfig, ServerError> {
    ServerConfig::parse(text, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_config_text;

    fn set(name: &str, value: Value) -> (String, Option<toml::Value>) {
        (name.to_string(), toml_of_json(name, &value).unwrap())
    }

    fn path() -> &'static Path {
        Path::new("server.toml")
    }

    #[test]
    fn every_key_has_a_value_and_the_default_text_sets_its_defaults() {
        let config = ServerConfig::default();
        for key in KEYS {
            assert!(value_of(&config, key.name).is_some(), "{}", key.name);
        }
        for limit in QUERY_LIMIT_KEYS {
            assert!(
                key_of(&format!("limits.query.{limit}")).is_some(),
                "{limit}"
            );
        }
        assert_eq!(value_of(&config, "nope"), None);
    }

    #[test]
    fn an_edit_keeps_the_comments_and_takes_over_commented_examples() {
        let text = default_config_text();
        let edited = edit(
            &text,
            path(),
            &[
                set("limits.upload_bytes", json!(1024)),
                set("limits.query.timeout_ms", json!(5000)),
                set("sessions.lifetime_secs", json!(3600)),
                set("http.allowed_hosts", json!(["db.example.com"])),
                set("defaults.auto_load", json!(true)),
                set("listen", json!("127.0.0.1:0")),
            ],
        )
        .unwrap();
        assert!(!edited.rewritten);
        assert!(edited.text.contains("# The largest import upload"));
        assert!(edited.text.contains("\nupload_bytes = 1024\n"));
        assert!(edited.text.contains("\ntimeout_ms = 5000\n"));
        assert!(!edited.text.contains("# timeout_ms = 30000"));
        assert!(edited.text.contains("\nlifetime_secs = 3600\n"));
        assert!(
            edited
                .text
                .contains("\nallowed_hosts = [\"db.example.com\"]\n")
        );
        assert!(edited.text.contains("\nlisten = \"127.0.0.1:0\"\n"));
        assert_eq!(edited.config.limits.upload_bytes, 1024);
        assert_eq!(edited.config.limits.query.timeout_ms, Some(5000));
        assert!(edited.config.defaults.auto_load);
        assert_eq!(
            edited.changed,
            [
                "listen",
                "defaults.auto_load",
                "limits.upload_bytes",
                "limits.query.timeout_ms",
                "sessions.lifetime_secs",
                "http.allowed_hosts"
            ]
        );
        // Back to the defaults: the lines are commented out again.
        let reset = edit(
            &edited.text,
            path(),
            &[
                ("limits.query.timeout_ms".to_string(), None),
                ("limits.upload_bytes".to_string(), None),
            ],
        )
        .unwrap();
        assert!(!reset.rewritten);
        assert!(reset.text.contains("# timeout_ms = 5000"));
        assert_eq!(reset.config.limits.query.timeout_ms, None);
        assert_eq!(
            reset.config.limits.upload_bytes,
            crate::config::DEFAULT_UPLOAD_BYTES
        );
    }

    #[test]
    fn a_missing_section_is_added_and_an_unknown_layout_is_rewritten() {
        let edited = edit(
            "listen = \"127.0.0.1:1\"\n",
            path(),
            &[set("jobs.backup_root", json!("vault"))],
        )
        .unwrap();
        assert!(!edited.rewritten);
        assert_eq!(
            edited.text,
            "listen = \"127.0.0.1:1\"\n\n[jobs]\nbackup_root = \"vault\"\n"
        );
        // A dotted key the line patch does not know: the file is rewritten from its table.
        let edited = edit(
            "# mine\nlimits.upload_bytes = 5\n",
            path(),
            &[set("limits.upload_bytes", json!(7))],
        )
        .unwrap();
        assert!(edited.rewritten);
        assert!(!edited.text.contains("# mine"));
        assert_eq!(edited.config.limits.upload_bytes, 7);
    }

    #[test]
    fn bad_values_and_unknown_keys_are_refused_with_the_parsers_words() {
        let text = default_config_text();
        for (name, value, needle) in [
            ("limits.upload_bytes", json!(0), "limits.upload_bytes"),
            ("listen", json!("nowhere"), "listen"),
            ("defaults.damage_policy", json!("panic"), "damage_policy"),
            ("http.public_scheme", json!("ftp"), "http"),
            ("http.allowed_hosts", json!(["a b"]), "allowed_hosts"),
            ("limits.memory_limit", json!("lots"), "memory_limit"),
            ("sessions.lifetime_secs", json!(0), "sessions.lifetime_secs"),
        ] {
            let changes = [(name.to_string(), toml_of_json(name, &value).unwrap())];
            let err = edit(&text, path(), &changes).unwrap_err().to_string();
            assert!(err.contains(needle), "{name}: {err}");
        }
        assert!(
            toml_of_json("nope", &json!(1))
                .unwrap_err()
                .to_string()
                .contains("unknown setting 'nope'")
        );
        assert!(
            toml_of_json("mcp.enabled", &json!(1))
                .unwrap_err()
                .to_string()
                .contains("true or false")
        );
        assert!(toml_of_json("limits.upload_bytes", &json!(-1)).is_err());
        assert_eq!(
            toml_of_text("mcp.enabled", "off").unwrap(),
            toml::Value::Boolean(false)
        );
        assert_eq!(
            toml_of_text("limits.upload_bytes", "1_024").unwrap(),
            toml::Value::Integer(1024)
        );
        assert_eq!(
            toml_of_text("limits.memory_limit", "Auto").unwrap(),
            toml::Value::String("auto".into())
        );
        assert_eq!(
            toml_of_text("http.allowed_hosts", "a.example, b.example").unwrap(),
            toml::Value::Array(vec![
                toml::Value::String("a.example".into()),
                toml::Value::String("b.example".into())
            ])
        );
        assert!(toml_of_text("limits.upload_bytes", "lots").is_err());
    }

    #[test]
    fn restart_pending_lists_the_keys_the_running_server_does_not_apply() {
        let live = LiveConfig::new(ServerConfig::default());
        let text = default_config_text();
        let edited = edit(
            &text,
            path(),
            &[
                set("listen", json!("127.0.0.1:0")),
                set("limits.upload_bytes", json!(99)),
                set("limits.query.timeout_ms", json!(1)),
                set("defaults.read_only", json!(true)),
            ],
        )
        .unwrap();
        assert_eq!(
            live.pending(&edited.config),
            ["listen", "limits.upload_bytes"]
        );
        assert_eq!(live.apply(&edited.config), ["limits.upload_bytes"]);
        assert_eq!(live.upload_bytes(), 99);
        assert_eq!(live.pending(&edited.config), ["listen"]);
        let view = view_json(path(), &edited.text, &edited.config, Some(&live));
        assert_eq!(view["restartPending"], json!(["listen"]));
        let row = |name: &str| {
            view["keys"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["key"] == name)
                .unwrap()
                .clone()
        };
        assert_eq!(row("listen")["pending"], json!(true));
        assert_eq!(row("listen")["running"], json!("127.0.0.1:8484"));
        assert_eq!(row("limits.query.timeout_ms")["applies"], json!("load"));
        assert_eq!(row("limits.query.timeout_ms")["set"], json!(true));
        assert_eq!(row("limits.query.max_operations")["set"], json!(false));
        assert_eq!(row("defaults.read_only")["applies"], json!("create"));
        assert_eq!(row("limits.upload_bytes")["running"], json!(99));
        assert_eq!(row("listen")["runningSource"], Value::Null);
    }

    #[test]
    fn a_listen_address_from_the_command_line_is_running_not_pending() {
        let text = default_config_text();
        let config = ServerConfig::default();
        let started = config.listen_addr().unwrap();
        let other: std::net::SocketAddr = "127.0.0.1:18731".parse().unwrap();
        let row_of = |live: &LiveConfig| {
            let view = view_json(path(), &text, &config, Some(live));
            assert_eq!(view["restartPending"], json!([]), "{view}");
            view["keys"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["key"] == "listen")
                .unwrap()
                .clone()
        };
        let row = row_of(&LiveConfig::new(config.clone()).with_listening(other, true));
        assert_eq!(row["pending"], json!(false));
        assert_eq!(row["running"], json!("127.0.0.1:18731"));
        assert_eq!(row["runningSource"], json!("command line"));
        let row = row_of(&LiveConfig::new(config.clone()).with_listening(other, false));
        assert_eq!(row["runningSource"], json!("picked port"));
        // The same address as the file: nothing to say.
        let row = row_of(&LiveConfig::new(config.clone()).with_listening(started, true));
        assert_eq!(row["runningSource"], Value::Null);
        assert_eq!(row["running"], json!(config.listen));
    }
}
