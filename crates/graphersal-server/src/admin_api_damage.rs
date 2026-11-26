//! The admin API's verify, repair and swap routes and the last unclean shutdown (round 3
//! B-CRASH; [`crate::recovery`], the admin core in `ops_damage.rs`). The web UI's Databases
//! tab and the console's `db verify`, `db repair` and `db swap` use them.
//!
//! | Method | Path | Body | Answer |
//! |---|---|---|---|
//! | POST | `databases/<name>/verify` | | `200` a verify result |
//! | POST | `verify` | | `200 {"results": [verify result \| {"database", "error"}...], "ok"}` (every database) |
//! | POST | `databases/<name>/repair` | `{"name"?}` | `201` entry of the new database + `"repair"` + `"repairedFrom"` + `"next"` |
//! | POST | `databases/<name>/swap` | `{"with", "confirm": true}` | `200` entry + `"swappedWith"`, `"trash"`, `"reloaded"`, `"grantsRemoved"` |
//! | GET | `recovery` | | `200 {"uncleanShutdown": report \| null}` (the last unclean shutdown) |
//!
//! A verify result: `database`, `state` (afterwards; `maintenance` when the verify of the open
//! Store found damage under the damage policy `maintenance`), `ok`, `how` (`open store` |
//! `files`), `problems` (`[{"path", "offset", "reason"}]`), `notes`, `snapshotsChecked`,
//! `segmentsChecked`, `recordsChecked`, `firstCommit`, `lastCommit`, `modelsChecked`,
//! `blobsChecked`, `damage` (a database in maintenance: the damage report), and when it is not
//! ok `help` (repair into a new database, or restore a backup). A verify is synchronous (it
//! reads the whole Store; the console waits up to 10 minutes).
//!
//! A repair (synchronous as well) builds a NEW plain database from the damaged one with the
//! engine's repair (never in place; default name `<name>-repaired`), verified before it is
//! registered; `"repair"`: `commitSeq`, `lossless`, `damage` (the scan's report in words),
//! `damageItems`, `repaired` (what was rebuilt from donors), `lostCommits`
//! (`[{"first", "last" \| null}]`), `lostRanges`, `lostElements`, `diverged` (at most 100
//! `{"element", "commitSeq", "reason"}`), `divergedCount`, `marksNotCarried`,
//! `modelsNotCarried`, `blobsNotCarried`, `notCarried` (what a repair never carries, in
//! words); `"next"`: `swap` (the request that lets the repaired database take the damaged one's
//! name), `keep` (keeping both: nothing to do) and `restore` (a backup instead). `409` for a
//! layer or base database (restore a backup), `400` for a bad name.
//!
//! A swap needs `"confirm": true` (`400` without it: it moves the database `<name>` to
//! `trash/`): the database `with` takes the name `<name>` (both unloaded first; `<name>` is
//! loaded again when it was loaded: `reloaded`); `<name>`'s grants, aliases and MCP settings
//! stay, `with`'s name and grants are gone. `409` for a layer, base or failed database, or
//! while an alias names `with`.

use graphersal_serve::{HttpError, Method, Request, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{body_of, entry_json, error_answer, relative};
use crate::names::fold;
use crate::ops::{Admin, Verified, repair_json};
use crate::registry::DbState;

/// `POST databases/<name>/repair`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RepairBody {
    #[serde(default)]
    name: Option<String>,
}

/// `POST databases/<name>/swap`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SwapBody {
    with: String,
    #[serde(default)]
    confirm: bool,
}

/// Answers the routes of this module; `None` when the path is not one of them.
pub(super) fn route(
    admin: &Admin,
    method: &Method,
    segments: &[&str],
    request: &mut Request<'_>,
) -> Option<Result<Response, HttpError>> {
    let read = matches!(method, Method::Get | Method::Head);
    let not_allowed = |path: &str| {
        Err(HttpError::new(
            405,
            format!("method {method} is not allowed on /api/admin/{path}"),
        ))
    };
    Some(match segments {
        ["recovery"] if read => Ok(Response::json(
            200,
            &json!({ "uncleanShutdown": unclean_json(admin) }),
        )),
        ["recovery"] => not_allowed("recovery"),
        ["verify"] if *method == Method::Post => Ok(verify_all(admin)),
        ["verify"] => not_allowed("verify"),
        ["databases", name, "verify"] if !name.is_empty() && *method == Method::Post => {
            Ok(match admin.verify(name) {
                Ok(verified) => Response::json(200, &verify_json(&verified)),
                Err(err) => error_answer(&err, None),
            })
        }
        ["databases", name, "repair"] if !name.is_empty() && *method == Method::Post => {
            // The body is optional: none (or `{}`) takes the default name.
            let body = match request.text_body() {
                Ok(text) if text.trim().is_empty() => RepairBody::default(),
                Ok(text) => match serde_json::from_str::<RepairBody>(&text) {
                    Ok(body) => body,
                    Err(err) => {
                        return Some(Err(HttpError::bad_request(format!(
                            "invalid request body: {err}"
                        ))));
                    }
                },
                Err(err) => return Some(Err(err)),
            };
            Ok(repair(admin, name, body.name.as_deref()))
        }
        ["databases", name, "swap"] if !name.is_empty() && *method == Method::Post => {
            let body: SwapBody = match body_of(request) {
                Ok(body) => body,
                Err(err) => return Some(Err(err)),
            };
            if !body.confirm {
                return Some(Err(HttpError::bad_request(format!(
                    "a swap moves the database '{}' to trash/ and gives its name to '{}': \
                     confirm it with \"confirm\": true",
                    fold(name),
                    fold(&body.with)
                ))));
            }
            Ok(swap(admin, name, &body.with))
        }
        ["databases", _, "verify" | "repair" | "swap"] => not_allowed(&segments.join("/")),
        _ => return None,
    })
}

