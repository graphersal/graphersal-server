//! The stderr start block of `serve` (the graphersal CLI's `--server` start block): the
//! banner, the warnings (access control off; a non-loopback address: tokens in clear text over
//! plain HTTP, or only the TLS proxy should reach it), then the sections Server,
//! Databases, Embedding models and MCP. stdout keeps only the address line
//! ([`address_line`]).

use std::path::Path;

use graphersal_serve::Exposure;
use serde_json::Value;

use super::render::{database_table, problems_section, shell_word};
use crate::config::{PublicScheme, ServerConfig};
use crate::serve::Running;
use crate::text::{self, Note, Section};

/// The banner of a server on the loopback.
const BANNER: &str = "\
==============================================================================
 graphersal-server - EXPERIMENTAL: many Graphersal graph databases over HTTP
 Users and grants in access.toml. Loopback only. Every database is a Store.
 The admin API and the server directory change without notice.
==============================================================================";

/// The line of [`BANNER`] a non-loopback server replaces.
const LOOPBACK_LINE: &str = " Users and grants in access.toml. Loopback only. ";

/// The one stdout line of `serve`: the address a browser on this machine opens (scripts read
/// it; everything else goes to stderr).
pub fn address_line(running: &Running) -> String {
    format!("Graphersal server: {}", running.url())
}

/// The stderr start block of `running` (module docs).
pub fn start_block(running: &Running) -> String {
    // The configuration the server runs with (read at start).
    let config = running.config();
    let mut out = vec![banner_for(running.exposure(), config.access.enabled)];
    out.extend(warnings(
        running.exposure(),
        &running.address().to_string(),
        config,
    ));
    out.push(String::new());
    out.push(text::render(&sections(running, Some(config))));
    out.join("\n")
}

/// The banner (module docs) for `exposure` with access control on.
#[cfg(test)]
fn banner(exposure: Exposure) -> String {
    banner_for(exposure, true)
}

/// The banner (module docs) for `exposure` and `[access] enabled`.
fn banner_for(exposure: Exposure, access_enabled: bool) -> String {
    let line = match (exposure, access_enabled) {
        (Exposure::Network, true) => {
            " REACHABLE FROM THE NETWORK, see the warning below.\n Users and grants in access.toml. "
        }
        (Exposure::Network, false) => {
            " REACHABLE FROM THE NETWORK WITH ACCESS CONTROL OFF: ANYONE IS AN ADMIN.\n \
             See the warning below. "
        }
        (_, false) => {
            " ACCESS CONTROL OFF ([access] enabled = false): every request is an admin.\n \
             Loopback only. "
        }
        (_, true) => LOOPBACK_LINE,
    };
    BANNER.replacen(LOOPBACK_LINE, line, 1)
}

/// The warnings under the banner (module docs) for `exposure`, the listen `address` and
/// `config`: each a `warning:` row at the left margin.
fn warnings(exposure: Exposure, address: &str, config: &ServerConfig) -> Vec<String> {
    let network = exposure == Exposure::Network;
    if !config.access.enabled {
        return vec![access_off_warning(network, address)];
    }
    if !network {
        return Vec::new();
    }
    match config.http.public_scheme {
        PublicScheme::Https => vec![proxy_warning(address)],
        _ => vec![network_warning(address)],
    }
}

/// The warning of a server with access control off: louder when it is reachable from the
/// network (`network`, listening on `address`).
fn access_off_warning(network: bool, address: &str) -> String {
    let text = if network {
        format!(
            "ACCESS CONTROL IS OFF ([access] enabled = false in server.toml) and \
             graphersal-server listens on {address}, reachable from the network: ANYONE WHO \
             REACHES IT IS AN ADMIN and can read, change and delete every database without a \
             token; set [access] enabled = true, or listen on 127.0.0.1 for this machine only \
             (serve --listen 127.0.0.1:PORT)"
        )
    } else {
        "access control is off ([access] enabled = false in server.toml): every request is \
         an admin without a token or login, so every program and user on this machine can \
         read, change and delete every database; set [access] enabled = true to require \
         tokens"
            .to_string()
    };
    warning_row(text)
}

