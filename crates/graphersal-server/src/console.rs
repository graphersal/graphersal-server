//! The console's admin commands (`graphersal-server db ...`, `import`, `models ...`,
//! `user ...`, `mcp ...`).
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
//! `user ...` edits `access.toml` (`/api/admin/users...`, `/api/admin/grants...`): offline the
//! console acts as the operator on the file (it holds the server lock); through a server with
//! the run's operator token, so the server's sessions of a changed token end at once.
//!
//! The console needs no credentials on the directory because the directory's OS permissions
//! are the boundary ([`crate::security`]): [`run`] refuses a directory that is not private to
//! the current OS user, and its audit records name that user (`operator (os:<name>)`).
//! [`run_with_token`] (`--token`) goes through the running server with a user's admin token
//! instead, touching no file of the directory.
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

mod backup;
mod first_admin;
mod help;
mod render;
mod service;
mod settings;
mod start;

pub use backup::{
    default_backup_file, restore_json, restore_text, server_backup, server_backup_text,
    server_backup_with_token,
};
pub use first_admin::first_admin_text;
pub(crate) use render::shell_word;
pub use render::{
    command_error_text, error_text, failed, init_text, render, render_outcome, usage_error_text,
};
pub use service::{
    started_note, started_output, status_json, status_json_of, status_text, stop_text,
};
pub use settings::{check_file as config_check_file, check_text as config_check_text};
pub use start::{address_line, start_block, stopped_text, stopping_text};

/// How long the console waits for a load to end (`db load` without `--no-wait`).
const LOAD_WAIT: Duration = Duration::from_secs(600);
/// How long the console waits for a starting server (its run file, its `503 loading`).
const START_WAIT: Duration = Duration::from_secs(10);

