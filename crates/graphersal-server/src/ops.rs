//! The admin operations: ONE implementation behind the admin HTTP API (`/api/admin/...`)
//! and the console (`graphersal-server db ...`, [`crate::console`]).
//!
//! An [`Admin`] is either **online** (inside a running server: it has the [`Loader`], so a
//! database can be loaded and unloaded, a read-only flip reaches the served context, a clone of
//! a loaded database forks its open Store) or **offline** (the console on a directory no server
//! holds: [`Admin::open_offline`] takes the server lock and scans the registry; nothing is
//! served, so `load`/`unload` are refused and every other operation acts on the files).
//!
//! - [`Admin::create`]: a new EMPTY database (plain, or a layer over a `role = "base"`
//!   database), optionally with a schema, built in `tmp/` and renamed into `databases/` in one
//!   step ([`Registry::prepare_tmp`] / [`Registry::commit`]): a failure or a crash in between
//!   leaves no `databases/<name>`, only debris in `tmp/` that the next scan removes. The new
//!   Store gets the creation parameters of `server.toml`'s `[defaults]` (chunk size, damage
//!   policy), read at each create.
//! - [`Admin::delete`]: only an UNLOADED database; it moves to `trash/<name>-<UTC time>/`
//!   (recoverable by hand). Refused while an alias names it (remove or flip the alias first) and
//!   for a base database that layers name.
//! - [`Admin::clone_database`]: a fork of the database's latest state into a new database
//!   (built in `tmp/` like a create); works on a loaded database (its open Store forks, the
//!   unload waits for it) and on an unloaded one (forked from its files); a layer's clone is a
//!   layer over the same base.
//! - [`Admin::backup`], [`Admin::backups`], [`Admin::restore`], [`Admin::export`]: backups into
//!   `backups/<name>/<UTC time>.zip`, their restore (unloaded only; into the same name or a new
//!   one; a layer's base checked by its identity) and `.gsnap` exports (with or without models;
//!   a layer as a layer snapshot or materialized), in `ops_transfer.rs`.
//! - [`Admin::import`]: another database (a packed snapshot, full or layer; a Store directory
//!   or `.gstore`; GraphSON; GraphML) as a new database, built in `tmp/` like a create, in
//!   `ops_import.rs`.
//! - [`Admin::models_index`], [`Admin::model_add`], [`Admin::model_remove`]: the shared
//!   embedding models in `models/` (`ops_models.rs`, [`crate::models`]); every build (create,
//!   clone, import, restore) shares its model blobs through `models/` before it is committed.
//! - [`Admin::promote`]: an UNLOADED plain database becomes a base (`role = "base"`,
//!   read-only), one way (a base is frozen forever, user decision U11); an import with
//!   [`DatabaseRole::Base`] ([`Admin::import_as`]) makes a base holding the imported data.
//! - [`Admin::runs`], [`Admin::cancel_run`]: the queued and running runs of the loaded
//!   databases, whatever their client, and the cancel of one by its id (online only;
//!   `ops_runs.rs`).
//! - `load`, `unload`, `set_read_only`, `set_auto_load`, `alias_set`, `alias_remove`: delegated
//!   to the [`Loader`] and the [`Registry`].
//! - [`Admin::alias_rename`]: [`Registry::alias_rename`], and online the MCP sessions that
//!   selected the old name move to the new one (`Mcp::rename_selection`).
//!
//! Database names are real names here (an alias is not followed): an operation on an alias's
//! name answers [`ServerError::NotFound`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use graphersal::persist::{RecoveryTarget, Store, StoreOptions};
use graphersal::prelude::{GraphSchema, GraphStorage, TraversalGraph};
use graphersal::storage::{LayerBase, LayeredGraph};
use graphersal_serve::layer_base_of_store;

