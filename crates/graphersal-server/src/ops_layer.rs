//! Reset a layer database (round 4 B-LAYER, `db reset-layer`): discard every change the layer
//! made over its base, base untouched.
//!
//! graphersal 0.8.0 has no reset of a layer Store, so the server builds a NEW empty layer
//! Store over the same base and swaps it in ([`crate::Registry::reset_layer_store`]): the old
//! layer Store moves to `trash/<name>-<UTC time>/` (with a copy of `database.toml`, so the
//! trash entry is a whole database directory that can be put back by hand) and the new one
//! takes its place under the same name. Kept: the database's name, kind and base,
//! `database.toml` (description, read-only and auto-load flags, query limit overrides), the
//! grants (also those via aliases), the aliases, the MCP settings (all keyed by the name),
//! and the layer's scheduled job definitions (copied into the new layer). Discarded: the
//! layer's data changes and its own schema and other catalog changes (saved queries, index
//! definitions: back to the base's), its history (`store rollback` cannot go back past a
//! reset: the old Store is in `trash/`), its workload statistics and its last verify. The new
//! Store keeps the old one's creation parameters (chunk size, damage policy).
//!
//! Online, a loaded layer is unloaded first and loaded again afterwards (`reloaded`; the
//! answer waits for that load, at most a minute); the
//! work runs under the loader's idle check ([`crate::Loader`] `while_idle`), so no load,
//! delete, swap or restore of the database runs meanwhile. Offline (the console, server lock
//! held) it acts on the files. Audited as [`AuditEvent::LayerReset`].
//!
//! FOLLOW-UP: once a graphersal release has the engine's `reset_layer` (a journaled unit on the
//! open layer Store), call it instead: it keeps the Store and its history, so `store rollback`
//! can go back before the reset, and needs no unload.

use graphersal::catalog::{Definition, DefinitionKind};
use graphersal::persist::{RecoveryTarget, Store, StoreOptions};
use graphersal::prelude::GraphStorage;
use graphersal::storage::{LayerBase, LayeredGraph};

use super::{Admin, store_error};
use crate::audit::AuditEvent;
use crate::database::{DatabaseKind, DatabaseRole, RESET_STORE_DIR};
use crate::error::ServerError;
use crate::registry::DatabaseEntry;

/// How long a reset waits for the layer it loads again.
const RELOAD_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

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
    /// Where the old layer went (`trash/<name>-<UTC time>/`: `database.toml` + `store/`).
    pub trash: std::path::PathBuf,
    /// The discarded changes: added, modified and removed `(vertices, edges)`.
    pub discarded: DiscardedChanges,
    /// How many scheduled job definitions the new layer kept.
    pub jobs_kept: usize,
    /// Whether it was loaded again (it was loaded before).
    pub reloaded: bool,
}

/// The counts of the changes a reset discarded (the old layer's summary).
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
}

/// What the old layer Store carried over: its changes' counts, its job definitions and the
/// options a new Store is made with.
struct OldLayer {
    discarded: DiscardedChanges,
    jobs: Vec<Definition>,
    options: StoreOptions,
}

impl Admin {
    /// Resets the layer database `name` (module docs).
    ///
    /// `Err`: [`ServerError::NotFound`]; [`ServerError::Refused`] for a plain or base
    /// database, a failed one, while it cannot be unloaded, or while it is being loaded;
    /// [`ServerError::Store`] when the old layer cannot be read (a damaged layer: restore a
    /// backup, or delete and create it) or the new one cannot be written (nothing changed);
    /// [`ServerError::Io`].
    pub fn reset_layer(&self, name: &str) -> Result<LayerResetDone, ServerError> {
        let entry = self.registry.get(name)?;
        let key = entry.name.clone();
        let base_name = reset_refusal(&entry)?;
        let was_loaded = entry.state.is_open();
        if let Some(loader) = &self.loader
            && was_loaded
        {
            loader.unload(&key)?;
        }
        // The base before the loader's lock (its pool is read under that lock); a base is
        // frozen, so it is the same base inside.
        let base = match self.base_of(&base_name) {
            Ok(base) => base,
            Err(err) => {
                self.load_again(&key, was_loaded);
                return Err(err);
            }
        };
        let reset = || self.reset_files(&key, &base);
        let outcome = match &self.loader {
            Some(loader) => loader.while_idle(&key, reset),
            None => reset(),
        };
        let reloaded = self.load_again(&key, was_loaded);
        let (trash, old) = outcome?;
        self.audit.record(AuditEvent::LayerReset {
            name: key.clone(),
            base: base_name.clone(),
            trash: trash.clone(),
        });
        Ok(LayerResetDone {
            entry: self.registry.get(&key)?,
            base: base_name,
            trash,
            discarded: old.discarded,
            jobs_kept: old.jobs.len(),
            reloaded,
        })
    }

