//! The audit trail: every administrative event goes through ONE abstraction.
//!
//! An [`AuditEvent`] is typed (database create/delete/clone/import/restore/promote/backup,
//! the read-only and auto-load flags, aliases, users, tokens, grants, logins); an [`Audit`]
//! handle records it, with the actor (the principal's identity when known), into an
//! [`AuditSink`]. The one sink today is [`TracingAudit`]: a log line on the target
//! `graphersal_server::audit` (an audit journal can be another sink later; nothing else
//! changes). Events never carry a secret: a token shows only its last four characters.
//!
//! A record of the operator (the console on the server directory, or the operator token of
//! `server.run` through the server: both only the OS user owning the directory can use,
//! [`crate::security`]) also names that OS user ([`AuditRecord::os_user`]); the log line's
//! `actor` reads `operator (os:<name>)`.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

/// An administrative event.
///
/// `#[non_exhaustive]` (as are its variants): further events and details may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuditEvent {
    /// A database was created.
    #[non_exhaustive]
    DatabaseCreated {
        /// Its name.
        name: String,
        /// Its kind, role and base in words.
        detail: String,
    },
    /// A database was moved to `trash/`.
    #[non_exhaustive]
    DatabaseDeleted {
        /// Its name.
        name: String,
        /// Where it went.
        trash: PathBuf,
    },
    /// A database was cloned.
    #[non_exhaustive]
    DatabaseCloned {
        /// The source.
        source: String,
        /// The new database.
        name: String,
    },
    /// A database was imported.
    #[non_exhaustive]
    DatabaseImported {
        /// The new database.
        name: String,
        /// The source file or directory.
        source: PathBuf,
        /// The format (`snapshot`, `store`, `graphson`, `graphml`).
        format: String,
        /// Its state afterwards (`unloaded`, `needs-model ...`).
        state: String,
    },
    /// A backup was restored.
    #[non_exhaustive]
    DatabaseRestored {
        /// The database the backup belongs to.
        source: String,
        /// The backup's name.
        backup: String,
        /// The database restored into.
        name: String,
        /// Whether it replaced an existing database (moved to `trash/`).
        replaced: bool,
    },
    /// A database became a base.
    #[non_exhaustive]
    DatabasePromoted {
        /// Its name.
        name: String,
    },
    /// A backup was written.
    #[non_exhaustive]
    DatabaseBackedUp {
        /// The database.
        name: String,
        /// The ZIP file.
        path: PathBuf,
        /// Its size.
        bytes: u64,
    },
    /// A backup of the whole server (its configuration, and with databases their data) was
    /// written ([`crate::server_backup`]).
    #[non_exhaustive]
    ServerBackedUp {
        /// Whether the databases' data is in it.
        with_databases: bool,
        /// How many databases' data it holds.
        databases: usize,
        /// The file (the console), `None` for a download of the admin API.
        path: Option<PathBuf>,
        /// Its size.
        bytes: u64,
    },
    /// A database's read-only flag was set.
    #[non_exhaustive]
    ReadOnlySet {
        /// The database.
        name: String,
        /// The new value.
        on: bool,
    },
    /// A database's auto-load flag was set.
    #[non_exhaustive]
    AutoLoadSet {
        /// The database.
        name: String,
        /// The new value.
        on: bool,
    },
    /// A database's description was changed (round 3 B7).
    #[non_exhaustive]
    DescriptionSet {
        /// The database.
        name: String,
        /// The new description (empty: cleared).
        description: String,
    },
    /// A query limit override of a database was set or cleared (round 3 B7; it applies to the
    /// database's new runs at once).
    #[non_exhaustive]
    QueryLimitSet {
        /// The database.
        name: String,
        /// The key of `[limits.query]`.
        key: String,
        /// The new value (`None`: cleared, the server's default applies).
        value: Option<u64>,
    },
    /// An alias was set (new or re-pointed).
    #[non_exhaustive]
    AliasSet {
        /// The alias.
        alias: String,
        /// Its database.
        target: String,
    },
    /// An alias was removed.
    #[non_exhaustive]
    AliasRemoved {
        /// The alias.
        alias: String,
    },
    /// MCP was switched on or off for the whole server (`mcp.toml` `enabled`).
    #[non_exhaustive]
    McpSwitched {
        /// The new value.
        on: bool,
    },
    /// Whether new databases and aliases are offered to MCP was changed (`mcp.toml`
    /// `expose_new`).
    #[non_exhaustive]
    McpExposeNewSet {
        /// The new value.
        on: bool,
    },
    /// A key of `server.toml` was changed through the admin API or the console (round 4
    /// B-SETTINGS, [`crate::server_settings`]).
    #[non_exhaustive]
    ConfigSet {
        /// The dotted key (`limits.upload_bytes`).
        key: String,
        /// The new value as JSON (`null`: back to its default).
        value: String,
    },
    /// A database's (or alias's) MCP settings were changed (`mcp.toml`).
    #[non_exhaustive]
    McpDatabaseSet {
        /// The database or alias.
        name: String,
        /// Whether MCP offers it.
        exposed: bool,
        /// Whether MCP serves it read-only.
        read_only: bool,
    },
    /// An alias was renamed.
    #[non_exhaustive]
    AliasRenamed {
        /// The old name.
        old: String,
        /// The new name.
        new: String,
        /// Its database.
        target: String,
        /// The MCP sessions moved to the new name.
        moved: usize,
    },
    /// A user was added (with a first token).
    #[non_exhaustive]
    UserAdded {
        /// The user.
        name: String,
        /// Its roles.
        roles: Vec<String>,
        /// The last four characters of its token.
        token_last4: String,
    },
    /// The first admin of a server directory without `access.toml` was created.
    #[non_exhaustive]
    FirstAdminCreated {
        /// The user.
        name: String,
        /// The last four characters of its token.
        token_last4: String,
    },
    /// A user was removed (with its grants and browser sessions).
    #[non_exhaustive]
    UserRemoved {
        /// The user.
        name: String,
    },
    /// A user's roles were set.
    #[non_exhaustive]
    RolesSet {
        /// The user.
        name: String,
        /// The new roles.
        roles: Vec<String>,
    },
    /// A user got a new token (the old one stops working, its browser sessions end).
    #[non_exhaustive]
    TokenIssued {
        /// The user.
        name: String,
        /// The last four characters of the new token.
        token_last4: String,
    },
    /// A user's token was revoked (no token now; its browser sessions end).
    #[non_exhaustive]
    TokenRevoked {
        /// The user.
        name: String,
    },
    /// A grant was set (new or changed level).
    #[non_exhaustive]
    GrantSet {
        /// The subject (`user:<name>`, `anonymous`, ...).
        subject: String,
        /// The database or `*`.
        database: String,
        /// `read` or `write`.
        level: String,
    },
    /// A grant was removed.
    #[non_exhaustive]
    GrantRevoked {
        /// The subject.
        subject: String,
        /// The database or `*`.
        database: String,
    },
    /// The grants on an alias moved with its rename to the new name.
    #[non_exhaustive]
    GrantsMoved {
        /// The alias's old name.
        from: String,
        /// Its new name.
        to: String,
        /// How many grants moved.
        grants: usize,
    },
    /// A browser login succeeded.
    #[non_exhaustive]
    LoginSucceeded {
        /// The principal's id.
        principal: String,
    },
    /// A browser login was refused (an unknown token).
    #[non_exhaustive]
    LoginFailed {
        /// Why.
        reason: String,
    },
    /// A browser session ended by logout.
    #[non_exhaustive]
    LoggedOut {
        /// The principal's id.
        principal: String,
    },
    /// The server was asked to stop through its admin API (`graphersal-server stop`,
    /// `POST /api/admin/shutdown`).
    #[non_exhaustive]
    ShutdownRequested {},
    /// The server found at its start that the last server did not stop cleanly (round 3
    /// B-CRASH, [`crate::recovery`]): the databases it checks.
    #[non_exhaustive]
    UncleanShutdown {
        /// The process id of the server that did not stop cleanly, when known.
        pid: Option<u32>,
        /// The databases checked.
        databases: Vec<String>,
    },
    /// A database's Store was checked: after an unclean shutdown, or by `db verify`
    /// (round 3 B-CRASH).
    #[non_exhaustive]
    DatabaseChecked {
        /// The database.
        name: String,
        /// The outcome (`ok`, `damaged`, `failed`, ...).
        outcome: String,
        /// What the open and the verify found, in words.
        detail: String,
    },
    /// A database was repaired into a new database (round 3 B-CRASH, the engine's
    /// `Store::repair_to`; the damaged one is unchanged).
    #[non_exhaustive]
    DatabaseRepaired {
        /// The damaged database.
        source: String,
        /// The new, repaired database.
        name: String,
        /// Whether the repair lost no data of the readable state.
        lossless: bool,
    },
    /// A database took the place of another (round 3 B-CRASH `db swap`: a repaired copy
    /// replaces the damaged database, which went to `trash/`).
    #[non_exhaustive]
    DatabaseSwapped {
        /// The name kept.
        name: String,
        /// The database that took that name (its own name is gone).
        with: String,
        /// Where the replaced database went.
        trash: PathBuf,
    },
    /// A layer database was reset (round 4 B-LAYER `db reset-layer`): its changes over its
    /// base were discarded in one commit of its own Store (its history keeps the state
    /// before).
    #[non_exhaustive]
    LayerReset {
        /// The layer database.
        name: String,
        /// Its base database.
        base: String,
    },
    /// A model download from Hugging Face started (S1, `models download`).
    #[non_exhaustive]
    ModelDownloadStarted {
        /// The repository.
        repo: String,
        /// The revision asked for.
        revision: String,
    },
    /// A model download ended with the model in `models/`.
    #[non_exhaustive]
    ModelDownloaded {
        /// The repository.
        repo: String,
        /// The revision asked for.
        revision: String,
        /// The model identity (`model2vec/<repo>@<12 hex>`).
        identity: String,
        /// The licence the repository states (`unknown` when it states none).
        license: String,
    },
    /// A model download failed or was cancelled; nothing was kept.
    #[non_exhaustive]
    ModelDownloadFailed {
        /// The repository.
        repo: String,
        /// The revision asked for.
        revision: String,
        /// Why (the error's text).
        reason: String,
    },
}