use crate::admin::Renamed;
use crate::audit::{Audit, AuditEvent};
use crate::database::{DatabaseConfig, DatabaseKind, DatabaseRole};
use crate::error::ServerError;
use crate::layout::ServerDir;
use crate::loader::{LoadStart, Loader, OpenStoreRef};
use crate::lock::ServerLock;
use crate::registry::{DatabaseEntry, Registry};

/// What a new database is ([`Admin::create`]).
///
/// `#[non_exhaustive]`: build it with [`CreateSpec::plain`] or [`CreateSpec::layer`] and the
/// `with_*` setters.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct CreateSpec {
    /// Plain Store or layer Store.
    pub kind: DatabaseKind,
    /// Ordinary database or frozen base (a base is always created read-only).
    pub role: DatabaseRole,
    /// A layer's base database (`role = "base"`).
    pub base: Option<String>,
    /// The schema of the new graph (graphersal's schema format as JSON), if any.
    pub schema: Option<serde_json::Value>,
    /// A free text shown in listings.
    pub description: String,
    /// Whether the server loads it at start.
    pub auto_load: bool,
    /// Whether it is served read-only.
    pub read_only: bool,
}

impl CreateSpec {
    /// An empty plain, ordinary database.
    pub fn plain() -> Self {
        Self::default()
    }

    /// An empty layer over the base database `base`.
    pub fn layer(base: impl Into<String>) -> Self {
        Self {
            kind: DatabaseKind::Layer,
            base: Some(base.into()),
            ..Self::default()
        }
    }

    /// The spec with the role `role`.
    pub fn with_role(mut self, role: DatabaseRole) -> Self {
        self.role = role;
        self
    }

    /// The spec with a schema (graphersal's schema format as JSON).
    pub fn with_schema(mut self, schema: serde_json::Value) -> Self {
        self.schema = Some(schema);
        self
    }

    /// The spec with a description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// The spec with `auto_load`.
    pub fn with_auto_load(mut self, auto_load: bool) -> Self {
        self.auto_load = auto_load;
        self
    }

    /// The spec with `read_only`.
    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// The `database.toml` of the new database.
    fn config(&self) -> DatabaseConfig {
        let config = match (self.kind, &self.base) {
            (DatabaseKind::Layer, Some(base)) => DatabaseConfig::layer(base.clone()),
            (kind, base) => {
                let mut config = DatabaseConfig::plain();
                config.kind = kind;
                config.base = base.clone();
                config
            }
        };
        config
            .with_role(self.role)
            .with_auto_load(self.auto_load)
            .with_read_only(self.read_only)
            .with_description(self.description.clone())
    }
}

/// What [`Admin::delete`] did.
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Deleted {
    /// The database's name.
    pub name: String,
    /// Where its directory went: `trash/<name>-<UTC time>/`.
    pub trash: PathBuf,
}

/// A test hook of [`Admin::create`] and [`Admin::restore`]: fail after the Store was written
/// into `tmp/`.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailPoint {
    /// An error: the build is dropped (its directory removed).
    AfterStore,
    /// A crash: the build is left behind in `tmp/` (forgotten), for the next scan.
    CrashAfterStore,
}

/// The admin operations of a server directory (module docs).
#[derive(Debug, Clone)]
pub struct Admin {
    registry: Arc<Registry>,
    loader: Option<Arc<Loader>>,
    /// Where the administrative events go ([`crate::audit`]), as which actor.
    audit: Audit,
    /// The server's live access state (online; offline the user operations open the file).
    access: Option<Arc<crate::access::Access>>,
    /// Who acts: the actor of the user and grant changes (default: the operator).
    principal: crate::access::Principal,
    #[cfg(test)]
    pub(crate) fail: Option<FailPoint>,
    /// Test hook: the shared models' hard links fail (the copy fallback).
    #[cfg(test)]
    pub(crate) no_links: bool,
}

