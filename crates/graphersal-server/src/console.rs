//! The console's admin commands (`graphersal-server db ...`).
//!
//! Every command is one request of the admin HTTP API ([`DbCommand::request`]). [`run`] sends it
//! where the directory is:
//! - no server holds the directory: the console takes the server lock itself, scans the registry
//!   and answers the request with the SAME route over an offline [`Admin`] (nothing is served,
//!   so `load` and `unload` are refused with a message naming `serve`);
//! - a server holds it (its `server.lock`): the request goes to that server's admin API at the
//!   address in `server.run` ([`crate::RunInfo`]), with a minimal HTTP client.
//!
//! Either way the answer is the admin API's JSON, and a refusal is [`ServerError::Remote`]
//! with the API's status, `error` and `help`. [`render`] turns an answer into the human text.

use std::io::Cursor;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use graphersal_serve::{Method, Request};
use serde_json::{Map, Value, json};

use crate::database::DatabaseKind;
use crate::error::ServerError;
use crate::layout::ServerDir;
use crate::ops::{Admin, CreateSpec};
use crate::run_file::RunInfo;

/// How long the console waits for a load to end (`db load` without `--no-wait`).
const LOAD_WAIT: Duration = Duration::from_secs(600);
/// How long the console waits for a starting server (its run file, its `503 loading`).
const START_WAIT: Duration = Duration::from_secs(10);

/// One admin command of the console.
///
/// `#[non_exhaustive]`: further commands will be added.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum DbCommand {
    /// `db list`.
    List,
    /// `db create NAME ...`.
    Create {
        /// The new database's name.
        name: String,
        /// What it is.
        spec: CreateSpec,
    },
    /// `db delete NAME`.
    Delete {
        /// The database.
        name: String,
    },
    /// `db clone SOURCE NAME`.
    Clone {
        /// The database cloned.
        source: String,
        /// The new database's name.
        name: String,
    },
    /// `db load NAME [--no-wait]`.
    Load {
        /// The database.
        name: String,
        /// Whether the console waits until the load ended.
        wait: bool,
    },
    /// `db unload NAME`.
    Unload {
        /// The database.
        name: String,
    },
    /// `db read-only NAME on|off`.
    ReadOnly {
        /// The database.
        name: String,
        /// On or off.
        on: bool,
    },
    /// `db auto-load NAME on|off`.
    AutoLoad {
        /// The database.
        name: String,
        /// On or off.
        on: bool,
    },
    /// `db alias set ALIAS TARGET`.
    AliasSet {
        /// The alias.
        alias: String,
        /// The database it names.
        target: String,
    },
    /// `db alias remove ALIAS`.
    AliasRemove {
        /// The alias.
        alias: String,
    },
}

impl DbCommand {
    /// The admin API request of the command: method, path below `/api/admin/`, JSON body.
    pub fn request(&self) -> (Method, String, Option<Value>) {
        match self {
            DbCommand::List => (Method::Get, "databases".into(), None),
            DbCommand::Create { name, spec } => {
                let mut body = Map::new();
                body.insert("name".into(), json!(name));
                body.insert("kind".into(), json!(spec.kind.as_str()));
                body.insert("role".into(), json!(spec.role.as_str()));
                if let Some(base) = &spec.base {
                    body.insert("base".into(), json!(base));
                }
                if let Some(schema) = &spec.schema {
                    body.insert("schema".into(), schema.clone());
                }
                body.insert("description".into(), json!(spec.description));
                body.insert("autoLoad".into(), json!(spec.auto_load));
                body.insert("readOnly".into(), json!(spec.read_only));
                (Method::Post, "databases".into(), Some(Value::Object(body)))
            }
            DbCommand::Delete { name } => (Method::Delete, format!("databases/{name}"), None),
            DbCommand::Clone { source, name } => (
                Method::Post,
                format!("databases/{source}/clone"),
                Some(json!({ "name": name })),
            ),
            DbCommand::Load { name, .. } => (Method::Post, format!("databases/{name}/load"), None),
            DbCommand::Unload { name } => (Method::Post, format!("databases/{name}/unload"), None),
            DbCommand::ReadOnly { name, on } => (
                Method::Put,
                format!("databases/{name}/read-only"),
                Some(json!({ "readOnly": on })),
            ),
            DbCommand::AutoLoad { name, on } => (
                Method::Put,
                format!("databases/{name}/auto-load"),
                Some(json!({ "autoLoad": on })),
            ),
            DbCommand::AliasSet { alias, target } => (
                Method::Put,
                format!("aliases/{alias}"),
                Some(json!({ "target": target })),
            ),
            DbCommand::AliasRemove { alias } => (Method::Delete, format!("aliases/{alias}"), None),
        }
    }
}

