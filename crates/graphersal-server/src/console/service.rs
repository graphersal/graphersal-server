//! The text of `graphersal-server start`, `stop` and `status` ([`crate::daemon`]).

use std::path::Path;

use serde_json::{Value, json};

use super::render::{BIN, shell_word};
use crate::daemon::{ADDRESS_PREFIX, Started, Status, Stopped};
use crate::text::{self, Note, Section};

/// What `start` prints: the server's own start block (stderr) and its address line (stdout,
/// `None` if it printed none), as `serve` prints them.
pub fn started_output(started: &Started) -> (String, Option<String>) {
    let mut block = Vec::new();
    let mut address = None;
    for line in started.output.lines() {
        if is_log_record(line) {
            // The server's log records stay in its log file.
            continue;
        }
        if line.starts_with(ADDRESS_PREFIX) {
            address = Some(line.to_string());
        } else {
            block.push(line);
        }
    }
    (block.join("\n").trim_matches('\n').to_string(), address)
}

/// Whether `line` is a record of the server's log (`2026-11-26T10:00:00.123Z  INFO ...`), not
/// a line of its start block.
fn is_log_record(line: &str) -> bool {
    let bytes = line.as_bytes();
    bytes.len() > 20
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
}

/// The note under the start block of `start` on `dir`: where the server runs and how to see
/// and stop it.
pub fn started_note(started: &Started, dir: &Path) -> String {
    let arg = shell_word(dir);
    Note::new(format!(
        "Started in the background: pid {}, {}",
        started.pid, started.run.url
    ))
    .detail("log", started.log.display().to_string())
    .command("its state", format!("{BIN} status --dir {arg}"))
    .command("stop it", format!("{BIN} stop --dir {arg}"))
    .text()
}

/// What `stop` on `dir` prints (stdout).
pub fn stop_text(stopped: &Stopped, dir: &Path) -> String {
    match stopped {
        Stopped::Stopped {
            pid,
            url,
            signalled,
        } => {
            let mut note = Note::new(format!("stopped: {}", dir.display()));
            if let Some(pid) = pid {
                note = note.detail("pid", pid.to_string());
            }
            if let Some(url) = url {
                note = note.detail("was at", url.clone());
            }
            if *signalled {
                note = note.detail(
                    "how",
                    "SIGTERM (the admin API could not be used; the same clean stop)",
                );
            }
            note.text()
        }
        Stopped::NotRunning { stale, address } => {
            let mut note = Note::new(format!("not running: {}", dir.display()));
            if let Some(stale) = stale {
                note = note.detail(
                    "stale",
                    format!(
                        "removed the server.run of pid {} ({}): that server did not stop \
                         cleanly",
                        stale.pid, stale.url
                    ),
                );
            }
            if let Some(address) = address {
                note = note.detail("no answer", format!("nothing answers at {address}"));
            }
            note.text()
        }
    }
}

/// `secs` as people read an uptime: `3 d 4 h`, `2 h 5 min`, `4 min 10 s`, `12 s`.
pub(crate) fn uptime_text(secs: u64) -> String {
    let (d, h, m, s) = (
        secs / 86_400,
        secs % 86_400 / 3_600,
        secs % 3_600 / 60,
        secs % 60,
    );
    if d > 0 {
        format!("{d} d {h} h")
    } else if h > 0 {
        format!("{h} h {m} min")
    } else if m > 0 {
        format!("{m} min {s} s")
    } else {
        format!("{s} s")
    }
}