impl AuditEvent {
    /// Whether the event is a refusal (logged as a warning).
    pub fn is_refusal(&self) -> bool {
        matches!(self, AuditEvent::LoginFailed { .. })
    }
}

impl fmt::Display for AuditEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuditEvent::DatabaseCreated { name, detail } => {
                write!(f, "database '{name}' created ({detail})")
            }
            AuditEvent::DatabaseDeleted { name, trash } => {
                write!(f, "database '{name}' deleted into {}", trash.display())
            }
            AuditEvent::DatabaseCloned { source, name } => {
                write!(f, "database '{source}' cloned into '{name}'")
            }
            AuditEvent::DatabaseImported {
                name,
                source,
                format,
                state,
            } => write!(
                f,
                "database '{name}' imported from {} ({format}, {state})",
                source.display()
            ),
            AuditEvent::DatabaseRestored {
                source,
                backup,
                name,
                replaced,
            } => write!(
                f,
                "backup {backup} of '{source}' restored into '{name}'{}",
                if *replaced { " (replaced)" } else { "" }
            ),
            AuditEvent::DatabasePromoted { name } => {
                write!(
                    f,
                    "database '{name}' promoted to a base (read-only, frozen)"
                )
            }
            AuditEvent::DatabaseBackedUp { name, path, bytes } => write!(
                f,
                "database '{name}' backed up into {} ({bytes} bytes)",
                path.display()
            ),
            AuditEvent::ServerBackedUp {
                with_databases,
                databases,
                path,
                bytes,
            } => {
                let what = if *with_databases {
                    format!("with {databases} database(s)")
                } else {
                    "configuration only".to_string()
                };
                match path {
                    Some(path) => write!(
                        f,
                        "server backed up ({what}) into {} ({bytes} bytes)",
                        path.display()
                    ),
                    None => write!(f, "server backup ({what}, {bytes} bytes) downloaded"),
                }
            }
            AuditEvent::ReadOnlySet { name, on } => {
                write!(f, "database '{name}' read-only {}", on_off(*on))
            }
            AuditEvent::AutoLoadSet { name, on } => {
                write!(f, "database '{name}' auto-load {}", on_off(*on))
            }
            AuditEvent::DescriptionSet { name, description } if description.is_empty() => {
                write!(f, "database '{name}' description cleared")
            }
            AuditEvent::DescriptionSet { name, description } => {
                write!(f, "database '{name}' description set to {description:?}")
            }
            AuditEvent::QueryLimitSet {
                name,
                key,
                value: Some(value),
            } => write!(
                f,
                "database '{name}' query limit {key} = {value} (applies to its new runs)"
            ),
            AuditEvent::QueryLimitSet {
                name,
                key,
                value: None,
            } => write!(
                f,
                "database '{name}' query limit {key} cleared (the server's default applies to \
                 its new runs)"
            ),
            AuditEvent::AliasSet { alias, target } => {
                write!(f, "alias '{alias}' set to database '{target}'")
            }
            AuditEvent::AliasRemoved { alias } => write!(f, "alias '{alias}' removed"),
            AuditEvent::McpSwitched { on } => write!(f, "MCP {}", on_off(*on)),
            AuditEvent::McpExposeNewSet { on } => write!(
                f,
                "MCP exposure of new databases and aliases {}",
                on_off(*on)
            ),
            AuditEvent::ConfigSet { key, value } => {
                write!(f, "server.toml: {key} = {value}")
            }
            AuditEvent::McpDatabaseSet {
                name,
                exposed,
                read_only,
            } => write!(
                f,
                "MCP for '{name}': exposed {}, read-only {}",
                on_off(*exposed),
                on_off(*read_only)
            ),
            AuditEvent::AliasRenamed {
                old,
                new,
                target,
                moved,
            } => write!(
                f,
                "alias '{old}' renamed to '{new}' (database '{target}', {moved} MCP session(s) \
                 moved)"
            ),
            AuditEvent::UserAdded {
                name,
                roles,
                token_last4,
            } => write!(
                f,
                "user '{name}' added (roles [{}], token …{token_last4})",
                roles.join(", ")
            ),
            AuditEvent::FirstAdminCreated { name, token_last4 } => write!(
                f,
                "first admin '{name}' created (token …{token_last4}, printed once)"
            ),
            AuditEvent::UserRemoved { name } => write!(f, "user '{name}' removed"),
            AuditEvent::RolesSet { name, roles } => {
                write!(f, "user '{name}' roles set to [{}]", roles.join(", "))
            }
            AuditEvent::TokenIssued { name, token_last4 } => {
                write!(f, "user '{name}' got a new token …{token_last4}")
            }
            AuditEvent::TokenRevoked { name } => write!(f, "user '{name}' token revoked"),
            AuditEvent::GrantSet {
                subject,
                database,
                level,
            } => write!(f, "grant {subject} {level} on '{database}'"),
            AuditEvent::GrantRevoked { subject, database } => {
                write!(f, "grant of {subject} on '{database}' revoked")
            }
            AuditEvent::GrantsMoved { from, to, grants } => {
                write!(f, "{grants} grant(s) moved from alias '{from}' to '{to}'")
            }
            AuditEvent::LoginSucceeded { principal } => write!(f, "login of '{principal}'"),
            AuditEvent::LoginFailed { reason } => write!(f, "login refused: {reason}"),
            AuditEvent::LoggedOut { principal } => write!(f, "logout of '{principal}'"),
            AuditEvent::ShutdownRequested {} => write!(f, "server shutdown requested"),
            AuditEvent::UncleanShutdown { pid, databases } => write!(
                f,
                "unclean shutdown of the last server{} found; checking [{}]",
                pid.map(|pid| format!(" (pid {pid})")).unwrap_or_default(),
                databases.join(", ")
            ),
            AuditEvent::DatabaseChecked {
                name,
                outcome,
                detail,
            } => write!(f, "database '{name}' checked: {outcome} ({detail})"),
            AuditEvent::DatabaseRepaired {
                source,
                name,
                lossless,
            } => write!(
                f,
                "database '{source}' repaired into '{name}' ({})",
                if *lossless {
                    "lossless"
                } else {
                    "DATA LOST, see the repair report"
                }
            ),
            AuditEvent::DatabaseSwapped { name, with, trash } => write!(
                f,
                "database '{with}' took the name '{name}'; the old '{name}' moved to {}",
                trash.display()
            ),
            AuditEvent::ModelDownloadStarted { repo, revision } => {
                write!(f, "model download of {repo} ({revision}) started")
            }
            AuditEvent::ModelDownloaded {
                repo,
                revision,
                identity,
                license,
            } => write!(
                f,
                "model {repo} ({revision}) downloaded into models/: {identity}, licence {license}"
            ),
            AuditEvent::ModelDownloadFailed {
                repo,
                revision,
                reason,
            } => write!(f, "model download of {repo} ({revision}) failed: {reason}"),
            AuditEvent::LayerReset { name, base } => write!(
                f,
                "layer database '{name}' reset to its base '{base}' (its Store keeps the history \
                 before the reset)"
            ),
        }
    }
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// One recorded event: who did it and what.
///
/// `#[non_exhaustive]`: further fields (a time, a request id) may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AuditRecord {
    /// The actor's identity (`user:<name>`, `operator`); `None` when unknown (the console on
    /// the directory, the server itself) or anonymous.
    pub actor: Option<String>,
    /// The OS user who acted, for a record of the operator (`None` for every other actor).
    pub os_user: Option<String>,
    /// The event.
    pub event: AuditEvent,
}

