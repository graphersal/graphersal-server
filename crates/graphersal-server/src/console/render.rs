//! The human text of the console: the answers of the admin commands ([`render`]), `init`, the
//! `serve` start block and stop lines, errors and usage errors, all in the layout of
//! [`crate::text`] (the graphersal CLI's).

use std::path::Path;

use serde_json::Value;

use super::{DbCommand, Outcome};
use crate::config::DEFAULT_LISTEN;
use crate::database::DatabaseKind;
use crate::error::ServerError;
use crate::layout::{IMPORTS_DIR, InitReport};
use crate::text::{self, Note, Section, table};

/// The binary's name in the commands the text shows.
pub(crate) const BIN: &str = "graphersal-server";

/// `path` as one shell word: as it is when it needs no quoting, else in single quotes.
pub(crate) fn shell_word(path: &Path) -> String {
    let text = path.display().to_string();
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:\\@%+=,".contains(c));
    if plain {
        text
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

/// `rel` (a path the admin API gives relative to the server directory, `/`-separated) below
/// `dir`, joined one component at a time (Windows).
fn below(dir: &Path, rel: &str) -> String {
    let mut path = dir.to_path_buf();
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        path = path.join(part);
    }
    path.display().to_string()
}

/// The URL of database `name` on the server at `server` (`http://HOST:PORT/`): its mount
/// `/db/<name>/` (the entry's real name; `fallback`, the name asked for, when it has none).
fn database_url(server: &str, name: &str, fallback: &str) -> String {
    let name = if name.is_empty() { fallback } else { name };
    format!("{}/db/{name}/", server.trim_end_matches('/'))
}

/// Whether the answer of `command` reports a failure although the request succeeded (a load
/// that ended `failed` or `needs-model`).
pub fn failed(command: &DbCommand, answer: &Value) -> bool {
    matches!(command, DbCommand::Load { .. })
        && matches!(answer["state"].as_str(), Some("failed" | "needs-model"))
}

/// The human text of `outcome` of `command` on the server directory `dir`: [`render`], and a
/// database loaded through a running server shows its URL (`<server>/db/<name>/`).
pub fn render_outcome(command: &DbCommand, outcome: &Outcome, dir: &Path) -> String {
    render_with(command, &outcome.body, dir, outcome.server_url.as_deref())
}

/// The human text of the answer of `command` on the server directory `dir` (the commands it
/// shows name `--dir` with it).
pub fn render(command: &DbCommand, answer: &Value, dir: &Path) -> String {
    render_with(command, answer, dir, None)
}

/// [`render`] with the URL of the server the command went through, if any.
fn render_with(command: &DbCommand, answer: &Value, dir: &Path, server: Option<&str>) -> String {
    let s = |key: &str| answer[key].as_str().unwrap_or_default().to_string();
    let on_off = |on: bool| if on { "on" } else { "off" };
    let dir_arg = shell_word(dir);
    match command {
        DbCommand::List => render_list(answer, &dir_arg),
        DbCommand::Create { .. } => entry_note(
            Note::new(format!("Created database '{}'", s("name"))),
            answer,
        )
        .text(),
        DbCommand::Delete { .. } => {
            let trash = below(dir, &s("trash"));
            Note::new(format!("Deleted database '{}'", s("name")))
                .detail("moved to", &trash)
                .detail(
                    "recover",
                    "move it back into databases/ by hand while no server runs",
                )
                .text()
        }
        DbCommand::Clone { source, .. } => entry_note(
            Note::new(format!("Cloned database '{source}' into '{}'", s("name"))),
            answer,
        )
        .text(),
        DbCommand::Load { name, .. } => match answer["state"].as_str() {
            Some("loaded") => {
                let headline = if s("load") == "already-loaded" {
                    format!("Database '{name}' is loaded already")
                } else {
                    format!("Database '{name}' is loaded")
                };
                let mut note = Note::new(headline);
                if let Some(server) = server {
                    note = note.detail("served at", database_url(server, &s("name"), name));
                }
                note.text()
            }
            Some("loading") => Note::new(format!("Database '{name}' is loading"))
                .command("see its state", format!("{BIN} db list --dir {dir_arg}"))
                .text(),
            _ => state_details(
                Note::new(format!("Database '{name}' is not loaded")),
                answer,
            )
            .text(),
        },
        DbCommand::Unload { name } => {
            if answer["unloaded"] == Value::Bool(true) {
                Note::new(format!("Unloaded database '{name}'")).text()
            } else {
                Note::new(format!("Database '{name}' was not loaded")).text()
            }
        }
        DbCommand::Promote { .. } => {
            let name = s("name");
            Note::new(format!("Database '{name}' is a base now"))
                .detail(
                    "read-only",
                    "for good: a base is frozen, there is no way back",
                )
                .command(
                    "make a layer over it",
                    format!("{BIN} db create NAME --base {name} --dir {dir_arg}"),
                )
                .text()
        }
        DbCommand::ReadOnly { name, .. } => Note::new(format!(
            "Read-only is {} for database '{name}'",
            on_off(answer["readOnly"] == Value::Bool(true))
        ))
        .text(),
        DbCommand::AutoLoad { name, .. } => Note::new(format!(
            "Auto-load is {} for database '{name}'",
            on_off(answer["autoLoad"] == Value::Bool(true))
        ))
        .text(),
        DbCommand::AliasSet { .. } => Note::new(format!(
            "Alias '{}' -> database '{}'",
            s("alias"),
            s("target")
        ))
        .text(),
        DbCommand::AliasRemove { .. } => {
            Note::new(format!("Removed alias '{}'", s("alias"))).text()
        }
        DbCommand::AliasRename { .. } => Note::new(format!(
            "Renamed alias '{}' to '{}' -> database '{}'",
            s("renamedFrom"),
            s("alias"),
            s("target")
        ))
        .detail(
            "MCP sessions moved",
            answer["mcpSessionsMoved"].as_u64().unwrap_or(0).to_string(),
        )
        .text(),
        DbCommand::Backup { .. } => {
            let name = s("database");
            Note::new(format!("Backed up database '{name}'"))
                .detail("file", below(dir, &s("file")))
                .detail("size", size(answer))
                .command(
                    "restore it into a new database",
                    format!(
                        "{BIN} db restore {name} {} --name NEW --dir {dir_arg}",
                        s("backup")
                    ),
                )
                .text()
        }
        DbCommand::Backups { name } => render_backups(name, answer, dir, &dir_arg),
        DbCommand::Restore { .. } => {
            let from = &answer["restoredFrom"];
            let mut note = entry_note(
                Note::new(format!("Restored database '{}'", s("name"))),
                answer,
            )
            .detail(
                "from",
                format!(
                    "backup {} of '{}'",
                    from["backup"].as_str().unwrap_or_default(),
                    from["database"].as_str().unwrap_or_default()
                ),
            );
            if let Some(trash) = answer["trash"].as_str() {
                note = note.detail("replaced", format!("moved to {}", below(dir, trash)));
            }
            note.text()
        }
        DbCommand::Import { source, .. } => render_import(source, answer, &dir_arg),
        DbCommand::ImportInbox { file, .. } => {
            render_import(&dir.join(IMPORTS_DIR).join(file), answer, &dir_arg)
        }
        DbCommand::InboxList => render_inbox(answer, &dir_arg),
        DbCommand::ModelsList => render_models(answer, &dir_arg),
        DbCommand::ModelsAdd { .. } => render_models_add(answer, &dir_arg),
        DbCommand::ModelsRemove { .. } => Note::new(format!(
            "Removed model '{}' from the shared models",
            s("model")
        ))
        .detail(
            "blobs",
            answer["removed"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default()
                .to_string(),
        )
        .detail("size", size(answer))
        .text(),
        DbCommand::UserList => render_users(answer, &dir_arg),
        DbCommand::UserAdd { .. } => {
            let name = s("name");
            let what = if answer["admin"] == Value::Bool(true) {
                "admin"
            } else {
                "user"
            };
            let note = token_note(Note::new(format!("Added {what} '{name}'")), answer);
            if answer["admin"] == Value::Bool(true) {
                note.text()
            } else {
                note.command(
                    "let it reach a database",
                    format!("{BIN} user grant {name} DATABASE read --dir {dir_arg}"),
                )
                .text()
            }
        }
        DbCommand::UserToken { revoke: false, .. } => {
            let name = s("name");
            token_note(Note::new(format!("New token for user '{name}'")), answer)
                .detail("old token", "stops working now")
                .detail("ended", sessions_ended(answer))
                .text()
        }
        DbCommand::UserToken { revoke: true, .. } => {
            let name = s("name");
            Note::new(format!("Revoked the token of user '{name}'"))
                .detail("ended", sessions_ended(answer))
                .command(
                    "give it a new one",
                    format!("{BIN} user token {name} --dir {dir_arg}"),
                )
                .text()
        }
        DbCommand::UserRemove { .. } => Note::new(format!("Removed user '{}'", s("name")))
            .detail(
                "grants removed",
                answer["grantsRemoved"].as_u64().unwrap_or(0).to_string(),
            )
            .detail("ended", sessions_ended(answer))
            .text(),
        DbCommand::UserAdmin { .. } => {
            let name = s("name");
            if answer["admin"] == Value::Bool(true) {
                Note::new(format!("User '{name}' is an admin"))
                    .detail("reaches", "every database and the admin API")
                    .text()
            } else {
                Note::new(format!("User '{name}' is not an admin"))
                    .detail("reaches", grants_text(answer))
                    .text()
            }
        }
        DbCommand::UserGrant { .. } => Note::new(format!(
            "Granted {} on {} to {}",
            s("level"),
            database_word(&s("database")),
            s("subject")
        ))
        .text(),
        DbCommand::UserRevoke { .. } => {
            let database = database_word(&s("database"));
            if answer["revoked"] == Value::Bool(true) {
                Note::new(format!(
                    "Revoked the grant of {} on {database}",
                    s("subject")
                ))
                .text()
            } else {
                Note::new(format!(
                    "{} had no grant on {database}; nothing changed",
                    s("subject")
                ))
                .command("see the grants", format!("{BIN} user list --dir {dir_arg}"))
                .text()
            }
        }
        DbCommand::Export { .. } => {
            let snapshot = match s("snapshot").as_str() {
                "layer" => "layer (loads only over its base)".to_string(),
                other => other.to_string(),
            };
            Note::new(format!("Exported database '{}'", s("database")))
                .detail("file", s("file"))
                .detail("size", size(answer))
                .detail("snapshot", snapshot)
                .detail("models", s("models"))
                .text()
        }
    }
}

/// `note` with the answer's new `token`, shown once.
fn token_note(note: Note, answer: &Value) -> Note {
    note.detail("token", answer["token"].as_str().unwrap_or_default())
        .detail(
            "shown once",
            "copy it now: only its SHA-256 hash is kept (listings show its last 4 characters)",
        )
        .detail(
            "use it",
            "\"Authorization: Bearer <token>\", or paste it on the login page",
        )
}

/// What a token change ended: the browser sessions (always) and the MCP sessions.
fn sessions_ended(answer: &Value) -> String {
    let mcp = answer["mcpSessionsEnded"].as_u64().unwrap_or(0) as usize;
    format!(
        "its browser sessions and {}",
        text::count(mcp, "MCP session")
    )
}

/// `'acme'`, or `every database` for `*`.
fn database_word(database: &str) -> String {
    if database == "*" {
        "every database".to_string()
    } else {
        format!("'{database}'")
    }
}

/// A user's grants of `row` (`acme read, * write`), `nothing` without one.
fn grants_text(row: &Value) -> String {
    let grants: Vec<String> = row["grants"]
        .as_array()
        .map(|grants| {
            grants
                .iter()
                .map(|g| {
                    format!(
                        "{} {}",
                        g["database"].as_str().unwrap_or_default(),
                        g["level"].as_str().unwrap_or_default()
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    if grants.is_empty() {
        "nothing (no grant)".to_string()
    } else {
        grants.join(", ")
    }
}

/// `user list`: the users (name, admin, token's last 4, grants), then the grants of other
/// subjects (anonymous, roles, groups).
fn render_users(answer: &Value, dir_arg: &str) -> String {
    let rows = answer["users"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        return Note::new("No users")
            .command(
                "add one",
                format!("{BIN} user add NAME [--admin] --dir {dir_arg}"),
            )
            .text();
    }
    let mut users = Section::new(format!("Users ({})", rows.len()));
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            let admin = row["admin"] == Value::Bool(true);
            let token = match row["tokenLast4"].as_str() {
                Some(last4) if row["hasToken"] == Value::Bool(true) => format!("…{last4}"),
                _ if row["hasToken"] == Value::Bool(true) => "set".to_string(),
                _ => "none (revoked)".to_string(),
            };
            let roles: Vec<&str> = row["roles"]
                .as_array()
                .map(|r| r.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            vec![
                row["name"].as_str().unwrap_or_default().to_string(),
                if roles.is_empty() {
                    "-".to_string()
                } else {
                    roles.join(", ")
                },
                token,
                if admin {
                    "every database".to_string()
                } else {
                    grants_text(row)
                },
            ]
        })
        .collect();
    users.lines(table(&["USER", "ROLES", "TOKEN", "REACHES"], &cells));
    let others: Vec<Vec<String>> = answer["grants"]
        .as_array()
        .map(|grants| {
            grants
                .iter()
                .filter(|g| {
                    !g["subject"]
                        .as_str()
                        .unwrap_or_default()
                        .starts_with("user:")
                })
                .map(|g| {
                    ["subject", "database", "level"]
                        .iter()
                        .map(|key| g[*key].as_str().unwrap_or_default().to_string())
                        .collect()
                })
                .collect()
        })
        .unwrap_or_default();
    let mut grants = Section::new("Other grants");
    if !others.is_empty() {
        grants.lines(table(&["SUBJECT", "DATABASE", "LEVEL"], &others));
    }
    text::render(&[users, grants])
}

/// `1.2 KiB (1234 bytes)` of the answer's `bytes`.
fn size(answer: &Value) -> String {
    let bytes = answer["bytes"].as_u64().unwrap_or_default();
    if bytes < 1024 {
        text::bytes(bytes)
    } else {
        format!("{} ({bytes} bytes)", text::bytes(bytes))
    }
}

/// `note` with the kind, flags and state of the database entry `entry`.
fn entry_note(note: Note, entry: &Value) -> Note {
    let note = note.detail("kind", kind(entry));
    let flags = flags(entry);
    let note = if flags.is_empty() {
        note
    } else {
        note.detail("flags", flags)
    };
    let note = match entry["description"].as_str() {
        Some(description) if !description.is_empty() => note.detail("description", description),
        _ => note,
    };
    state_details(note, entry)
}

/// `note` with the state of `entry` and, for a failed or needs-model one, its reason.
fn state_details(note: Note, entry: &Value) -> Note {
    let note = note.detail("state", entry["state"].as_str().unwrap_or("?"));
    match entry["reason"].as_str() {
        Some(reason) => note.detail("reason", reason_text(reason)),
        None => note,
    }
}

/// A failure reason for the console: the engine's `Error: ` prefix dropped (it stays a
/// reason), its `Help:` lines kept.
fn reason_text(reason: &str) -> String {
    reason
        .trim()
        .strip_prefix("Error: ")
        .unwrap_or(reason.trim())
        .to_string()
}

/// `plain`, `layer over 'b'`, `base`.
fn kind(entry: &Value) -> String {
    if entry["role"] == "base" {
        "base".to_string()
    } else if entry["kind"] == DatabaseKind::Layer.as_str() {
        format!(
            "layer over '{}'",
            entry["base"].as_str().unwrap_or_default()
        )
    } else if entry.get("kind").is_some() {
        "plain".to_string()
    } else {
        String::new()
    }
}

/// `read-only, auto-load` (empty for none).
fn flags(entry: &Value) -> String {
    let mut flags = Vec::new();
    if entry["readOnly"] == Value::Bool(true) {
        flags.push("read-only");
    }
    if entry["autoLoad"] == Value::Bool(true) {
        flags.push("auto-load");
    }
    flags.join(", ")
}

/// The table of the database rows `rows` (no alias rows): with the size and description
/// columns for `db list`, without them for the start block.
pub(crate) fn database_table(rows: &[&Value], full: bool) -> Vec<String> {
    let mut header = vec!["NAME", "STATE", "KIND", "FLAGS"];
    if full {
        header.extend(["SIZE", "DESCRIPTION"]);
    }
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            let mut cells = vec![
                row["name"].as_str().unwrap_or_default().to_string(),
                row["state"].as_str().unwrap_or("?").to_string(),
                kind(row),
                flags(row).replace(", ", ","),
            ];
            if full {
                cells.push(
                    row["sizeBytes"]
                        .as_u64()
                        .map(text::bytes)
                        .unwrap_or_default(),
                );
                cells.push(row["description"].as_str().unwrap_or_default().to_string());
            }
            cells
        })
        .collect();
    table(&header, &cells)
}

/// The problems of a listing as a section: why a database failed or waits for a model, its
/// model copies, the registry's problems.
pub(crate) fn problems_section(rows: &[&Value], problems: &[String], dir_arg: &str) -> Section {
    // (key, text, whether it is a note that may wrap); entries set apart by a blank line.
    let mut entries: Vec<(String, String, bool)> = Vec::new();
    for row in rows {
        let name = row["name"].as_str().unwrap_or_default().to_string();
        match (row["state"].as_str(), row["reason"].as_str()) {
            (Some("needs-model"), Some(reason)) => entries.push((
                name.clone(),
                Note::new(format!("needs-model: {reason}"))
                    .command(
                        "add the model to the shared models",
                        format!("{BIN} models add <model directory or .gsnap> --dir {dir_arg}"),
                    )
                    .text(),
                true,
            )),
            // Never wrapped: the reason names paths and commands.
            (Some(state), Some(reason)) => entries.push((
                name.clone(),
                format!("{state}: {}", reason_text(reason)),
                false,
            )),
            _ => {}
        }
        if let Some(warning) = row["linkWarning"].as_str() {
            entries.push((name, warning.to_string(), true));
        }
    }
    for problem in problems {
        entries.push(("registry".to_string(), problem.clone(), true));
    }
    let mut section = Section::new("Problems");
    for (i, (key, text, note)) in entries.into_iter().enumerate() {
        if i > 0 {
            section.lines([String::new()]);
        }
        if note {
            section.note(&key, &text);
        } else {
            section.unwrapped(key, text);
        }
    }
    section
}

/// `db list`: the databases, the aliases and the problems, each a section.
fn render_list(answer: &Value, dir_arg: &str) -> String {
    let rows: Vec<&Value> = answer["databases"]
        .as_array()
        .map(|rows| rows.iter().filter(|r| r.get("aliasOf").is_none()).collect())
        .unwrap_or_default();
    let problems: Vec<String> = answer["problems"]
        .as_array()
        .map(|p| {
            p.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if rows.is_empty() && problems.is_empty() {
        return Note::new("No databases")
            .command(
                "create one",
                format!("{BIN} db create NAME --dir {dir_arg}"),
            )
            .text();
    }
    let mut databases = Section::new(format!("Databases ({})", rows.len()));
    if !rows.is_empty() {
        databases.lines(database_table(&rows, true));
    }
    let mut aliases = Section::new("Aliases");
    if let Some(map) = answer["aliases"].as_object()
        && !map.is_empty()
    {
        let cells: Vec<Vec<String>> = map
            .iter()
            .map(|(alias, target)| {
                vec![
                    alias.clone(),
                    target.as_str().unwrap_or_default().to_string(),
                ]
            })
            .collect();
        aliases.lines(table(&["ALIAS", "DATABASE"], &cells));
    }
    text::render(&[
        databases,
        aliases,
        problems_section(&rows, &problems, dir_arg),
    ])
}

/// `db backups NAME`.
fn render_backups(name: &str, answer: &Value, dir: &Path, dir_arg: &str) -> String {
    let rows = answer["backups"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        return Note::new(format!("Database '{name}' has no backups"))
            .command(
                "make one",
                format!("{BIN} db backup {name} --dir {dir_arg}"),
            )
            .text();
    }
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            let kind = match row["base"].as_str() {
                Some(base) => format!("layer over '{base}'"),
                None if row["role"] == "base" => "base".to_string(),
                None => row["kind"].as_str().unwrap_or("plain").to_string(),
            };
            vec![
                row["backup"].as_str().unwrap_or_default().to_string(),
                text::bytes(row["bytes"].as_u64().unwrap_or_default()),
                kind,
                below(dir, row["file"].as_str().unwrap_or_default()),
            ]
        })
        .collect();
    let mut section = Section::new(format!("Backups of '{name}' ({})", rows.len()));
    section.lines(table(&["BACKUP", "SIZE", "KIND", "FILE"], &cells));
    section.lines([String::new()]);
    section.note(
        "restore",
        &Note::new("into a new database, or over the unloaded database itself")
            .command(
                "restore one",
                format!("{BIN} db restore {name} BACKUP [--name NEW] --dir {dir_arg}"),
            )
            .text(),
    );
    text::render(&[section])
}

/// `import DIR --list`: the import inbox.
fn render_inbox(answer: &Value, dir_arg: &str) -> String {
    let inbox = answer["dir"].as_str().unwrap_or_default();
    let rows = answer["imports"].as_array().cloned().unwrap_or_default();
    let import = format!("{BIN} import {dir_arg} --inbox FILE --name NAME");
    if rows.is_empty() {
        return Note::new(format!("The import inbox {inbox} is empty"))
            .command(
                "put a file or Store directory there, then import it",
                import,
            )
            .text();
    }
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            let mut name = row["name"].as_str().unwrap_or_default().to_string();
            if row["directory"] == true {
                name.push('/');
            }
            vec![
                name,
                text::bytes(row["bytes"].as_u64().unwrap_or_default()),
                row["kind"].as_str().unwrap_or("?").to_string(),
                row["modified"].as_str().unwrap_or_default().to_string(),
            ]
        })
        .collect();
    let mut section = Section::new(format!("Import inbox ({})", rows.len()));
    section.unwrapped("directory", inbox);
    section.lines([String::new()]);
    section.lines(table(&["NAME", "SIZE", "KIND", "MODIFIED"], &cells));
    section.lines([String::new()]);
    section.note(
        "import",
        &Note::new("a new database from one of them (KIND is guessed from the name)")
            .command("import one", import)
            .text(),
    );
    text::render(&[section])
}

/// `import`: the new database and what the import did.
fn render_import(source: &Path, answer: &Value, dir_arg: &str) -> String {
    let import = &answer["import"];
    let name = answer["name"].as_str().unwrap_or_default();
    let list = |key: &str| -> Vec<String> {
        import[key]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut note = Note::new(format!("Imported database '{name}'"))
        .detail("source", shell_word(source))
        .detail("format", import["format"].as_str().unwrap_or_default());
    if let Some(layer_base) = import["layerBase"].as_str() {
        note = note.detail("layer base", layer_base);
    }
    let mut note = entry_note(note, answer);
    let skipped = import["skipped"].as_u64().unwrap_or_default();
    if skipped > 0 {
        note = note.detail("skipped", text::count(skipped as usize, "element"));
    }
    let resolved = list("modelsResolved");
    if !resolved.is_empty() {
        note = note.detail(
            "models",
            format!("{} (from the shared models)", resolved.join(", ")),
        );
    }
    let missing = list("modelsMissing");
    if !missing.is_empty() {
        note = note.detail("missing", missing.join(", "));
    }
    let notes = list("notes");
    if !notes.is_empty() {
        note = note.detail("notes", notes.join("\n"));
    }
    if !missing.is_empty() {
        note = note.command(
            "add the missing model to the shared models, then load it",
            format!("{BIN} models add <model directory or .gsnap> --dir {dir_arg}"),
        );
    }
    note.text()
}

/// `models list`: the shared models as a table, then what is missing or copied.
fn render_models(answer: &Value, dir_arg: &str) -> String {
    let rows = answer["models"].as_array().cloned().unwrap_or_default();
    let missing = answer["missing"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() && missing.is_empty() {
        return Note::new("No shared models")
            .command(
                "add one (a Model2Vec directory or a .gsnap carrying models)",
                format!("{BIN} models add SOURCE --dir {dir_arg}"),
            )
            .text();
    }
    let mut models = Section::new(format!("Shared models ({})", rows.len()));
    if !rows.is_empty() {
        let cells: Vec<Vec<String>> = rows
            .iter()
            .map(|row| {
                let users: Vec<&str> = row["users"]
                    .as_array()
                    .map(|u| u.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                vec![
                    row["repo"]
                        .as_str()
                        .unwrap_or("(not a model archive)")
                        .to_string(),
                    text::bytes(row["bytes"].as_u64().unwrap_or_default()),
                    row["license"].as_str().unwrap_or("-").to_string(),
                    row["blob"]
                        .as_str()
                        .unwrap_or_default()
                        .chars()
                        .take(12)
                        .collect(),
                    if users.is_empty() {
                        "nobody".to_string()
                    } else {
                        users.join(", ")
                    },
                ]
            })
            .collect();
        models.lines(table(
            &["MODEL", "SIZE", "LICENSE", "BLOB", "USED BY"],
            &cells,
        ));
    }
    let mut problems = Section::new("Problems");
    for row in &rows {
        if let Some(warning) = row["linkWarning"].as_str() {
            problems.note(row["repo"].as_str().unwrap_or("model"), warning);
        }
    }
    for row in &missing {
        problems.note(
            row["database"].as_str().unwrap_or_default(),
            &Note::new(format!(
                "needs the model '{}'",
                row["model"].as_str().unwrap_or_default()
            ))
            .command(
                "add it",
                format!("{BIN} models add <model directory or .gsnap> --dir {dir_arg}"),
            )
            .text(),
        );
    }
    text::render(&[models, problems])
}

/// `models add`: the models added, then the databases they resolved.
fn render_models_add(answer: &Value, dir_arg: &str) -> String {
    let strings = |value: &Value| -> Vec<String> {
        value
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let added = answer["added"].as_array().cloned().unwrap_or_default();
    let resolved = strings(&answer["resolved"]);
    let missing = answer["stillMissing"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let notes = strings(&answer["notes"]);
    let headline = match added.as_slice() {
        [one] => format!(
            "Model {} is in the shared models",
            one["repo"].as_str().unwrap_or("?")
        ),
        many => format!(
            "{} are in the shared models",
            text::count(many.len(), "model")
        ),
    };
    let mut note = Note::new(headline);
    for model in &added {
        let detail = format!(
            "{}, licence {}, blob {}",
            text::bytes(model["bytes"].as_u64().unwrap_or_default()),
            model["license"].as_str().unwrap_or("unknown"),
            model["blob"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(12)
                .collect::<String>()
        );
        let key = if added.len() == 1 {
            "model".to_string()
        } else {
            model["repo"].as_str().unwrap_or("?").to_string()
        };
        note = note.detail(&key, detail);
    }
    if !resolved.is_empty() {
        note = note.detail(
            "resolved",
            format!("{} (unloaded; load it)", resolved.join(", ")),
        );
    }
    for row in &missing {
        note = note.detail(
            "still missing",
            format!(
                "database '{}' needs the model '{}'",
                row["database"].as_str().unwrap_or_default(),
                row["model"].as_str().unwrap_or_default()
            ),
        );
    }
    if !notes.is_empty() {
        note = note.detail("notes", notes.join("\n"));
    }
    if !resolved.is_empty() {
        note = note.command(
            "load a resolved database (on a running server)",
            format!("{BIN} db load {} --dir {dir_arg}", resolved[0]),
        );
    }
    note.text()
}

/// The result of `init DIR` (`dir` as the user named it).
pub fn init_text(report: &InitReport, dir: &Path) -> String {
    let dir_arg = shell_word(dir);
    let note = if !report.existed {
        Note::new(format!("Created the server directory {}", dir.display())).detail(
            "config",
            format!(
                "{} (listen {DEFAULT_LISTEN}, MCP on: edit it before serving)",
                below(dir, "server.toml")
            ),
        )
    } else if report.created.is_empty() && report.secured.is_empty() {
        Note::new(format!(
            "{} is already a server directory: nothing to do",
            dir.display()
        ))
    } else {
        let lines = |paths: &[std::path::PathBuf]| {
            paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<String>>()
                .join("\n")
        };
        let what = match (report.created.is_empty(), report.secured.is_empty()) {
            (false, true) => "created the missing parts",
            (true, false) => "made it private to its owner",
            _ => "created the missing parts and made it private to its owner",
        };
        let mut note = Note::new(format!(
            "{} is already a server directory: {what}",
            dir.display()
        ));
        if !report.created.is_empty() {
            note = note.detail("created", lines(&report.created));
        }
        if !report.secured.is_empty() {
            note = note.detail("owner only", lines(&report.secured));
        }
        note
    };
    note.command("start it", format!("{BIN} serve --dir {dir_arg}"))
        .command(
            "or create a database first",
            format!("{BIN} db create NAME --dir {dir_arg}"),
        )
        .text()
}

/// The text of a failed command: `Error: <message>`, then `Help: <help>` when the admin API
/// gave one.
pub fn error_text(err: &ServerError) -> String {
    match err {
        ServerError::Remote {
            help: Some(help), ..
        } => format!("Error: {err}\nHelp: {help}"),
        ServerError::InsecureDir { fixes, .. } => {
            let mut text = format!(
                "Error: {err}\nHelp: the OS user owning the server directory is its operator: \
                 whoever else can change it, or read its secrets, could make themselves an \
                 admin; keep it private (as ssh keeps ~/.ssh)"
            );
            for (what, command) in fixes {
                text.push_str(&format!("\n  # {what}\n  {command}"));
            }
            text
        }
        ServerError::Service { help, log_tail, .. } => {
            let mut text = format!("Error: {err}");
            if let Some(help) = help {
                text.push_str(&format!("\nHelp: {help}"));
            }
            if !log_tail.is_empty() {
                text.push_str("\nThe server's log (last lines):");
                for line in log_tail {
                    text.push_str(&format!("\n  | {line}"));
                }
            }
            text
        }
        _ => format!("Error: {err}"),
    }
}

/// The text of a failed command on the server directory `dir` ([`error_text`]): a help naming
/// admin API routes shows the console commands doing the same instead (`db list --dir DIR`
/// for `GET /api/admin/databases`; the mapping is in `console/help.rs`).
pub fn command_error_text(err: &ServerError, dir: &Path) -> String {
    match err {
        ServerError::Remote {
            help: Some(help), ..
        } => format!(
            "Error: {err}\nHelp: {}",
            super::help::console_help(help, &shell_word(dir))
        ),
        _ => error_text(err),
    }
}

/// The text of an invalid invocation: `Error: <message>`, then the command that shows the
/// usage under a comment (the CLI's usage errors).
pub fn usage_error_text(message: &str) -> String {
    Note::new(format!("Error: {message}"))
        .command("show the usage", format!("{BIN} --help"))
        .text()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;

    fn dir() -> PathBuf {
        PathBuf::from("srv")
    }

    #[test]
    fn db_list_shows_every_state_in_columns_and_the_reasons_under_problems() {
        let answer = json!({
            "databases": [
                {"name": "acme", "state": "loaded", "kind": "plain", "role": "normal",
                 "readOnly": false, "autoLoad": true, "description": "Acme graph",
                 "sizeBytes": 9_728},
                {"name": "broken", "state": "failed", "kind": "plain", "role": "normal",
                 "readOnly": false, "autoLoad": false, "description": "", "sizeBytes": 100,
                 "reason": "Error: 'srv/databases/broken/store' is not a Graphersal store\nHelp: \
                            create it first (graphersal store create srv/databases/broken/store)"},
                {"name": "cur", "aliasOf": "acme", "state": "loaded", "kind": "plain"},
                {"name": "emb", "state": "needs-model", "kind": "plain", "role": "normal",
                 "readOnly": false, "autoLoad": false, "description": "", "sizeBytes": 2048,
                 "reason": "the embedding model 'acme/tiny' is missing", "model": "acme/tiny"},
                {"name": "prod", "state": "loading", "kind": "plain", "role": "base",
                 "readOnly": true, "autoLoad": false, "description": "", "sizeBytes": 1},
            ],
            "aliases": {"cur": "acme"},
            "problems": ["databases/x: no database.toml"],
        });
        let text = render(&DbCommand::List, &answer, &dir());
        let expected = [
            format!("── Databases (4) {}", "─".repeat(61)),
            "  NAME    STATE        KIND   FLAGS      SIZE     DESCRIPTION".to_string(),
            "  acme    loaded       plain  auto-load  9.5 KiB  Acme graph".to_string(),
            "  broken  failed       plain             100 B".to_string(),
            "  emb     needs-model  plain             2.0 KiB".to_string(),
            "  prod    loading      base   read-only  1 B".to_string(),
            String::new(),
            format!("── Aliases {}", "─".repeat(67)),
            "  ALIAS  DATABASE".to_string(),
            "  cur    acme".to_string(),
            String::new(),
            format!("── Problems {}", "─".repeat(66)),
            "  broken:   failed: 'srv/databases/broken/store' is not a Graphersal store".to_string(),
            "            Help: create it first (graphersal store create srv/databases/broken/store)"
                .to_string(),
            String::new(),
            "  emb:      needs-model: the embedding model 'acme/tiny' is missing".to_string(),
            "            # add the model to the shared models".to_string(),
            "            graphersal-server models add <model directory or .gsnap> --dir srv"
                .to_string(),
            String::new(),
            "  registry: databases/x: no database.toml".to_string(),
        ];
        assert_eq!(text.lines().collect::<Vec<_>>(), expected, "\n{text}");
    }

    #[test]
    fn an_empty_listing_says_how_to_create_a_database() {
        let answer = json!({"databases": [], "aliases": {}, "problems": []});
        assert_eq!(
            render(&DbCommand::List, &answer, Path::new("my srv")),
            "No databases\n  # create one\n  graphersal-server db create NAME --dir 'my srv'"
        );
    }

    #[test]
    fn a_failed_load_shows_its_reason_under_the_value_column() {
        let answer = json!({"name": "b", "state": "failed",
                            "reason": "Error: no GRAPH file\nHelp: create it first"});
        let command = DbCommand::Load {
            name: "b".to_string(),
            wait: true,
        };
        assert!(failed(&command, &answer));
        assert_eq!(
            render(&command, &answer, &dir()),
            "Database 'b' is not loaded\n  state:  failed\n  reason: no GRAPH file\n          \
             Help: create it first"
        );
    }

    #[test]
    fn models_list_is_a_table_with_the_missing_models_under_problems() {
        let answer = json!({
            "models": [{"repo": "acme/tiny", "bytes": 8704, "license": "mit",
                        "blob": "b2ef4b279fd4aaaa", "users": ["a", "b"],
                        "linkWarning": "copied, not linked, in b: no disk saving"}],
            "missing": [{"database": "emb", "model": "acme/other"}],
        });
        let text = render(&DbCommand::ModelsList, &answer, &dir());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[1],
            "  MODEL      SIZE     LICENSE  BLOB          USED BY"
        );
        assert_eq!(
            lines[2],
            "  acme/tiny  8.5 KiB  mit      b2ef4b279fd4  a, b"
        );
        assert!(lines[4].starts_with("── Problems ───"), "{text}");
        assert_eq!(
            lines[5],
            "  acme/tiny: copied, not linked, in b: no disk saving"
        );
        assert_eq!(lines[7], "  emb:       needs the model 'acme/other'");
        assert_eq!(lines[8], "             # add it");
        let empty = json!({"models": [], "missing": []});
        assert!(render(&DbCommand::ModelsList, &empty, &dir()).starts_with("No shared models\n"),);
    }

    #[test]
    fn an_import_names_what_is_missing_and_how_to_add_it() {
        let answer = json!({
            "name": "emb", "state": "needs-model", "kind": "plain", "role": "normal",
            "readOnly": false, "autoLoad": false, "description": "",
            "reason": "the embedding model 'acme/tiny' is missing",
            "import": {"format": "snapshot", "modelsMissing": ["acme/tiny"],
                       "modelsResolved": [], "skipped": 2, "notes": ["one", "two"]},
        });
        let command = DbCommand::Import {
            source: PathBuf::from("/data/emb.gsnap"),
            name: "emb".to_string(),
            base: None,
            format: None,
            description: None,
            role: crate::database::DatabaseRole::Normal,
        };
        assert_eq!(
            render(&command, &answer, &dir()),
            "Imported database 'emb'\n  source:  /data/emb.gsnap\n  format:  snapshot\n  \
             kind:    plain\n  state:   needs-model\n  reason:  the embedding model 'acme/tiny' \
             is missing\n  skipped: 2 elements\n  missing: acme/tiny\n  notes:   one\n           \
             two\n  # add the missing model to the shared models, then load it\n  \
             graphersal-server models add <model directory or .gsnap> --dir srv"
        );
    }

    #[test]
    fn errors_put_the_help_on_its_own_line_and_usage_errors_name_the_help_command() {
        let err = ServerError::Remote {
            status: 409,
            message: "database 'a' is loaded".to_string(),
            help: Some("unload it first".to_string()),
        };
        let text = error_text(&err);
        assert!(text.starts_with("Error: "), "{text}");
        assert!(text.ends_with("\nHelp: unload it first"), "{text}");
        assert_eq!(
            usage_error_text("db needs --dir DIR"),
            "Error: db needs --dir DIR\n  # show the usage\n  graphersal-server --help"
        );
    }

    #[test]
    fn shell_words_are_quoted_only_when_needed() {
        assert_eq!(shell_word(Path::new("/srv/a-b_c.d")), "/srv/a-b_c.d");
        assert_eq!(shell_word(Path::new("my srv")), "'my srv'");
        assert_eq!(shell_word(Path::new("it's")), r"'it'\''s'");
    }
}
