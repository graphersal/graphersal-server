//! Loading and unloading databases: the served [`Context`] of each loaded database, the pool
//! of shared layer bases, and the states in the [`Registry`].
//!
//! [`Loader::load`] starts a background thread that opens the database's Store, launches its
//! session (`graphersal_serve::launch`) and builds its [`Context`] (mounted under
//! `/db/<name>`, with the listener's `Host`/`Origin` checks and the runtime read-only toggle);
//! the context is inserted only when it is ready, so requests to the other loaded databases
//! never wait for a load. The registry shows `loading` meanwhile, then `loaded`, `failed
//! (reason)` or `needs-model (repo)` (an embedding model whose blob is missing: the database is
//! not served until the model is added).
//!
//! With MCP on ([`crate::config::McpConfig`]), every context is a host of the server's ONE MCP
//! endpoint (`shared_mcp`): `/db/<name>/mcp` and the graphs of the server-wide `/mcp`
//! (module `mcp`); a base's context has a read-only MCP profile.
//!
//! **Backup root** (`server.toml [jobs] backup_root`, resolved at start; `Loader::with_backup_root`):
//! every context gets it as its file root (`ContextBuilder::file_root`): a backup job's path
//! (`jobs/define`) is resolved under it and refused outside it, `catalog/load` refuses a
//! catalog with a job writing outside it, and the `dir` of the Store panel's fork, attic fork
//! and backup must lie inside it. The router refuses an absolute job path before forwarding
//! (`crate::router`). At load, the stored jobs of a plain or layer database that write outside
//! the root (an imported, restored or cloned store, or a root moved in `server.toml`) are
//! DROPPED in the launch, before the context serves a request or the ticker starts, each with a
//! warning naming the job and its path (`drop_jobs_outside`).
//!
//! The three kinds of database:
//! - **plain**: its Store opened read-write ([`graphersal_serve::open_store`]), served as the
//!   session's database (Store panel, history, checkpoints), with a scheduled-jobs ticker;
//! - **layer**: its base put into the pool first (below), then its layer store opened over the
//!   pooled base ([`graphersal_serve::open_layer_store`]), with a ticker;
//! - **base** (`role = "base"`): served for reading as an EMPTY layer over the pooled base with
//!   the read-only toggle on for good, whatever `database.toml` says (the engine has no served
//!   graph over a bare frozen base; the empty layer shares its memory, nothing is copied).
//!
//! The **base pool** holds one [`LayerBase`] per base database (read from its Store without a
//! lock: nothing writes a base), shared by every loaded layer over it and by the base's own
//! reading context: they all use the same frozen graph (the same `Arc`). A base enters the pool
//! with its first user and leaves it with its last; unloading a base database is refused while
//! a loaded layer uses it.
//!
//! [`Loader::unload`] removes the context from the map first (new requests will not reach it),
//! stops the ticker, waits (bounded) until the scheduled-job runs and the requests holding the
//! context are done, drops the context (the session thread ends) and shuts the Store down
//! cleanly (a checkpoint per the Store's normal close). [`Loader::shutdown`] unloads every
//! database when the server stops.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use graphersal::persist::{ModelInfo, ModelState, Store};
use graphersal::prelude::{Graph, GraphStorage};
use graphersal::storage::{LayerBase, LayeredGraph};
use graphersal_mcp::{Mcp, Profile};
use graphersal_serve::{
    Context, HostCheck, JobTicker, NoteLevel, OpenNote, OpenStore, ServeError, ServedSource,
};
use graphersal_session::{Session, SessionPresets};

use crate::database::{DatabaseConfig, DatabaseKind, DatabaseRole};
use crate::error::ServerError;
use crate::limits::QueryLimits;
use crate::mounts::DatabaseInfo;
use crate::names::fold;
use crate::registry::{DatabaseEntry, DbState, Registry};

/// How long [`Loader::unload`] waits for the scheduled-job runs and the requests of a database
/// before it closes the Store anyway (a commit after that is refused by the closed Store).
pub const UNLOAD_WAIT: Duration = Duration::from_secs(30);

/// The help text of a base database's read-only refusals.
const BASE_HELP: &str = "A base database is always served read-only: layers are made over it, \
                         change the data in a layer.";

/// What [`Loader::load`] did.
///
/// `#[non_exhaustive]`: further answers may be added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LoadStart {
    /// A background load started (the state is `loading`).
    Started,
    /// The database is being loaded already: nothing new started.
    AlreadyLoading,
    /// The database is loaded already: nothing to do.
    AlreadyLoaded,
}

/// The open Store of a loaded plain or layer database ([`Loader::open_store_of`]).
pub(crate) enum OpenStoreRef {
    /// A plain database's Store.
    Plain(Arc<Store>),
    /// A layer database's layer Store.
    Layer(Arc<Store<LayeredGraph>>),
}

/// What a loaded database is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Plain,
    Layer,
    Base,
}

/// A loaded database: its context and what its close needs.
struct LoadedDb {
    /// The name (folded).
    name: String,
    kind: Kind,
    /// The served context (requests clone it for their duration).
    context: Arc<Context>,
    /// The scheduled-jobs ticker (plain and layer databases).
    ticker: Option<JobTicker>,
    /// The Store of a plain database.
    store: Option<Arc<Store>>,
    /// The layer store of a layer database.
    layer_store: Option<Arc<Store<LayeredGraph>>>,
    /// The empty layer a base database is served as.
    served_layer: Option<Arc<Graph<LayeredGraph>>>,
    /// The pooled base this database uses (a layer's base, or a base's own name).
    base: Option<String>,
    /// The query limits its session runs under (`server.toml`'s with its `database.toml`'s
    /// over them, as read by this load; [`Loader::limits_in_effect`]).
    limits: QueryLimits,
}