impl AuditRecord {
    /// The actor as the log line shows it: `user:<name>`, `operator (os:<name>)` (`operator`
    /// when the OS user is unknown), `-` for no actor.
    pub fn actor_text(&self) -> String {
        match (&self.actor, &self.os_user) {
            (Some(actor), Some(os)) => format!("{actor} (os:{os})"),
            (Some(actor), None) => actor.clone(),
            (None, _) => "-".to_string(),
        }
    }
}

/// Where audit records go.
pub trait AuditSink: Send + Sync {
    /// Records `record` (never fails: a sink that cannot write logs that itself).
    fn record(&self, record: &AuditRecord);
}

/// The audit sink of this version: one log line per event on the target
/// `graphersal_server::audit` (`info`, a refusal `warn`).
#[derive(Debug, Clone, Copy, Default)]
pub struct TracingAudit;

impl AuditSink for TracingAudit {
    fn record(&self, record: &AuditRecord) {
        let actor = record.actor_text();
        let actor = actor.as_str();
        if record.event.is_refusal() {
            tracing::warn!(target: "graphersal_server::audit", actor, "{}", record.event);
        } else {
            tracing::info!(target: "graphersal_server::audit", actor, "{}", record.event);
        }
    }
}

/// A handle that records events into a sink as one actor (cheap to clone).
#[derive(Clone)]
pub struct Audit {
    sink: Arc<dyn AuditSink>,
    actor: Option<String>,
    os_user: Option<String>,
}

