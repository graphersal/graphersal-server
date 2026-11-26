//! A database's settings as admin operations (round 3 B7; [`crate::settings`]): one change of
//! its description, read-only and auto-load flags and query limit overrides, based on its
//! revision ([`Admin::expecting`]) and audited field by field; and what the settings view reads
//! besides the registry: the limits in effect, the server's default limits and the Store's
//! creation parameters.

use graphersal::persist::Store;

use crate::audit::AuditEvent;
use crate::database::{DatabaseRole, utc_text};
use crate::error::ServerError;
use crate::limits::QueryLimits;
use crate::registry::DatabaseEntry;
use crate::settings::SettingsChange;

use super::Admin;

/// What the engine's [`Store::inspect`] says about a database's Store: its creation parameters
/// (fixed for the Store's life) and its position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoreFacts {
    /// The Store's identity (32 hex digits).
    pub(crate) store_id: String,
    /// The current lineage id (32 hex digits).
    pub(crate) graph_id: String,
    /// What the Store does on damage found while open (`maintenance` | `continue`).
    pub(crate) damage_policy: String,
    /// The snapshot chunk size in bytes.
    pub(crate) chunk_bytes: u32,
    /// When the Store was created (RFC 3339 UTC; `None`: no time recorded).
    pub(crate) created_at: Option<String>,
    /// The format version of its directory.
    pub(crate) format_version: u32,
    /// The storage backend: `directory` (every database's `store/`) or `single-file`.
    pub(crate) backend: &'static str,
    /// The last commit (`0`: none).
    pub(crate) commit_seq: u64,
    /// Its time (RFC 3339 UTC; `None`: no commit yet).
    pub(crate) last_commit_at: Option<String>,
    /// A layer Store's base identity (as text); `None` for a plain Store.
    pub(crate) layer_base: Option<String>,
    /// The number of snapshots.
    pub(crate) snapshots: usize,
    /// The WAL's size in bytes.
    pub(crate) wal_bytes: u64,
    /// Whether the last close was clean (`false` while a writer has it open).
    pub(crate) clean: bool,
}

/// Microseconds since the Unix epoch as RFC 3339 UTC; `None` for `0` or less.
fn micros_text(micros: i64) -> Option<String> {
    u64::try_from(micros)
        .ok()
        .filter(|m| *m > 0)
        .map(|m| utc_text(m / 1_000_000))
}

impl Admin {
    /// Changes the settings of the database `name` (module docs of [`crate::settings`]): online
    /// through the loader (a read-only change flips a loaded context at once), offline on the
    /// file (a base refuses read-only off). Every field changed is audited. Takes the real
    /// database name (not an alias).
    ///
    /// `Err`: [`ServerError::InvalidInput`] (nothing to change, a bad description or limit),
    /// [`ServerError::NotFound`], [`ServerError::DatabaseFailed`], [`ServerError::Conflict`]
    /// (a stale revision), [`ServerError::Refused`] (read-only off for a base),
    /// [`ServerError::Io`], [`ServerError::Serve`]; nothing changed but for `Serve` (the file
    /// was written, the loaded context refused the flip).
    pub(crate) fn change_settings(
        &self,
        name: &str,
        change: &SettingsChange,
    ) -> Result<DatabaseEntry, ServerError> {
        change.check(name)?;
        let entry = match &self.loader {
            Some(loader) => loader.change_settings(name, change, self.expected())?,
            None => {
                let entry = self.registry.get(name)?;
                if change.read_only == Some(false)
                    && entry
                        .config
                        .as_ref()
                        .is_some_and(|config| config.role == DatabaseRole::Base)
                {
                    return Err(ServerError::Refused {
                        name: entry.name,
                        message: "a base database is always served read-only (layers are made \
                                  over it)"
                            .to_string(),
                    });
                }
                self.registry
                    .change_settings(&entry.name, self.expected(), change)?
            }
        };
        let name = entry.name.clone();
        if let Some(description) = &change.description {
            self.audit.record(AuditEvent::DescriptionSet {
                name: name.clone(),
                description: description.clone(),
            });
        }
        if let Some(on) = change.read_only {
            self.audit.record(AuditEvent::ReadOnlySet {
                name: name.clone(),
                on,
            });
        }
        if let Some(on) = change.auto_load {
            self.audit.record(AuditEvent::AutoLoadSet {
                name: name.clone(),
                on,
            });
        }
        for (key, value) in &change.limits {
            self.audit.record(AuditEvent::QueryLimitSet {
                name: name.clone(),
                key: key.clone(),
                value: *value,
            });
        }
        Ok(entry)
    }

    /// The query limits the loaded database `name` runs under (what its last load read), `None`
    /// when it is not loaded (or offline).
    pub(crate) fn limits_in_effect(&self, name: &str) -> Option<QueryLimits> {
        self.loader.as_ref()?.limits_in_effect(name)
    }

    /// The server's default query limits: `server.toml`'s `[limits.query]`, read now (it has no
    /// runtime writer, so this is what the next load reads).
    ///
    /// `Err`: [`ServerError::Config`] / [`ServerError::Io`] for a `server.toml` that cannot be
    /// read.
    pub(crate) fn server_query_limits(&self) -> Result<QueryLimits, ServerError> {
        Ok(self.registry.dir().load_config()?.limits.query)
    }

    /// The Store facts of the database `entry` ([`StoreFacts`]), read from its files without a
    /// lock (works while it is loaded). `Err` with the reason when they cannot be read.
    pub(crate) fn store_facts(&self, entry: &DatabaseEntry) -> Result<StoreFacts, String> {
        let info = Store::inspect(entry.store_dir().as_path()).map_err(|err| err.to_string())?;
        Ok(StoreFacts {
            store_id: format!("{:032x}", info.store_id),
            graph_id: format!("{:032x}", info.graph_id),
            damage_policy: info.damage_policy.name().to_string(),
            chunk_bytes: info.chunk_bytes,
            created_at: micros_text(info.created_at),
            format_version: info.format_version,
            backend: if info.space.is_some() {
                "single-file"
            } else {
                "directory"
            },
            commit_seq: info.commit_seq,
            last_commit_at: micros_text(info.last_commit_at),
            layer_base: info.layer_base.map(|base| base.to_string()),
            snapshots: info.snapshots.len(),
            wal_bytes: info.wal_bytes,
            clean: info.clean,
        })
    }
}
