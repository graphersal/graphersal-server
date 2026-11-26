//! Reset a layer database (round 4 B-LAYER, S1: through the engine; `db reset-layer`): discard
//! every change the layer made over its base, base untouched.
//!
//! The reset is the engine's `reset_layer` (graphersal 0.9.0) on the layer database's OWN
//! Store: one journaled commit, so the Store, its name and its history stay, and `store
//! rollback` (or a backup) can go back before the reset. Kept: the database's name, kind and
//! base, `database.toml` (description, read-only and auto-load flags, query limit overrides),
//! the grants (also those via aliases), the aliases, the MCP settings (all keyed by the name),
//! the history, and the layer's scheduled job definitions: the engine's reset brings the
//! catalog back to the base's, so the same unit removes the base's jobs again and sets the
//! layer's own (a crash leaves the layer either as it was or reset with its jobs, never in
//! between). Discarded: the layer's data changes and its own schema and other catalog changes
//! (saved queries, index definitions: back to the base's).
//!
//! Online, a loaded layer is reset in place on its open Store (no unload: its pages and MCP
//! sessions see the base's data at their next request; a query already running finishes
//! first, the reset waits for the graph's write lock at most [`LOCK_WAIT`]); an unloaded one
//! (also offline, the console holding the server lock) by opening its layer Store directly,
//! under the loader's idle check ([`crate::Loader`] `while_idle`) so no load runs meanwhile.
//! Audited as [`AuditEvent::LayerReset`].

use std::time::{Duration, Instant};

use graphersal::catalog::{Definition, DefinitionKind};
use graphersal::persist::{Store, StoreOptions};
use graphersal::prelude::{Graph, GraphStorage, Transactional};
use graphersal::storage::{GraphError, LayerBase, LayerReset, LayeredGraph};

use super::{Admin, store_error};
use crate::audit::AuditEvent;
use crate::database::{DatabaseKind, DatabaseRole};
use crate::error::ServerError;
use crate::loader::OpenStoreRef;
use crate::registry::DatabaseEntry;

/// How long a reset of a loaded layer waits for the queries running on it.
const LOCK_WAIT: Duration = Duration::from_secs(30);

/// What [`Admin::reset_layer`] did.
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct LayerResetDone {
    /// The layer database (same name, same settings), now reading exactly its base.
    pub entry: DatabaseEntry,
    /// Its base database.
    pub base: String,
    /// The discarded changes: added, modified and removed `(vertices, edges)`.
    pub discarded: DiscardedChanges,
    /// Whether the layer's own schema was discarded (it is the base's again).
    pub schema: bool,
    /// How many catalog definitions differed from the base's and are the base's again (before
    /// the layer's own jobs are set back).
    pub definitions: usize,
    /// How many scheduled job definitions the layer kept.
    pub jobs_kept: usize,
    /// Whether it was reset while loaded (in place, on its open Store).
    pub loaded: bool,
}

/// The counts of the changes a reset discarded.
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct DiscardedChanges {
    /// Elements the layer created: `(vertices, edges)`.
    pub added: (u64, u64),
    /// Base elements the layer changed.
    pub modified: (u64, u64),
    /// Base elements the layer removed.
    pub removed: (u64, u64),
}

impl DiscardedChanges {
    /// Whether the layer had no data change at all.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    fn of(reset: &LayerReset) -> Self {
        Self {
            added: (reset.added.vertices, reset.added.edges),
            modified: (reset.modified.vertices, reset.modified.edges),
            removed: (reset.removed.vertices, reset.removed.edges),
        }
    }
}

impl Admin {
    /// Resets the layer database `name` (module docs).
    ///
    /// `Err`: [`ServerError::NotFound`]; [`ServerError::Refused`] for a plain or base
    /// database, a failed one, one being loaded or unloaded, or a loaded one whose queries do
    /// not end within 30 s; [`ServerError::Store`] when the layer Store cannot be
    /// opened or written (a damaged layer: restore a backup, or delete and create it; nothing
    /// changed); [`ServerError::Io`].
    pub fn reset_layer(&self, name: &str) -> Result<LayerResetDone, ServerError> {
        let entry = self.registry.get(name)?;
        let key = entry.name.clone();
        let base_name = reset_refusal(&entry)?;
        let open = self.loader.as_ref().and_then(|l| l.open_store_of(&key));
        let (reset, jobs_kept, loaded) = match open {
            Some((_context, OpenStoreRef::Layer(store))) => {
                // The context is held: an unload waits for this request.
                let (reset, jobs) = reset_graph(&key, store.graph(), LOCK_WAIT)?;
                (reset, jobs, true)
            }
            Some((_context, OpenStoreRef::Plain(_))) => {
                return Err(ServerError::Refused {
                    name: key,
                    message: "it is served as a plain database; only a layer can be reset"
                        .to_string(),
                });
            }
            None => {
                // The base before the loader's lock (its pool is read under that lock); a base
                // is frozen, so it is the same base inside.
                let base = self.base_of(&base_name)?;
                let reset = || self.reset_unloaded(&key, &base);
                let (reset, jobs) = match &self.loader {
                    Some(loader) => loader.while_idle(&key, reset)?,
                    None => reset()?,
                };
                (reset, jobs, false)
            }
        };
        self.audit.record(AuditEvent::LayerReset {
            name: key.clone(),
            base: base_name.clone(),
        });
        Ok(LayerResetDone {
            entry: self.registry.get(&key)?,
            base: base_name,
            discarded: DiscardedChanges::of(&reset),
            schema: reset.schema,
            definitions: reset.definitions,
            jobs_kept,
            loaded,
        })
    }