/// A base in the pool and the databases using it.
struct PoolEntry {
    base: LayerBase,
    users: BTreeSet<String>,
}

/// The loader's state behind its lock.
#[derive(Default)]
struct State {
    loaded: BTreeMap<String, LoadedDb>,
    loading: BTreeSet<String>,
    unloading: BTreeSet<String>,
    pool: BTreeMap<String, PoolEntry>,
}

/// The engine's footprint of `graph` without waiting (`None` while a writer holds it, or when
/// the storage reports no figures).
fn footprint<S: GraphStorage>(graph: &Graph<S>) -> Option<u64> {
    graph.try_read()?.memory_footprint_bytes().ok().flatten()
}

/// Why a background load did not serve the database.
enum LoadFailure {
    NeedsModel(String),
    Failed(String),
}

impl From<ServeError> for LoadFailure {
    fn from(err: ServeError) -> Self {
        LoadFailure::Failed(err.to_string())
    }
}

/// The loaded databases of a server, their base pool and their loads (module docs).
pub struct Loader {
    registry: Arc<Registry>,
    host_check: HostCheck,
    state: Mutex<State>,
    /// Notified whenever a load ends.
    changed: Condvar,
    /// Serializes the base loads into the pool (one base read at a time).
    pool_loads: Mutex<()>,
    /// Serializes the read-only flips (an admin flip and the sync after a load).
    flips: Mutex<()>,
    /// The background load threads (joined by [`Loader::shutdown`]).
    threads: Mutex<Vec<JoinHandle<()>>>,
    stopping: AtomicBool,
    unload_wait: Duration,
    /// The server's MCP endpoint, shared by every context it builds (`None`: MCP is off).
    mcp: Option<Arc<Mcp>>,
    /// The MCP switches (`mcp.toml`) and usage statistics (`serve` sets them once; unset in the
    /// unit tests: everything on, no statistics).
    mcp_control: std::sync::OnceLock<Arc<crate::mcp_control::McpControl>>,
    /// The backup root (`server.toml [jobs] backup_root`, resolved): every context's file root
    /// (`ContextBuilder::file_root`), and the root the stored jobs of a loaded database are
    /// checked against (`None`: no confinement, the unit tests).
    backup_root: Option<PathBuf>,
    /// The last memory figure measured per graph (a loaded database's own graph by its folded
    /// name, a pooled base by `base:<name>`): answered while a writer holds the graph's lock
    /// ([`Loader::memory`]).
    memory_seen: Mutex<BTreeMap<String, u64>>,
}

/// The memory of the loaded databases ([`Loader::memory`]): the engine's footprint of each
/// graph (`GraphStorage::memory_footprint_bytes`: the graph's own data, cached by the engine
/// per commit, so an unchanged graph is not walked again; loaded embedding models and the
/// workload statistics are not part of it).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LoadedMemory {
    /// Per loaded database (folded name): its memory. A plain database: its graph; a layer:
    /// its own changes (the delta over the base); a base: the frozen base graph.
    pub(crate) databases: BTreeMap<String, DatabaseMemory>,
    /// Every loaded graph once: the plain databases, the layers' deltas and each pooled base
    /// once (a base shared by several layers and its own reading context counts one time).
    pub(crate) total_bytes: u64,
}

/// One loaded database's memory ([`LoadedMemory`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DatabaseMemory {
    /// Its own memory (`None`: not measured: the graph was busy from the start, or the storage
    /// reports no figures).
    pub(crate) bytes: Option<u64>,
    /// A layer: the memory of its base, shared with the base and the other layers over it (not
    /// part of `bytes`).
    pub(crate) shared_bytes: Option<u64>,
}