/// Where a command ran.
///
/// `#[non_exhaustive]`: further ways may be added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Via {
    /// On the directory itself (no server held it).
    Directory,
    /// Through the admin API of the server running on the directory, at this address.
    Server(SocketAddr),
}

/// The answer of a command ([`run`]).
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Outcome {
    /// The admin API's JSON answer.
    pub body: Value,
    /// Where it ran.
    pub via: Via,
}

/// Runs `command` on the server directory `dir` (module docs).
///
/// `Err`: [`ServerError::Remote`] for a refusal (the admin API's status, `error`, `help`);
/// [`ServerError::Connect`] when the running server cannot be reached; the errors of opening
/// the directory ([`ServerError::NotServerDir`], ...).
pub fn run(dir: &ServerDir, command: &DbCommand) -> Result<Outcome, ServerError> {
    let (method, path, body) = command.request();
    match Admin::open_offline(dir) {
        Ok((admin, _lock)) => {
            let body = offline(&admin, &method, &path, body.as_ref())?;
            Ok(Outcome {
                body,
                via: Via::Directory,
            })
        }
        Err(ServerError::Locked { .. }) => {
            let address = server_address(dir)?;
            let mut answer = remote(address, &method, &path, body.as_ref())?;
            if let DbCommand::Load { name, wait: true } = command {
                answer = wait_for_load(address, name, answer)?;
            }
            Ok(Outcome {
                body: answer,
                via: Via::Server(address),
            })
        }
        Err(err) => Err(err),
    }
}

/// The request answered by the admin route over the offline `admin`.
fn offline(
    admin: &Admin,
    method: &Method,
    path: &str,
    body: Option<&Value>,
) -> Result<Value, ServerError> {
    let bytes = body.map(Value::to_string).unwrap_or_default().into_bytes();
    let length = bytes.len();
    let url = format!("/api/admin/{path}");
    let mut request = Request::new(method.clone(), &url, Cursor::new(bytes))
        .with_header("Content-Type", "application/json")
        .with_body_length(Some(length));
    match crate::admin_api::route(admin, method, path, &mut request) {
        Ok(response) => {
            let json = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
            answer_of(response.status(), json)
        }
        Err(err) => Err(ServerError::Remote {
            status: err.status(),
            message: err.message().to_string(),
            help: None,
        }),
    }
}

