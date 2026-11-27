//! A database's settings as admin operations (round 3 B7; [`crate::settings`]): one change of
//! its description, read-only and auto-load flags and query limit overrides, based on its
//! revision ([`Admin::expecting`]) and audited field by field; and what the settings view reads
//! besides the registry: the limits in effect, the server's default limits and the Store's
//! creation parameters.

use std::sync::Arc;

use graphersal::persist::Store;

use crate::audit::AuditEvent;
use crate::config::ServerConfig;
use crate::database::{DatabaseRole, utc_text};
use crate::error::ServerError;
use crate::limits::QueryLimits;
use crate::registry::DatabaseEntry;
use crate::server_settings::{self, Edited, LiveConfig};
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

    /// The server's default query limits: `server.toml`'s `[limits.query]`, read now.
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

    /// The settings the running server applies (`None` offline).
    pub(crate) fn live_config(&self) -> Option<&Arc<LiveConfig>> {
        self.loader.as_ref().and_then(|loader| loader.live_config())
    }

    /// The server settings view (`GET /api/admin/config`, [`crate::server_settings`]): every
    /// key of `server.toml` with its value, default, class and restart-pending mark.
    ///
    /// `Err`: [`ServerError::Io`] / [`ServerError::Config`] for a file that cannot be read.
    pub(crate) fn server_settings(&self) -> Result<serde_json::Value, ServerError> {
        let path = self.registry.dir().config_path();
        let text = read_config(&path)?;
        let config = ServerConfig::parse(&text, &path)?;
        Ok(server_settings::view_json(
            &path,
            &text,
            &config,
            self.live_config().map(|live| live.as_ref()),
        ))
    }

    /// Changes keys of `server.toml` (`PUT /api/admin/config`, `config set`): `changes` (key,
    /// new TOML value or `None` = its default) as ONE write based on the file's revision
    /// ([`Admin::expecting`]), under the file's writer lock (online; offline the server lock
    /// the console holds); then the running server applies the `now` keys
    /// ([`LiveConfig::apply`]). Each changed key is audited (`ConfigSet`). Answers the edit and
    /// the keys applied now.
    ///
    /// `Err`: [`ServerError::InvalidInput`] (an unknown key, a value the parser refuses),
    /// [`ServerError::Conflict`] (a stale revision), [`ServerError::Io`] /
    /// [`ServerError::Config`].
    pub(crate) fn change_server_settings(
        &self,
        changes: &[(String, Option<toml::Value>)],
    ) -> Result<(Edited, Vec<&'static str>), ServerError> {
        let path = self.registry.dir().config_path();
        let live = self.live_config();
        let write = || -> Result<(Edited, Vec<&'static str>), ServerError> {
            let text = read_config(&path)?;
            crate::revision::check(
                self.expected(),
                &crate::revision::of(text.as_bytes()),
                crate::layout::CONFIG_FILE,
            )?;
            let edited = server_settings::edit(&text, &path, changes)?;
            if edited.text != text {
                crate::layout::write_atomic(&path, edited.text.as_bytes())?;
            }
            let applied = live
                .map(|live| live.apply(&edited.config))
                .unwrap_or_default();
            Ok((edited, applied))
        };
        let (edited, applied) = match live {
            Some(live) => live.writing(write)?,
            None => write()?,
        };
        // New query limits reach every loaded database at once (its next runs).
        if let Some(loader) = &self.loader
            && edited
                .changed
                .iter()
                .any(|key| key.starts_with("limits.query."))
            && let Err(err) = loader.apply_limits_everywhere()
        {
            tracing::warn!(
                target: "graphersal_server",
                "server.toml was changed, but a database keeps its query limits: {err}"
            );
        }
        for key in &edited.changed {
            let value =
                server_settings::value_of(&edited.config, key).unwrap_or(serde_json::Value::Null);
            self.audit().record(AuditEvent::ConfigSet {
                key: (*key).to_string(),
                value: value.to_string(),
            });
        }
        Ok((edited, applied))
    }
}

/// The text of `server.toml` at `path`.
fn read_config(path: &std::path::Path) -> Result<String, ServerError> {
    std::fs::read_to_string(path).map_err(|err| ServerError::io("read", path, err))
}
