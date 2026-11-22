//! The console's admin commands (`graphersal-server db ...`, `import`, `models ...`).
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
//! with the API's status, `error` and `help`. [`render`] turns an answer into the human text,
//! in the graphersal CLI's layout (the crate-private `text` module); the `serve` start block
//! is [`start_block`].
//!
//! `import` sends the source's absolute path (`POST /api/admin/import {"path", ...}`, made by
//! [`source_path`]: no `.`/`..` segments): a running server reads the file itself, on this
//! machine; no upload.
//!
//! `models add` sends an absolute path too (`POST /api/admin/models {"path"}`).
//!
//! `db export` is a download: offline the admin core writes the file itself
//! ([`Admin::export_to_file`]); through a server the client streams the API's answer into
//! `<FILE>.part` and renames it. Its answer is the JSON `{"database", "file", "bytes",
//! "snapshot": "full" | "layer", "models": "included" | "excluded"}`.

use std::io::Cursor;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use graphersal_serve::{Method, Request};
use serde_json::{Map, Value, json};

use crate::database::DatabaseRole;
use crate::error::ServerError;
use crate::layout::ServerDir;
use crate::ops::{Admin, CreateSpec, ExportSpec};
use crate::run_file::RunInfo;

mod help;
mod render;
mod start;

pub use render::{
    command_error_text, error_text, failed, init_text, render, render_outcome, usage_error_text,
};
pub use start::{address_line, start_block, stopped_text, stopping_text};

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
    /// `db promote NAME`: the unloaded plain database becomes a base (read-only, for good).
    Promote {
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
    /// `db backup NAME`.
    Backup {
        /// The database.
        name: String,
    },
    /// `db backups NAME`.
    Backups {
        /// The database name (also of a deleted database).
        name: String,
    },
    /// `db restore NAME BACKUP [--name NEW]`.
    Restore {
        /// The database name whose backup is restored.
        name: String,
        /// The backup (its name or file name).
        backup: String,
        /// The database restored into (default: `name`, which must be unloaded).
        target: Option<String>,
    },
    /// `import DIR SOURCE --name NAME [--base BASE] [--format F] [--description TEXT] [--role
    /// normal|base]`: the file or directory `source` (absolute: a running server reads it on
    /// this machine) as the new database `name`.
    Import {
        /// The file or directory imported (absolute).
        source: PathBuf,
        /// The new database's name.
        name: String,
        /// A layer's base database.
        base: Option<String>,
        /// The format hint (`snapshot`, `store`, `graphson`, `graphml`).
        format: Option<String>,
        /// The new database's description.
        description: Option<String>,
        /// The new database's role ([`DatabaseRole::Base`]: a base of a plain source).
        role: DatabaseRole,
    },
    /// `models list`.
    ModelsList,
    /// `models add SOURCE [--repo R] [--license L]`: a Model2Vec directory, a `.tar` (or
    /// `.tar.gz`) of one or a `.gsnap`
    /// (`path`, absolute: a running server reads it on this machine) or a repository already
    /// in `models/` (`repo` alone).
    ModelsAdd {
        /// The directory or file added (absolute).
        path: Option<PathBuf>,
        /// The repository (of a directory that does not name it, or alone).
        repo: Option<String>,
        /// The licence of a directory that does not state it.
        license: Option<String>,
    },
    /// `models remove REPO`.
    ModelsRemove {
        /// The repository (or blob hash).
        model: String,
    },
    /// `db export NAME --out FILE [--no-models] [--materialize]`.
    Export {
        /// The database.
        name: String,
        /// What the file holds.
        spec: ExportSpec,
        /// The file written.
        out: PathBuf,
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
            DbCommand::Promote { name } => {
                (Method::Post, format!("databases/{name}/promote"), None)
            }
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
            DbCommand::Backup { name } => (Method::Post, format!("databases/{name}/backup"), None),
            DbCommand::Backups { name } => (Method::Get, format!("databases/{name}/backups"), None),
            DbCommand::Restore {
                name,
                backup,
                target,
            } => {
                let mut body = Map::new();
                body.insert("backup".into(), json!(backup));
                if let Some(target) = target {
                    body.insert("name".into(), json!(target));
                }
                (
                    Method::Post,
                    format!("databases/{name}/restore"),
                    Some(Value::Object(body)),
                )
            }
            DbCommand::Import {
                source,
                name,
                base,
                format,
                description,
                role,
            } => {
                let mut body = Map::new();
                body.insert("path".into(), json!(source.to_string_lossy()));
                if *role != DatabaseRole::Normal {
                    body.insert("role".into(), json!(role.as_str()));
                }
                body.insert("name".into(), json!(name));
                if let Some(base) = base {
                    body.insert("base".into(), json!(base));
                }
                if let Some(format) = format {
                    body.insert("format".into(), json!(format));
                }
                if let Some(description) = description {
                    body.insert("description".into(), json!(description));
                }
                (Method::Post, "import".into(), Some(Value::Object(body)))
            }
            DbCommand::ModelsList => (Method::Get, "models".into(), None),
            DbCommand::ModelsAdd {
                path,
                repo,
                license,
            } => {
                let mut body = Map::new();
                if let Some(path) = path {
                    body.insert("path".into(), json!(path.to_string_lossy()));
                }
                if let Some(repo) = repo {
                    body.insert("repo".into(), json!(repo));
                }
                if let Some(license) = license {
                    body.insert("license".into(), json!(license));
                }
                (Method::Post, "models".into(), Some(Value::Object(body)))
            }
            DbCommand::ModelsRemove { model } => (Method::Delete, format!("models/{model}"), None),
            DbCommand::Export { name, spec, .. } => (
                Method::Get,
                format!(
                    "databases/{name}/export?models={}&materialize={}",
                    spec.models, spec.materialize
                ),
                None,
            ),
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
    /// The URL of the server it went through (`server.run`'s, e.g. `http://127.0.0.1:8484/`),
    /// `None` on the directory itself.
    pub server_url: Option<String>,
}

/// Runs `command` on the server directory `dir` (module docs).
///
/// `Err`: [`ServerError::Remote`] for a refusal (the admin API's status, `error`, `help`);
/// [`ServerError::Connect`] when the running server cannot be reached; the errors of opening
/// the directory ([`ServerError::NotServerDir`], ...).
pub fn run(dir: &ServerDir, command: &DbCommand) -> Result<Outcome, ServerError> {
    let (method, path, body) = command.request();
    match Admin::open_offline(dir) {
        Ok((admin, _lock)) if matches!(command, DbCommand::Export { .. }) => Ok(Outcome {
            body: export_offline(&admin, command)?,
            via: Via::Directory,
            server_url: None,
        }),
        Err(ServerError::Locked { .. }) if matches!(command, DbCommand::Export { .. }) => {
            let server = server_info(dir)?;
            Ok(Outcome {
                body: export_remote(server.address, &path, command)?,
                via: Via::Server(server.address),
                server_url: Some(server.url),
            })
        }
        Ok((admin, _lock)) => {
            let body = offline(&admin, &method, &path, body.as_ref())?;
            Ok(Outcome {
                body,
                via: Via::Directory,
                server_url: None,
            })
        }
        Err(ServerError::Locked { .. }) => {
            let server = server_info(dir)?;
            let address = server.address;
            let mut answer = remote(address, &method, &path, body.as_ref())?;
            if let DbCommand::Load { name, wait: true } = command {
                answer = wait_for_load(address, name, answer)?;
            }
            Ok(Outcome {
                body: answer,
                via: Via::Server(address),
                server_url: Some(server.url),
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

/// `db export` on the directory: the admin core writes the file.
fn export_offline(admin: &Admin, command: &DbCommand) -> Result<Value, ServerError> {
    let DbCommand::Export { name, spec, out } = command else {
        return Err(ServerError::InvalidInput {
            name: "export".to_string(),
            message: "not an export command".to_string(),
        });
    };
    let exported = admin
        .export_to_file(name, *spec, out)
        .map_err(|err| remote_error(&err))?;
    Ok(json!({
        "database": exported.database,
        "file": out.display().to_string(),
        "bytes": exported.bytes,
        "snapshot": if exported.layer { "layer" } else { "full" },
        "models": if exported.models { "included" } else { "excluded" },
    }))
}

/// An admin core error as the API would answer it ([`ServerError::Remote`]).
fn remote_error(err: &ServerError) -> ServerError {
    ServerError::Remote {
        status: crate::admin_api::status_of(err),
        message: err.to_string(),
        help: None,
    }
}

/// `db export` through the server at `address`: its download streamed into `<out>.part`,
/// renamed into `out` when complete.
fn export_remote(
    address: SocketAddr,
    path: &str,
    command: &DbCommand,
) -> Result<Value, ServerError> {
    let DbCommand::Export { out, .. } = command else {
        return Err(ServerError::InvalidInput {
            name: "export".to_string(),
            message: "not an export command".to_string(),
        });
    };
    let part = part_path(out);
    let url = format!("/api/admin/{path}");
    let deadline = Instant::now() + START_WAIT;
    let answer = loop {
        let mut file =
            std::fs::File::create(&part).map_err(|err| ServerError::io("create", &part, err))?;
        let answer = crate::client::download(address, &url, &mut file);
        let answer = answer.and_then(|answer| {
            file.sync_all()
                .map_err(|err| ServerError::io("write", &part, err))?;
            Ok(answer)
        });
        match answer {
            Ok(answer) if answer.status == 503 && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            other => break other,
        }
    };
    let answer = match answer {
        Ok(answer) => answer,
        Err(err) => {
            let _ = std::fs::remove_file(&part);
            return Err(err);
        }
    };
    if answer.status != 200 {
        let _ = std::fs::remove_file(&part);
        return answer_of(answer.status, answer.error);
    }
    std::fs::rename(&part, out).map_err(|err| {
        let _ = std::fs::remove_file(&part);
        ServerError::io("rename", &part, err)
    })?;
    let name = match command {
        DbCommand::Export { name, .. } => name.clone(),
        _ => String::new(),
    };
    Ok(json!({
        "database": name,
        "file": out.display().to_string(),
        "bytes": answer.bytes,
        "snapshot": answer.header("X-Graphersal-Snapshot").unwrap_or("full"),
        "models": answer.header("X-Graphersal-Models").unwrap_or("included"),
    }))
}

/// `<path>.part`.
fn part_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    path.with_file_name(name)
}

/// The run file of the server holding `dir` (address, URL) (waits a moment for a starting
/// server).
fn server_info(dir: &ServerDir) -> Result<RunInfo, ServerError> {
    let deadline = Instant::now() + START_WAIT;
    loop {
        if let Some(info) = RunInfo::read(dir)? {
            return Ok(info);
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

/// The absolute path of the import source `source` as given on the command line, without `.`
/// and `..` segments (lexically, like `std::path::absolute` does on Windows; symlinks are not
/// resolved). When `..` follows a symlink the lexical path could name another file than the
/// operating system would: then (both exist but differ once resolved) the plain absolute path
/// is kept.
///
/// `Err`: the current directory cannot be read.
pub fn source_path(source: &Path) -> std::io::Result<PathBuf> {
    let absolute = std::path::absolute(source)?;
    let clean = normalize_lexically(&absolute);
    if clean == absolute {
        return Ok(absolute);
    }
    match (
        std::fs::canonicalize(&absolute),
        std::fs::canonicalize(&clean),
    ) {
        (Ok(real), Ok(real_clean)) if real != real_clean => Ok(absolute),
        (Ok(_), Err(_)) => Ok(absolute),
        _ => Ok(clean),
    }
}

/// `path` without `.` segments and with each `..` folded into the segment before it (a `..`
/// right below the root is dropped, as the operating system does); one component at a time
/// (Windows).
fn normalize_lexically(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    let mut normals = 0usize;
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir if normals > 0 => {
                out.pop();
                normals -= 1;
            }
            Component::ParentDir if out.has_root() => {}
            Component::ParentDir => out.push(".."),
            Component::Normal(part) => {
                out.push(part);
                normals += 1;
            }
            Component::Prefix(_) | Component::RootDir => out.push(component.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_lexically_drops_dot_segments() {
        let root = std::path::absolute("/").unwrap();
        let p = |parts: &[&str]| {
            let mut path = root.clone();
            for part in parts {
                path.push(part);
            }
            path
        };
        assert_eq!(normalize_lexically(&p(&["a", ".", "b"])), p(&["a", "b"]));
        assert_eq!(
            normalize_lexically(&p(&["a", "b", "..", "c"])),
            p(&["a", "c"])
        );
        assert_eq!(normalize_lexically(&p(&["a", "..", "..", "c"])), p(&["c"]));
        assert_eq!(normalize_lexically(&p(&["..", "x.json"])), p(&["x.json"]));
        assert_eq!(normalize_lexically(&p(&["a", "b"])), p(&["a", "b"]));
        assert_eq!(
            normalize_lexically(Path::new("../a/./b")),
            PathBuf::from("../a/b")
        );
    }

    #[test]
    fn source_path_is_absolute_and_clean() {
        let base = std::env::temp_dir().join(format!("gs-source-path-{}", std::process::id()));
        let sub = base.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let file = base.join("tiny.json");
        std::fs::write(&file, b"{}").unwrap();
        let given = sub.join("..").join(".").join("tiny.json");
        let clean = source_path(&given).unwrap();
        assert_eq!(clean, std::path::absolute(&file).unwrap());
        // A source that does not exist is normalised too (the import names the error).
        let missing = source_path(&sub.join("..").join("nope.json")).unwrap();
        assert_eq!(
            missing,
            std::path::absolute(base.join("nope.json")).unwrap()
        );
        #[cfg(unix)]
        {
            // `link/..` is the link target's parent for the OS: the lexical path would name
            // another file, so the plain absolute path is kept.
            let other = base.join("other");
            let deep = other.join("deep");
            std::fs::create_dir_all(&deep).unwrap();
            std::fs::write(other.join("tiny.json"), b"[]").unwrap();
            let link = sub.join("link");
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(&deep, &link).unwrap();
            let through = link.join("..").join("tiny.json");
            assert_eq!(source_path(&through).unwrap(), through);
            // Also when the lexical path exists: it is another file.
            std::fs::write(sub.join("tiny.json"), b"[1]").unwrap();
            assert_eq!(source_path(&through).unwrap(), through);
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
