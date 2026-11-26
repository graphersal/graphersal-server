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
//!
//! Grants never outlive their name (round 3 B3-FIX): [`Admin::delete`] and
//! [`Admin::alias_remove`] remove the grants on the name; every operation that gives a name to a
//! new database or alias (create, clone, import, restore into a new name or a deleted
//! database's name, a new alias, an alias rename's new name) first removes the grants still
//! left on that name (`Admin::commit_new`, [`Registry::alias_set_with`],
//! [`Registry::alias_rename_with`]), each audited as a revoked grant, and reports how many.
//! A restore therefore never brings a deleted database's grants back.
//!
//! Concurrent changes (round 3 B-CONCUR; the inventory of files, writers and locks is in
//! `book/src/reference/server_directory.md` "Concurrent changes"): every change takes the lock of its file and
//! holds it across read, change and write; the memory follows only a successful write. Lock
//! order, never reversed: the loader's state lock ([`Loader`] `while_idle`), the loader's
//! read-only flip lock, the registry (write or read), the access file ([`crate::access::Access`]),
//! a session table; the models lock and the backup naming lock take none. So the grants of a
//! deleted database or removed alias go under the registry's write lock
//! ([`Registry::delete_with`], [`Registry::alias_remove_with`]) and a grant is written under its
//! read lock while its name must exist ([`Admin::grant_set`]). [`Admin::expecting`] bases a
//! change of a shared object on its revision ([`crate::revision`]): checked under the same lock,
//! a stale one is [`ServerError::Conflict`].
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
    /// Whether the server loads it at start (`None`: `server.toml`'s `[defaults] auto_load`,
    /// read at the create).
    pub auto_load: Option<bool>,
    /// Whether it is served read-only (`None`: `server.toml`'s `[defaults] read_only`; a base
    /// is read-only whatever this says).
    pub read_only: Option<bool>,
    /// Whether MCP offers it (`None`: `mcp.toml`'s `expose_new`, default off; an
    /// [`Admin::exposing`] choice wins over this one).
    pub mcp: Option<bool>,
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
        self.auto_load = Some(auto_load);
        self
    }

    /// The spec with `read_only`.
    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = Some(read_only);
        self
    }

    /// The spec with `auto_load` given or left to the server's default (`None`).
    pub fn with_auto_load_opt(mut self, auto_load: Option<bool>) -> Self {
        self.auto_load = auto_load;
        self
    }

    /// The spec with `read_only` given or left to the server's default (`None`).
    pub fn with_read_only_opt(mut self, read_only: Option<bool>) -> Self {
        self.read_only = read_only;
        self
    }

    /// The spec offered to MCP (`Some(true)`), hidden from it (`Some(false)`) or following
    /// the server's default (`None`).
    pub fn with_mcp(mut self, mcp: Option<bool>) -> Self {
        self.mcp = mcp;
        self
    }

    /// The `database.toml` of the new database; `defaults` (`server.toml`'s `[defaults]`) fill
    /// the flags the spec leaves open.
    fn config(&self, defaults: &crate::config::DatabaseDefaults) -> DatabaseConfig {
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
            .with_auto_load(self.auto_load.unwrap_or(defaults.auto_load))
            .with_read_only(self.read_only.unwrap_or(defaults.read_only))
            .with_description(self.description.clone())
    }
}