/// How long `models download` through a server waits for the download's end.
const DOWNLOAD_WAIT: Duration = Duration::from_secs(4 * 3600);

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
    /// `db clone SOURCE NAME [--mcp|--no-mcp]`.
    Clone {
        /// The database cloned.
        source: String,
        /// The new database's name.
        name: String,
        /// Offer the new database to MCP (`--mcp`: `Some(true)`, `--no-mcp`: `Some(false)`; `None`:
        /// the server's default, `mcp.toml`'s `expose_new`).
        mcp: Option<bool>,
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
    /// `db info NAME`: everything about a database (or the database an alias names): its
    /// identity, state, sizes, query limits, MCP settings, grants, backups and Store creation
    /// parameters (`GET databases/<name>/settings`).
    Info {
        /// The database or alias.
        name: String,
    },
    /// `db set NAME description TEXT` / `db set NAME limit KEY VALUE|--clear`: one setting of a
    /// database (`PUT databases/<name>/settings`).
    Set {
        /// The database (its real name).
        name: String,
        /// What is set.
        setting: DbSetting,
    },
    /// `db verify NAME` / `db verify --all` (`name` `None`): the engine's scrub of the
    /// database's Store, or of every database's (round 3 B-CRASH).
    Verify {
        /// The database (`None`: every database).
        name: Option<String>,
    },
    /// `db repair NAME [--name NEW]`: the engine's repair of NAME into the NEW database
    /// (default `NAME-repaired`; never in place).
    Repair {
        /// The damaged database.
        name: String,
        /// The new database's name (`None`: `NAME-repaired`).
        target: Option<String>,
    },
    /// `db swap NAME WITH --yes`: the database WITH (a repaired copy) takes NAME's name; the
    /// old NAME moves to `trash/`.
    Swap {
        /// The name kept.
        name: String,
        /// The database that takes it.
        with: String,
    },
    /// `db reset-layer NAME --yes`: discard every change of the layer database NAME over its
    /// base; the old layer moves to `trash/` (round 4 B-LAYER).
    ResetLayer {
        /// The layer database.
        name: String,
    },
    /// `db alias set ALIAS TARGET [--mcp|--no-mcp]`.
    AliasSet {
        /// The alias.
        alias: String,
        /// The database it names.
        target: String,
        /// Offer the new alias (a flip keeps its settings) to MCP (`--mcp`: `Some(true)`, `--no-mcp`: `Some(false)`; `None`:
        /// the server's default, `mcp.toml`'s `expose_new`).
        mcp: Option<bool>,
    },
    /// `db alias remove ALIAS`.
    AliasRemove {
        /// The alias.
        alias: String,
    },
    /// `db alias rename OLD NEW`.
    AliasRename {
        /// The alias.
        alias: String,
        /// Its new name.
        name: String,
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
        /// Offer the new database of a restore into a new name (a restore over a database keeps its settings) to MCP (`--mcp`: `Some(true)`, `--no-mcp`: `Some(false)`; `None`:
        /// the server's default, `mcp.toml`'s `expose_new`).
        mcp: Option<bool>,
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
        /// Offer the new database to MCP (`--mcp`: `Some(true)`, `--no-mcp`: `Some(false)`; `None`:
        /// the server's default, `mcp.toml`'s `expose_new`).
        mcp: Option<bool>,
    },
    /// `import DIR --inbox FILE --name NAME [--base BASE] [--format F] [--description TEXT]
    /// [--role normal|base]`: the entry `file` of the import inbox `DIR/imports/` (read where
    /// the server directory is) as the new database `name`.
    ImportInbox {
        /// The inbox entry's name (one file or directory name in `imports/`).
        file: String,
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
        /// Offer the new database to MCP (`--mcp`: `Some(true)`, `--no-mcp`: `Some(false)`; `None`:
        /// the server's default, `mcp.toml`'s `expose_new`).
        mcp: Option<bool>,
    },
    /// `import DIR --list`: the entries of the import inbox `DIR/imports/`.
    InboxList,
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
    /// `models download REPO [--revision R]`: the model downloaded from Hugging Face into the
    /// shared models (through a running server it runs there; the console waits for its end).
    ModelsDownload {
        /// The repository (`owner/name`).
        repo: String,
        /// The branch, tag or commit (`None`: `main`).
        revision: Option<String>,
    },
    /// `models download --cancel`: the running download of a server stops.
    ModelsDownloadCancel,
    /// `db export NAME --out FILE [--no-models] [--materialize]`.
    Export {
        /// The database.
        name: String,
        /// What the file holds.
        spec: ExportSpec,
        /// The file written.
        out: PathBuf,
    },
    /// `user list`.
    UserList,
    /// `user add NAME [--admin]`: the new user's token is printed once.
    UserAdd {
        /// The user's name.
        name: String,
        /// Whether it gets the `admin` role.
        admin: bool,
    },
    /// `user token NAME [--revoke]`: a new token (printed once), or with `--revoke` none.
    UserToken {
        /// The user.
        name: String,
        /// Take the token away instead of issuing a new one.
        revoke: bool,
    },
    /// `user remove NAME`.
    UserRemove {
        /// The user.
        name: String,
    },
    /// `user admin NAME on|off`.
    UserAdmin {
        /// The user.
        name: String,
        /// Give (`true`) or take the `admin` role.
        on: bool,
    },
    /// `user grant SUBJECT DATABASE read|write`.
    UserGrant {
        /// A user name, `anonymous`, or `user:<n>`, `role:<r>`, `group:<g>`.
        subject: String,
        /// A database's name or `*`.
        database: String,
        /// `read` or `write`.
        level: String,
    },
    /// `user revoke SUBJECT DATABASE`.
    UserRevoke {
        /// As for [`DbCommand::UserGrant`].
        subject: String,
        /// A database's name or `*`.
        database: String,
    },
    /// `mcp status`: the MCP switches, the databases' MCP settings and (through a running
    /// server) the usage statistics.
    McpStatus,
    /// `mcp on|off`: MCP for the whole server.
    McpSwitch {
        /// On or off.
        on: bool,
    },
    /// `mcp expose-new on|off`: whether a new database or alias is offered to MCP when its
    /// creator does not say (`mcp.toml`'s `expose_new`, default off).
    McpExposeNew {
        /// On or off.
        on: bool,
    },
    /// `config get [KEY]`: the keys of `server.toml` (round 4 B-SETTINGS), or one of them.
    ConfigGet {
        /// One dotted key (`limits.upload_bytes`); `None`: every key.
        key: Option<String>,
    },
    /// `config set KEY VALUE` / `config unset KEY`: change keys of `server.toml` (one write).
    ConfigSet {
        /// Each dotted key with its new value as JSON (`null`: back to its default).
        changes: Vec<(String, Value)>,
    },
    /// `mcp db NAME on|off` (exposure) or `mcp db NAME read-only on|off`.
    McpDatabase {
        /// A database or an alias.
        name: String,
        /// Offer it to MCP (`None`: unchanged).
        exposed: Option<bool>,
        /// Serve it read-only to MCP (`None`: unchanged).
        read_only: Option<bool>,
    },
}