impl Default for Audit {
    /// The [`TracingAudit`] sink, no actor.
    fn default() -> Self {
        Self::new(Arc::new(TracingAudit))
    }
}

impl fmt::Debug for Audit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Audit")
            .field("actor", &self.actor)
            .field("os_user", &self.os_user)
            .finish()
    }
}

impl Audit {
    /// Records into `sink`, no actor.
    pub fn new(sink: Arc<dyn AuditSink>) -> Self {
        Self {
            sink,
            actor: None,
            os_user: None,
        }
    }

    /// The same sink, recording as `actor` (a principal's identity; `None` = unknown or
    /// anonymous); the OS user is kept.
    pub fn with_actor(&self, actor: Option<String>) -> Self {
        Self {
            sink: self.sink.clone(),
            actor,
            os_user: self.os_user.clone(),
        }
    }

    /// The same handle naming `os_user` as the OS user behind the operator: records whose
    /// actor is the operator carry it ([`AuditRecord::os_user`]); `None` = unknown. The server
    /// passes its own process's user (only the directory's owner can read the operator token),
    /// the console on the directory its own ([`crate::security::os_user`]).
    pub fn with_os_user(&self, os_user: Option<String>) -> Self {
        Self {
            sink: self.sink.clone(),
            actor: self.actor.clone(),
            os_user,
        }
    }