/// The warning of a server reachable from the network behind a TLS proxy
/// (`public_scheme = "https"`).
fn proxy_warning(address: &str) -> String {
    warning_row(format!(
        "graphersal-server listens on {address} and is reachable from the network; \
         [http] public_scheme = \"https\" says a TLS proxy ends the connections: let only \
         that proxy reach this address (plain HTTP to it carries tokens and session cookies \
         in clear text)"
    ))
}

/// The warning under the banner of a server reachable from the network over plain HTTP
/// (`public_scheme = "http"`).
fn network_warning(address: &str) -> String {
    warning_row(format!(
        "graphersal-server listens on {address} and is reachable from the network over plain \
         HTTP ([http] public_scheme = \"http\"): requests need a user token or a browser \
         login, and tokens travel in clear text, session cookies too; put a TLS proxy in \
         front and set public_scheme = \"https\", or listen on 127.0.0.1 for this machine \
         only (serve --listen 127.0.0.1:PORT)"
    ))
}

/// `text` as one `warning:` row at the left margin, wrapped.
fn warning_row(text: String) -> String {
    let mut section = Section::new("");
    section.row("warning", text);
    // The section's rows without its heading, at the left margin.
    text::render(&[section])
        .lines()
        .skip(1)
        .map(|line| line.strip_prefix("  ").unwrap_or(line).to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The sections of the start block.
fn sections(running: &Running, config: Option<&ServerConfig>) -> Vec<Section> {
    let url = running.url().trim_end_matches('/');
    let dir_arg = shell_word(running.dir().root());

    let mut server = Section::new("Server");
    server.unwrapped("directory", running.dir().root().display().to_string());
    server.row(
        "version",
        format!("{} (EXPERIMENTAL)", env!("CARGO_PKG_VERSION")),
    );
    let reach = match running.exposure() {
        Exposure::Network => "REACHABLE FROM THE NETWORK",
        _ => "loopback, this machine only",
    };
    server.row("listening", format!("{} ({reach})", running.address()));
    if let Some(config) = config {
        for (key, value) in mode_rows(config) {
            server.row(key, value);
        }
    }
    server.unwrapped("UI", format!("{url}/"));
    if let Some(config) = config {
        server.row(
            "uploads",
            format!(
                "up to {} per import",
                text::bytes(config.limits.upload_bytes)
            ),
        );
    }
    if let Some(config) = config {
        for (key, value) in limit_rows(config) {
            server.row(key, value);
        }
    }
    let limits = running.connection_limits();
    server.row(
        "connections",
        format!("up to {} open (HTTP/1.1, h2c)", limits.max_connections),
    );
    server.row(
        "timeouts",
        format!(
            "request head {} s, idle connection {} s",
            limits.header_timeout.as_secs(),
            limits.idle_timeout.as_secs()
        ),
    );
    server.unwrapped(
        "stop",
        format!("Ctrl+C, SIGTERM, or graphersal-server stop --dir {dir_arg}"),
    );

    let registry = running.registry();
    let mut entries = registry.list();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let rows: Vec<Value> = entries
        .iter()
        .map(|entry| crate::admin_api::entry_json(entry, None))
        .collect();
    let rows: Vec<&Value> = rows.iter().collect();
    let mut databases = Section::new(format!("Databases ({})", rows.len()));
    if rows.is_empty() {
        databases.note(
            "none",
            &Note::new("the server directory holds no database yet")
                .command(
                    "create one",
                    format!("graphersal-server db create NAME --dir {dir_arg}"),
                )
                .text(),
        );
    } else {
        databases.lines(database_table(&rows, false));
        databases.lines([String::new()]);
    }
    let aliases = registry.aliases();
    if !aliases.is_empty() {
        let list: Vec<String> = aliases
            .iter()
            .map(|(alias, target)| format!("{alias} -> {target}"))
            .collect();
        databases.row("aliases", list.join(", "));
    }
    if !rows.is_empty() {
        databases.unwrapped("served at", format!("{url}/db/<name>/ (once loaded)"));
    }
    let problems = problems_section(&rows, &registry.problems(), &dir_arg);

    let mut models = Section::new("Embedding models");
    let blobs = model_blobs(&running.dir().model_blobs());
    models.row(
        "shared",
        format!(
            "{} in models/blobs/, hard-linked into the databases using them",
            text::count(blobs, "model blob")
        ),
    );
    models.unwrapped(
        "list",
        format!("graphersal-server models list --dir {dir_arg}"),
    );

    let mut mcp = Section::new("MCP");
    match config.map(|c| &c.mcp) {
        Some(m) if m.enabled => {
            mcp.unwrapped("endpoint", format!("{url}/mcp"));
            mcp.row(
                "graphs",
                "every database and alias (list_graphs, use_graph)",
            );
            mcp.unwrapped("per database", format!("{url}/db/<name>/mcp (once loaded)"));
            let connect = format!(
                "claude mcp add --transport http --header \"Authorization: Bearer <token>\" \
                 graphersal-server {url}/mcp"
            );
            mcp.row(
                "access",
                "per principal: a user token (Authorization: Bearer <token>) sees its granted \
                 databases, a request without one the anonymous grants (access.toml)",
            );
            mcp.unwrapped("connect", connect);
        }
        Some(_) => mcp.row("endpoint", "off ([mcp] enabled = false in server.toml)"),
        None => mcp.row("endpoint", "unknown (server.toml could not be read again)"),
    }
    vec![server, databases, problems, models, mcp]
}

/// The start-block rows of the access and HTTP modes of `config`: access control, the public
/// scheme (and the login cookie it implies), the allowed host names.
fn mode_rows(config: &ServerConfig) -> Vec<(&'static str, String)> {
    let access = if config.access.enabled {
        "on: user tokens and browser logins (access.toml)".to_string()
    } else {
        "OFF: every request is an admin ([access] enabled = false)".to_string()
    };
    let cookie_days = config
        .sessions
        .lifetime_secs
        .unwrap_or(crate::router::DEFAULT_COOKIE_MAX_AGE_SECS)
        .div_ceil(86_400);
    let scheme = match config.http.public_scheme {
        PublicScheme::Https => format!(
            "https, TLS ended by a proxy (login cookie Secure, kept {})",
            text::count(cookie_days as usize, "day")
        ),
        _ => format!(
            "http (login cookie without Secure, kept {})",
            text::count(cookie_days as usize, "day")
        ),
    };
    let hosts = if config.http.allowed_hosts.is_empty() {
        "none besides this machine's names and the listen address ([http] allowed_hosts)"
            .to_string()
    } else {
        format!("{} (any port)", config.http.allowed_hosts.join(", "))
    };
    vec![
        ("access", access),
        ("scheme", scheme),
        ("allowed hosts", hosts),
    ]
}

/// The start-block rows of the query limits and the process memory ceiling of `config`, each
/// only when `server.toml` sets it.
fn limit_rows(config: &ServerConfig) -> Vec<(&'static str, String)> {
    let mut rows = Vec::new();
    let query = &config.limits.query;
    if !query.is_empty() {
        rows.push((
            "query limits",
            format!(
                "{} ([limits.query]; a database's database.toml overrides single keys; applied \
                 at each load)",
                query.summary()
            ),
        ));
    }
    let limits = &config.limits;
    if limits.memory_limit.is_some() || limits.memory_headroom.is_some() {
        let setting = limits
            .memory_limit
            .map(|limit| format!("memory_limit = {limit}"))
            .into_iter()
            .chain(
                limits
                    .memory_headroom
                    .map(|bytes| format!("memory_headroom = {}", text::bytes(bytes))),
            )
            .collect::<Vec<_>>()
            .join(", ");
        let effective = match graphersal::exec::memory_limit() {
            Ok(limit) => match limit.bytes() {
                Some(bytes) => format!("{} ({})", text::bytes(bytes), limit.source()),
                None => format!("none ({})", limit.source()),
            },
            Err(err) => format!("unknown ({err})"),
        };
        rows.push((
            "memory ceiling",
            format!("{effective} for the whole process ([limits] {setting})"),
        ));
    }
    rows
}

/// How many blobs `models/blobs/<2 hex>/` holds (0 when it cannot be read).
fn model_blobs(blobs: &Path) -> usize {
    let Ok(prefixes) = std::fs::read_dir(blobs) else {
        return 0;
    };
    prefixes
        .flatten()
        .filter_map(|prefix| std::fs::read_dir(prefix.path()).ok())
        .map(|files| {
            files
                .flatten()
                .filter(|f| f.file_type().map(|t| t.is_file()).unwrap_or(false))
                .count()
        })
        .sum()
}

/// The stderr line when a stop signal arrived.
pub fn stopping_text() -> String {
    "Stopping: no new requests; the running ones get 5 s, then every database is unloaded \
     and its Store closed."
        .to_string()
}

/// The stderr line after a clean stop of the server on `dir`.
pub fn stopped_text(dir: &Path) -> String {
    format!(
        "Stopped: {} is free again (server.run removed, lock released).",
        dir.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::WIDTH;

    #[test]
    fn the_banner_is_as_wide_as_the_blocks_and_names_the_network() {
        let loopback = banner(Exposure::Loopback);
        assert!(
            loopback.lines().all(|l| l.chars().count() <= WIDTH),
            "{loopback}"
        );
        assert_eq!(loopback.lines().next().unwrap().chars().count(), WIDTH);
        assert!(loopback.contains("Loopback only"), "{loopback}");
        let network = banner(Exposure::Network);
        assert!(network.contains("REACHABLE FROM THE NETWORK"), "{network}");
        assert!(!network.contains("Loopback only"), "{network}");
        assert!(
            network.lines().all(|l| l.chars().count() <= WIDTH),
            "{network}"
        );
        let warning = network_warning("0.0.0.0:8484");
        let lines: Vec<&str> = warning.lines().collect();
        assert!(lines[0].starts_with("warning: graphersal-server listens on 0.0.0.0:8484"));
        assert!(lines[1].starts_with("         "), "{warning}");
        assert!(
            lines.iter().all(|l| l.chars().count() <= WIDTH),
            "{warning}"
        );
    }

    fn config(text: &str) -> ServerConfig {
        ServerConfig::parse(text, Path::new("server.toml")).unwrap()
    }

    fn assert_fits(text: &str) {
        assert!(text.lines().all(|l| l.chars().count() <= WIDTH), "{text}");
    }

    #[test]
    fn the_banner_says_when_access_control_is_off() {
        for exposure in [Exposure::Loopback, Exposure::Network] {
            let on = banner_for(exposure, true);
            assert_eq!(on, banner(exposure));
            assert!(on.contains("Users and grants in access.toml"), "{on}");
            let off = banner_for(exposure, false);
            assert_fits(&off);
            assert_eq!(off.lines().next().unwrap().chars().count(), WIDTH);
            assert!(!off.contains("Users and grants"), "{off}");
            assert!(off.contains("ACCESS CONTROL OFF"), "{off}");
        }
        let loopback = banner_for(Exposure::Loopback, false);
        assert!(loopback.contains("every request is an admin"), "{loopback}");
        assert!(loopback.contains("Loopback only"), "{loopback}");
        let network = banner_for(Exposure::Network, false);
        assert!(network.contains("REACHABLE FROM THE NETWORK"), "{network}");
        assert!(network.contains("ANYONE IS AN ADMIN"), "{network}");
        assert!(!network.contains("Loopback only"), "{network}");
    }

    #[test]
    fn warnings_per_mode_and_exposure() {
        let address = "0.0.0.0:8484";
        // Access on, on the loopback: no warning, whatever the scheme.
        for text in ["", "[http]\npublic_scheme = \"https\""] {
            assert!(warnings(Exposure::Loopback, address, &config(text)).is_empty());
        }
        // Access on, reachable over plain HTTP: tokens travel in clear text.
        let flat = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
        let http = warnings(Exposure::Network, address, &config("")).join("\n");
        assert!(http.starts_with("warning: graphersal-server listens on 0.0.0.0:8484"));
        assert!(
            flat(&http).contains("tokens travel in clear text"),
            "{http}"
        );
        assert!(flat(&http).contains("public_scheme = \"https\""), "{http}");
        assert_fits(&http);
        // Behind a TLS proxy: no clear-text claim, but only the proxy should reach it.
        let https = warnings(
            Exposure::Network,
            address,
            &config("[http]\npublic_scheme = \"https\""),
        )
        .join("\n");
        assert!(!https.contains("tokens travel in clear text"), "{https}");
        assert!(
            https.contains("let only that proxy reach this address"),
            "{https}"
        );
        assert_fits(&https);
        // Access off: one loud warning, louder off the loopback.
        let off = config("[access]\nenabled = false");
        let local = warnings(Exposure::Loopback, address, &off);
        assert_eq!(local.len(), 1);
        assert!(
            local[0].starts_with("warning: access control is off"),
            "{local:?}"
        );
        assert!(flat(&local[0]).contains("every program and user on this machine"));
        assert_fits(&local[0]);
        let remote = warnings(Exposure::Network, address, &off);
        assert_eq!(remote.len(), 1);
        assert!(
            remote[0].starts_with("warning: ACCESS CONTROL IS OFF"),
            "{remote:?}"
        );
        assert!(flat(&remote[0]).contains("ANYONE WHO REACHES IT IS AN ADMIN"));
        assert!(remote[0].contains("0.0.0.0:8484"));
        assert_fits(&remote[0]);
    }

    #[test]
    fn mode_rows_name_access_scheme_cookie_and_hosts() {
        let rows = mode_rows(&config(""));
        assert_eq!(
            rows[0],
            (
                "access",
                "on: user tokens and browser logins (access.toml)".to_string()
            )
        );
        assert_eq!(rows[1].0, "scheme");
        assert!(
            rows[1]
                .1
                .starts_with("http (login cookie without Secure, kept 400 days"),
            "{rows:?}"
        );
        assert_eq!(rows[2].0, "allowed hosts");
        assert!(rows[2].1.starts_with("none"), "{rows:?}");
        let rows = mode_rows(&config(
            "[access]\nenabled = false\n[sessions]\nlifetime_secs = 86400\n\
             [http]\npublic_scheme = \"https\"\nallowed_hosts = [\"db.example.com\", \"proxy\"]",
        ));
        assert!(
            rows[0].1.starts_with("OFF: every request is an admin"),
            "{rows:?}"
        );
        assert!(
            rows[1]
                .1
                .starts_with("https, TLS ended by a proxy (login cookie Secure, kept 1 day"),
            "{rows:?}"
        );
        assert_eq!(rows[2].1, "db.example.com, proxy (any port)");
    }

    #[test]
    fn limit_rows_only_when_server_toml_sets_limits() {
        assert!(limit_rows(&config("")).is_empty());
        assert!(limit_rows(&config(&crate::config::default_config_text())).is_empty());
        let rows = limit_rows(&config(
            "[limits]\nmemory_limit = 8589934592\nmemory_headroom = 1048576\n\
             [limits.query]\ntimeout_ms = 30000\nmax_traversers = 100",
        ));
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0].0, "query limits");
        assert!(
            rows[0]
                .1
                .starts_with("timeout_ms=30000, max_traversers=100 ([limits.query]"),
            "{rows:?}"
        );
        assert_eq!(rows[1].0, "memory ceiling");
        assert!(
            rows[1]
                .1
                .contains("for the whole process ([limits] memory_limit = 8589934592, memory_headroom = 1.0 MiB)"),
            "{rows:?}"
        );
        let mut section = Section::new("Server");
        for (key, value) in rows {
            section.row(key, value);
        }
        assert_fits(&text::render(&[section]));
    }
}