    /// Loads the database `key` again when it was loaded (also after a failed reset: the old
    /// layer is still in place then) and waits for the load (at most [`RELOAD_WAIT`]); a
    /// failure is a warning.
    fn load_again(&self, key: &str, was_loaded: bool) -> bool {
        let Some(loader) = self.loader.as_ref().filter(|_| was_loaded) else {
            return false;
        };
        match loader.load(key) {
            Ok(_) => {
                // The answer shows the state after the load (an empty layer loads quickly; a
                // slow base load answers `loading` after the bound).
                let _ = loader.wait(key, RELOAD_WAIT);
                true
            }
            Err(err) => {
                tracing::warn!(
                    target: "graphersal_server",
                    "database '{key}': cannot load it again after the layer reset: {err}"
                );
                false
            }
        }
    }

    /// The files' part (the database is idle): read the old layer, build the new one in
    /// `store-reset/`, swap it in.
    fn reset_files(
        &self,
        key: &str,
        base: &LayerBase,
    ) -> Result<(std::path::PathBuf, OldLayer), ServerError> {
        let entry = self.registry.get(key)?;
        // Checked again: the database may have changed before the loader's lock was taken.
        reset_refusal(&entry)?;
        let old = read_old_layer(&entry, base)?;
        let fresh = entry.dir.join(RESET_STORE_DIR);
        if fresh.exists() {
            std::fs::remove_dir_all(&fresh)
                .map_err(|err| ServerError::io("remove directory", &fresh, err))?;
        }
        if let Err(err) = build_empty_layer(&fresh, base, &old) {
            let _ = std::fs::remove_dir_all(&fresh);
            return Err(store_error(key, &err));
        }
        match self.registry.reset_layer_store(key) {
            Ok(trash) => Ok((trash, old)),
            Err(err) => {
                let _ = std::fs::remove_dir_all(&fresh);
                Err(err)
            }
        }
    }
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

/// Reads the old layer Store of `entry` (read-only, no lock: the database is idle): the
/// counts of its changes, its job definitions and its creation parameters.
fn read_old_layer(entry: &DatabaseEntry, base: &LayerBase) -> Result<OldLayer, ServerError> {
    let dir = entry.store_dir();
    let unreadable = |err: &dyn std::fmt::Display| {
        store_error(
            &entry.name,
            &format!(
                "cannot read the layer to reset it: {err}; restore a backup instead, or delete \
                 the database and create the layer again"
            ),
        )
    };
    let info = Store::inspect(dir.as_path()).map_err(|err| unreadable(&err))?;
    let options = StoreOptions::new()
        .with_chunk_bytes(info.chunk_bytes)
        .with_damage_policy(info.damage_policy);
    let graph =
        Store::<LayeredGraph>::open_layer_read_only(dir.as_path(), base, RecoveryTarget::Latest)
            .map_err(|err| unreadable(&err))?
            .into_graph();
    let graph = graph.read_checked().map_err(|err| unreadable(&err))?;
    let discarded = match graph.layer_summary().map_err(|err| unreadable(&err))? {
        Some(summary) => DiscardedChanges {
            added: (summary.added.vertices, summary.added.edges),
            modified: (summary.modified.vertices, summary.modified.edges),
            removed: (summary.removed.vertices, summary.removed.edges),
        },
        None => DiscardedChanges::default(),
    };
    let jobs = graph
        .definitions()
        .map_err(|err| unreadable(&err))?
        .filter(|d| d.kind() == DefinitionKind::JOB)
        .cloned()
        .collect();
    Ok(OldLayer {
        discarded,
        jobs,
        options,
    })
}

/// An empty layer over `base` in `dir` whose job definitions are exactly the old layer's,
/// closed again.
fn build_empty_layer(
    dir: &std::path::Path,
    base: &LayerBase,
    old: &OldLayer,
) -> Result<(), String> {
    let mut layer = LayeredGraph::over_base(base)
        .map_err(|err| format!("the base cannot carry a layer: {err}"))?;
    // The base's jobs, copied into a new layer's catalog, give way to the old layer's.
    let inherited: Vec<String> = layer
        .definitions()
        .map_err(|err| format!("cannot read the new layer's catalog: {err}"))?
        .filter(|d| d.kind() == DefinitionKind::JOB)
        .map(|d| d.name().to_string())
        .collect();
    if !inherited.is_empty() {
        let names: Vec<(DefinitionKind, &str)> = inherited
            .iter()
            .map(|n| (DefinitionKind::JOB, n.as_str()))
            .collect();
        layer
            .remove_definitions(&names)
            .map_err(|err| format!("cannot drop the base's jobs from the new layer: {err}"))?;
    }
    if !old.jobs.is_empty() {
        layer
            .set_definitions(old.jobs.clone())
            .map_err(|err| format!("cannot keep the layer's scheduled jobs: {err}"))?;
    }
    Store::<LayeredGraph>::create_layer_with(dir, base, layer, old.options.clone())
        .and_then(Store::close)
        .map_err(|err| format!("cannot create the new layer Store: {err}"))
}