/// The last unclean shutdown: the running server's, else `recovery.json` of the directory.
pub(crate) fn unclean_json(admin: &Admin) -> Value {
    let report = match admin.recovery() {
        Some(recovery) => recovery.current(),
        None => crate::recovery::read_report(admin.registry().dir()),
    };
    report.map_or(Value::Null, |r| {
        serde_json::to_value(r).unwrap_or(Value::Null)
    })
}

/// A verify result as JSON (module docs).
pub(crate) fn verify_json(verified: &Verified) -> Value {
    let mut out = serde_json::to_value(&verified.facts).unwrap_or_else(|_| json!({}));
    out["database"] = Value::String(verified.name.clone());
    out["state"] = Value::String(verified.state.as_str().to_string());
    if let DbState::Maintenance { damage } = &verified.state {
        out["damage"] = Value::String(damage.clone());
    }
    let damaged = !verified.facts.ok || matches!(verified.state, DbState::Maintenance { .. });
    if damaged {
        out["help"] = Value::String(damage_help(&verified.name));
    }
    out
}

/// What to do about the damaged database `name`.
pub(crate) fn damage_help(name: &str) -> String {
    format!(
        "repair it into a new database: POST /api/admin/databases/{name}/repair (the damaged \
         one is not changed; then let the copy take its name: POST \
         /api/admin/databases/{name}/swap {{\"with\": \"{name}-repaired\", \"confirm\": \
         true}}), or restore a backup: GET /api/admin/databases/{name}/backups"
    )
}

/// `POST verify`.
fn verify_all(admin: &Admin) -> Response {
    let mut ok = true;
    let results: Vec<Value> = admin
        .verify_all()
        .into_iter()
        .map(|(name, result)| match result {
            Ok(verified) => {
                if !verified.facts.ok || matches!(verified.state, DbState::Maintenance { .. }) {
                    ok = false;
                }
                verify_json(&verified)
            }
            Err(err) => {
                ok = false;
                json!({ "database": name, "ok": false, "error": err.to_string() })
            }
        })
        .collect();
    Response::json(200, &json!({ "results": results, "ok": ok }))
}

/// `POST databases/<name>/repair`.
fn repair(admin: &Admin, name: &str, new_name: Option<&str>) -> Response {
    match admin.repair(name, new_name) {
        Ok(repaired) => {
            let (source, new) = (&repaired.source, &repaired.entry.name);
            let mut entry = entry_json(&repaired.entry, None);
            entry["repairedFrom"] = Value::String(source.clone());
            entry["repair"] = repair_json(&repaired.report);
            entry["next"] = json!({
                "swap": format!(
                    "POST /api/admin/databases/{source}/swap {{\"with\": \"{new}\", \"confirm\": true}}"
                ),
                "keep": format!(
                    "nothing to do: '{source}' stays as it is (in maintenance while damaged) and \
                     '{new}' is a database of its own; delete one later with DELETE \
                     /api/admin/databases/<name>"
                ),
                "restore": format!(
                    "or restore a backup instead: GET /api/admin/databases/{source}/backups"
                ),
            });
            Response::json(201, &entry)
        }
        Err(err) => {
            let help = match &err {
                crate::error::ServerError::Refused { .. } => Some(format!(
                    "restore a backup: GET /api/admin/databases/{}/backups, then POST \
                     /api/admin/databases/{}/restore {{\"backup\": \"<backup>\", \"name\": \
                     \"<new>\"}}",
                    fold(name),
                    fold(name)
                )),
                _ => None,
            };
            error_answer(&err, help)
        }
    }
}

/// `POST databases/<name>/swap`.
fn swap(admin: &Admin, name: &str, with: &str) -> Response {
    match admin.swap(name, with) {
        Ok(swapped) => {
            let mut entry = entry_json(&swapped.entry, None);
            entry["swappedWith"] = Value::String(swapped.with.clone());
            entry["trash"] = Value::String(relative(admin.registry().dir().root(), &swapped.trash));
            entry["reloaded"] = Value::Bool(swapped.reloaded);
            entry["grantsRemoved"] = json!(swapped.grants_removed);
            Response::json(200, &entry)
        }
        Err(err) => error_answer(&err, None),
    }
}
