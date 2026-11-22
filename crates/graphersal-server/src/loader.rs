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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use graphersal::persist::{ModelInfo, ModelState, Store};
use graphersal::prelude::Graph;
use graphersal::storage::{LayerBase, LayeredGraph};
use graphersal_serve::{
    Context, HostCheck, JobTicker, NoteLevel, OpenNote, OpenStore, ServeError, ServedSource,
};
use graphersal_session::{Session, SessionPresets};

use crate::database::{DatabaseConfig, DatabaseKind, DatabaseRole};
use crate::error::ServerError;
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
        })
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

    /// The names of the loaded databases.
    pub fn loaded(&self) -> Vec<String> {
        self.lock().loaded.keys().cloned().collect()
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
            self.registry.set_state(&key, DbState::Loading)?;
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
        let entry = self.registry.set_read_only(&entry.name, read_only)?;
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
        let (launched, ()) = graphersal_serve::launch(
            source,
            SessionPresets::new(),
            |_: &mut Session| Ok::<_, String>(()),
            None,
        )?;
        for warning in &launched.warnings {
            tracing::warn!(target: "graphersal_server", "database '{key}': {warning}");
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
            .mount_path(Some(format!("/db/{key}")))
            .read_only(read_only)
            .runtime_read_only(Some(help));
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
        })
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

#[cfg(test)]
#[path = "loader_tests.rs"]
mod tests;