impl Admin {
    /// The admin of a running server (its loader and, through it, its registry).
    pub fn online(loader: Arc<Loader>) -> Self {
        Self {
            registry: loader.registry().clone(),
            loader: Some(loader),
            audit: Audit::default(),
            access: None,
            principal: crate::access::Principal::operator(),
            #[cfg(test)]
            fail: None,
            #[cfg(test)]
            no_links: false,
        }
    }

    /// The admin of a directory no server holds, over its scanned `registry` (take the server
    /// lock first: [`Admin::open_offline`] does both).
    pub fn offline(registry: Arc<Registry>) -> Self {
        Self {
            registry,
            loader: None,
            audit: Audit::default(),
            access: None,
            principal: crate::access::Principal::operator(),
            #[cfg(test)]
            fail: None,
            #[cfg(test)]
            no_links: false,
        }
    }

    /// Takes the server lock of `dir` and scans its registry: the offline admin and the lock to
    /// hold while it works. [`ServerError::Locked`] when a server runs on `dir` (then go
    /// through its admin API).
    pub fn open_offline(dir: &ServerDir) -> Result<(Admin, ServerLock), ServerError> {
        let lock = ServerLock::acquire(dir)?;
        let registry = Registry::scan(dir)?;
        Ok((Self::offline(registry), lock))
    }

    /// The admin recording its events through `audit` (its sink and actor; default: the
    /// tracing sink, no actor).
    pub fn with_audit(mut self, audit: Audit) -> Self {
        self.audit = audit;
        self
    }

    /// The audit handle events are recorded with.
    pub(crate) fn audit(&self) -> &Audit {
        &self.audit
    }

    /// The registry.
    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    /// Every base in the loader's pool with the databases using it (empty offline).
    pub fn pooled_bases(&self) -> std::collections::BTreeMap<String, Vec<String>> {
        self.loader
            .as_ref()
            .map(|loader| loader.pooled_bases())
            .unwrap_or_default()
    }

    /// The databases using the pooled base `base` (empty when it is not in the pool).
    pub(crate) fn pooled_users(&self, base: &str) -> Vec<String> {
        self.pooled_bases().remove(base).unwrap_or_default()
    }

    /// Whether this admin works inside a running server.
    pub fn is_online(&self) -> bool {
        self.loader.is_some()
    }

    /// Creates the empty database `name` (module docs).
    ///
    /// `Err`: [`ServerError::InvalidName`], [`ServerError::NameInUse`], [`ServerError::Refused`]
    /// (a layer without a base database, a layer that is a base, ...),
    /// [`ServerError::InvalidInput`] (a schema that does not parse or does not fit the base's
    /// data), [`ServerError::Store`] (the Store could not be written), [`ServerError::Config`]
    /// (`server.toml`), [`ServerError::Io`].
    pub fn create(&self, name: &str, spec: &CreateSpec) -> Result<DatabaseEntry, ServerError> {
        let schema = match &spec.schema {
            Some(json) => Some(GraphSchema::from_json(&json.to_string()).map_err(|err| {
                ServerError::InvalidInput {
                    name: name.to_string(),
                    message: format!("schema: {err}"),
                }
            })?),
            None => None,
        };
        let options = self.store_options()?;
        let build = self.registry.prepare_tmp(name, spec.config())?;
        let store_dir = build.store_dir();
        let built = match build.config().kind {
            DatabaseKind::Plain => create_plain(&store_dir, schema, options),
            DatabaseKind::Layer => {
                let base_name = build.config().base.clone().unwrap_or_default();
                self.base_of(&base_name)
                    .map_err(BuildError::Server)
                    .and_then(|base| create_layer(&store_dir, &base, schema, options))
            }
        };
        built.map_err(|err| err.for_database(name))?;
        #[cfg(test)]
        match self.fail {
            Some(FailPoint::AfterStore) => return Err(store_error(name, "injected failure")),
            Some(FailPoint::CrashAfterStore) => {
                std::mem::forget(build);
                return Err(store_error(name, "injected crash"));
            }
            None => {}
        }
        self.adopt_build(name, &store_dir);
        let entry = self.registry.commit(build)?;
        self.audit.record(AuditEvent::DatabaseCreated {
            name: entry.name.clone(),
            detail: describe(entry.config.as_ref()),
        });
        Ok(entry)
    }