    /// The actor events are recorded as.
    pub fn actor(&self) -> Option<&str> {
        self.actor.as_deref()
    }

    /// Records `event`.
    pub fn record(&self, event: AuditEvent) {
        let operator = self.actor.as_deref() == Some(crate::access::OPERATOR);
        self.sink.record(&AuditRecord {
            actor: self.actor.clone(),
            os_user: self.os_user.clone().filter(|_| operator),
            event,
        });
    }
}

/// A sink that keeps every record in memory (tests).
#[cfg(test)]
#[derive(Default)]
pub(crate) struct MemoryAudit(pub(crate) std::sync::Mutex<Vec<AuditRecord>>);

#[cfg(test)]
impl AuditSink for MemoryAudit {
    fn record(&self, record: &AuditRecord) {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(record.clone());
    }
}

#[cfg(test)]
impl MemoryAudit {
    /// The events recorded so far, as text with the actor.
    pub(crate) fn lines(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|r| format!("{}: {}", r.actor_text(), r.event))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{Admin, CreateSpec};
    use crate::test_support::TempDir;

    #[test]
    fn the_admin_operations_record_their_events() {
        let temp = TempDir::new("audit-admin");
        let dir = crate::layout::ServerDir::init(temp.path().join("srv"))
            .unwrap()
            .dir;
        let memory = Arc::new(MemoryAudit::default());
        let (admin, _lock) = Admin::open_offline(&dir).unwrap();
        let admin =
            admin.with_audit(Audit::new(memory.clone()).with_actor(Some("user:alice".to_string())));
        admin.create("acme", &CreateSpec::plain()).unwrap();
        admin.clone_database("acme", "copy").unwrap();
        admin.set_read_only("copy", true).unwrap();
        admin.set_auto_load("acme", true).unwrap();
        admin.alias_set("prod", "ACME").unwrap();
        admin.alias_rename("prod", "live").unwrap();
        admin.alias_remove("live").unwrap();
        let backup = admin.backup("acme").unwrap();
        let deleted = admin.delete("copy").unwrap();
        // A refused operation records nothing.
        assert!(admin.alias_remove("live").is_err());
        assert_eq!(
            memory.lines(),
            [
                "user:alice: database 'acme' created (plain)".to_string(),
                "user:alice: database 'acme' cloned into 'copy'".to_string(),
                "user:alice: database 'copy' read-only on".to_string(),
                "user:alice: database 'acme' auto-load on".to_string(),
                "user:alice: alias 'prod' set to database 'acme'".to_string(),
                "user:alice: alias 'prod' renamed to 'live' (database 'acme', 0 MCP session(s) \
                 moved)"
                    .to_string(),
                "user:alice: alias 'live' removed".to_string(),
                format!(
                    "user:alice: database 'acme' backed up into {} ({} bytes)",
                    backup.file.display(),
                    backup.bytes
                ),
                format!(
                    "user:alice: database 'copy' deleted into {}",
                    deleted.trash.display()
                ),
            ]
        );
    }

