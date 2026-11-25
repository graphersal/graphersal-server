//! The stderr start block of `serve` (the graphersal CLI's `--server` start block): the
//! banner, for a non-loopback address the network warning, then the sections Server,
//! Databases, Embedding models and MCP. stdout keeps only the address line
//! ([`address_line`]).

use std::path::Path;

use graphersal_serve::Exposure;
use serde_json::Value;

use super::render::{database_table, problems_section, shell_word};
use crate::config::ServerConfig;
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
    let config = running.dir().load_config().ok();
    let mut out = vec![banner(running.exposure())];
    if running.exposure() == Exposure::Network {
        out.push(network_warning(&running.address().to_string()));
    }
    out.push(String::new());
    out.push(text::render(&sections(running, config.as_ref())));
    out.join("\n")
}

/// The banner (module docs) for `exposure`.
fn banner(exposure: Exposure) -> String {
    match exposure {
        Exposure::Network => BANNER.replacen(
            LOOPBACK_LINE,
            " REACHABLE FROM THE NETWORK, see the warning below.\n Users and grants in access.toml. ",
            1,
        ),
        _ => BANNER.to_string(),
    }
}

/// The warning under the banner of a server reachable from the network.
fn network_warning(address: &str) -> String {
    let mut section = Section::new("");
    section.row(
        "warning",
        format!(
            "graphersal-server listens on {address} and is reachable from the network: \
             requests need a user token or a browser login, and over plain HTTP tokens and \
             session cookies travel in clear text; put a TLS proxy in front, or listen on \
             127.0.0.1 for this machine only (serve --listen 127.0.0.1:PORT)"
        ),
    );
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
    server.row("stop", "Ctrl+C or SIGTERM");

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
}