    /// Deletes the unloaded database `name` into `trash/` (module docs).
    ///
    /// `Err`: [`ServerError::NotFound`]; [`ServerError::Refused`] while it is loaded, being
    /// loaded or unloaded, named by an alias, or the base of layers; [`ServerError::Io`].
    pub fn delete(&self, name: &str) -> Result<Deleted, ServerError> {
        let (name, trash) = match &self.loader {
            Some(loader) => loader.while_idle(name, || self.registry.delete(name))?,
            None => self.registry.delete(name)?,
        };
        let deleted = Deleted { name, trash };
        self.audit.record(AuditEvent::DatabaseDeleted {
            name: deleted.name.clone(),
            trash: deleted.trash.clone(),
        });
        Ok(deleted)
    }

    /// Clones the database `source` into the new database `new_name` (module docs): the same
    /// kind, role, base, description and read-only flag, `auto_load` off.
    ///
    /// `Err`: [`ServerError::NotFound`]; [`ServerError::DatabaseFailed`] for a source without
    /// configuration; the name errors of [`Admin::create`]; [`ServerError::Store`] when the
    /// fork fails.
    pub fn clone_database(
        &self,
        source: &str,
        new_name: &str,
    ) -> Result<DatabaseEntry, ServerError> {
        let entry = self.registry.get(source)?;
        let Some(config) = entry.config.clone() else {
            return Err(ServerError::DatabaseFailed {
                name: entry.name.clone(),
                reason: entry.state.to_string(),
            });
        };
        let mut new_config = match (&config.kind, &config.base) {
            (DatabaseKind::Layer, Some(base)) => DatabaseConfig::layer(base.clone()),
            _ => DatabaseConfig::plain(),
        }
        .with_role(config.role)
        .with_read_only(config.read_only)
        .with_description(config.description.clone());
        new_config.kind = config.kind;
        let build = self.registry.prepare_tmp(new_name, new_config)?;
        let target = build.store_dir();
        let open = self
            .loader
            .as_ref()
            .and_then(|l| l.open_store_of(&entry.name));
        let forked = match open {
            // Loaded: its open Store forks; the context held meanwhile makes an unload wait.
            Some((_context, OpenStoreRef::Plain(store))) => store
                .fork(RecoveryTarget::Latest, target.as_path())
                .map(drop)
                .map_err(|err| store_error(&entry.name, &format!("cannot fork it: {err}"))),
            Some((_context, OpenStoreRef::Layer(store))) => store
                .fork(RecoveryTarget::Latest, target.as_path())
                .map(drop)
                .map_err(|err| store_error(&entry.name, &format!("cannot fork it: {err}"))),
            None => match (&config.kind, &config.base) {
                (DatabaseKind::Layer, Some(base)) => self.base_of(base).and_then(|base| {
                    Store::<LayeredGraph>::fork_layer_dir(
                        entry.store_dir().as_path(),
                        &base,
                        RecoveryTarget::Latest,
                        target.as_path(),
                    )
                    .map(drop)
                    .map_err(|err| store_error(&entry.name, &format!("cannot fork it: {err}")))
                }),
                _ => Store::fork_dir(
                    entry.store_dir().as_path(),
                    RecoveryTarget::Latest,
                    target.as_path(),
                )
                .map(drop)
                .map_err(|err| store_error(&entry.name, &format!("cannot fork it: {err}"))),
            },
        };
        forked?;
        self.adopt_build(new_name, &target);
        let cloned = self.registry.commit(build)?;
        self.audit.record(AuditEvent::DatabaseCloned {
            source: entry.name.clone(),
            name: cloned.name.clone(),
        });
        Ok(cloned)
    }