    #[test]
    fn operator_records_name_the_os_user() {
        let memory = Arc::new(MemoryAudit::default());
        let audit = Audit::new(memory.clone()).with_os_user(Some("alice".to_string()));
        let event = || AuditEvent::AliasRemoved {
            alias: "prod".to_string(),
        };
        audit
            .with_actor(Some(crate::access::OPERATOR.to_string()))
            .record(event());
        // Only the operator's records carry the OS user: a user's or the server's own do not.
        audit
            .with_actor(Some("user:bob".to_string()))
            .record(event());
        audit.record(event());
        // An operator whose OS user is unknown.
        Audit::new(memory.clone())
            .with_actor(Some(crate::access::OPERATOR.to_string()))
            .record(event());
        assert_eq!(
            memory.lines(),
            [
                "operator (os:alice): alias 'prod' removed",
                "user:bob: alias 'prod' removed",
                "-: alias 'prod' removed",
                "operator: alias 'prod' removed",
            ]
        );
        let records = memory.0.lock().unwrap();
        assert_eq!(records[0].os_user.as_deref(), Some("alice"));
        assert_eq!(records[1].os_user, None);
    }

    #[test]
    fn records_carry_the_actor_and_the_event() {
        let memory = Arc::new(MemoryAudit::default());
        let audit = Audit::new(memory.clone());
        audit.record(AuditEvent::AliasRemoved {
            alias: "prod".to_string(),
        });
        audit
            .with_actor(Some("user:alice".to_string()))
            .record(AuditEvent::GrantSet {
                subject: "user:bob".to_string(),
                database: "acme".to_string(),
                level: "read".to_string(),
            });
        assert_eq!(
            memory.lines(),
            [
                "-: alias 'prod' removed",
                "user:alice: grant user:bob read on 'acme'"
            ]
        );
        // The tracing sink accepts every event (no subscriber needed).
        Audit::default().record(AuditEvent::LoginFailed {
            reason: "unknown token".to_string(),
        });
    }
}
