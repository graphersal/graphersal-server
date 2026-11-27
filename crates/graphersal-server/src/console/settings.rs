//! The console's `config` commands (round 4 B-SETTINGS, [`crate::server_settings`]): the text
//! of `config get [KEY]`, `config set KEY VALUE` / `config unset KEY` and `config check
//! [FILE]`.

use std::path::Path;

use serde_json::Value;

use super::render::BIN;
use crate::error::ServerError;
use crate::server_settings::{self, Applies};
use crate::text::{Note, Section, table};

/// A key's value for people: `null` (not set) shows its default with `(default)`.
fn value_text(row: &Value) -> String {
    let shown = |value: &Value| match value {
        Value::Null => "not set".to_string(),
        Value::String(text) => text.clone(),
        Value::Array(items) if items.is_empty() => "[]".to_string(),
        Value::Array(items) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map_or_else(|| item.to_string(), str::to_string)
            })
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    };
    if row["set"] == Value::Bool(true) {
        shown(&row["value"])
    } else {
        format!("{} (default)", shown(&row["default"]))
    }
}

/// The text of `config get` (`key`: one key, else every key).
pub(crate) fn render_get(key: Option<&str>, answer: &Value, dir_arg: &str) -> String {
    let rows: Vec<&Value> = answer["keys"]
        .as_array()
        .map(|rows| rows.iter().collect())
        .unwrap_or_default();
    let pending: Vec<&str> = answer["restartPending"]
        .as_array()
        .map(|keys| keys.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if let Some(key) = key {
        let Some(row) = rows.iter().find(|row| row["key"] == key) else {
            return format!("No setting '{key}'");
        };
        let mut note = Note::new(format!("{key} = {}", value_text(row)))
            .detail("applies", row["appliesText"].as_str().unwrap_or_default())
            .detail("meaning", row["help"].as_str().unwrap_or_default());
        if row["pending"] == Value::Bool(true) {
            note = note.detail(
                "running",
                format!(
                    "{} (restart pending: restart the server to apply it)",
                    shown_json(&row["running"])
                ),
            );
        }
        return note
            .command(
                "change it",
                format!("{BIN} config set {key} VALUE --dir {dir_arg}"),
            )
            .text();
    }
    let mut section = Section::new("server.toml");
    section.unwrapped("file", answer["path"].as_str().unwrap_or_default());
    section.row(
        "restart pending",
        if pending.is_empty() {
            "none".to_string()
        } else {
            pending.join(", ")
        },
    );
    let lines = table(
        &["KEY", "VALUE", "APPLIES"],
        &rows
            .iter()
            .map(|row| {
                let key = row["key"].as_str().unwrap_or_default();
                let applies = if row["pending"] == Value::Bool(true) {
                    "restart pending".to_string()
                } else {
                    row["applies"].as_str().unwrap_or_default().to_string()
                };
                vec![key.to_string(), value_text(row), applies]
            })
            .collect::<Vec<_>>(),
    );
    let mut keys = Section::new("Keys");
    keys.lines(lines);
    keys.note(
        "applies",
        "now = at once (through the admin API or this console with a running server); load = \
         at each database's next load; create = for new databases; restart = at the next start",
    );
    crate::text::render(&[section, keys])
}

fn shown_json(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "not set".to_string(),
        other => other.to_string(),
    }
}

/// The text of `config set` / `config unset`.
pub(crate) fn render_set(answer: &Value, dir_arg: &str) -> String {
    let words = |key: &str| -> Vec<String> {
        answer[key]
            .as_array()
            .map(|keys| {
                keys.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let changed = words("changed");
    let applied = words("applied");
    let pending = words("restartPending");
    if changed.is_empty() {
        return Note::new("server.toml unchanged (the values were already set)").text();
    }
    let rows: Vec<&Value> = answer["keys"]
        .as_array()
        .map(|rows| rows.iter().collect())
        .unwrap_or_default();
    let mut note = Note::new(format!("Changed server.toml: {}", changed.join(", ")));
    for key in &changed {
        if let Some(row) = rows.iter().find(|row| row["key"] == key.as_str()) {
            let class = server_settings::key_of(key).map(|k| k.applies);
            let when = match class {
                Some(Applies::Now) if applied.contains(key) => "applied now",
                Some(Applies::Now) if answer["running"] == Value::Bool(true) => "applies now",
                Some(Applies::Now) | Some(Applies::Restart)
                    if answer["running"] == Value::Bool(true) =>
                {
                    "restart pending"
                }
                Some(Applies::Now) | Some(Applies::Restart) => "at the next start",
                Some(Applies::NewDatabases) => "for new databases",
                _ => "",
            };
            note = note.detail(key, format!("{} ({when})", value_text(row)));
        }
    }
    if answer["rewritten"] == Value::Bool(true) {
        note = note.detail(
            "note",
            "the file was rewritten from its keys: its own comments could not be kept",
        );
    }
    if !pending.is_empty() {
        note = note.detail("restart pending", pending.join(", ")).command(
            "apply them",
            format!("{BIN} stop --dir {dir_arg}\n{BIN} start --dir {dir_arg}"),
        );
    }
    note.text()
}

/// The text of `config check FILE` for a valid file.
pub fn check_text(path: &Path) -> String {
    Note::new(format!("{} is a valid server.toml", path.display()))
        .detail(
            "use it",
            format!("{BIN} init DIR --config {}", path.display()),
        )
        .text()
}

/// Checks the `server.toml` at `path` with the server's own parser.
///
/// `Err`: [`ServerError::Io`] (unreadable), [`ServerError::Config`] (the parser's refusal,
/// naming the key and line).
pub fn check_file(path: &Path) -> Result<crate::config::ServerConfig, ServerError> {
    server_settings::check_text(&server_settings::read_text(path)?, path)
}