    /// Starts loading the database `name` ([`Loader::load`]). Offline: refused (nothing
    /// serves it).
    pub fn load(&self, name: &str) -> Result<LoadStart, ServerError> {
        match &self.loader {
            Some(loader) => loader.load(name),
            None => Err(self.not_running(name, "load")),
        }
    }

    /// Unloads the database `name` ([`Loader::unload`]): `Ok(true)` once unloaded,
    /// `Ok(false)` when it was not loaded. Offline: refused (nothing is loaded).
    pub fn unload(&self, name: &str) -> Result<bool, ServerError> {
        match &self.loader {
            Some(loader) => loader.unload(name),
            None => Err(self.not_running(name, "unload")),
        }
    }

    /// Turns read-only on or off for the database `name`: persisted, and flipped at once on
    /// its served context when it is loaded ([`Loader::set_read_only`]). A base database
    /// refuses `false`.
    pub fn set_read_only(&self, name: &str, read_only: bool) -> Result<DatabaseEntry, ServerError> {
        let entry = self.set_read_only_inner(name, read_only)?;
        self.audit.record(AuditEvent::ReadOnlySet {
            name: entry.name.clone(),
            on: read_only,
        });
        Ok(entry)
    }

    fn set_read_only_inner(
        &self,
        name: &str,
        read_only: bool,
    ) -> Result<DatabaseEntry, ServerError> {
        if let Some(loader) = &self.loader {
            return loader.set_read_only(name, read_only);
        }
        let entry = self.registry.get(name)?;
        if !read_only
            && entry
                .config
                .as_ref()
                .is_some_and(|config| config.role == DatabaseRole::Base)
        {
            return Err(ServerError::Refused {
                name: entry.name,
                message: "a base database is always served read-only (layers are made over it)"
                    .to_string(),
            });
        }
        self.registry.set_read_only(name, read_only)
    }

    /// Promotes the UNLOADED plain database `name` to a base (`role = "base"`, read-only): the
    /// frozen graph that layers are made over. One way (user decision U11: a base is frozen
    /// forever; no demotion is offered): from now on nothing writes its Store, so the identity
    /// a layer binds to ([`graphersal::storage::BaseRef`] of its latest commit) stays stable.
    ///
    /// `Err`: [`ServerError::NotFound`]; [`ServerError::DatabaseFailed`] (no configuration);
    /// [`ServerError::Refused`] while it is loaded, being loaded or unloaded, for a layer, a
    /// base, a `needs-model` or failed database, or a Store that holds a layer;
    /// [`ServerError::Store`] (its Store cannot be inspected); [`ServerError::Io`].
    pub fn promote(&self, name: &str) -> Result<DatabaseEntry, ServerError> {
        let entry = self.registry.get(name)?;
        let Some(config) = entry.config.clone() else {
            return Err(ServerError::DatabaseFailed {
                name: entry.name.clone(),
                reason: entry.state.to_string(),
            });
        };
        if let Some(message) = crate::admin::promote_refusal(&config, &entry.state) {
            return Err(ServerError::Refused {
                name: entry.name,
                message,
            });
        }
        let promote = || {
            // The Store must be a plain one (a hand-edited `database.toml` could say otherwise).
            let info = Store::inspect(entry.store_dir().as_path()).map_err(|err| {
                store_error(&entry.name, &format!("cannot inspect its Store: {err}"))
            })?;
            if info.layer_base.is_some() {
                return Err(ServerError::Refused {
                    name: entry.name.clone(),
                    message: "its Store is a layer store, not a whole graph".to_string(),
                });
            }
            self.registry.promote_to_base(&entry.name)
        };
        let promoted = match &self.loader {
            Some(loader) => loader.while_idle(&entry.name, promote)?,
            None => promote()?,
        };
        self.audit.record(AuditEvent::DatabasePromoted {
            name: promoted.name.clone(),
        });
        Ok(promoted)
    }