/// The names of the JSON array `value` (strings only).
fn names(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// What `status` on `dir` prints (stdout).
pub fn status_text(status: &Status, dir: &Path) -> String {
    let arg = shell_word(dir);
    let mut section = Section::new("Server");
    section.unwrapped("directory", dir.display().to_string());
    if !status.running {
        section.row("state", "stopped");
        if let (true, Some(run)) = (status.stale, &status.run) {
            section.row(
                "stale",
                format!(
                    "server.run of pid {} ({}) is left from a server that did not stop \
                     cleanly; the next start replaces it, stop removes it",
                    run.pid, run.url
                ),
            );
        }
        if let Some(why) = &status.unanswered {
            section.row("no answer", why.clone());
        }
        if let Some(log) = &status.log {
            section.unwrapped("log", log.display().to_string());
        }
        section.unwrapped("start", format!("{BIN} start --dir {arg}"));
        return text::render(&[section]);
    }
    section.row("state", "running");
    let server = status.server.as_ref();
    let field = |key: &str| server.and_then(|s| s.get(key));
    if let Some(run) = &status.run {
        section.row("pid", run.pid.to_string());
        section.unwrapped("address", run.url.clone());
        let version = field("version")
            .and_then(Value::as_str)
            .unwrap_or(&run.version);
        section.row("version", version.to_string());
    }
    let started = field("startedAtMs")
        .and_then(Value::as_u64)
        .or_else(|| status.run.as_ref().and_then(|run| run.started_at_ms));
    let uptime = field("uptimeSecs").and_then(Value::as_u64).or_else(|| {
        let now = crate::control::unix_ms(std::time::SystemTime::now());
        started.map(|ms| now.saturating_sub(ms) / 1000)
    });
    if let Some(uptime) = uptime {
        let since = started
            .map(|ms| format!(" (since {})", crate::database::utc_text(ms / 1000)))
            .unwrap_or_default();
        section.row("uptime", format!("{}{since}", uptime_text(uptime)));
    }
    if let Some(databases) = field("databases") {
        let total = databases
            .get("total")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let loaded = names(&databases["loaded"]);
        let loading = names(&databases["loading"]);
        let mut text = format!("{total} ({} loaded", loaded.len());
        if !loaded.is_empty() {
            text.push_str(&format!(": {}", loaded.join(", ")));
        }
        text.push(')');
        if !loading.is_empty() {
            text.push_str(&format!("; loading: {}", loading.join(", ")));
        }
        section.row("databases", text);
    }
    if let Some(memory) = field("memory") {
        for (name, text) in memory_rows(memory) {
            section.row(name, text);
        }
    }
    if let Some(why) = &status.unanswered {
        section.row(
            "no answer",
            format!("{why} (the server holds the directory but did not answer its admin API)"),
        );
    }
    if let Some(log) = &status.log {
        section.unwrapped("log", log.display().to_string());
    }
    section.unwrapped("stop", format!("{BIN} stop --dir {arg}"));
    text::render(&[section])
}

/// The memory rows of `status` from the `memory` object of `GET /api/admin/server`
/// (`crate::usage`): `memory` (the process's resident memory and the loaded databases'),
/// `machine` (total and available), `memory limit` (the engine's ceiling, where it comes from,
/// what `server.toml` sets) and `free` (under the limit, else on the machine). A figure the
/// server does not know is said so, never shown as 0.
pub(crate) fn memory_rows(memory: &Value) -> Vec<(&'static str, String)> {
    let num = |key: &str| memory.get(key).and_then(Value::as_u64);
    let known = |n: Option<u64>| n.map_or_else(|| "unknown".to_string(), text::bytes);
    let mut rows = Vec::new();
    let mut used = format!("process {} resident", known(num("processBytes")));
    if let Some(live) = num("liveBytes") {
        used.push_str(&format!(" ({} live heap)", text::bytes(live)));
    }
    used.push_str(&format!(
        ", loaded databases {}",
        text::bytes(num("databasesBytes").unwrap_or(0))
    ));
    rows.push(("memory", used));
    rows.push((
        "machine",
        format!(
            "{} total, {} available",
            known(num("machineTotalBytes")),
            known(num("machineAvailableBytes"))
        ),
    ));
    let source = memory.get("limitSource").and_then(Value::as_str);
    let configured = match memory.get("configuredLimit") {
        Some(Value::Number(n)) => format!("memory_limit = {n}"),
        Some(Value::String(word)) => format!("memory_limit = \"{word}\""),
        _ => "memory_limit not set: \"auto\"".to_string(),
    };
    let headroom = num("headroomBytes")
        .map(|h| format!(", memory_headroom = {h}"))
        .unwrap_or_default();
    let limit = match (num("limitBytes"), source) {
        (Some(limit), Some("cgroup")) => format!(
            "{} (the container's cgroup limit minus the headroom)",
            text::bytes(limit)
        ),
        (Some(limit), Some("physical")) => {
            format!("{} (80 % of the machine's memory)", text::bytes(limit))
        }
        (Some(limit), _) => text::bytes(limit),
        (None, _) => "none (the process may grow until the system stops it)".to_string(),
    };
    rows.push((
        "memory limit",
        format!("{limit}; server.toml [limits] {configured}{headroom}"),
    ));
    let free = num("freeBytes");
    let basis = memory.get("freeBasis").and_then(Value::as_str);
    let free = match (free, basis) {
        (Some(free), Some("limit")) => format!("{} under the limit", text::bytes(free)),
        (Some(free), _) => format!("{} available on the machine (no limit)", text::bytes(free)),
        (None, _) => "unknown".to_string(),
    };
    rows.push(("free", free));
    rows
}

/// What `status --json` prints: `state` (`running` | `stopped`), the run file's `pid`,
/// `address`, `url`, `version`, `startedAtMs` (no token), `staleRunFile`, the server's answer
/// as `server`, `unanswered`, `log`.
pub fn status_json(status: &Status) -> Value {
    let run = status.run.as_ref().map(|run| {
        json!({
            "pid": run.pid,
            "address": run.address.to_string(),
            "url": run.url,
            "version": run.version,
            "startedAtMs": run.started_at_ms,
        })
    });
    json!({
        "state": if status.running { "running" } else { "stopped" },
        "run": run,
        "staleRunFile": status.stale,
        "server": status.server,
        "unanswered": status.unanswered,
        "log": status.log.as_ref().map(|p| p.display().to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_records_are_told_from_the_start_block() {
        assert!(is_log_record(
            "2026-11-25T22:12:55.924550Z  INFO graphersal_server: registry: 0 databases"
        ));
        assert!(!is_log_record("── Server ──────"));
        assert!(!is_log_record("Graphersal server: http://127.0.0.1:1/"));
        assert!(!is_log_record("2026"));
    }

    #[test]
    fn memory_rows_name_every_figure() {
        let memory = json!({
            "processBytes": 50 * 1024 * 1024, "liveBytes": null, "databasesBytes": 2048,
            "machineTotalBytes": 16u64 << 30, "machineAvailableBytes": 8u64 << 30,
            "limitBytes": 12u64 << 30, "limitSource": "physical", "configuredLimit": null,
            "headroomBytes": null, "availableBytes": 12u64 << 30,
            "freeBytes": (12u64 << 30) - 50 * 1024 * 1024, "freeBasis": "limit",
        });
        let rows = memory_rows(&memory);
        let names: Vec<&str> = rows.iter().map(|(name, _)| *name).collect();
        assert_eq!(names, ["memory", "machine", "memory limit", "free"]);
        assert!(rows[0].1.starts_with("process 50"), "{}", rows[0].1);
        assert!(rows[0].1.contains("loaded databases 2"), "{}", rows[0].1);
        assert!(rows[1].1.contains("total") && rows[1].1.contains("available"));
        assert!(rows[2].1.contains("80 % of the machine"), "{}", rows[2].1);
        assert!(rows[2].1.contains("memory_limit not set"), "{}", rows[2].1);
        assert!(rows[3].1.ends_with("under the limit"), "{}", rows[3].1);

        let configured = json!({
            "processBytes": null, "databasesBytes": 0, "limitBytes": null,
            "limitSource": "none", "configuredLimit": "none", "headroomBytes": 1024,
            "freeBytes": 4096, "freeBasis": "machine",
        });
        let rows = memory_rows(&configured);
        assert!(rows[0].1.starts_with("process unknown"), "{}", rows[0].1);
        assert_eq!(rows[1].1, "unknown total, unknown available");
        assert!(rows[2].1.starts_with("none"), "{}", rows[2].1);
        assert!(
            rows[2]
                .1
                .contains("memory_limit = \"none\", memory_headroom = 1024")
        );
        assert!(
            rows[3].1.contains("available on the machine"),
            "{}",
            rows[3].1
        );
    }

    #[test]
    fn uptimes_read_like_people_say_them() {
        assert_eq!(uptime_text(0), "0 s");
        assert_eq!(uptime_text(59), "59 s");
        assert_eq!(uptime_text(250), "4 min 10 s");
        assert_eq!(uptime_text(2 * 3600 + 5 * 60 + 9), "2 h 5 min");
        assert_eq!(uptime_text(3 * 86_400 + 4 * 3600 + 1), "3 d 4 h");
    }
}
