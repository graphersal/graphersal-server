//! The human text of the console: the answers of the admin commands ([`render`]), `init`, the
//! `serve` start block and stop lines, errors and usage errors, all in the layout of
//! [`crate::text`] (the graphersal CLI's).

use std::path::Path;

use serde_json::Value;

use super::{DbCommand, DbSetting, Outcome};
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
/// that ended `failed` or `needs-model`; a verify that found damage, or a database in
/// maintenance).
pub fn failed(command: &DbCommand, answer: &Value) -> bool {
    match command {
        DbCommand::Load { .. } => {
            matches!(answer["state"].as_str(), Some("failed" | "needs-model"))
        }
        DbCommand::Verify { name: Some(_) } => {
            answer["ok"] != Value::Bool(true) || answer["state"] == "maintenance"
        }
        DbCommand::Verify { name: None } => answer["ok"] != Value::Bool(true),
        _ => false,
    }
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
        DbCommand::Create { .. } => mcp_note(
            leftover_note(
                entry_note(
                    Note::new(format!("Created database '{}'", s("name"))),
                    answer,
                ),
                answer,
            ),
            answer,
            &dir_arg,
        )
        .text(),
        DbCommand::Delete { .. } => {
            let trash = below(dir, &s("trash"));
            let note = Note::new(format!("Deleted database '{}'", s("name")))
                .detail("moved to", &trash)
                .detail(
                    "recover",
                    "move it back into databases/ by hand while no server runs",
                );
            match answer["grantsRemoved"].as_u64().unwrap_or(0) {
                0 => note.text(),
                grants => note
                    .detail(
                        "grants removed",
                        format!("{grants} (a restore does not bring them back)"),
                    )
                    .text(),
            }
        }
        DbCommand::Clone { source, .. } => mcp_note(
            leftover_note(
                entry_note(
                    Note::new(format!("Cloned database '{source}' into '{}'", s("name"))),
                    answer,
                ),
                answer,
            ),
            answer,
            &dir_arg,
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
        DbCommand::Info { .. } => render_info(answer, &dir_arg),
        DbCommand::Verify { name: Some(_) } => verify_note(answer, &dir_arg).text(),
        DbCommand::Verify { name: None } => render_verify_all(answer, &dir_arg),
        DbCommand::Repair { .. } => render_repair(answer, &dir_arg),
        DbCommand::Swap { .. } => {
            let name = s("name");
            let mut note = Note::new(format!(
                "Database '{}' took the name '{name}'",
                s("swappedWith")
            ))
            .detail(
                "replaced",
                format!("the old '{name}' moved to {}", below(dir, &s("trash"))),
            )
            .detail(
                "kept",
                format!("the grants, aliases and settings of '{name}'"),
            );
            if answer["reloaded"] == Value::Bool(true) {
                note = note.detail("loaded", "again (it was loaded)");
            }
            note.command(
                "verify it",
                format!("{BIN} db verify {name} --dir {dir_arg}"),
            )
            .text()
        }
        DbCommand::ResetLayer { .. } => render_reset_layer(answer, dir, &dir_arg),
        DbCommand::Set { name, setting } => render_set(name, setting, answer, &dir_arg),
        DbCommand::AliasSet { .. } => mcp_note(
            leftover_note(
                Note::new(format!(
                    "Alias '{}' -> database '{}'",
                    s("alias"),
                    s("target")
                )),
                answer,
            ),
            answer,
            &dir_arg,
        )
        .text(),
        DbCommand::AliasRemove { .. } => {
            let note = Note::new(format!("Removed alias '{}'", s("alias")));
            match answer["grantsRemoved"].as_u64().unwrap_or(0) {
                0 => note.text(),
                grants => note.detail("grants removed", grants.to_string()).text(),
            }
        }
        DbCommand::AliasRename { .. } => {
            let note = Note::new(format!(
                "Renamed alias '{}' to '{}' -> database '{}'",
                s("renamedFrom"),
                s("alias"),
                s("target")
            ));
            let note = match answer["grantsMoved"].as_u64().unwrap_or(0) {
                0 => note,
                grants => note.detail("grants moved", grants.to_string()),
            };
            leftover_note(note, answer)
                .detail(
                    "MCP sessions moved",
                    answer["mcpSessionsMoved"].as_u64().unwrap_or(0).to_string(),
                )
                .text()
        }
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
            match answer["trash"].as_str() {
                Some(trash) => {
                    note = note.detail("replaced", format!("moved to {}", below(dir, trash)));
                }
                None => {
                    note = note.detail(
                        "grants",
                        "none come back with a restore; grant access again with `user grant`",
                    );
                }
            }
            mcp_note(leftover_note(note, answer), answer, &dir_arg).text()
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
        DbCommand::ConfigGet { key } => {
            super::settings::render_get(key.as_deref(), answer, &dir_arg)
        }
        DbCommand::ConfigSet { .. } => super::settings::render_set(answer, &dir_arg),
        DbCommand::McpStatus => render_mcp(answer, &dir_arg),
        DbCommand::McpExposeNew { .. } => {
            let on = answer["exposeNew"] == Value::Bool(true);
            Note::new(if on {
                "New databases and aliases are offered to MCP"
            } else {
                "New databases and aliases are not offered to MCP"
            })
            .detail(
                "applies to",
                "databases and aliases created from now on (create, import, clone, a restore \
                 into a new name, repair, alias set); --mcp or --no-mcp on such a command wins; \
                 the names there now keep their settings",
            )
            .text()
        }
        DbCommand::McpSwitch { .. } => {
            let on = answer["enabled"] == Value::Bool(true);
            let note = Note::new(format!("MCP is switched {}", on_off(on)));
            let note = if on {
                note.detail("endpoints", "/mcp and /db/<name>/mcp answer again")
            } else {
                note.detail(
                    "endpoints",
                    "/mcp and /db/<name>/mcp answer 503 to every request until it is switched \
                     on, also to the agents connected now; their MCP sessions are kept (not \
                     ended) and continue then with the same session id and selection",
                )
            };
            let note = if answer["endpointBuilt"] == Value::Bool(false) {
                note.detail(
                    "but",
                    "server.toml [mcp] enabled = false: no MCP endpoint until it is true and \
                     the server restarted",
                )
            } else {
                note
            };
            note.text()
        }
        DbCommand::McpDatabase { .. } => {
            let changed = &answer["changed"];
            let name = changed["name"].as_str().unwrap_or_default();
            let exposed = changed["exposed"] == Value::Bool(true);
            let read_only = changed["readOnly"] == Value::Bool(true);
            Note::new(format!("MCP settings of '{name}'"))
                .detail(
                    "offered",
                    if exposed {
                        "yes".to_string()
                    } else {
                        format!("no: hidden from list_graphs and use_graph, /db/{name}/mcp 404")
                    },
                )
                .detail(
                    "read-only",
                    if read_only {
                        "yes: MCP sessions only read, whatever their grant"
                    } else {
                        "no: the caller's grant and the database's mode decide"
                    },
                )
                .text()
        }
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
        DbCommand::UserGrant { .. } => {
            let note = Note::new(format!(
                "Granted {} on {} to {}",
                s("level"),
                database_word(&s("database")),
                s("subject")
            ));
            if answer["kind"] == "alias" {
                note.detail(
                    "alias of",
                    format!(
                        "'{}' now; the grant follows the alias (reachable only through '{}')",
                        s("target"),
                        s("database")
                    ),
                )
                .text()
            } else {
                note.text()
            }
        }
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

/// One verify result (`db verify NAME`, and each of `db verify --all`).
fn verify_note(answer: &Value, dir_arg: &str) -> Note {
    let name = answer["database"].as_str().unwrap_or_default();
    let n = |key: &str| answer[key].as_u64().unwrap_or_default();
    let ok = answer["ok"] == Value::Bool(true);
    let maintenance = answer["state"] == "maintenance";
    if let Some(error) = answer["error"].as_str() {
        return Note::new(format!("Database '{name}': the verify failed"))
            .detail("error", error)
            .command(
                "restore a backup",
                format!("{BIN} db backups {name} --dir {dir_arg}"),
            );
    }
    let needs_model = !maintenance && answer["verdict"] == "needs-model";
    let models_missing = || {
        answer["modelsMissing"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let headline = match (ok, maintenance) {
        (true, false) => format!("Database '{name}' verifies: no problem found"),
        (true, true) => format!("Database '{name}' is in maintenance (read-only)"),
        (false, _) if needs_model => format!(
            "Database '{name}' needs its embedding model: {} is missing (the graph data is \
             intact)",
            models_missing()
        ),
        (false, _) => format!(
            "Database '{name}' is DAMAGED: {} problem(s)",
            answer["problems"].as_array().map(Vec::len).unwrap_or(0)
        ),
    };
    let mut note = Note::new(headline)
        .detail("verified", answer["how"].as_str().unwrap_or_default())
        .detail(
            "checked",
            format!(
                "{} snapshot(s), {} WAL segment(s), {} record(s) (commits {}..{}), {} model(s), \
                 {} blob(s)",
                n("snapshotsChecked"),
                n("segmentsChecked"),
                n("recordsChecked"),
                n("firstCommit"),
                n("lastCommit"),
                n("modelsChecked"),
                n("blobsChecked")
            ),
        )
        .detail("state", answer["state"].as_str().unwrap_or_default());
    let mut model_commands: Vec<(String, String)> = Vec::new();
    for problem in answer["problems"].as_array().into_iter().flatten() {
        note = note.detail("problem", problem_text(problem));
        if problem["kind"] == "model" {
            let command = match (problem["model"].as_str(), problem["shared"] == true) {
                (Some(model), true) => (
                    format!("the shared models hold {model}: add it to the database again"),
                    format!("{BIN} models add {model} --dir {dir_arg}"),
                ),
                (Some(model), false) => (
                    format!(
                        "download the model {model} (a Model2Vec directory), then import it \
                         (a directory, .tar/.tar.gz or a .gsnap carrying it)"
                    ),
                    format!("{BIN} models add SOURCE --dir {dir_arg}"),
                ),
                (None, _) => (
                    "import the missing model (a directory, .tar/.tar.gz or .gsnap)".to_string(),
                    format!("{BIN} models add SOURCE --dir {dir_arg}"),
                ),
            };
            if !model_commands.contains(&command) {
                model_commands.push(command);
            }
        }
    }
    if let Some(damage) = answer["damage"].as_str() {
        note = note.detail("damage", damage);
    }
    if needs_model {
        for (why, command) in model_commands {
            note = note.command(&why, command);
        }
        note = note.command(
            "then load it",
            format!("{BIN} db load {name} --dir {dir_arg}"),
        );
    } else if !ok || maintenance {
        for (why, command) in model_commands {
            note = note.command(&why, command);
        }
        note = note
            .command(
                "repair it into a new database (it is not changed)",
                format!("{BIN} db repair {name} --dir {dir_arg}"),
            )
            .command(
                "or restore a backup",
                format!("{BIN} db backups {name} --dir {dir_arg}"),
            );
    }
    note
}

/// One verify problem in words: `path at byte N: reason`; a missing model blob: `the
/// embedding model NAME (blob f0ed7648) is missing`.
fn problem_text(problem: &Value) -> String {
    if problem["kind"] == "model" {
        let blob = problem["blob"].as_str().unwrap_or("?");
        let short = crate::recovery::short_hash(blob);
        return match problem["model"].as_str() {
            Some(model) => format!("the embedding model {model} (blob {short}) is missing"),
            None => format!(
                "the blob {short} of one of the embedding models {} is missing",
                problem["candidates"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
    }
    let at = problem["offset"]
        .as_u64()
        .map(|o| format!(" at byte {o}"))
        .unwrap_or_default();
    format!(
        "{}{at}: {}",
        problem["path"].as_str().unwrap_or_default(),
        problem["reason"].as_str().unwrap_or_default()
    )
}

/// `db verify --all`.
fn render_verify_all(answer: &Value, dir_arg: &str) -> String {
    let results = answer["results"].as_array().cloned().unwrap_or_default();
    if results.is_empty() {
        return Note::new("No databases to verify").text();
    }
    let cells: Vec<Vec<String>> = results
        .iter()
        .map(|r| {
            let verdict = if r.get("error").is_some() {
                "verify failed".to_string()
            } else if r["ok"] == Value::Bool(true) && r["state"] != "maintenance" {
                "ok".to_string()
            } else if r["verdict"] == "needs-model" && r["state"] != "maintenance" {
                "needs model".to_string()
            } else {
                format!(
                    "DAMAGED ({} problem(s))",
                    r["problems"].as_array().map(Vec::len).unwrap_or(0)
                )
            };
            vec![
                r["database"].as_str().unwrap_or_default().to_string(),
                verdict,
                r["state"].as_str().unwrap_or_default().to_string(),
                r["how"].as_str().unwrap_or_default().to_string(),
            ]
        })
        .collect();
    let mut out = String::new();
    let mut section = Section::new(format!("Verify ({})", results.len()));
    section.lines(table(&["DATABASE", "RESULT", "STATE", "VERIFIED"], &cells));
    out.push_str(&text::render(&[section]));
    for r in &results {
        let ok = r["ok"] == Value::Bool(true) && r["state"] != "maintenance";
        if !ok {
            out.push('\n');
            out.push_str(&verify_note(r, dir_arg).text());
        }
    }
    out
}

/// `db repair NAME`.
fn render_repair(answer: &Value, dir_arg: &str) -> String {
    let name = answer["name"].as_str().unwrap_or_default();
    let source = answer["repairedFrom"].as_str().unwrap_or_default();
    let repair = &answer["repair"];
    let lossless = repair["lossless"] == Value::Bool(true);
    let mut note = Note::new(format!("Repaired database '{source}' into '{name}'"))
        .detail(
            "result",
            if lossless {
                "lossless: no data of the readable state lost, nothing diverged".to_string()
            } else {
                "DATA LOST or diverged: read the lines below".to_string()
            },
        )
        .detail(
            "position",
            format!(
                "commit {}",
                repair["commitSeq"].as_u64().unwrap_or_default()
            ),
        )
        .detail("verified", "yes (the engine verifies the copy)")
        .detail(
            "unchanged",
            format!("'{source}' (in maintenance while it is damaged)"),
        );
    let list = |key: &str| -> Vec<String> {
        repair[key]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .map(|v| match v.as_str() {
                        Some(text) => text.to_string(),
                        None => v.to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    for line in list("repaired") {
        note = note.detail("rebuilt", line);
    }
    for range in repair["lostCommits"].as_array().into_iter().flatten() {
        let first = range["first"].as_u64().unwrap_or_default();
        let text = match range["last"].as_u64() {
            Some(last) => format!("commits {first}..={last}"),
            None => format!("commits from {first} on"),
        };
        note = note.detail("lost", text);
    }
    for line in list("lostRanges") {
        note = note.detail("lost", line);
    }
    for line in list("lostElements") {
        note = note.detail("lost", line);
    }
    let diverged = repair["divergedCount"].as_u64().unwrap_or_default();
    if diverged > 0 {
        note = note.detail(
            "diverged",
            format!("{diverged} element(s): check them by hand (--json lists them)"),
        );
    }
    for line in list("modelsNotCarried") {
        note = note.detail("model not carried", line);
    }
    note = note.detail(
        "never carried",
        repair["notCarried"].as_str().unwrap_or_default(),
    );
    note.command(
        "let the repaired copy take the name (the old one moves to trash/)",
        format!("{BIN} db swap {source} {name} --yes --dir {dir_arg}"),
    )
    .command(
        "or keep both: nothing to do; or restore a backup instead",
        format!("{BIN} db backups {source} --dir {dir_arg}"),
    )
    .text()
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

/// The database of the grant `g` as `user list` shows it: a real database by its name, an
/// alias with its current target (`prod → prod-2026-11 (alias)`), a name that names nothing
/// now `old (missing)`, `*` as it is.
fn grant_database(g: &Value) -> String {
    let database = g["database"].as_str().unwrap_or_default();
    match g["kind"].as_str() {
        Some("alias") => format!(
            "{database} → {} (alias)",
            g["target"].as_str().unwrap_or("?")
        ),
        Some("missing") => format!("{database} (missing)"),
        _ => database.to_string(),
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
                        grant_database(g),
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
                    vec![
                        g["subject"].as_str().unwrap_or_default().to_string(),
                        grant_database(g),
                        g["level"].as_str().unwrap_or_default().to_string(),
                    ]
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

/// `mcp status`: the switches, the databases' MCP settings and the usage statistics.
fn render_mcp(answer: &Value, dir_arg: &str) -> String {
    let yes_no = |v: &Value| if *v == Value::Bool(true) { "yes" } else { "no" };
    let mut status = Section::new("MCP");
    let serving = answer["serving"] == Value::Bool(true);
    status.row(
        "state",
        if serving {
            "on".to_string()
        } else if answer["endpointBuilt"] == Value::Bool(false) {
            "off: server.toml [mcp] enabled = false (restart after changing it)".to_string()
        } else {
            format!("off (switch it on: {BIN} mcp on --dir {dir_arg})")
        },
    );
    status.row("endpoint", answer["endpoint"].as_str().unwrap_or("/mcp"));
    status.row(
        "new databases",
        if answer["exposeNew"] == Value::Bool(true) {
            format!("offered to MCP (change it: {BIN} mcp expose-new off --dir {dir_arg})")
        } else {
            format!(
                "not offered to MCP unless created with --mcp (change it: {BIN} mcp expose-new \
                 on --dir {dir_arg})"
            )
        },
    );
    let usage = &answer["usage"];
    if usage.is_object() {
        status.row(
            "sessions",
            format!("{} open", usage["sessions"]["open"].as_u64().unwrap_or(0)),
        );
        status.row(
            "tool calls",
            format!(
                "{} ({} errors, {} refused while off) since the start",
                usage["calls"]["total"].as_u64().unwrap_or(0),
                usage["calls"]["errors"].as_u64().unwrap_or(0),
                usage["calls"]["refused"].as_u64().unwrap_or(0)
            ),
        );
    } else {
        status.row("statistics", "only while the server runs");
    }
    let mut databases = Section::new("Databases");
    let rows: Vec<Vec<String>> = answer["databases"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    let name = row["name"].as_str().unwrap_or_default();
                    let name = match (row["kind"].as_str(), row["target"].as_str()) {
                        (Some("alias"), Some(target)) => format!("{name} → {target} (alias)"),
                        (Some("missing"), _) => format!("{name} (missing)"),
                        _ => name.to_string(),
                    };
                    vec![
                        name,
                        yes_no(&row["effectiveExposed"]).to_string(),
                        yes_no(&row["effectiveReadOnly"]).to_string(),
                        row["sessions"].as_u64().unwrap_or(0).to_string(),
                        row["calls"].as_u64().unwrap_or(0).to_string(),
                    ]
                })
                .collect()
        })
        .unwrap_or_default();
    if rows.is_empty() {
        databases.row("none", "no databases or aliases");
    } else {
        databases.lines(table(
            &["NAME", "OFFERED", "READ-ONLY", "SESSIONS", "CALLS"],
            &rows,
        ));
    }
    let mut sections = vec![status, databases];
    let tools: Vec<Vec<String>> = usage["tools"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    vec![
                        row["tool"].as_str().unwrap_or_default().to_string(),
                        row["calls"].as_u64().unwrap_or(0).to_string(),
                        row["errors"].as_u64().unwrap_or(0).to_string(),
                    ]
                })
                .collect()
        })
        .unwrap_or_default();
    if !tools.is_empty() {
        let mut section = Section::new("Tool calls");
        section.lines(table(&["TOOL", "CALLS", "ERRORS"], &tools));
        sections.push(section);
    }
    text::render(&sections)
}

/// A limit value for people: `5000`, `0 (no limit)`, `-` (not set).
fn limit_text(key: &str, value: &Value) -> String {
    match value.as_u64() {
        None => "-".to_string(),
        Some(0) if key == "max_value_depth" => "0 (default 128)".to_string(),
        Some(0) => "0 (no limit)".to_string(),
        Some(n) => n.to_string(),
    }
}

/// `db info NAME` (`GET databases/<name>/settings`).
fn render_info(answer: &Value, dir_arg: &str) -> String {
    let name = answer["name"].as_str().unwrap_or_default();
    let db = &answer["database"];
    let yes_no = |v: &Value| if *v == Value::Bool(true) { "yes" } else { "no" };
    let list = |v: &Value| {
        let items: Vec<&str> = v
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if items.is_empty() {
            "none".to_string()
        } else {
            items.join(", ")
        }
    };
    let mut identity = Section::new(format!("Database '{name}'"));
    if let Some(requested) = answer["requested"].as_str() {
        identity.row("asked for", format!("the alias '{requested}'"));
    }
    identity.row("kind", kind(db));
    identity.row("role", db["role"].as_str().unwrap_or("-"));
    if answer["layers"].as_array().is_some_and(|l| !l.is_empty()) {
        identity.row("layers over it", list(&answer["layers"]));
    }
    identity.row("aliases", list(&answer["aliases"]));
    identity.row("created", db["created"].as_str().unwrap_or("-"));
    let description = db["description"].as_str().unwrap_or_default();
    identity.row(
        "description",
        if description.is_empty() {
            "-"
        } else {
            description
        },
    );
    let mut state = Section::new("State");
    state.row("state", db["state"].as_str().unwrap_or("?"));
    if let Some(reason) = db["reason"].as_str() {
        state.row("reason", reason_text(reason));
    }
    state.row("read-only", yes_no(&db["readOnly"]));
    state.row("auto-load", yes_no(&db["autoLoad"]));
    state.row(
        "on disk",
        db["sizeBytes"]
            .as_u64()
            .map_or("-".to_string(), text::bytes),
    );
    state.row(
        "in memory",
        match db["memoryBytes"].as_u64() {
            Some(bytes) => match db["sharedMemoryBytes"].as_u64() {
                Some(shared) => format!(
                    "{} (+ {} of its base, shared)",
                    text::bytes(bytes),
                    text::bytes(shared)
                ),
                None => text::bytes(bytes),
            },
            None => "- (not loaded)".to_string(),
        },
    );
    let limits = &answer["limits"];
    let loaded = limits["loaded"] == Value::Bool(true);
    let mut limit_section = Section::new("Query limits");
    let rows: Vec<Vec<String>> = limits["keys"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    let key = row["key"].as_str().unwrap_or_default();
                    let mut cells = vec![
                        key.to_string(),
                        limit_text(key, &row["override"]),
                        limit_text(key, &row["server"]),
                        limit_text(key, &row["effective"]),
                        row["source"].as_str().unwrap_or_default().to_string(),
                    ];
                    if loaded {
                        cells.push(limit_text(key, &row["inEffect"]));
                    }
                    cells
                })
                .collect()
        })
        .unwrap_or_default();
    let mut header = vec!["KEY", "DATABASE", "SERVER", "NEXT LOAD", "FROM"];
    if loaded {
        header.push("NOW");
    }
    limit_section.lines(table(&header, &rows));
    if let Some(problem) = limits["serverError"].as_str() {
        limit_section.row("server.toml", problem);
    }
    if limits["reloadNeeded"] == Value::Bool(true) {
        limit_section.row("note", "changed since the load: applies at the next load");
        limit_section.unwrapped(
            "reload",
            format!(
                "{BIN} db unload {name} --dir {dir_arg} && {BIN} db load {name} --dir {dir_arg}"
            ),
        );
    }
    limit_section.unwrapped(
        "change",
        format!("{BIN} db set {name} limit KEY VALUE|--clear --dir {dir_arg}"),
    );
    let mcp = &answer["mcp"];
    let mut mcp_section = Section::new("MCP");
    if let Some(problem) = mcp["error"].as_str() {
        mcp_section.row("error", problem);
    } else {
        mcp_section.row(
            "server",
            if mcp["serving"] == Value::Bool(true) {
                "on"
            } else {
                "off"
            },
        );
        for row in mcp["names"].as_array().into_iter().flatten() {
            let label = match row["kind"].as_str() {
                Some("alias") => format!("alias {}", row["name"].as_str().unwrap_or_default()),
                _ => row["name"].as_str().unwrap_or_default().to_string(),
            };
            mcp_section.row(
                label,
                format!(
                    "offered {}, read-only {}, {} open session(s), {} call(s)",
                    yes_no(&row["effectiveExposed"]),
                    yes_no(&row["effectiveReadOnly"]),
                    row["sessions"].as_u64().unwrap_or(0),
                    row["calls"].as_u64().unwrap_or(0)
                ),
            );
        }
    }
    let mut grants = Section::new("Grants");
    let grant_rows: Vec<Vec<String>> = answer["grants"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|g| {
                    let database = g["database"].as_str().unwrap_or_default();
                    let via = match g["via"].as_str() {
                        Some("alias") => format!("{database} (alias)"),
                        Some("all") => "* (every database)".to_string(),
                        _ => database.to_string(),
                    };
                    vec![
                        g["subject"].as_str().unwrap_or_default().to_string(),
                        via,
                        g["level"].as_str().unwrap_or_default().to_string(),
                    ]
                })
                .collect()
        })
        .unwrap_or_default();
    if grant_rows.is_empty() {
        grants.row("grants", "none");
    } else {
        grants.lines(table(&["SUBJECT", "ON", "LEVEL"], &grant_rows));
    }
    grants.row(
        "admins",
        format!("{} (every database)", list(&answer["admins"])),
    );
    let backups = &answer["backups"];
    let mut backup_section = Section::new("Backups");
    backup_section.row(
        "count",
        format!(
            "{} ({})",
            backups["count"].as_u64().unwrap_or(0),
            text::bytes(backups["bytes"].as_u64().unwrap_or(0))
        ),
    );
    if let Some(latest) = backups["latest"].as_object() {
        backup_section.row(
            "latest",
            latest["backup"].as_str().unwrap_or_default().to_string(),
        );
    }
    let mut store = Section::new("Store (creation parameters are fixed)");
    if let Some(problem) = answer["storeError"].as_str() {
        store.row("error", problem);
    } else {
        let info = &answer["store"];
        let text_of = |v: &Value| match v {
            Value::String(s) => s.clone(),
            Value::Null => "-".to_string(),
            other => other.to_string(),
        };
        store.row("store id", text_of(&info["storeId"]));
        store.row("damage policy", text_of(&info["damagePolicy"]));
        store.row(
            "chunk size",
            info["chunkBytes"]
                .as_u64()
                .map_or("-".to_string(), text::bytes),
        );
        store.row("created", text_of(&info["createdAt"]));
        store.row("backend", text_of(&info["backend"]));
        store.row("format version", text_of(&info["formatVersion"]));
        store.row(
            "last commit",
            format!(
                "{} at {}",
                text_of(&info["commitSeq"]),
                text_of(&info["lastCommitAt"])
            ),
        );
        if let Some(base) = info["layerBase"].as_str() {
            store.row("layer base", base);
        }
    }
    // Round 3 B-CRASH: the damage state and this database's unclean-shutdown check; B-VERIFY:
    // the needs-model state and the last verify (always a row: "never verified").
    let recovery = &answer["recovery"];
    let mut damage = Section::new("Damage and recovery");
    if recovery["maintenance"] == Value::Bool(true) {
        damage.row(
            "maintenance",
            "its Store is damaged: served read-only (queries read the intact data)",
        );
        if let Some(report) = recovery["damage"].as_str() {
            damage.lines(report.lines().map(|l| format!("  {l}")));
        }
        damage.unwrapped(
            "repair",
            format!("{BIN} db repair {name} --dir {dir_arg} (or db backups {name})"),
        );
    }
    // B-VERIFY: the needs-model state and the last verify, in the web UI's words.
    if let Some(repo) = recovery["needsModel"].as_str() {
        damage.row(
            "needs model",
            format!("the embedding model {repo} is missing (the graph data is intact)"),
        );
        damage.unwrapped(
            "add it",
            format!(
                "{BIN} models add SOURCE --dir {dir_arg} (or models add {repo} when the shared \
                 models hold it)"
            ),
        );
    }
    match recovery["lastVerify"].as_object() {
        Some(last) => {
            let headline = last
                .get("headline")
                .and_then(Value::as_str)
                .unwrap_or_default();
            damage.row(
                "last verify",
                headline.strip_prefix("Last verify ").unwrap_or(headline),
            );
            for problem in last
                .get("problems")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .take(5)
            {
                damage.row("problem", problem_text(problem));
            }
        }
        None => damage.row(
            "last verify",
            format!(
                "never verified ({BIN} db verify {name} --dir {dir_arg} checks the whole Store)"
            ),
        ),
    }
    let unclean = &recovery["uncleanShutdown"];
    if let Some(check) = unclean.get("check").filter(|c| c.is_object()) {
        let when = unclean["detectedAtMs"]
            .as_u64()
            .map(|ms| crate::database::utc_text(ms / 1000))
            .unwrap_or_default();
        damage.row(
            "unclean shutdown",
            format!(
                "checked at the start of {when}: {}",
                check["outcome"].as_str().unwrap_or_default()
            ),
        );
        let rec = &check["recovery"];
        if rec.is_object() {
            damage.row(
                "journal",
                format!(
                    "{} commit(s) replayed onto snapshot {}, at commit {}{}",
                    rec["commitsReplayed"].as_u64().unwrap_or_default(),
                    rec["snapshotCommitSeq"].as_u64().unwrap_or_default(),
                    rec["commitSeq"].as_u64().unwrap_or_default(),
                    rec["tornTail"]
                        .as_str()
                        .map(|t| format!("; torn tail cut: {t}"))
                        .unwrap_or_default()
                ),
            );
        }
        if let Some(ok) = check["verify"]["ok"].as_bool() {
            damage.row(
                "verify",
                if ok {
                    "no problem found".to_string()
                } else {
                    format!(
                        "{} problem(s)",
                        check["verify"]["problems"]
                            .as_array()
                            .map(Vec::len)
                            .unwrap_or(0)
                    )
                },
            );
        }
    }
    text::render(&[
        identity,
        state,
        limit_section,
        mcp_section,
        grants,
        backup_section,
        damage,
        store,
    ])
}

/// `db set NAME ...`.
fn render_set(name: &str, setting: &DbSetting, answer: &Value, dir_arg: &str) -> String {
    match setting {
        DbSetting::Description(text) if text.is_empty() => {
            Note::new(format!("Cleared the description of database '{name}'")).text()
        }
        DbSetting::Description(text) => {
            Note::new(format!("Set the description of database '{name}'"))
                .detail("description", text.as_str())
                .text()
        }
        DbSetting::Limit { key, value } => {
            let row = answer["limits"]["keys"]
                .as_array()
                .and_then(|rows| rows.iter().find(|r| r["key"] == key.as_str()));
            let effective = row.map_or("-".to_string(), |r| limit_text(key, &r["effective"]));
            let headline = match value {
                Some(value) => format!("Set the query limit {key} = {value} of database '{name}'"),
                None => format!("Cleared the query limit override {key} of database '{name}'"),
            };
            let note = Note::new(headline).detail("next load", effective);
            if answer["limits"]["loaded"] == Value::Bool(true) {
                let now = row.map_or("-".to_string(), |r| limit_text(key, &r["inEffect"]));
                note.detail("now", format!("{now} (it is loaded: the change applies at its next load)"))
                    .command(
                        "apply it now",
                        format!(
                            "{BIN} db unload {name} --dir {dir_arg}\n{BIN} db load {name} --dir {dir_arg}"
                        ),
                    )
                    .text()
            } else {
                note.detail("applies", "when it is loaded").text()
            }
        }
    }
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

/// `note` with a warning when the answer says grants left on the name (from a deleted
/// database or alias) were removed before the new database or alias took it
/// (`leftoverGrantsRemoved`).
fn leftover_note(note: Note, answer: &Value) -> Note {
    match answer["leftoverGrantsRemoved"].as_u64().unwrap_or(0) {
        0 => note,
        n => note.detail(
            "warning",
            format!(
                "removed {} left on this name from a deleted database or alias (it does not \
                 inherit them)",
                text::count(n as usize, "grant")
            ),
        ),
    }
}

/// `note` with the MCP exposure of a new database or alias (the answer's `mcpExposed`; round
/// 4 B-MCPDEF): how to offer a hidden one to MCP.
fn mcp_note(note: Note, answer: &Value, dir_arg: &str) -> Note {
    let name = answer["alias"]
        .as_str()
        .or_else(|| answer["name"].as_str())
        .unwrap_or_default();
    match answer["mcpExposed"].as_bool() {
        Some(true) => note.detail("MCP", "offered (when MCP is on)"),
        Some(false) => note.detail("MCP", "not offered").command(
            "offer it to MCP",
            format!(
                "{BIN} mcp db {} on --dir {dir_arg}",
                shell_word(Path::new(name))
            ),
        ),
        None => note,
    }
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

/// The table of the database rows `rows` (no alias rows): with the size on disk, memory (a
/// loaded database's graph; a layer's own changes, without its base) and description columns
/// for `db list`, without them for the start block.
pub(crate) fn database_table(rows: &[&Value], full: bool) -> Vec<String> {
    let mut header = vec!["NAME", "STATE", "KIND", "FLAGS"];
    if full {
        header.extend(["ON DISK", "MEMORY", "DESCRIPTION"]);
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
                cells.push(
                    row["memoryBytes"]
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
            (Some("maintenance"), _) => {
                let first = row["damage"]
                    .as_str()
                    .and_then(|d| d.lines().next())
                    .unwrap_or("its Store is damaged");
                entries.push((
                    name.clone(),
                    Note::new(format!("maintenance (read-only): {first}"))
                        .command(
                            "repair it into a new database",
                            format!("{BIN} db repair {name} --dir {dir_arg}"),
                        )
                        .command(
                            "or restore a backup",
                            format!("{BIN} db backups {name} --dir {dir_arg}"),
                        )
                        .text(),
                    true,
                ));
            }
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
    let mut note = mcp_note(entry_note(note, answer), answer, dir_arg);
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
    let mut note = leftover_note(note, answer);
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
    // The listen address of the server.toml written (init --config/--set may set another).
    let listen = report
        .dir
        .load_config()
        .map_or_else(|_| DEFAULT_LISTEN.to_string(), |config| config.listen);
    let note = if !report.existed {
        Note::new(format!("Created the server directory {}", dir.display()))
            .detail(
                "config",
                format!(
                    "{} (listen {listen}: edit it before serving)",
                    below(dir, "server.toml")
                ),
            )
            .detail(
                "MCP",
                format!(
                    "off, and new databases are not offered to it (switch it on: {BIN} mcp on \
                     --dir {dir_arg})"
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

/// `db reset-layer`: what was discarded, kept, and where the old layer went.
fn render_reset_layer(answer: &Value, dir: &Path, dir_arg: &str) -> String {
    let s = |key: &str| answer[key].as_str().unwrap_or("").to_string();
    let name = s("name");
    let base = s("base");
    let count = |part: &str, kind: &str| answer["discarded"][part][kind].as_u64().unwrap_or(0);
    let pair = |part: &str| {
        format!(
            "{} vertices, {} edges",
            count(part, "vertices"),
            count(part, "edges")
        )
    };
    let mut note = Note::new(format!(
        "Layer database '{name}' was reset: it reads exactly its base '{base}' now"
    ))
    .detail("discarded added", pair("added"))
    .detail("discarded changes of", pair("modified"))
    .detail("discarded removals of", pair("removed"))
    .detail("old layer", format!("moved to {}", below(dir, &s("trash"))))
    .detail(
        "kept",
        format!(
            "name, base, description, flags, query limits, grants, aliases, MCP settings and \
             {} scheduled job(s)",
            answer["jobsKept"].as_u64().unwrap_or(0)
        ),
    );
    if answer["reloaded"] == Value::Bool(true) {
        note = note.detail("loaded", "again (it was loaded)");
    }
    note.command(
        "everything about it",
        format!("{BIN} db info {name} --dir {dir_arg}"),
    )
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
    fn db_verify_names_a_missing_model_and_the_command_that_adds_it() {
        let hash = "f0ed7648aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let answer = json!({
            "database": "kb", "ok": false, "verdict": "needs-model", "state": "needs-model",
            "how": "files", "modelsMissing": ["acme/tiny"],
            "problems": [{"path": format!("blobs/f0/{hash}"), "offset": null,
                          "reason": "the blob is missing", "kind": "model",
                          "model": "acme/tiny", "blob": hash, "remedy": "..."}],
        });
        let command = DbCommand::Verify {
            name: Some("kb".to_string()),
        };
        let text = render(&command, &answer, &dir());
        let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            flat.contains("Database 'kb' needs its embedding model: acme/tiny is missing"),
            "{text}"
        );
        assert!(
            flat.contains("the embedding model acme/tiny (blob f0ed7648) is missing"),
            "{text}"
        );
        assert!(
            flat.contains("graphersal-server models add SOURCE --dir srv"),
            "{text}"
        );
        assert!(
            flat.contains("graphersal-server db load kb --dir srv"),
            "{text}"
        );
        assert!(!flat.contains("db repair"), "{text}");
        assert!(
            failed(&command, &answer),
            "a needs-model verify is not a success"
        );
        // Shared: the command adds it by its repository; verify --all says "needs model".
        let mut shared = answer.clone();
        shared["problems"][0]["shared"] = json!(true);
        let text = render(&command, &shared, &dir());
        assert!(text.contains("models add acme/tiny --dir srv"), "{text}");
        let all = render(
            &DbCommand::Verify { name: None },
            &json!({"ok": false, "results": [answer]}),
            &dir(),
        );
        assert!(all.contains("needs model"), "{all}");
        // A generic missing blob keeps its path and the repair commands.
        let generic = json!({
            "database": "kb", "ok": false, "verdict": "damaged", "state": "unloaded",
            "how": "files",
            "problems": [{"path": "blobs/aa/aa11", "offset": null,
                          "reason": "the blob is missing", "kind": "damage"}],
        });
        let text = render(&command, &generic, &dir());
        assert!(
            text.contains("blobs/aa/aa11: the blob is missing"),
            "{text}"
        );
        assert!(
            text.contains("is DAMAGED") && text.contains("db repair kb"),
            "{text}"
        );
    }

    #[test]
    fn db_list_shows_every_state_in_columns_and_the_reasons_under_problems() {
        let answer = json!({
            "databases": [
                {"name": "acme", "state": "loaded", "kind": "plain", "role": "normal",
                 "readOnly": false, "autoLoad": true, "description": "Acme graph",
                 "sizeBytes": 9_728, "memoryBytes": 4096},
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
            "  NAME    STATE        KIND   FLAGS      ON DISK  MEMORY   DESCRIPTION".to_string(),
            "  acme    loaded       plain  auto-load  9.5 KiB  4.0 KiB  Acme graph".to_string(),
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
            mcp: None,
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