    /// Sets whether the database `name` is loaded at server start ([`Registry::set_auto_load`]).
    pub fn set_auto_load(&self, name: &str, auto_load: bool) -> Result<DatabaseEntry, ServerError> {
        let entry = self.registry.set_auto_load(name, auto_load)?;
        self.audit.record(AuditEvent::AutoLoadSet {
            name: entry.name.clone(),
            on: auto_load,
        });
        Ok(entry)
    }

    /// Sets (or flips) the alias `alias` to the database `target` ([`Registry::alias_set`]).
    pub fn alias_set(&self, alias: &str, target: &str) -> Result<(), ServerError> {
        self.registry.alias_set(alias, target)?;
        self.audit.record(AuditEvent::AliasSet {
            alias: crate::names::fold(alias),
            target: crate::names::fold(target),
        });
        Ok(())
    }

    /// Removes the alias `alias` ([`Registry::alias_remove`]) and the grants on it (a grant on
    /// an alias never outlives it, so a later alias or database of that name does not inherit
    /// it): the number of grants removed.
    ///
    /// `Err` after the alias is gone: `access.toml` could not be rewritten (its grants on the
    /// name stay, listed as `missing`; revoke them by hand).
    pub fn alias_remove(&self, alias: &str) -> Result<usize, ServerError> {
        self.registry.alias_remove(alias)?;
        self.audit.record(AuditEvent::AliasRemoved {
            alias: crate::names::fold(alias),
        });
        self.drop_alias_grants(alias)
    }

    /// Renames the alias `old` to `new` ([`Registry::alias_rename`]); online, every MCP session
    /// that selected `old` moves to `new` in the same step (its next answer carries one note):
    /// [`Renamed::moved`] is the number of sessions moved (0 offline or with MCP off). The
    /// grants on `old` move to `new` in the same step too ([`Renamed::grants`]).
    ///
    /// `Err` after the alias is renamed: `access.toml` could not be rewritten (the grants stay
    /// on the old name, listed as `missing`).
    pub fn alias_rename(&self, old: &str, new: &str) -> Result<Renamed<usize>, ServerError> {
        let mcp = self.loader.as_ref().and_then(|loader| loader.mcp());
        // Under the registry's write lock: no request sees the new name without its grants.
        let renamed = self.registry.alias_rename(old, new, |old, new| {
            let sessions = mcp.map_or(0, |mcp| mcp.rename_selection(old, new));
            (sessions, self.move_alias_grants(old, new))
        })?;
        let (sessions, grants) = renamed.moved;
        let mut renamed = Renamed {
            old: renamed.old,
            new: renamed.new,
            target: renamed.target,
            moved: sessions,
            grants: 0,
        };
        self.audit.record(AuditEvent::AliasRenamed {
            old: renamed.old.clone(),
            new: renamed.new.clone(),
            target: renamed.target.clone(),
            moved: renamed.moved,
        });
        renamed.grants = grants?;
        Ok(renamed)
    }

    /// The Store options of a new database: `server.toml`'s `[defaults]`.
    fn store_options(&self) -> Result<StoreOptions, ServerError> {
        let config = self.registry.dir().load_config()?;
        Ok(StoreOptions::new()
            .with_chunk_bytes(config.defaults.chunk_bytes)
            .with_damage_policy(config.damage_policy()?))
    }

    /// The base database `base` as a [`LayerBase`]: the loader's pooled one when a loaded layer
    /// (or the base itself) uses it, else read from its Store (without a lock: nothing writes
    /// a base).
    fn base_of(&self, base: &str) -> Result<LayerBase, ServerError> {
        if let Some(pooled) = self.loader.as_ref().and_then(|l| l.pooled_base(base)) {
            return Ok(pooled);
        }
        let entry = self.registry.get(base)?;
        layer_base_of_store(entry.store_dir().as_path())
            .map(|opened| opened.base.with_name(entry.name.clone()))
            .map_err(|err| store_error(&entry.name, &format!("cannot read the base: {err}")))
    }