/// The Store options of a new database under the server configuration `config` (its
/// `[defaults]`).
fn store_options_of(config: &crate::config::ServerConfig) -> Result<StoreOptions, ServerError> {
    Ok(StoreOptions::new()
        .with_chunk_bytes(config.defaults.chunk_bytes)
        .with_damage_policy(config.damage_policy()?))
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
    /// How many grants on its name were removed with it (a grant never outlives its database,
    /// so a later database or alias of that name does not inherit it; a restore does not bring
    /// them back).
    pub grants: usize,
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
    /// The revision the next change is based on ([`Admin::expecting`]; `None`: no check).
    expected: Option<String>,
    /// Whether the names this admin creates are offered to MCP ([`Admin::exposing`]; `None`:
    /// `mcp.toml`'s `expose_new`).
    expose: Option<bool>,
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
            expected: None,
            expose: None,
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
            expected: None,
            expose: None,
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

    /// The admin whose changes of a shared object are based on its revision `expected`
    /// ([`crate::revision`]): the users and grants ([`Admin::user_add`] ... [`Admin::grant_revoke`]),
    /// the aliases ([`Admin::alias_set`], [`Admin::alias_remove`], [`Admin::alias_rename`]) or
    /// one database's settings ([`Admin::set_read_only`], [`Admin::set_auto_load`], and the
    /// settings change of the admin API's `PUT databases/<name>/settings`). A change
    /// whose object has another revision now is [`ServerError::Conflict`] (nothing changed);
    /// `None` (the default) checks nothing.
    pub fn expecting(mut self, expected: Option<String>) -> Self {
        self.expected = expected;
        self
    }

    /// The admin whose new names (a database made by create, import, clone, a restore into a
    /// new name or repair; a new alias) are offered to MCP when `expose` is `Some(true)`,
    /// hidden from it when `Some(false)`; `None` (the default) follows `mcp.toml`'s
    /// `expose_new` (default off; [`crate::mcp_control`]).
    pub fn exposing(mut self, expose: Option<bool>) -> Self {
        self.expose = expose;
        self
    }

    /// The revision the changes are based on ([`Admin::expecting`]).
    pub(crate) fn expected(&self) -> Option<&str> {
        self.expected.as_deref()
    }

    /// The audit handle events are recorded with.
    pub(crate) fn audit(&self) -> &Audit {
        &self.audit
    }

    /// The last unclean shutdown's state of the running server (`None` offline).
    pub(crate) fn recovery(&self) -> Option<&Arc<crate::recovery::Recovery>> {
        self.loader.as_ref().and_then(|l| l.recovery())
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

    /// The memory of the loaded databases (empty offline: nothing is loaded).
    pub(crate) fn loaded_memory(&self) -> crate::loader::LoadedMemory {
        self.loader
            .as_ref()
            .map(|loader| loader.memory())
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
        self.create_reported(name, spec).map(|(entry, _)| entry)
    }

    /// [`Admin::create`] answering also how many grants left on the name (from a deleted
    /// database or alias) were removed before the new database took it
    /// (`Admin::commit_new`).
    pub fn create_reported(
        &self,
        name: &str,
        spec: &CreateSpec,
    ) -> Result<(DatabaseEntry, usize), ServerError> {
        let schema = match &spec.schema {
            Some(json) => Some(GraphSchema::from_json(&json.to_string()).map_err(|err| {
                ServerError::InvalidInput {
                    name: name.to_string(),
                    message: format!("schema: {err}"),
                }
            })?),
            None => None,
        };
        let server = self.registry.dir().load_config()?;
        let options = store_options_of(&server)?;
        let build = self
            .registry
            .prepare_tmp(name, spec.config(&server.defaults))?;
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
        let admin = match (self.expose, spec.mcp) {
            (None, Some(mcp)) => self.clone().exposing(Some(mcp)),
            _ => self.clone(),
        };
        let (entry, leftover) = admin.commit_new(build)?;
        self.audit.record(AuditEvent::DatabaseCreated {
            name: entry.name.clone(),
            detail: describe(entry.config.as_ref()),
        });
        Ok((entry, leftover))
    }

    /// Registers the new database `build` ([`Registry::commit`]) after removing the grants left
    /// on its name ([`Admin::drop_leftover_grants`]): the name is reserved by the build, so no
    /// request reaches it meanwhile, and the new database never inherits a deleted one's
    /// grants. Answers the entry and how many grants were removed. A failure to rewrite
    /// `access.toml` refuses the commit (the build is discarded). The new name gets its MCP
    /// settings first ([`Admin::exposing`]; a failure to write `mcp.toml` refuses the commit
    /// too: the name would be offered to MCP otherwise).
    pub(crate) fn commit_new(
        &self,
        build: crate::admin::TmpBuild,
    ) -> Result<(DatabaseEntry, usize), ServerError> {
        let leftover = self.drop_leftover_grants(build.name())?;
        let name = build.name().to_string();
        self.claim_mcp_settings(&name)?;
        match self.registry.commit(build) {
            Ok(entry) => Ok((entry, leftover)),
            Err(err) => {
                self.forget_mcp_settings(&name);
                Err(err)
            }
        }
    }

    /// Deletes the unloaded database `name` into `trash/` (module docs) and removes every
    /// grant on its name ([`Deleted::grants`], each audited as a revoked grant).
    ///
    /// `Err`: [`ServerError::NotFound`]; [`ServerError::Refused`] while it is loaded, being
    /// loaded or unloaded, named by an alias, or the base of layers; [`ServerError::Io`].
    /// After the database is gone: `access.toml` could not be rewritten (its grants stay,
    /// listed as `missing`; a new database or alias of that name removes them first).
    pub fn delete(&self, name: &str) -> Result<Deleted, ServerError> {
        // The grants go under the registry's write lock: no new database or alias takes the
        // name (and gets grants of its own) before the old ones are gone.
        let delete = || {
            self.registry
                .delete_with(name, |name| self.drop_grants_on(name))
        };
        let (name, trash, grants) = match &self.loader {
            Some(loader) => loader.while_idle(name, delete)?,
            None => delete()?,
        };
        self.audit.record(AuditEvent::DatabaseDeleted {
            name: name.clone(),
            trash: trash.clone(),
        });
        let grants = grants?;
        Ok(Deleted {
            name,
            trash,
            grants,
        })
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
        self.clone_reported(source, new_name)
            .map(|(entry, _)| entry)
    }

    /// [`Admin::clone_database`] answering also how many grants left on the new name were
    /// removed before the clone took it (`Admin::commit_new`).
    pub fn clone_reported(
        &self,
        source: &str,
        new_name: &str,
    ) -> Result<(DatabaseEntry, usize), ServerError> {
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
        let (cloned, leftover) = self.commit_new(build)?;
        self.audit.record(AuditEvent::DatabaseCloned {
            source: entry.name.clone(),
            name: cloned.name.clone(),
        });
        Ok((cloned, leftover))
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
            return loader.set_read_only_expecting(name, read_only, self.expected());
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
        self.registry
            .set_read_only_expecting(name, read_only, self.expected())
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
        let entry = self
            .registry
            .set_auto_load_expecting(name, auto_load, self.expected())?;
        self.audit.record(AuditEvent::AutoLoadSet {
            name: entry.name.clone(),
            on: auto_load,
        });
        Ok(entry)
    }

    /// Sets (or flips) the alias `alias` to the database `target` ([`Registry::alias_set`]).
    /// A NEW alias first removes the grants left on its name (from a deleted database or
    /// alias, each audited as a revoked grant), under the registry's write lock: the number
    /// removed (0 for a flip).
    ///
    /// `Err`: as [`Registry::alias_set`]; `access.toml` could not be rewritten (nothing
    /// changed).
    pub fn alias_set(&self, alias: &str, target: &str) -> Result<usize, ServerError> {
        let leftover = self
            .registry
            .alias_set_with(alias, target, self.expected(), |name| {
                let leftover = self.drop_leftover_grants(name)?;
                self.claim_mcp_settings(name)?;
                Ok(leftover)
            })?;
        self.audit.record(AuditEvent::AliasSet {
            alias: crate::names::fold(alias),
            target: crate::names::fold(target),
        });
        Ok(leftover)
    }

    /// Removes the alias `alias` ([`Registry::alias_remove`]) and the grants on it (a grant on
    /// an alias never outlives it, so a later alias or database of that name does not inherit
    /// it): the number of grants removed.
    ///
    /// `Err` after the alias is gone: `access.toml` could not be rewritten (its grants on the
    /// name stay, listed as `missing`; revoke them by hand, or a new database or alias of that
    /// name removes them first).
    pub fn alias_remove(&self, alias: &str) -> Result<usize, ServerError> {
        // The grants go under the registry's write lock: no new alias or database takes the
        // name (and gets grants of its own) before the old ones are gone.
        let grants = self
            .registry
            .alias_remove_with(alias, self.expected(), |name| self.drop_grants_on(name))?;
        self.audit.record(AuditEvent::AliasRemoved {
            alias: crate::names::fold(alias),
        });
        grants
    }

    /// Renames the alias `old` to `new` ([`Registry::alias_rename`]); online, every MCP session
    /// that selected `old` moves to `new` in the same step (its next answer carries one note):
    /// [`Renamed::moved`] is the number of sessions moved (0 offline or with MCP off). The
    /// grants on `old` move to `new` in the same step too ([`Renamed::grants`]); grants left on
    /// `new` (from a deleted database or alias) are removed before, each audited as a revoked
    /// grant ([`Renamed::leftover`]; failing to rewrite `access.toml` there refuses the rename).
    ///
    /// `Err` after the alias is renamed: `access.toml` could not be rewritten (the grants stay
    /// on the old name, listed as `missing`).
    pub fn alias_rename(&self, old: &str, new: &str) -> Result<Renamed<usize>, ServerError> {
        let mcp = self.loader.as_ref().and_then(|loader| loader.mcp());
        // Under the registry's write lock: no request sees the new name without its grants.
        // The grants left on the new name go first (a refusal there changes nothing).
        let renamed = self.registry.alias_rename_with(
            old,
            new,
            self.expected(),
            |new| self.drop_leftover_grants(new),
            |old, new| {
                let sessions = mcp.map_or(0, |mcp| mcp.rename_selection(old, new));
                (sessions, self.move_alias_grants(old, new))
            },
        )?;
        let (sessions, grants) = renamed.moved;
        let mut renamed = Renamed {
            old: renamed.old,
            new: renamed.new,
            target: renamed.target,
            moved: sessions,
            grants: 0,
            leftover: renamed.leftover,
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
    pub(crate) fn store_options(&self) -> Result<StoreOptions, ServerError> {
        store_options_of(&self.registry.dir().load_config()?)
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

#[path = "ops_mcp.rs"]
mod mcp;

#[path = "ops_settings.rs"]
mod settings;

#[path = "ops_damage.rs"]
mod damage;
pub(crate) use damage::repair_json;
pub use damage::{Repaired, Swapped, Verified};

#[path = "ops_server_backup.rs"]
mod server_backup;

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
#[path = "ops_concurrency_tests.rs"]
mod concurrency_tests;

#[cfg(test)]
#[path = "ops_import_tests.rs"]
mod import_tests;

#[cfg(test)]
#[path = "ops_promote_tests.rs"]
mod promote_tests;

#[cfg(test)]
#[path = "ops_damage_tests.rs"]
mod damage_tests;

#[cfg(test)]
#[path = "ops_server_backup_tests.rs"]
mod server_backup_tests;