    /// The reset of a layer nothing serves: its layer Store opened for writing (its own lock
    /// refuses a second opener), reset, closed.
    fn reset_unloaded(
        &self,
        key: &str,
        base: &LayerBase,
    ) -> Result<(LayerReset, usize), ServerError> {
        let entry = self.registry.get(key)?;
        // Checked again: the database may have changed before the loader's lock was taken.
        reset_refusal(&entry)?;
        let store = Store::<LayeredGraph>::open_layer(entry.store_dir(), base, StoreOptions::new())
            .map_err(|err| unreadable(key, &err))?;
        let outcome = reset_graph(key, store.graph(), LOCK_WAIT);
        let closed = store
            .close()
            .map_err(|err| store_error(key, &format!("cannot close the layer Store: {err}")));
        let outcome = outcome?;
        closed?;
        Ok(outcome)
    }
}

/// The engine's reset of `graph` plus the layer's own job definitions set back, in ONE unit
/// (module docs): `(the engine's report, jobs kept)`. Waits for the write lock at most `wait`.
pub(super) fn reset_graph(
    key: &str,
    graph: &Graph<LayeredGraph>,
    wait: Duration,
) -> Result<(LayerReset, usize), ServerError> {
    let deadline = Instant::now() + wait;
    let mut guard = loop {
        match graph.try_write_checked() {
            Ok(Some(guard)) => break guard,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                return Err(ServerError::Refused {
                    name: key.to_string(),
                    message: format!(
                        "a query still runs on it after {} s: stop it (GET \
                         /api/admin/databases/{key}/runs, then .../runs/cancel) and reset again",
                        wait.as_secs()
                    ),
                });
            }
            Err(err) => return Err(unreadable(key, &err)),
        }
    };
    guard
        .transaction(|layer| -> Result<(LayerReset, usize), GraphError> {
            let jobs = job_definitions(layer)?;
            let reset = layer.reset_layer()?;
            // The catalog is the base's now: its jobs give way to the layer's own set.
            let now = job_definitions(layer)?;
            if now != jobs {
                let names: Vec<(DefinitionKind, &str)> = now
                    .iter()
                    .map(|d| (DefinitionKind::JOB, d.name()))
                    .collect();
                if !names.is_empty() {
                    layer.remove_definitions(&names)?;
                }
                if !jobs.is_empty() {
                    layer.set_definitions(jobs.clone())?;
                }
            }
            Ok((reset, jobs.len()))
        })
        .map_err(|err| store_error(key, &format!("cannot reset the layer: {err}")))
}

/// The scheduled job definitions of `layer`'s catalog.
fn job_definitions(layer: &LayeredGraph) -> Result<Vec<Definition>, GraphError> {
    Ok(layer
        .definitions()?
        .filter(|d| d.kind() == DefinitionKind::JOB)
        .cloned()
        .collect())
}

/// The refusal of a layer Store that cannot be opened or locked.
fn unreadable(key: &str, err: &dyn std::fmt::Display) -> ServerError {
    store_error(
        key,
        &format!(
            "cannot open the layer to reset it: {err}; restore a backup instead, or delete the \
             database and create the layer again"
        ),
    )
}

/// Why `entry` cannot be reset (the refusal), else its base's name.
fn reset_refusal(entry: &DatabaseEntry) -> Result<String, ServerError> {
    let refuse = |message: String| ServerError::Refused {
        name: entry.name.clone(),
        message,
    };
    let Some(config) = entry.config.as_ref() else {
        return Err(refuse(format!(
            "it is failed ({}); only a working layer database can be reset",
            entry.state
        )));
    };
    match (config.kind, config.role) {
        (DatabaseKind::Layer, _) => config.base.clone().ok_or_else(|| {
            refuse("its database.toml names no base; it cannot be reset".to_string())
        }),
        (DatabaseKind::Plain, DatabaseRole::Base) => Err(refuse(
            "it is a base database: a base is frozen and has no changes of its own to discard; \
             reset the layers over it instead"
                .to_string(),
        )),
        (DatabaseKind::Plain, _) => Err(refuse(
            "it is a plain database, not a layer: only a layer's changes over its base can be \
             discarded; to go back, restore a backup or roll its Store back"
                .to_string(),
        )),
    }
}
