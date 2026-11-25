//! The audit trail: every administrative event goes through ONE abstraction.
//!
//! An [`AuditEvent`] is typed (database create/delete/clone/import/restore/promote/backup,
//! the read-only and auto-load flags, aliases, users, tokens, grants, logins); an [`Audit`]
//! handle records it, with the actor (the principal's identity when known), into an
//! [`AuditSink`]. The one sink today is [`TracingAudit`]: a log line on the target
//! `graphersal_server::audit` (an audit journal can be another sink later; nothing else
//! changes). Events never carry a secret: a token shows only its last four characters.

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
            AuditEvent::ReadOnlySet { name, on } => {
                write!(f, "database '{name}' read-only {}", on_off(*on))
            }
            AuditEvent::AutoLoadSet { name, on } => {
                write!(f, "database '{name}' auto-load {}", on_off(*on))
            }
            AuditEvent::AliasSet { alias, target } => {
                write!(f, "alias '{alias}' set to database '{target}'")
            }
            AuditEvent::AliasRemoved { alias } => write!(f, "alias '{alias}' removed"),
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
            AuditEvent::LoginSucceeded { principal } => write!(f, "login of '{principal}'"),
            AuditEvent::LoginFailed { reason } => write!(f, "login refused: {reason}"),
            AuditEvent::LoggedOut { principal } => write!(f, "logout of '{principal}'"),
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
    /// The event.
    pub event: AuditEvent,
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
        let actor = record.actor.as_deref().unwrap_or("-");
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
}

impl Default for Audit {
    /// The [`TracingAudit`] sink, no actor.
    fn default() -> Self {
        Self::new(Arc::new(TracingAudit))
    }
}

impl fmt::Debug for Audit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Audit").field("actor", &self.actor).finish()
    }
}

impl Audit {
    /// Records into `sink`, no actor.
    pub fn new(sink: Arc<dyn AuditSink>) -> Self {
        Self { sink, actor: None }
    }

    /// The same sink, recording as `actor` (a principal's identity; `None` = unknown or
    /// anonymous).
    pub fn with_actor(&self, actor: Option<String>) -> Self {
        Self {
            sink: self.sink.clone(),
            actor,
        }
    }

    /// The actor events are recorded as.
    pub fn actor(&self) -> Option<&str> {
        self.actor.as_deref()
    }

    /// Records `event`.
    pub fn record(&self, event: AuditEvent) {
        self.sink.record(&AuditRecord {
            actor: self.actor.clone(),
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
            .map(|r| format!("{}: {}", r.actor.as_deref().unwrap_or("-"), r.event))
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