impl std::fmt::Debug for Loader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.lock();
        f.debug_struct("Loader")
            .field("loaded", &state.loaded.keys().collect::<Vec<_>>())
            .field("loading", &state.loading)
            .field("pool", &state.pool.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Loader {
    /// The loader of `registry`'s databases; their contexts check requests with `host_check`
    /// (the listener's).
    pub fn new(registry: Arc<Registry>, host_check: HostCheck) -> Arc<Loader> {
        Self::with_mcp(registry, host_check, None)
    }

    /// [`Loader::new`] whose contexts serve the MCP endpoint `mcp` (`/db/<name>/mcp`, and the
    /// graphs of the server-wide `/mcp`): every context gets it with `shared_mcp`, a base
    /// (`role = "base"`) also a read-only profile (only reading tools).
    pub(crate) fn with_mcp(
        registry: Arc<Registry>,
        host_check: HostCheck,
        mcp: Option<Arc<Mcp>>,
    ) -> Arc<Loader> {
        Self::with_backup_root(registry, host_check, mcp, None)
    }

    /// [`Loader::with_mcp`] whose contexts confine file paths to `backup_root` (the resolved
    /// `server.toml [jobs] backup_root`; module docs, "Backup root"): `serve` builds its loader
    /// with it.
    pub(crate) fn with_backup_root(
        registry: Arc<Registry>,
        host_check: HostCheck,
        mcp: Option<Arc<Mcp>>,
        backup_root: Option<PathBuf>,
    ) -> Arc<Loader> {
        Arc::new(Loader {
            registry,
            host_check,
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            pool_loads: Mutex::new(()),
            flips: Mutex::new(()),
            threads: Mutex::new(Vec::new()),
            stopping: AtomicBool::new(false),
            unload_wait: UNLOAD_WAIT,
            mcp,
            mcp_control: std::sync::OnceLock::new(),
            backup_root,
            memory_seen: Mutex::new(BTreeMap::new()),
        })
    }

    /// Sets the MCP switches and statistics (once; a second call is ignored).
    pub(crate) fn set_mcp_control(&self, control: Arc<crate::mcp_control::McpControl>) {
        let _ = self.mcp_control.set(control);
    }

    /// The MCP switches and statistics (`None`: not set, the unit tests: everything on).
    pub(crate) fn mcp_control(&self) -> Option<&Arc<crate::mcp_control::McpControl>> {
        self.mcp_control.get()
    }

    /// The server's MCP endpoint, `None` when MCP is off (`server.toml` `[mcp] enabled`).
    pub(crate) fn mcp(&self) -> Option<&Arc<Mcp>> {
        self.mcp.as_ref()
    }

    /// The registry.
    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    /// The served context of the loaded database `name` (case-insensitive), `None` when it is
    /// not loaded. A request holds the clone for its duration (an unload waits for it).
    pub fn context(&self, name: &str) -> Option<Arc<Context>> {
        self.lock()
            .loaded
            .get(&fold(name))
            .map(|db| db.context.clone())
    }

    /// The query limits the loaded database `name` runs under: what its last load read from
    /// `server.toml` and its `database.toml` (a change of either applies at the next load).
    /// `None` when it is not loaded.
    pub(crate) fn limits_in_effect(&self, name: &str) -> Option<QueryLimits> {
        self.lock()
            .loaded
            .get(&fold(name))
            .map(|db| db.limits.clone())
    }

    /// The names of the loaded databases.
    pub fn loaded(&self) -> Vec<String> {
        self.lock().loaded.keys().cloned().collect()
    }

    /// The memory of the loaded databases and their sum ([`LoadedMemory`]). Never waits for a
    /// graph: the handles are cloned under the loader's lock, then each graph is asked with a
    /// `try_read`; a graph a writer holds answers its last measured figure (`None` before the
    /// first). A frozen base is never written, so it always answers.
    pub(crate) fn memory(&self) -> LoadedMemory {
        enum Handle {
            Plain(Arc<Store>),
            Layer(Arc<Store<LayeredGraph>>),
            Base,
        }
        let (dbs, bases) = {
            let state = self.lock();
            let dbs: Vec<(String, Handle, Option<String>)> = state
                .loaded
                .values()
                .map(|db| {
                    let handle = match (db.kind, &db.store, &db.layer_store) {
                        (Kind::Plain, Some(store), _) => Handle::Plain(store.clone()),
                        (Kind::Layer, _, Some(store)) => Handle::Layer(store.clone()),
                        _ => Handle::Base,
                    };
                    (db.name.clone(), handle, db.base.clone())
                })
                .collect();
            let bases: BTreeMap<String, LayerBase> = state
                .pool
                .iter()
                .map(|(name, entry)| (name.clone(), entry.base.clone()))
                .collect();
            (dbs, bases)
        };
        let base_bytes: BTreeMap<String, Option<u64>> = bases
            .iter()
            .map(|(name, base)| {
                let bytes = base.graph().memory_footprint_bytes().ok().flatten();
                (name.clone(), self.seen(format!("base:{name}"), bytes))
            })
            .collect();
        let mut memory = LoadedMemory::default();
        for (name, handle, base) in dbs {
            let shared = base
                .as_ref()
                .and_then(|b| base_bytes.get(b).copied().flatten());
            // A base's own row is the pooled base, counted once below.
            let is_base = matches!(handle, Handle::Base);
            let one = match handle {
                Handle::Plain(store) => DatabaseMemory {
                    bytes: self.seen(name.clone(), footprint(store.graph())),
                    shared_bytes: None,
                },
                Handle::Layer(store) => DatabaseMemory {
                    bytes: self.seen(name.clone(), footprint(store.graph())),
                    shared_bytes: shared,
                },
                Handle::Base => DatabaseMemory {
                    bytes: shared,
                    shared_bytes: None,
                },
            };
            if !is_base {
                memory.total_bytes = memory.total_bytes.saturating_add(one.bytes.unwrap_or(0));
            }
            memory.databases.insert(name, one);
        }
        for bytes in base_bytes.values() {
            memory.total_bytes = memory.total_bytes.saturating_add(bytes.unwrap_or(0));
        }
        // Forget the figures of graphs no longer loaded.
        if let Ok(mut seen) = self.memory_seen.lock() {
            seen.retain(|key, _| match key.strip_prefix("base:") {
                Some(base) => base_bytes.contains_key(base),
                None => memory.databases.contains_key(key),
            });
        }
        memory
    }

    /// `bytes` when measured (remembered as the last figure of `key`), else the last figure.
    fn seen(&self, key: String, bytes: Option<u64>) -> Option<u64> {
        let Ok(mut seen) = self.memory_seen.lock() else {
            return bytes;
        };
        match bytes {
            Some(bytes) => {
                seen.insert(key, bytes);
                Some(bytes)
            }
            None => seen.get(&key).copied(),
        }
    }

    /// Every base in the pool with the databases using it (its layers, and the base itself
    /// when it is loaded for reading).
    pub fn pooled_bases(&self) -> BTreeMap<String, Vec<String>> {
        self.lock()
            .pool
            .iter()
            .map(|(name, entry)| (name.clone(), entry.users.iter().cloned().collect()))
            .collect()
    }

    /// The open Store of the loaded plain or layer database `name` with its context: holding
    /// the context makes an unload wait (bounded) until it is dropped. `None` when it is not
    /// loaded, or is a base (served without opening its Store).
    pub(crate) fn open_store_of(&self, name: &str) -> Option<(Arc<Context>, OpenStoreRef)> {
        let state = self.lock();
        let db = state.loaded.get(&fold(name))?;
        let store = match (&db.store, &db.layer_store) {
            (Some(store), _) => OpenStoreRef::Plain(store.clone()),
            (None, Some(store)) => OpenStoreRef::Layer(store.clone()),
            (None, None) => return None,
        };
        Some((db.context.clone(), store))
    }

    /// The pooled base `name`, when a loaded layer or the base itself uses it.
    pub(crate) fn pooled_base(&self, name: &str) -> Option<LayerBase> {
        self.lock()
            .pool
            .get(&fold(name))
            .map(|entry| entry.base.clone())
    }

    /// Runs `change` while the database `name` is neither loaded nor being loaded or unloaded,
    /// holding the loader's lock so that no load starts meanwhile (a delete). Refused
    /// otherwise.
    pub(crate) fn while_idle<T>(
        &self,
        name: &str,
        change: impl FnOnce() -> Result<T, ServerError>,
    ) -> Result<T, ServerError> {
        let key = fold(name);
        let state = self.lock();
        let busy = if state.loaded.contains_key(&key) {
            Some("it is loaded; unload it first")
        } else if state.loading.contains(&key) {
            Some("it is being loaded; unload it once it is loaded")
        } else if state.unloading.contains(&key) {
            Some("it is being unloaded; try again in a moment")
        } else if state.pool.contains_key(&key) {
            Some("loaded layers use it as their base; unload them first")
        } else {
            None
        };
        if let Some(message) = busy {
            return Err(refused(&key, message));
        }
        let out = change();
        drop(state);
        out
    }

    /// Starts loading the database `name` on a background thread (module docs). Loading a
    /// database that is loaded or being loaded is no error: the answer says so. A database
    /// whose last load failed (or that needs a model) may be loaded again.
    ///
    /// `Err`: [`ServerError::NotFound`]; [`ServerError::DatabaseFailed`] when its
    /// `database.toml` cannot be read; [`ServerError::Refused`] while it is being unloaded or
    /// the server stops; [`ServerError::Thread`].
    pub fn load(self: &Arc<Self>, name: &str) -> Result<LoadStart, ServerError> {
        let entry = self.registry.get(name)?;
        let Some(config) = entry.config.clone() else {
            return Err(ServerError::DatabaseFailed {
                name: entry.name.clone(),
                reason: entry.state.to_string(),
            });
        };
        let key = entry.name.clone();
        let previous = entry.state.clone();
        {
            let mut state = self.lock();
            if self.stopping.load(Ordering::SeqCst) {
                return Err(refused(&key, "the server is stopping"));
            }
            if state.loaded.contains_key(&key) {
                return Ok(LoadStart::AlreadyLoaded);
            }
            if state.loading.contains(&key) {
                return Ok(LoadStart::AlreadyLoading);
            }
            if state.unloading.contains(&key) {
                return Err(refused(
                    &key,
                    "it is being unloaded; load it again once it is unloaded",
                ));
            }
            state.loading.insert(key.clone());
            if let Err(err) = self.registry.set_state(&key, DbState::Loading) {
                // Deleted meanwhile.
                state.loading.remove(&key);
                return Err(err);
            }
        }
        tracing::info!(target: "graphersal_server", "loading database '{key}'");
        let loader = self.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("graphersal-server-load-{key}"))
            .spawn(move || loader.run_load(entry, config));
        match spawned {
            Ok(handle) => {
                let mut threads = self.threads.lock().unwrap_or_else(|p| p.into_inner());
                threads.retain(|thread| !thread.is_finished());
                threads.push(handle);
                Ok(LoadStart::Started)
            }
            Err(err) => {
                self.lock().loading.remove(&key);
                let _ = self.registry.set_state(&key, previous);
                self.changed.notify_all();
                Err(ServerError::Thread {
                    message: err.to_string(),
                })
            }
        }
    }

    /// Waits until the database `name` is no longer being loaded (at most `timeout`) and
    /// answers its state then (`loading` when the time ran out).
    pub fn wait(&self, name: &str, timeout: Duration) -> Result<DbState, ServerError> {
        let key = self.registry.get(name)?.name;
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        while state.loading.contains(&key) {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            state = self
                .changed
                .wait_timeout(state, deadline - now)
                .map(|(guard, _)| guard)
                .unwrap_or_else(|p| p.into_inner().0);
        }
        drop(state);
        Ok(self.registry.get(&key)?.state)
    }

    /// Unloads the database `name` (module docs): `Ok(true)` once it is unloaded, `Ok(false)`
    /// when it was not loaded. Its state is `unloaded` afterwards, also when its Store reported
    /// an error at the close (then `Err`).
    ///
    /// `Err`: [`ServerError::NotFound`]; [`ServerError::Refused`] while it is being loaded or
    /// unloaded, or for a base database a loaded layer uses; [`ServerError::Serve`] when its
    /// Store could not be closed cleanly.
    pub fn unload(&self, name: &str) -> Result<bool, ServerError> {
        let key = self.registry.get(name)?.name;
        let db = {
            let mut state = self.lock();
            if state.loading.contains(&key) {
                return Err(refused(
                    &key,
                    "it is being loaded; unload it once it is loaded",
                ));
            }
            if state.unloading.contains(&key) {
                return Err(refused(&key, "it is being unloaded already"));
            }
            let Some(db) = state.loaded.get(&key) else {
                return Ok(false);
            };
            if db.kind == Kind::Base {
                let layers: Vec<&str> = state
                    .pool
                    .get(&key)
                    .map(|entry| {
                        entry
                            .users
                            .iter()
                            .filter(|user| **user != key)
                            .map(String::as_str)
                            .collect()
                    })
                    .unwrap_or_default();
                if !layers.is_empty() {
                    return Err(refused(
                        &key,
                        &format!(
                            "it is the base of the loaded layers {}; unload them first",
                            layers.join(", ")
                        ),
                    ));
                }
            }
            state.unloading.insert(key.clone());
            match state.loaded.remove(&key) {
                Some(db) => db,
                None => return Ok(false),
            }
        };
        tracing::info!(target: "graphersal_server", "unloading database '{key}'");
        let closed = self.close(db);
        self.lock().unloading.remove(&key);
        let _ = self.registry.set_state(&key, DbState::Unloaded);
        self.changed.notify_all();
        closed.map(|()| true)
    }

    /// Turns read-only on or off for the database `name`: persisted in its `database.toml`
    /// (the admin core), and flipped at once on its context when it is loaded (a request
    /// already running finishes under the old setting). A base database refuses `false`: it is
    /// always served read-only.
    ///
    /// `Err`: the admin core's errors ([`Registry::set_read_only`]); [`ServerError::Refused`]
    /// for `false` on a base; [`ServerError::Serve`] when the loaded context refuses the flip.
    pub fn set_read_only(&self, name: &str, read_only: bool) -> Result<DatabaseEntry, ServerError> {
        self.set_read_only_expecting(name, read_only, None)
    }

    /// [`Loader::set_read_only`] based on the database's revision `expected`
    /// ([`crate::registry::DatabaseEntry::revision`]; `None`: no check): a stale one is
    /// [`ServerError::Conflict`], nothing changed.
    pub fn set_read_only_expecting(
        &self,
        name: &str,
        read_only: bool,
        expected: Option<&str>,
    ) -> Result<DatabaseEntry, ServerError> {
        let entry = self.registry.get(name)?;
        if !read_only
            && entry
                .config
                .as_ref()
                .is_some_and(|config| config.role == DatabaseRole::Base)
        {
            return Err(refused(
                &entry.name,
                "a base database is always served read-only (layers are made over it)",
            ));
        }
        let _flip = self.flips.lock().unwrap_or_else(|p| p.into_inner());
        let entry = self
            .registry
            .set_read_only_expecting(&entry.name, read_only, expected)?;
        if let Some(context) = self.context(&entry.name) {
            context
                .set_read_only(read_only)
                .map_err(|err| ServerError::Serve {
                    name: entry.name.clone(),
                    message: format!("cannot turn read-only {}: {err}", on_off(read_only)),
                })?;
        }
        Ok(entry)
    }

    /// Changes the settings of the database `name` ([`Registry::change_settings`], based on
    /// its revision `expected`), and, when the change sets read-only, flips its context at once
    /// when it is loaded (as [`Loader::set_read_only`]; a base refuses `false`). A query limit
    /// override applies at the next load ([`crate::settings`]).
    pub(crate) fn change_settings(
        &self,
        name: &str,
        change: &crate::settings::SettingsChange,
        expected: Option<&str>,
    ) -> Result<DatabaseEntry, ServerError> {
        let entry = self.registry.get(name)?;
        if change.read_only == Some(false)
            && entry
                .config
                .as_ref()
                .is_some_and(|config| config.role == DatabaseRole::Base)
        {
            return Err(refused(
                &entry.name,
                "a base database is always served read-only (layers are made over it)",
            ));
        }
        let _flip = self.flips.lock().unwrap_or_else(|p| p.into_inner());
        let entry = self
            .registry
            .change_settings(&entry.name, expected, change)?;
        if let Some(read_only) = change.read_only
            && let Some(context) = self.context(&entry.name)
        {
            context
                .set_read_only(read_only)
                .map_err(|err| ServerError::Serve {
                    name: entry.name.clone(),
                    message: format!("cannot turn read-only {}: {err}", on_off(read_only)),
                })?;
        }
        Ok(entry)
    }

    /// Starts loading every database with `auto_load = true` in the background; a failed one
    /// (its directory or configuration broken) is skipped with a warning.
    pub fn auto_load(self: &Arc<Self>) {
        for entry in self.registry.list() {
            let auto = entry.config.as_ref().is_some_and(|config| config.auto_load);
            if !auto {
                continue;
            }
            if let DbState::Failed { reason } = &entry.state {
                tracing::warn!(
                    target: "graphersal_server",
                    "database '{}' is not loaded at start: it is failed ({reason})",
                    entry.name
                );
                continue;
            }
            if let Err(err) = self.load(&entry.name) {
                tracing::warn!(target: "graphersal_server", "auto_load: {err}");
            }
        }
    }

    /// Unloads every database (the server stops): no new load starts, the running loads are
    /// awaited, then the layers are unloaded before the bases. Every failure is logged; the
    /// first is answered.
    pub fn shutdown(&self) -> Result<(), ServerError> {
        self.stopping.store(true, Ordering::SeqCst);
        let threads: Vec<JoinHandle<()>> =
            std::mem::take(&mut *self.threads.lock().unwrap_or_else(|p| p.into_inner()));
        for thread in threads {
            if thread.join().is_err() {
                tracing::error!(target: "graphersal_server", "a load thread panicked (a bug)");
            }
        }
        let mut names: Vec<(bool, String)> = self
            .lock()
            .loaded
            .values()
            .map(|db| (db.kind == Kind::Base, db.name.clone()))
            .collect();
        // Layers and plain databases first, the bases after their layers.
        names.sort();
        let mut first = None;
        for (_, name) in names {
            if let Err(err) = self.unload(&name) {
                tracing::warn!(target: "graphersal_server", "at stop: {err}");
                first.get_or_insert(err);
            }
        }
        first.map_or(Ok(()), Err)
    }

    /// The loader's state (a poisoned lock is taken over: every change keeps it consistent).
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The background load of `entry` (module docs).
    fn run_load(self: Arc<Self>, entry: DatabaseEntry, config: DatabaseConfig) {
        let key = entry.name.clone();
        let started = Instant::now();
        let result = self.open_and_build(&entry, &config);
        let mut leftover = None;
        let state_now = {
            let mut state = self.lock();
            state.loading.remove(&key);
            let next = match result {
                Ok(db) if self.stopping.load(Ordering::SeqCst) => {
                    // The server stops: closed again below, never served.
                    leftover = Some(db);
                    DbState::Unloaded
                }
                Ok(db) => {
                    state.loaded.insert(key.clone(), db);
                    DbState::Loaded
                }
                Err(LoadFailure::NeedsModel(repo)) => DbState::needs_model(repo),
                Err(LoadFailure::Failed(reason)) => DbState::failed(reason),
            };
            let _ = self.registry.set_state(&key, next.clone());
            next
        };
        self.changed.notify_all();
        if let Some(db) = leftover {
            let _ = self.close(db);
        }
        match &state_now {
            DbState::Loaded => {
                tracing::info!(
                    target: "graphersal_server",
                    "database '{key}' is loaded ({} ms)",
                    started.elapsed().as_millis()
                );
                self.sync_read_only(&key, config.read_only);
                // An import's `needs_model` marker: the model is there now.
                if let Err(err) = self.registry.clear_needs_model(&key) {
                    tracing::warn!(
                        target: "graphersal_server",
                        "database '{key}': cannot clear its needs_model marker: {err}"
                    );
                }
            }
            other => tracing::warn!(
                target: "graphersal_server",
                "database '{key}' is not loaded: {other}"
            ),
        }
    }

    /// After a load: a read-only flip made while the database was loading reaches its context.
    fn sync_read_only(&self, key: &str, built_with: bool) {
        let _flip = self.flips.lock().unwrap_or_else(|p| p.into_inner());
        let Ok(entry) = self.registry.get(key) else {
            return;
        };
        let Some(config) = entry.config else {
            return;
        };
        if config.role == DatabaseRole::Base || config.read_only == built_with {
            return;
        }
        if let Some(context) = self.context(key)
            && let Err(err) = context.set_read_only(config.read_only)
        {
            tracing::warn!(
                target: "graphersal_server",
                "database '{key}': cannot turn read-only {}: {err}",
                on_off(config.read_only)
            );
        }
    }

    /// Opens the database of `entry` and builds its served context (module docs). Whatever it
    /// opened is closed again when it fails.
    fn open_and_build(
        &self,
        entry: &DatabaseEntry,
        config: &DatabaseConfig,
    ) -> Result<LoadedDb, LoadFailure> {
        let key = entry.name.clone();
        let (presets, limits) = self.query_presets(entry)?;
        let kind = if config.role == DatabaseRole::Base {
            Kind::Base
        } else if config.kind == DatabaseKind::Layer {
            Kind::Layer
        } else {
            Kind::Plain
        };
        let mut db = Partial {
            loader: self,
            name: key.clone(),
            store: None,
            layer_store: None,
            base: None,
        };
        let (source, served_layer) = match kind {
            Kind::Plain => {
                let opened = graphersal_serve::open_store(entry.store_dir(), OpenStore::new())?;
                db.store = Some(opened.store.clone());
                log_notes(&key, &opened.notes);
                if let Some(repo) = missing_model(&opened.models) {
                    return Err(LoadFailure::NeedsModel(repo));
                }
                (ServedSource::Store(opened.store), None)
            }
            Kind::Layer => {
                let Some(base_name) = config.base.clone() else {
                    return Err(LoadFailure::Failed(
                        "a layer names its base (base = \"<name>\")".to_string(),
                    ));
                };
                let base = self.acquire_base(&base_name, &key)?;
                db.base = Some(fold(&base_name));
                let opened =
                    graphersal_serve::open_layer_store(entry.store_dir(), &base, OpenStore::new())?;
                db.layer_store = Some(opened.store.clone());
                log_notes(&key, &opened.notes);
                (
                    ServedSource::layer_store(opened.store, key.clone(), "store"),
                    None,
                )
            }
            Kind::Base => {
                let base = self.acquire_base(&key, &key)?;
                db.base = Some(key.clone());
                let layer = base.new_layer().map_err(|err| {
                    LoadFailure::Failed(format!("cannot serve the base as a layer: {err}"))
                })?;
                let layer = Arc::new(layer);
                (
                    ServedSource::graph(layer.clone(), key.clone(), "store"),
                    Some(layer),
                )
            }
        };
        // A base is read-only for good (its jobs are never run): only a plain or layer
        // database's stored jobs are checked against the backup root.
        let root = match kind {
            Kind::Base => None,
            Kind::Plain | Kind::Layer => self.backup_root.clone(),
        };
        let (launched, dropped) = graphersal_serve::launch(
            source,
            presets,
            move |session: &mut Session| match root {
                Some(root) => drop_jobs_outside(session, &root),
                None => Ok(Vec::new()),
            },
            None,
        )?;
        for warning in &launched.warnings {
            tracing::warn!(target: "graphersal_server", "database '{key}': {warning}");
        }
        for job in &dropped {
            tracing::warn!(
                target: "graphersal_server",
                "database '{key}': dropped the backup job '{}': it writes '{}' outside the \
                 backup root ({}); define it again with a path inside the root \
                 (server.toml [jobs] backup_root)",
                job.job,
                job.path.display(),
                job.reason
            );
        }
        let (read_only, help) = match kind {
            Kind::Base => (true, BASE_HELP.to_string()),
            Kind::Plain | Kind::Layer => (
                config.read_only,
                format!(
                    "An operator turned read-only on for the database '{key}'; ask them to turn \
                     it off to change the data."
                ),
            ),
        };
        let mut builder = launched
            .context_builder(self.host_check.clone())
            .server_name(crate::router::SERVER_NAME)
            .mount_path(Some(format!("/db/{key}")))
            .hooks(Arc::new(DatabaseInfo { name: key.clone() }))
            .read_only(read_only)
            .runtime_read_only(Some(help))
            .file_root(self.backup_root.clone());
        if let Some(mcp) = &self.mcp {
            builder = builder.shared_mcp(mcp.clone());
            // A base is read-only for good: its agents see only the reading tools. A plain or
            // layer database keeps the endpoint's profile: its read-only flag is the runtime
            // toggle, whose commit hook refuses an agent's writes too.
            if kind == Kind::Base {
                builder = builder.mcp_profile(Profile::new(true).with_config(mcp.config().clone()));
            }
        }
        if kind == Kind::Layer {
            builder = builder.save_refusal(Some(
                "the database is a layer store: every commit is already durable in it, there is \
                 nothing to save (the base is never written)"
                    .to_string(),
            ));
        }
        let context = builder.build();
        // The launch's job sender is dropped here: the context holds the only one, so the
        // session thread ends with the context.
        drop(launched);
        let ticker = match kind {
            // A base is read-only for good: its ticker would never tick.
            Kind::Base => None,
            Kind::Plain | Kind::Layer => match graphersal_serve::start_job_ticker(&context, None) {
                Ok(ticker) => Some(ticker),
                Err(err) => {
                    tracing::warn!(
                        target: "graphersal_server",
                        "database '{key}': no scheduled jobs: {err}"
                    );
                    None
                }
            },
        };
        let (store, layer_store, base) = db.keep();
        Ok(LoadedDb {
            name: key,
            kind,
            context,
            ticker,
            store,
            layer_store,
            served_layer,
            base,
            limits,
        })
    }

    /// The session presets of the database of `entry`: `server.toml`'s `[limits.query]` with
    /// its `database.toml`'s over it, both read now (`crate::limits`: a change applies at the
    /// next load); the registry's copy of the database's limits is refreshed. A file that does
    /// not parse fails the load with its reason.
    /// Also answers the merged limits themselves (kept as the limits in effect).
    fn query_presets(
        &self,
        entry: &DatabaseEntry,
    ) -> Result<(SessionPresets, QueryLimits), LoadFailure> {
        let global = self
            .registry
            .dir()
            .load_config()
            .map_err(|err| LoadFailure::Failed(err.to_string()))?
            .limits
            .query;
        let own = DatabaseConfig::read(&entry.dir)
            .map_err(|err| LoadFailure::Failed(err.to_string()))?
            .limits;
        let limits = own.query.over(&global);
        if !limits.is_empty() {
            tracing::info!(
                target: "graphersal_server",
                "database '{}': query limits {}",
                entry.name,
                limits.summary()
            );
        }
        self.registry.refresh_limits(&entry.name, own);
        Ok((limits.presets(), limits))
    }

    /// The pooled base `base` for the database `user`: from the pool, or read from the base
    /// database's Store into it. `user` is recorded as one of its users.
    fn acquire_base(&self, base: &str, user: &str) -> Result<LayerBase, LoadFailure> {
        let base_key = fold(base);
        let _one_at_a_time = self.pool_loads.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(entry) = self.lock().pool.get_mut(&base_key) {
            entry.users.insert(user.to_string());
            return Ok(entry.base.clone());
        }
        let entry = self
            .registry
            .get(&base_key)
            .map_err(|err| LoadFailure::Failed(format!("its base: {err}")))?;
        match &entry.config {
            Some(config) if config.role == DatabaseRole::Base => {}
            Some(_) => {
                return Err(LoadFailure::Failed(format!(
                    "its base '{base_key}' is not a base database (role = \"base\")"
                )));
            }
            None => {
                return Err(LoadFailure::Failed(format!(
                    "its base '{base_key}' is {}",
                    entry.state
                )));
            }
        }
        let started = Instant::now();
        let opened = graphersal_serve::layer_base_of_store(entry.store_dir())?;
        log_notes(&base_key, &opened.notes);
        if let Some(repo) = missing_model(&opened.models) {
            return Err(LoadFailure::NeedsModel(repo));
        }
        let layer_base = opened.base.with_name(base_key.clone());
        tracing::info!(
            target: "graphersal_server",
            "base '{base_key}' is in the pool ({} ms)",
            started.elapsed().as_millis()
        );
        self.lock().pool.insert(
            base_key,
            PoolEntry {
                base: layer_base.clone(),
                users: BTreeSet::from([user.to_string()]),
            },
        );
        Ok(layer_base)
    }

    /// `user` no longer uses the pooled base `base`: the base leaves the pool with its last
    /// user.
    fn release_base(&self, base: &str, user: &str) {
        let mut state = self.lock();
        if let Some(entry) = state.pool.get_mut(base) {
            entry.users.remove(user);
            if entry.users.is_empty() {
                state.pool.remove(base);
                tracing::info!(target: "graphersal_server", "base '{base}' left the pool");
            }
        }
    }

    /// Closes an unloaded database (module docs).
    fn close(&self, mut db: LoadedDb) -> Result<(), ServerError> {
        let name = db.name.clone();
        if let Some(ticker) = db.ticker.take()
            && let Err(err) = ticker.stop()
        {
            tracing::warn!(target: "graphersal_server", "database '{name}': {err}");
        }
        // The scheduled-job runs and the requests holding the context finish first.
        let deadline = Instant::now() + self.unload_wait;
        loop {
            let runs = db.context.background_runs().unwrap_or(0);
            let holders = Arc::strong_count(&db.context) - 1;
            if runs == 0 && holders == 0 {
                break;
            }
            if Instant::now() >= deadline {
                tracing::warn!(
                    target: "graphersal_server",
                    "database '{name}': closing with {runs} scheduled-job runs and {holders} \
                     requests still running (waited {} s); a commit they make is refused",
                    self.unload_wait.as_secs()
                );
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let LoadedDb {
            context,
            store,
            layer_store,
            served_layer,
            base,
            ..
        } = db;
        // The session's job queue closes: the session thread ends.
        drop(context);
        drop(served_layer);
        let mut result = Ok(());
        if let Some(store) = store
            && let Err(err) = store.shutdown()
        {
            result = Err(close_error(&name, &err.to_string()));
        }
        if let Some(store) = layer_store
            && let Err(err) = store.shutdown()
        {
            result = Err(close_error(&name, &err.to_string()));
        }
        if let Some(base) = base {
            self.release_base(&base, &name);
        }
        if let Err(err) = &result {
            tracing::warn!(target: "graphersal_server", "{err}");
        } else {
            tracing::info!(target: "graphersal_server", "database '{name}' is unloaded");
        }
        result
    }
}

/// What a load opened so far: closed again unless [`Partial::keep`] takes it.
struct Partial<'a> {
    loader: &'a Loader,
    name: String,
    store: Option<Arc<Store>>,
    layer_store: Option<Arc<Store<LayeredGraph>>>,
    base: Option<String>,
}

impl Partial<'_> {
    /// The handles, kept by the loaded database.
    #[allow(clippy::type_complexity)]
    fn keep(
        mut self,
    ) -> (
        Option<Arc<Store>>,
        Option<Arc<Store<LayeredGraph>>>,
        Option<String>,
    ) {
        (self.store.take(), self.layer_store.take(), self.base.take())
    }
}