/// The address of the server holding `dir`, from its run file (waits a moment for a starting
/// server).
fn server_address(dir: &ServerDir) -> Result<SocketAddr, ServerError> {
    let deadline = Instant::now() + START_WAIT;
    loop {
        if let Some(info) = RunInfo::read(dir)? {
            return Ok(info.address);
        }
        if Instant::now() >= deadline {
            return Err(ServerError::Connect {
                address: dir.run_path().display().to_string(),
                message: "a server holds the directory's lock but wrote no server.run; is it \
                          starting or stuck?"
                    .to_string(),
            });
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The request sent to the server at `address` (`503` while it starts is retried a moment).
fn remote(
    address: SocketAddr,
    method: &Method,
    path: &str,
    body: Option<&Value>,
) -> Result<Value, ServerError> {
    let deadline = Instant::now() + START_WAIT;
    let url = format!("/api/admin/{path}");
    loop {
        let (status, json) = crate::client::request(address, method.as_str(), &url, body)?;
        if status == 503 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        return answer_of(status, json);
    }
}

/// Polls the database `name` until its load ended (`answer`: the load's answer).
fn wait_for_load(address: SocketAddr, name: &str, answer: Value) -> Result<Value, ServerError> {
    let deadline = Instant::now() + LOAD_WAIT;
    let mut current = answer;
    let load = current.get("load").cloned();
    while current["state"] == "loading" && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        current = remote(address, &Method::Get, &format!("databases/{name}"), None)?;
    }
    if let Some(load) = load {
        current["load"] = load;
    }
    Ok(current)
}

/// `Ok(body)` for a success, [`ServerError::Remote`] for an error status.
fn answer_of(status: u16, json: Value) -> Result<Value, ServerError> {
    if status < 400 {
        return Ok(json);
    }
    let text = |key: &str| json.get(key).and_then(Value::as_str).map(str::to_string);
    Err(ServerError::Remote {
        status,
        message: text("error").unwrap_or_else(|| format!("the server answered {status}")),
        help: text("help"),
    })
}

/// Whether the answer of `command` reports a failure although the request succeeded (a load
/// that ended `failed` or `needs-model`).
pub fn failed(command: &DbCommand, answer: &Value) -> bool {
    matches!(command, DbCommand::Load { .. })
        && matches!(answer["state"].as_str(), Some("failed" | "needs-model"))
}

/// The human text of the answer of `command`.
pub fn render(command: &DbCommand, answer: &Value) -> String {
    let s = |key: &str| answer[key].as_str().unwrap_or_default().to_string();
    let on_off = |on: bool| if on { "on" } else { "off" };
    match command {
        DbCommand::List => render_list(answer),
        DbCommand::Create { .. } => format!("created database '{}' ({})", s("name"), kind(answer)),
        DbCommand::Delete { .. } => format!(
            "deleted database '{}' (moved to {}; recoverable by hand)",
            s("name"),
            s("trash")
        ),
        DbCommand::Clone { source, .. } => format!(
            "cloned database '{source}' into '{}' ({})",
            s("name"),
            kind(answer)
        ),
        DbCommand::Load { name, .. } => match answer["state"].as_str() {
            Some("loaded") if s("load") == "already-loaded" => {
                format!("database '{name}' is loaded already")
            }
            Some("loaded") => format!("database '{name}' is loaded"),
            Some("loading") => format!(
                "database '{name}' is loading (see `graphersal-server db list` for its state)"
            ),
            _ => format!("database '{name}' is not loaded: {}", state(answer)),
        },
        DbCommand::Unload { name } => {
            if answer["unloaded"] == Value::Bool(true) {
                format!("unloaded database '{name}'")
            } else {
                format!("database '{name}' was not loaded")
            }
        }
        DbCommand::ReadOnly { name, .. } => format!(
            "read-only is {} for database '{name}'",
            on_off(answer["readOnly"] == Value::Bool(true))
        ),
        DbCommand::AutoLoad { name, .. } => format!(
            "auto-load is {} for database '{name}'",
            on_off(answer["autoLoad"] == Value::Bool(true))
        ),
        DbCommand::AliasSet { .. } => {
            format!("alias '{}' -> database '{}'", s("alias"), s("target"))
        }
        DbCommand::AliasRemove { .. } => format!("removed alias '{}'", s("alias")),
    }
}

/// `plain`, `layer over 'b'`, `base`.
fn kind(entry: &Value) -> String {
    if entry["role"] == "base" {
        "base, read-only".to_string()
    } else if entry["kind"] == DatabaseKind::Layer.as_str() {
        format!(
            "layer over '{}'",
            entry["base"].as_str().unwrap_or_default()
        )
    } else {
        "plain".to_string()
    }
}

/// The state with its reason.
fn state(entry: &Value) -> String {
    let state = entry["state"].as_str().unwrap_or("?");
    match entry["reason"].as_str() {
        Some(reason) => format!("{state} ({reason})"),
        None => state.to_string(),
    }
}

/// The listing as a table.
fn render_list(answer: &Value) -> String {
    let rows: Vec<&Value> = answer["databases"]
        .as_array()
        .map(|rows| rows.iter().collect())
        .unwrap_or_default();
    if rows.is_empty() {
        return "no databases".to_string();
    }
    let mut table = vec![[
        "NAME".to_string(),
        "STATE".to_string(),
        "KIND".to_string(),
        "FLAGS".to_string(),
        "DESCRIPTION".to_string(),
    ]];
    for row in rows {
        let name = row["name"].as_str().unwrap_or_default().to_string();
        if let Some(target) = row["aliasOf"].as_str() {
            table.push([
                name,
                String::new(),
                format!("alias of '{target}'"),
                String::new(),
                String::new(),
            ]);
            continue;
        }
        let mut flags = Vec::new();
        if row["readOnly"] == Value::Bool(true) {
            flags.push("read-only");
        }
        if row["autoLoad"] == Value::Bool(true) {
            flags.push("auto-load");
        }
        table.push([
            name,
            state(row),
            if row.get("kind").is_some() {
                kind(row)
            } else {
                String::new()
            },
            flags.join(","),
            row["description"].as_str().unwrap_or_default().to_string(),
        ]);
    }
    let mut widths = [0usize; 5];
    for row in &table {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let mut out = String::new();
    for row in &table {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            if i + 1 == row.len() {
                line.push_str(cell);
            } else {
                line.push_str(&format!("{cell:<width$}  ", width = widths[i]));
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    if let Some(problems) = answer["problems"].as_array() {
        for problem in problems {
            out.push_str(&format!(
                "problem: {}\n",
                problem.as_str().unwrap_or_default()
            ));
        }
    }
    out.trim_end().to_string()
}