    fn not_running(&self, name: &str, what: &str) -> ServerError {
        ServerError::Refused {
            name: name.to_string(),
            message: format!(
                "no server runs on {}: only a running server can {what} a database; start it \
                 with `graphersal-server serve --dir {}`",
                self.registry.dir().root().display(),
                self.registry.dir().root().display()
            ),
        }
    }
}

/// A plain Store in `dir` for an empty graph with `schema`, closed again.
fn create_plain(
    dir: &Path,
    schema: Option<GraphSchema>,
    options: StoreOptions,
) -> Result<(), BuildError> {
    let mut graph = TraversalGraph::new();
    if let Some(schema) = schema {
        graph
            .set_schema(schema)
            .map_err(|err| BuildError::Input(format!("schema: {err}")))?;
    }
    Store::create_with(dir, graph, options)
        .and_then(Store::close)
        .map_err(|err| BuildError::Store(format!("cannot create its Store: {err}")))
}

/// A layer Store in `dir` for an empty layer over `base` with `schema`, closed again.
fn create_layer(
    dir: &Path,
    base: &LayerBase,
    schema: Option<GraphSchema>,
    options: StoreOptions,
) -> Result<(), BuildError> {
    let mut layer = LayeredGraph::over_base(base)
        .map_err(|err| BuildError::Store(format!("the base cannot carry a layer: {err}")))?;
    if let Some(schema) = schema {
        layer
            .set_schema(schema)
            .map_err(|err| BuildError::Input(format!("schema: {err}")))?;
    }
    Store::<LayeredGraph>::create_layer_with(dir, base, layer, options)
        .and_then(Store::close)
        .map_err(|err| BuildError::Store(format!("cannot create its layer Store: {err}")))
}

/// Why a build failed, before it is named.
enum BuildError {
    /// The request does not fit (a schema the data refuses).
    Input(String),
    /// The Store could not be written.
    Store(String),
    /// A server error already.
    Server(ServerError),
}

impl BuildError {
    fn for_database(self, name: &str) -> ServerError {
        match self {
            BuildError::Input(message) => ServerError::InvalidInput {
                name: name.to_string(),
                message,
            },
            BuildError::Store(message) => store_error(name, &message),
            BuildError::Server(err) => err,
        }
    }
}

fn store_error(name: &str, message: &str) -> ServerError {
    ServerError::Store {
        name: name.to_string(),
        message: message.to_string(),
    }
}

/// `plain`, `layer over <base>`, `base` for the log.
fn describe(config: Option<&DatabaseConfig>) -> String {
    match config {
        Some(c) if c.role == DatabaseRole::Base => "base".to_string(),
        Some(c) if c.kind == DatabaseKind::Layer => {
            format!("layer over '{}'", c.base.as_deref().unwrap_or_default())
        }
        _ => "plain".to_string(),
    }
}

#[path = "ops_transfer.rs"]
mod transfer;
pub(crate) use transfer::TmpFile;

#[path = "ops_import.rs"]
mod import;
pub use import::{ImportFormat, ImportSource, Imported};
pub(crate) use transfer::ExportFile;
pub use transfer::{BackupInfo, ExportSpec, Exported, Restored};

#[path = "ops_inbox.rs"]
mod inbox;
pub use inbox::InboxFile;

#[path = "ops_models.rs"]
mod models;
pub use models::{ModelAddSpec, ModelAdded, ModelRemoved};

#[path = "ops_runs.rs"]
mod runs;
pub use runs::DatabaseRun;

#[path = "ops_users.rs"]
mod users;
pub use users::SessionsEnded;
pub(crate) use users::parse_subject;

#[cfg(test)]
#[path = "ops_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "ops_transfer_tests.rs"]
mod transfer_tests;

#[cfg(test)]
#[path = "ops_import_tests.rs"]
mod import_tests;

#[cfg(test)]
#[path = "ops_promote_tests.rs"]
mod promote_tests;