impl Drop for Partial<'_> {
    fn drop(&mut self) {
        if let Some(store) = self.store.take()
            && let Err(err) = store.shutdown()
        {
            tracing::warn!(target: "graphersal_server", "database '{}': {err}", self.name);
        }
        if let Some(store) = self.layer_store.take()
            && let Err(err) = store.shutdown()
        {
            tracing::warn!(target: "graphersal_server", "database '{}': {err}", self.name);
        }
        if let Some(base) = self.base.take() {
            self.loader.release_base(&base, &self.name);
        }
    }
}

/// The first embedding model whose blob is missing.
fn missing_model(models: &[ModelInfo]) -> Option<String> {
    models
        .iter()
        .find(|model| matches!(model.state, ModelState::Missing))
        .map(|model| model.repo.clone())
}

/// Logs what an open found.
fn log_notes(name: &str, notes: &[OpenNote]) {
    for note in notes {
        let mut text = format!("database '{name}': {}", note.headline);
        for (key, value) in &note.details {
            text.push_str(&format!("; {key}: {value}"));
        }
        for (why, command) in &note.commands {
            text.push_str(&format!("; {why}: {command}"));
        }
        match note.level {
            NoteLevel::Warning => tracing::warn!(target: "graphersal_server", "{text}"),
            _ => tracing::info!(target: "graphersal_server", "{text}"),
        }
    }
}