/// One setting of [`DbCommand::Set`].
///
/// `#[non_exhaustive]`: further settings may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DbSetting {
    /// The description (empty clears it).
    Description(String),
    /// A query limit override of `[limits.query]`: `Some` sets it, `None` clears it (the
    /// server's default applies again). Applies at the database's next load.
    Limit {
        /// The key ([`crate::limits::QUERY_LIMIT_KEYS`]).
        key: String,
        /// The value.
        value: Option<u64>,
    },
}

impl DbSetting {
    /// The setting `db set NAME limit KEY VALUE` (`value` `None`: `--clear`): the key and the
    /// value checked as the server checks them. `Err` with the reason (an unknown key, a value
    /// that is not a whole number from 0 to 2^63-1).
    pub fn limit(key: &str, value: Option<&str>) -> Result<DbSetting, String> {
        crate::settings::check_limit_key(key)?;
        let value = value
            .map(|text| crate::settings::limit_value_of_text(key, text))
            .transpose()?;
        Ok(DbSetting::Limit {
            key: key.to_string(),
            value,
        })
    }
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
                if let Some(auto_load) = spec.auto_load {
                    body.insert("autoLoad".into(), json!(auto_load));
                }
                if let Some(read_only) = spec.read_only {
                    body.insert("readOnly".into(), json!(read_only));
                }
                if let Some(mcp) = spec.mcp {
                    body.insert("mcp".into(), json!(mcp));
                }
                (Method::Post, "databases".into(), Some(Value::Object(body)))
            }
            DbCommand::Delete { name } => (Method::Delete, format!("databases/{name}"), None),
            DbCommand::Clone { source, name, mcp } => (
                Method::Post,
                format!("databases/{source}/clone"),
                Some(with_mcp(json!({ "name": name }), *mcp)),
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
            DbCommand::Info { name } => (
                Method::Get,
                format!("databases/{}/settings", segment(name)),
                None,
            ),
            DbCommand::Set { name, setting } => {
                let body = match setting {
                    DbSetting::Description(text) => json!({ "description": text }),
                    DbSetting::Limit { key, value } => json!({ "limits": { key: value } }),
                };
                (
                    Method::Put,
                    format!("databases/{}/settings", segment(name)),
                    Some(body),
                )
            }
            DbCommand::Verify { name: Some(name) } => (
                Method::Post,
                format!("databases/{}/verify", segment(name)),
                None,
            ),
            DbCommand::Verify { name: None } => (Method::Post, "verify".into(), None),
            DbCommand::Repair { name, target } => {
                let mut body = Map::new();
                if let Some(target) = target {
                    body.insert("name".into(), json!(target));
                }
                (
                    Method::Post,
                    format!("databases/{}/repair", segment(name)),
                    Some(Value::Object(body)),
                )
            }
            DbCommand::Swap { name, with } => (
                Method::Post,
                format!("databases/{}/swap", segment(name)),
                Some(json!({ "with": with, "confirm": true })),
            ),
            DbCommand::ResetLayer { name } => (
                Method::Post,
                format!("databases/{}/reset-layer", segment(name)),
                Some(json!({ "confirm": true })),
            ),
            DbCommand::AliasSet { alias, target, mcp } => (
                Method::Put,
                format!("aliases/{alias}"),
                Some(with_mcp(json!({ "target": target }), *mcp)),
            ),
            DbCommand::AliasRemove { alias } => (Method::Delete, format!("aliases/{alias}"), None),
            DbCommand::AliasRename { alias, name } => (
                Method::Post,
                format!("aliases/{alias}/rename"),
                Some(json!({ "name": name })),
            ),
            DbCommand::Backup { name } => (Method::Post, format!("databases/{name}/backup"), None),
            DbCommand::Backups { name } => (Method::Get, format!("databases/{name}/backups"), None),
            DbCommand::Restore {
                name,
                backup,
                target,
                mcp,
            } => {
                let mut body = Map::new();
                body.insert("backup".into(), json!(backup));
                if let Some(target) = target {
                    body.insert("name".into(), json!(target));
                }
                (
                    Method::Post,
                    format!("databases/{name}/restore"),
                    Some(with_mcp(Value::Object(body), *mcp)),
                )
            }
            DbCommand::Import {
                source,
                name,
                base,
                format,
                description,
                role,
                mcp,
            } => {
                let source = ("path", json!(source.to_string_lossy()));
                let body = import_body(source, name, base, format, description, *role);
                (Method::Post, "import".into(), Some(with_mcp(body, *mcp)))
            }
            DbCommand::ImportInbox {
                file,
                name,
                base,
                format,
                description,
                role,
                mcp,
            } => {
                let source = ("inbox", json!(file));
                let body = import_body(source, name, base, format, description, *role);
                (Method::Post, "import".into(), Some(with_mcp(body, *mcp)))
            }
            DbCommand::InboxList => (Method::Get, "imports".into(), None),
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
            DbCommand::ModelsDownload { repo, revision } => {
                let mut body = json!({ "repo": repo });
                if let Some(revision) = revision {
                    body["modelRevision"] = json!(revision);
                }
                (Method::Post, "models/download".into(), Some(body))
            }
            DbCommand::ModelsDownloadCancel => {
                (Method::Post, "models/download/cancel".into(), None)
            }
            DbCommand::UserList => (Method::Get, "users".into(), None),
            DbCommand::UserAdd { name, admin } => (
                Method::Post,
                "users".into(),
                Some(json!({ "name": name, "admin": admin })),
            ),
            DbCommand::UserToken { name, revoke } => {
                let method = if *revoke {
                    Method::Delete
                } else {
                    Method::Post
                };
                (method, format!("users/{name}/token"), None)
            }
            DbCommand::UserRemove { name } => (Method::Delete, format!("users/{name}"), None),
            DbCommand::UserAdmin { name, on } => (
                Method::Put,
                format!("users/{name}/admin"),
                Some(json!({ "admin": on })),
            ),
            DbCommand::UserGrant {
                subject,
                database,
                level,
            } => (
                Method::Post,
                "grants".into(),
                Some(json!({ "subject": subject, "database": database, "level": level })),
            ),
            DbCommand::UserRevoke { subject, database } => (
                Method::Delete,
                format!("grants/{}/{}", segment(subject), segment(database)),
                None,
            ),
            DbCommand::ConfigGet { .. } => (Method::Get, "config".into(), None),
            DbCommand::ConfigSet { changes } => {
                let set: Map<String, Value> = changes.iter().cloned().collect();
                (Method::Put, "config".into(), Some(json!({ "set": set })))
            }
            DbCommand::McpStatus => (Method::Get, "mcp".into(), None),
            DbCommand::McpSwitch { on } => {
                (Method::Put, "mcp".into(), Some(json!({ "enabled": on })))
            }
            DbCommand::McpExposeNew { on } => {
                (Method::Put, "mcp".into(), Some(json!({ "exposeNew": on })))
            }
            DbCommand::McpDatabase {
                name,
                exposed,
                read_only,
            } => {
                let mut body = Map::new();
                if let Some(exposed) = exposed {
                    body.insert("exposed".into(), json!(exposed));
                }
                if let Some(read_only) = read_only {
                    body.insert("readOnly".into(), json!(read_only));
                }
                (
                    Method::Put,
                    format!("mcp/databases/{}", segment(name)),
                    Some(Value::Object(body)),
                )
            }
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

/// `text` as one path segment: every byte but the URL's unreserved characters as `%XX` (the
/// admin API decodes them; `user:bob` -> `user%3Abob`).
fn segment(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
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
/// The directory must be private to the current OS user first ([`crate::security::check`]):
/// the console acts as the operator without a token because only the directory's owner can
/// use it. Offline, the audit records name the operator and this OS user.
///
/// `Err`: [`ServerError::InsecureDir`] for a directory others could change or read;
/// [`ServerError::Remote`] for a refusal (the admin API's status, `error`, `help`);
/// [`ServerError::Connect`] when the running server cannot be reached; the errors of opening
/// the directory ([`ServerError::NotServerDir`], ...).
pub fn run(dir: &ServerDir, command: &DbCommand) -> Result<Outcome, ServerError> {
    crate::security::check(dir)?;
    let (method, path, body) = command.request();
    let audit = crate::audit::Audit::default()
        .with_actor(Some(crate::access::OPERATOR.to_string()))
        .with_os_user(crate::security::os_user());
    match Admin::open_offline(dir) {
        Ok((admin, _lock)) if matches!(command, DbCommand::Export { .. }) => Ok(Outcome {
            body: export_offline(&admin.with_audit(audit), command)?,
            via: Via::Directory,
            server_url: None,
        }),
        Ok((admin, _lock)) => {
            let body = offline(&admin.with_audit(audit), &method, &path, body.as_ref())?;
            Ok(Outcome {
                body,
                via: Via::Directory,
                server_url: None,
            })
        }
        Err(ServerError::Locked { .. }) => through_server(server_info(dir)?, command),
        Err(err) => Err(err),
    }
}

/// Runs `command` through the running server of `dir` with the admin token `token` (a user's,
/// `--token`), instead of the operator token of `server.run`: the server's address is
/// `address`, else the one `dir`'s `server.run` names while a server holds the directory's lock
/// and the file is the directory owner's ([`RunInfo::read_live`], SEC L2: a stale file's address
/// may be another program's now). No file of the directory is written and
/// its permissions are not checked (the token is the credential; the audit records name its
/// user). Without a running server it is refused: nothing acts on the directory offline with
/// a token.
///
/// `Err`: [`ServerError::Connect`] when no server runs (no `server.run`, or it cannot be read:
/// name the address) or it cannot be reached; [`ServerError::Remote`] for a refusal (a wrong
/// token is `401`, a user without the admin role `404`).
pub fn run_with_token(
    dir: &ServerDir,
    command: &DbCommand,
    token: &str,
    address: Option<SocketAddr>,
) -> Result<Outcome, ServerError> {
    let server = match address {
        Some(address) => RunInfo::new(address, format!("http://{address}/")),
        None => match RunInfo::read_live(dir) {
            Ok(Some(info)) => RunInfo::new(info.address, info.url),
            Ok(None) => {
                return Err(ServerError::Connect {
                    address: dir.run_path().display().to_string(),
                    message: "--token sends the command to a running server, and no server \
                              runs on this directory (no server.run); start it, or name its \
                              address with --server ADDRESS"
                        .to_string(),
                });
            }
            Err(err @ ServerError::Connect { .. }) => return Err(err),
            Err(err) => {
                return Err(ServerError::Connect {
                    address: dir.run_path().display().to_string(),
                    message: format!(
                        "{err}; name the running server's address with --server ADDRESS"
                    ),
                });
            }
        },
    };
    through_server(
        server.with_operator_token(crate::access::Secret::from_text(token)),
        command,
    )
}

/// `command` sent to the admin API of `server` with its token (`operator_token`).
fn through_server(server: RunInfo, command: &DbCommand) -> Result<Outcome, ServerError> {
    let (method, path, body) = command.request();
    let address = server.address;
    let answer = if matches!(command, DbCommand::Export { .. }) {
        export_remote(&server, &path, command)?
    } else {
        let mut answer = remote(&server, &method, &path, body.as_ref())?;
        if let DbCommand::Load { name, wait: true } = command {
            answer = wait_for_load(&server, name, answer)?;
        }
        if let DbCommand::ModelsDownload { .. } = command {
            answer = wait_for_download(&server, answer)?;
        }
        answer
    };
    Ok(Outcome {
        body: answer,
        via: Via::Server(address),
        server_url: Some(server.url),
    })
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

/// `db export` through the server of `server` (with its operator token): its download
/// streamed into `<out>.part`, renamed into `out` when complete.
fn export_remote(server: &RunInfo, path: &str, command: &DbCommand) -> Result<Value, ServerError> {
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
        let answer = crate::client::download(server.address, bearer(server), &url, &mut file);
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
pub(crate) fn server_info(dir: &ServerDir) -> Result<RunInfo, ServerError> {
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

/// The operator token of the server's run, the console's credentials.
pub(crate) fn bearer(server: &RunInfo) -> Option<&str> {
    server
        .operator_token
        .as_ref()
        .map(crate::access::Secret::expose)
}

/// The request sent to the server of `server` with its operator token (`503` while it starts
/// is retried a moment).
fn remote(
    server: &RunInfo,
    method: &Method,
    path: &str,
    body: Option<&Value>,
) -> Result<Value, ServerError> {
    let deadline = Instant::now() + START_WAIT;
    let url = format!("/api/admin/{path}");
    loop {
        let (status, json) =
            crate::client::request(server.address, bearer(server), method.as_str(), &url, body)?;
        if status == 503 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        return answer_of(status, json);
    }
}

/// Polls the database `name` until its load ended (`answer`: the load's answer).
fn wait_for_load(server: &RunInfo, name: &str, answer: Value) -> Result<Value, ServerError> {
    let deadline = Instant::now() + LOAD_WAIT;
    let mut current = answer;
    let load = current.get("load").cloned();
    while current["state"] == "loading" && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        current = remote(server, &Method::Get, &format!("databases/{name}"), None)?;
    }
    if let Some(load) = load {
        current["load"] = load;
    }
    Ok(current)
}

/// A model download started through `server`, polled until it ends (at most
/// [`DOWNLOAD_WAIT`]; then its state so far, still `running`).
fn wait_for_download(server: &RunInfo, answer: Value) -> Result<Value, ServerError> {
    let deadline = Instant::now() + DOWNLOAD_WAIT;
    let id = answer["download"]["id"].clone();
    let mut current = answer;
    while current["download"]["state"] == "running" && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        let polled = remote(server, &Method::Get, "models/download", None)?;
        // Another download replaced it (an admin started one after this one ended).
        if polled["download"]["id"] != id {
            break;
        }
        current = polled;
    }
    Ok(current)
}

/// `Ok(body)` for a success, [`ServerError::Remote`] for an error status.
pub(crate) fn answer_of(status: u16, json: Value) -> Result<Value, ServerError> {
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

/// The JSON body of `POST import`: the source (`("path", ..)` or `("inbox", ..)`), then the
/// fields that are set (`role` only when not normal).
/// `body` with `"mcp": mcp` when given (the MCP exposure of a new name).
fn with_mcp(mut body: Value, mcp: Option<bool>) -> Value {
    if let Some(mcp) = mcp {
        body["mcp"] = json!(mcp);
    }
    body
}

fn import_body(
    (key, source): (&str, Value),
    name: &str,
    base: &Option<String>,
    format: &Option<String>,
    description: &Option<String>,
    role: DatabaseRole,
) -> Value {
    let mut body = Map::new();
    body.insert(key.into(), source);
    if role != DatabaseRole::Normal {
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
    Value::Object(body)
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

#[cfg(test)]
mod user_tests;