fn refused(name: &str, message: &str) -> ServerError {
    ServerError::Refused {
        name: name.to_string(),
        message: message.to_string(),
    }
}

fn close_error(name: &str, reason: &str) -> ServerError {
    ServerError::Serve {
        name: name.to_string(),
        message: format!("its Store did not close cleanly: {reason}"),
    }
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// Drops the stored jobs of the session's database that write outside the backup `root`
/// (`graphersal_serve::jobs_outside_file_root`: an imported, restored or cloned store may carry
/// backup jobs pointing anywhere, and the ticker or `jobs/run` would run them). Runs in the
/// launch, before the context exists, so no request or tick sees them; answers what it dropped
/// (each job once). A drop that fails fails the load: such a job is never served.
fn drop_jobs_outside(
    session: &mut Session,
    root: &Path,
) -> Result<Vec<graphersal_serve::JobOutsideFileRoot>, String> {
    let definitions = session
        .graph()
        .definitions(Some(graphersal::catalog::DefinitionKind::JOB))
        .map_err(|err| format!("cannot read the stored jobs: {err}"))?;
    let mut outside = graphersal_serve::jobs_outside_file_root(&definitions, root);
    outside.dedup_by(|a, b| a.job == b.job);
    for job in &outside {
        session.drop_job(&job.job).map_err(|err| {
            format!(
                "the backup job '{}' writes outside the backup root and cannot be dropped: {err}",
                job.job
            )
        })?;
    }
    Ok(outside)
}

#[cfg(test)]
#[path = "loader_tests.rs"]
mod tests;
