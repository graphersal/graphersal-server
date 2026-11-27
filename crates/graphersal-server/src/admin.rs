//! The admin core: the registry operations that the HTTP admin API and the console call (no
//! HTTP here).
//!
//! Every change writes its file first (atomically: a temporary file and a rename) and then the
//! registry's memory, under the registry's write lock. Database names are looked up
//! case-insensitively ([`crate::names::fold`]); [`Registry::resolve`] also follows an alias.
//!
//! New databases (create, import, clone, restore) are built in `tmp/` and renamed into
//! `databases/` in one step: [`Registry::prepare_tmp`] reserves the name and makes the build
//! directory, the caller writes the Store into [`TmpBuild::store_dir`], and
//! [`Registry::commit`] writes `database.toml` and renames the directory. A crash in between
//! leaves only debris in `tmp/`, which the next [`Registry::scan`] removes.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::database::{DatabaseConfig, DatabaseKind, DatabaseRole, STORE_DIR};
use crate::error::ServerError;
use crate::limits::DatabaseLimits;
use crate::names::{fold, validate_name, validate_new_name};
use crate::registry::{DatabaseEntry, DbState, Inner, Registry, aliases_revision, write_aliases};

impl Registry {
    /// Every database, by name.
    pub fn list(&self) -> Vec<DatabaseEntry> {
        self.read().entries.values().cloned().collect()
    }

    /// The database `name` (case-insensitive; an alias is not followed).
    pub fn get(&self, name: &str) -> Result<DatabaseEntry, ServerError> {
        self.read()
            .entries
            .get(&fold(name))
            .cloned()
            .ok_or_else(|| not_found(name))
    }

    /// The database a name or alias names (case-insensitive).
    pub fn resolve(&self, name_or_alias: &str) -> Result<DatabaseEntry, ServerError> {
        let key = fold(name_or_alias);
        let inner = self.read();
        let name = inner.aliases.get(&key).unwrap_or(&key);
        inner
            .entries
            .get(name)
            .cloned()
            .ok_or_else(|| not_found(name_or_alias))
    }

    /// Runs `f` while `name` names a database or an alias (case-insensitive), holding the
    /// registry's read lock so that no delete, alias removal or rename runs meanwhile:
    /// [`ServerError::NotFound`] when it names none (nothing run). `f` must not take the
    /// registry's lock itself (lock order: registry, then access).
    pub(crate) fn while_named<T>(
        &self,
        name: &str,
        f: impl FnOnce() -> Result<T, ServerError>,
    ) -> Result<T, ServerError> {
        let key = fold(name);
        let inner = self.read();
        let target = inner.aliases.get(&key).unwrap_or(&key);
        if !inner.entries.contains_key(target) {
            return Err(not_found(name));
        }
        let out = f();
        drop(inner);
        out
    }

    /// Every alias with its database.
    pub fn aliases(&self) -> BTreeMap<String, String> {
        self.read().aliases.clone()
    }

    /// Every alias with its database and their revision ([`crate::revision`]), read together.
    pub fn aliases_with_revision(&self) -> (BTreeMap<String, String>, String) {
        let inner = self.read();
        (inner.aliases.clone(), aliases_revision(&inner.aliases))
    }

    /// The revision of the aliases now ([`crate::revision`]).
    pub fn aliases_revision(&self) -> String {
        aliases_revision(&self.read().aliases)
    }

    /// Sets the alias `alias` to the database `target` (a new alias, or a flip of an existing
    /// one). Refused: an invalid alias name, a name taken by a database or a build, a target
    /// that is not a database (an alias cannot name an alias: no chains, no cycles), a broken
    /// `aliases.toml`.
    pub fn alias_set(&self, alias: &str, target: &str) -> Result<(), ServerError> {
        self.alias_set_with(alias, target, None, |_| Ok(0))
            .map(drop)
    }

    /// [`Registry::alias_set`] with `claim`, run under the registry's write lock for a NEW
    /// alias only, after every check and before `aliases.toml` is written, with the alias name:
    /// the server removes the grants left on that name there
    /// ([`crate::ops::Admin::alias_set`]), so no request sees the new alias with them. An
    /// `Err` of `claim` refuses the alias (nothing changed). Answers what `claim` returned (0
    /// for a flip of an existing alias). `expected`: the aliases' revision the change is based
    /// on ([`crate::revision`]; `None`: no check), checked under the same lock.
    pub fn alias_set_with(
        &self,
        alias: &str,
        target: &str,
        expected: Option<&str>,
        claim: impl FnOnce(&str) -> Result<usize, ServerError>,
    ) -> Result<usize, ServerError> {
        validate_name(alias)?;
        let target_key = fold(target);
        let mut inner = self.write();
        refuse_broken_aliases(&inner)?;
        check_aliases(&inner, expected)?;
        let new_alias = inner.holder_of(alias).is_none();
        match inner.holder_of(alias) {
            // A new alias may not take a reserved name; an existing one (made before the
            // reservation) may still be flipped.
            None => validate_new_name(alias)?,
            Some("alias") => {}
            Some(holder) => {
                return Err(ServerError::NameInUse {
                    name: alias.to_string(),
                    holder,
                });
            }
        }
        if !inner.entries.contains_key(&target_key) {
            if inner.aliases.contains_key(&target_key) {
                return Err(ServerError::Refused {
                    name: alias.to_string(),
                    message: format!(
                        "'{target}' is an alias; an alias names a database, not another alias"
                    ),
                });
            }
            return Err(not_found(target));
        }
        let claimed = if new_alias { claim(alias)? } else { 0 };
        let mut aliases = inner.aliases.clone();
        aliases.insert(alias.to_string(), target_key);
        write_aliases(&self.dir.aliases_path(), &aliases)?;
        inner.aliases = aliases;
        Ok(claimed)
    }

    /// Removes the alias `alias` (case-insensitive).
    pub fn alias_remove(&self, alias: &str) -> Result<(), ServerError> {
        self.alias_remove_with(alias, None, |_| ()).map(drop)
    }

    /// [`Registry::alias_remove`] based on the aliases' revision `expected` (`None`: no
    /// check), then `after` with the removed (folded) name, still under the registry's write
    /// lock (the server removes the alias's grants there, so no new alias of that name can take
    /// the name before they are gone): its result.
    pub fn alias_remove_with<T>(
        &self,
        alias: &str,
        expected: Option<&str>,
        after: impl FnOnce(&str) -> T,
    ) -> Result<T, ServerError> {
        let key = fold(alias);
        let mut inner = self.write();
        refuse_broken_aliases(&inner)?;
        check_aliases(&inner, expected)?;
        if !inner.aliases.contains_key(&key) {
            return Err(not_found(alias));
        }
        let mut aliases = inner.aliases.clone();
        aliases.remove(&key);
        write_aliases(&self.dir.aliases_path(), &aliases)?;
        inner.aliases = aliases;
        Ok(after(&key))
    }

    /// Renames the alias `old` (case-insensitive) to `new`, keeping its database: one atomic
    /// rewrite of `aliases.toml`. `moved` runs under the registry's write lock right after the
    /// file and memory changed, with the old (folded) and the new name (the server moves its
    /// MCP sessions there, so no lookup sees the rename half done); its result is returned in
    /// [`Renamed::moved`]. Refused: `old` is no alias (a database has no rename), `new` is an
    /// invalid name or a name taken by a database, an alias (also `old` itself) or a build, a
    /// broken `aliases.toml`.
    pub fn alias_rename<T>(
        &self,
        old: &str,
        new: &str,
        moved: impl FnOnce(&str, &str) -> T,
    ) -> Result<Renamed<T>, ServerError> {
        self.alias_rename_with(old, new, None, |_| Ok(0), moved)
    }

    /// [`Registry::alias_rename`] with `claim`, run under the registry's write lock after
    /// every check and before `aliases.toml` is written, with the new name: the server removes
    /// the grants left on that name there ([`crate::ops::Admin::alias_rename`]), so the renamed
    /// alias never inherits them. An `Err` of `claim` refuses the rename (nothing changed);
    /// its count is [`Renamed::leftover`]. `expected`: the aliases' revision the rename is
    /// based on ([`crate::revision`]; `None`: no check).
    pub fn alias_rename_with<T>(
        &self,
        old: &str,
        new: &str,
        expected: Option<&str>,
        claim: impl FnOnce(&str) -> Result<usize, ServerError>,
        moved: impl FnOnce(&str, &str) -> T,
    ) -> Result<Renamed<T>, ServerError> {
        let old_key = fold(old);
        let mut inner = self.write();
        refuse_broken_aliases(&inner)?;
        check_aliases(&inner, expected)?;
        let Some(target) = inner.aliases.get(&old_key).cloned() else {
            if inner.entries.contains_key(&old_key) {
                return Err(ServerError::Refused {
                    name: old_key,
                    message: "it is a database, not an alias; only an alias can be renamed"
                        .to_string(),
                });
            }
            return Err(not_found(old));
        };
        validate_new_name(new)?;
        if let Some(holder) = inner.holder_of(new) {
            return Err(ServerError::NameInUse {
                name: new.to_string(),
                holder,
            });
        }
        let leftover = claim(new)?;
        let mut aliases = inner.aliases.clone();
        aliases.remove(&old_key);
        aliases.insert(new.to_string(), target.clone());
        write_aliases(&self.dir.aliases_path(), &aliases)?;
        inner.aliases = aliases;
        let moved = moved(&old_key, new);
        Ok(Renamed {
            old: old_key,
            new: new.to_string(),
            target,
            moved,
            grants: 0,
            leftover,
        })
    }

    /// Sets whether the database `name` is loaded at server start (persisted in its
    /// `database.toml`).
    pub fn set_auto_load(&self, name: &str, auto_load: bool) -> Result<DatabaseEntry, ServerError> {
        self.set_auto_load_expecting(name, auto_load, None)
    }

    /// [`Registry::set_auto_load`] based on the database's revision `expected`
    /// ([`DatabaseEntry::revision`]; `None`: no check), checked under the same lock.
    pub fn set_auto_load_expecting(
        &self,
        name: &str,
        auto_load: bool,
        expected: Option<&str>,
    ) -> Result<DatabaseEntry, ServerError> {
        self.update_config(name, expected, |_, config| {
            config.auto_load = auto_load;
            Ok(())
        })
    }

    /// Sets whether the database `name` is served read-only (persisted in its `database.toml`;
    /// this function does not change a loaded database's runtime mode). Refused: turning it off
    /// for a base database while a layer names it.
    pub fn set_read_only(&self, name: &str, read_only: bool) -> Result<DatabaseEntry, ServerError> {
        self.set_read_only_expecting(name, read_only, None)
    }

    /// [`Registry::set_read_only`] based on the database's revision `expected`
    /// ([`DatabaseEntry::revision`]; `None`: no check), checked under the same lock.
    pub fn set_read_only_expecting(
        &self,
        name: &str,
        read_only: bool,
        expected: Option<&str>,
    ) -> Result<DatabaseEntry, ServerError> {
        self.update_config(name, expected, |inner, config| {
            if !read_only && config.role == DatabaseRole::Base && inner.has_layers_over(name) {
                return Err(ServerError::Refused {
                    name: name.to_string(),
                    message: "it is the base of layers and stays read-only while a layer names it"
                        .to_string(),
                });
            }
            config.read_only = read_only;
            Ok(())
        })
    }

    /// Changes the settings of the database `name` (round 3 B7, [`crate::settings`]): its
    /// description, read-only and auto-load flags and query limit overrides, written in ONE
    /// atomic write of its `database.toml` under the registry's write lock, based on its
    /// revision `expected` (`None`: no check). Like [`Registry::set_read_only`] this does not
    /// flip a loaded database's runtime mode (the loader does, [`crate::loader::Loader`]), and
    /// a base with layers refuses read-only off. A limit override changed here applies at the
    /// database's next load.
    ///
    /// `Err`: [`ServerError::NotFound`], [`ServerError::DatabaseFailed`] (no configuration),
    /// [`ServerError::Conflict`], [`ServerError::Refused`], [`ServerError::Io`]; nothing
    /// changed.
    pub(crate) fn change_settings(
        &self,
        name: &str,
        expected: Option<&str>,
        change: &crate::settings::SettingsChange,
    ) -> Result<DatabaseEntry, ServerError> {
        self.update_config(name, expected, |inner, config| {
            if let Some(read_only) = change.read_only {
                if !read_only && config.role == DatabaseRole::Base && inner.has_layers_over(name) {
                    return Err(ServerError::Refused {
                        name: name.to_string(),
                        message: "it is the base of layers and stays read-only while a layer \
                                  names it"
                            .to_string(),
                    });
                }
                config.read_only = read_only;
            }
            if let Some(auto_load) = change.auto_load {
                config.auto_load = auto_load;
            }
            if let Some(description) = &change.description {
                config.description = description.clone();
            }
            for (key, value) in &change.limits {
                if !config.limits.query.set(key, *value) {
                    return Err(ServerError::InvalidInput {
                        name: name.to_string(),
                        message: format!("unknown query limit '{key}'"),
                    });
                }
            }
            Ok(())
        })
    }

    /// The registry half of a promotion ([`crate::ops::Admin::promote`]): the plain, ordinary
    /// database `name` becomes `role = "base"` with `read_only = true`, written to its
    /// `database.toml`. Refused for a layer, a base, and a database that is not `unloaded`
    /// (the caller holds the loader idle meanwhile). One way: nothing turns a base back.
    pub fn promote_to_base(&self, name: &str) -> Result<DatabaseEntry, ServerError> {
        let state = self.get(name)?.state;
        self.update_config(name, None, |_, config| {
            if let Some(message) = promote_refusal(config, &state) {
                return Err(ServerError::Refused {
                    name: fold(name),
                    message,
                });
            }
            config.role = DatabaseRole::Base;
            config.read_only = true;
            Ok(())
        })
    }

    /// Sets the `needs_model` marker of the database `name` to `repo` (a verify found only
    /// that model's blob missing); nothing is written when it is set already.
    pub(crate) fn set_needs_model(&self, name: &str, repo: &str) -> Result<(), ServerError> {
        let marked = self
            .get(name)?
            .config
            .is_some_and(|config| config.needs_model.as_deref() == Some(repo));
        if !marked {
            self.update_config(name, None, |_, config| {
                config.needs_model = Some(repo.to_string());
                Ok(())
            })?;
        }
        Ok(())
    }

    /// Clears the `needs_model` marker of the database `name` (a load found every model it
    /// needs); nothing is written when it has none.
    pub fn clear_needs_model(&self, name: &str) -> Result<(), ServerError> {
        let marked = self
            .get(name)?
            .config
            .is_some_and(|config| config.needs_model.is_some());
        if marked {
            self.update_config(name, None, |_, config| {
                config.needs_model = None;
                Ok(())
            })?;
        }
        Ok(())
    }

    /// Replaces the limits in memory of the database `name` with `limits`, as a load read them
    /// from its `database.toml` (the file already holds them: nothing is written). A database
    /// gone meanwhile is no error.
    pub(crate) fn refresh_limits(&self, name: &str, limits: DatabaseLimits) {
        let mut inner = self.write();
        if let Some(config) = inner
            .entries
            .get_mut(&fold(name))
            .and_then(|entry| entry.config.as_mut())
        {
            config.limits = limits;
        }
    }

    /// Changes the configuration of the database `name` through `change`, then writes it.
    /// `expected`: the revision the change is based on (`None`: no check). The file is written
    /// before the memory changes, all under the registry's write lock.
    fn update_config(
        &self,
        name: &str,
        expected: Option<&str>,
        change: impl FnOnce(&Inner, &mut DatabaseConfig) -> Result<(), ServerError>,
    ) -> Result<DatabaseEntry, ServerError> {
        let key = fold(name);
        let mut inner = self.write();
        let entry = inner.entries.get(&key).ok_or_else(|| not_found(name))?;
        if expected.is_some() {
            let current = entry.revision().unwrap_or_default();
            crate::revision::check(expected, &current, &format!("settings of database '{key}'"))?;
        }
        let Some(mut config) = entry.config.clone() else {
            return Err(ServerError::DatabaseFailed {
                name: entry.name.clone(),
                reason: entry.state.to_string(),
            });
        };
        let dir = entry.dir.clone();
        // The file's limits (perhaps edited by hand since the scan or the last load) are the
        // ones changed or kept, not the copy in memory: only a settings change
        // (`Registry::change_settings`) touches them.
        if let Ok(on_disk) = DatabaseConfig::read(&dir) {
            config.limits = on_disk.limits;
        }
        change(&inner, &mut config)?;
        config.write(&dir)?;
        let entry = inner.entries.get_mut(&key).ok_or_else(|| not_found(name))?;
        entry.config = Some(config);
        Ok(entry.clone())
    }

    /// Starts building the new database `name` with `config` in `tmp/` (module docs): the name
    /// is checked and reserved, the build directory made. A base database is always created
    /// read-only; a layer's base must be a base database. Dropping the [`TmpBuild`] without
    /// [`Registry::commit`] removes the build directory and frees the name.
    pub fn prepare_tmp(
        self: &Arc<Self>,
        name: &str,
        config: DatabaseConfig,
    ) -> Result<TmpBuild, ServerError> {
        validate_new_name(name)?;
        self.prepare_tmp_checked(name, config)
    }

    /// [`Registry::prepare_tmp`] for a name that a database had before (a backup restored
    /// under its own database's name after that database was deleted): the name takes the rule
    /// of an EXISTING name ([`validate_name`]), so a database named before the stricter rule of
    /// new names (`crate::names`) can come back from its backup under that name.
    pub fn prepare_tmp_former_name(
        self: &Arc<Self>,
        name: &str,
        config: DatabaseConfig,
    ) -> Result<TmpBuild, ServerError> {
        validate_name(name)?;
        self.prepare_tmp_checked(name, config)
    }

    /// The rest of [`Registry::prepare_tmp`] after the name check.
    fn prepare_tmp_checked(
        self: &Arc<Self>,
        name: &str,
        mut config: DatabaseConfig,
    ) -> Result<TmpBuild, ServerError> {
        config.check().map_err(|message| ServerError::Refused {
            name: name.to_string(),
            message,
        })?;
        if config.role == DatabaseRole::Base {
            config.read_only = true;
        }
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let tmp = self.dir.tmp();
        let dir = tmp.join(format!(
            "build-{name}-{}",
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let mut inner = self.write();
        check_new(&inner, name, &config)?;
        fs::create_dir_all(&dir).map_err(|err| ServerError::io("create directory", &dir, err))?;
        inner.pending.insert(name.to_string());
        Ok(TmpBuild {
            registry: self.clone(),
            name: name.to_string(),
            config,
            dir,
            done: false,
            replace: false,
        })
    }

    /// Registers the build: writes its `database.toml`, renames its directory to
    /// `databases/<name>/` and lists it as [`DbState::Unloaded`]. Refused when the build has no
    /// `store/` or a layer's base is no longer a base database; a refused or failed commit
    /// discards the build.
    pub fn commit(&self, mut build: TmpBuild) -> Result<DatabaseEntry, ServerError> {
        if build.replace {
            return Err(ServerError::Refused {
                name: build.name.clone(),
                message: "a replacement build (use commit_replace)".to_string(),
            });
        }
        let mut inner = self.write();
        inner.pending.remove(&build.name);
        let committed = commit_build(&self.dir.databases(), &mut inner, &build);
        if committed.is_err() {
            // Keep the name reserved until the drop below removes the directory.
            inner.pending.insert(build.name.clone());
        } else {
            build.done = true;
        }
        drop(inner);
        committed
    }

    /// Starts building a REPLACEMENT of the existing database `name` with `config` in `tmp/`
    /// (a restore over the same name): like [`Registry::prepare_tmp`], but the name must be
    /// a database; [`Registry::commit_replace`] swaps the build in. Refused while another
    /// replacement of it is being built, and for a base database that layers name (its
    /// identity would change under them).
    pub fn prepare_replace(
        self: &Arc<Self>,
        name: &str,
        mut config: DatabaseConfig,
    ) -> Result<TmpBuild, ServerError> {
        let key = fold(name);
        config.check().map_err(|message| ServerError::Refused {
            name: key.clone(),
            message,
        })?;
        if config.role == DatabaseRole::Base {
            config.read_only = true;
        }
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = self.dir.tmp().join(format!(
            "replace-{key}-{}",
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let mut inner = self.write();
        let entry = inner.entries.get(&key).ok_or_else(|| not_found(name))?;
        if inner.pending.contains(&key) {
            return Err(ServerError::Refused {
                name: key,
                message: "it is being replaced already; try again when that ends".to_string(),
            });
        }
        if entry
            .config
            .as_ref()
            .is_some_and(|c| c.role == DatabaseRole::Base)
            && inner.has_layers_over(&key)
        {
            return Err(ServerError::Refused {
                name: key,
                message: "it is the base of layers; replacing it would change the base under \
                          them: delete the layers first, or restore into a new name"
                    .to_string(),
            });
        }
        if config.kind == DatabaseKind::Layer
            && let Some(problem) = config.base.as_deref().and_then(|b| inner.base_problem(b))
        {
            return Err(ServerError::Refused {
                name: key,
                message: problem,
            });
        }
        fs::create_dir_all(&dir).map_err(|err| ServerError::io("create directory", &dir, err))?;
        inner.pending.insert(key.clone());
        Ok(TmpBuild {
            registry: self.clone(),
            name: key,
            config,
            dir,
            done: false,
            replace: true,
        })
    }

    /// Swaps the replacement `build` ([`Registry::prepare_replace`]) in: the current
    /// `databases/<name>/` moves into `trash/<name>-<UTC time>/`, then the build is renamed
    /// into its place (if that rename fails, the old directory is moved back). Answers the new
    /// entry and the trash directory of the old one. Refused while the database is loaded or
    /// being loaded (the caller holding the loader makes sure it stays so); a refused or
    /// failed swap discards the build.
    pub fn commit_replace(
        &self,
        mut build: TmpBuild,
    ) -> Result<(DatabaseEntry, PathBuf), ServerError> {
        if !build.replace {
            return Err(ServerError::Refused {
                name: build.name.clone(),
                message: "not a replacement build (use commit)".to_string(),
            });
        }
        let mut inner = self.write();
        let key = build.name.clone();
        let entry = inner.entries.get(&key).ok_or_else(|| not_found(&key))?;
        if entry.state.is_busy() {
            return Err(ServerError::Refused {
                name: key,
                message: format!("it is {}; unload it first", entry.state.as_str()),
            });
        }
        if build.config.kind == DatabaseKind::Layer
            && let Some(problem) = build
                .config
                .base
                .as_deref()
                .and_then(|b| inner.base_problem(b))
        {
            return Err(ServerError::Refused {
                name: key,
                message: problem,
            });
        }
        if !build.store_dir().is_dir() {
            return Err(ServerError::Refused {
                name: key,
                message: format!("the build has no Store ({STORE_DIR}/ is missing)"),
            });
        }
        let old = entry.dir.clone();
        build.config.write(&build.dir)?;
        let trash = self.trash_target(&key)?;
        fs::rename(&old, &trash).map_err(|err| ServerError::io("rename", &old, err))?;
        if let Err(err) = fs::rename(&build.dir, &old) {
            if let Err(back) = fs::rename(&trash, &old) {
                tracing::error!(
                    target: "graphersal_server",
                    "database '{key}': cannot move {} back after a failed replace: {back}; \
                     it is kept there",
                    trash.display()
                );
            }
            return Err(ServerError::io("rename", &build.dir, err));
        }
        build.done = true;
        inner.pending.remove(&key);
        let entry = DatabaseEntry {
            name: key.clone(),
            dir: old,
            config: Some(build.config.clone()),
            state: DbState::at_rest(Some(&build.config)),
        };
        inner.entries.insert(key.clone(), entry.clone());
        tracing::info!(
            target: "graphersal_server",
            "database '{key}' replaced (the old one moved to {})",
            trash.display()
        );
        Ok((entry, trash))
    }

    /// A free `trash/<key>-<UTC time>` path (`-2`, `-3`, ... on a clash; dashes instead of
    /// colons for Windows), its parent made.
    fn trash_target(&self, key: &str) -> Result<PathBuf, ServerError> {
        let trash_root = self.dir.trash();
        fs::create_dir_all(&trash_root)
            .map_err(|err| ServerError::io("create directory", &trash_root, err))?;
        let stamp = crate::database::utc_now().replace(':', "-");
        let mut target = trash_root.join(format!("{key}-{stamp}"));
        let mut n = 1;
        while target.exists() {
            n += 1;
            target = trash_root.join(format!("{key}-{stamp}-{n}"));
        }
        Ok(target)
    }

    /// Lets the database `with` take the place of the database `name` (round 3 B-CRASH `db
    /// swap`, [`crate::ops::Admin::swap`]): `databases/<name>/` moves into
    /// `trash/<name>-<UTC time>/`, `databases/<with>/` is renamed to `databases/<name>/` (if
    /// that fails the old directory is moved back) and gets the old `database.toml` back
    /// (byte for byte: description, flags, limits); the name `with` leaves the registry, and
    /// `after` runs with it under the write lock (the server removes its grants there). Answers
    /// the new entry of `name`, the trash directory and `after`'s result. Refused while either
    /// is loaded or being loaded (the caller holding the loader makes sure they stay so), being
    /// built or replaced, for anything but plain normal databases, and while an alias names
    /// `with`.
    pub fn swap_in<T>(
        &self,
        name: &str,
        with: &str,
        after: impl FnOnce(&str) -> T,
    ) -> Result<(DatabaseEntry, PathBuf, T), ServerError> {
        let (key, other) = (fold(name), fold(with));
        let mut inner = self.write();
        let old = inner
            .entries
            .get(&key)
            .ok_or_else(|| not_found(name))?
            .clone();
        let new = inner
            .entries
            .get(&other)
            .ok_or_else(|| not_found(with))?
            .clone();
        let refuse = |name: &str, message: String| ServerError::Refused {
            name: name.to_string(),
            message,
        };
        for entry in [&old, &new] {
            if entry.state.is_busy() {
                return Err(refuse(
                    &entry.name,
                    format!("it is {}; unload it first", entry.state.as_str()),
                ));
            }
            if inner.pending.contains(&entry.name) {
                return Err(refuse(
                    &entry.name,
                    "it is being built or replaced; try again when that ends".to_string(),
                ));
            }
            let plain = entry
                .config
                .as_ref()
                .is_some_and(|c| c.kind == DatabaseKind::Plain && c.role == DatabaseRole::Normal);
            if !plain {
                return Err(refuse(
                    &entry.name,
                    "only a plain normal database takes part in a swap (a layer, a base or a                      failed database does not)"
                        .to_string(),
                ));
            }
        }
        let aliases: Vec<&str> = inner
            .aliases
            .iter()
            .filter(|(_, target)| **target == other)
            .map(|(alias, _)| alias.as_str())
            .collect();
        if !aliases.is_empty() {
            return Err(refuse(
                &other,
                format!(
                    "the aliases {} name it, and its name goes away in the swap; remove them or                      point them at another database first",
                    aliases.join(", ")
                ),
            ));
        }
        let config_path = old.dir.join(crate::database::DATABASE_FILE);
        let config_bytes = crate::layout::read_replaced_bytes(&config_path)
            .map_err(|err| ServerError::io("read", &config_path, err))?;
        let trash = self.trash_target(&key)?;
        fs::rename(&old.dir, &trash).map_err(|err| ServerError::io("rename", &old.dir, err))?;
        if let Err(err) = fs::rename(&new.dir, &old.dir) {
            if let Err(back) = fs::rename(&trash, &old.dir) {
                tracing::error!(
                    target: "graphersal_server",
                    "database '{key}': cannot move {} back after a failed swap: {back}; it is \
                     kept there",
                    trash.display()
                );
            }
            return Err(ServerError::io("rename", &new.dir, err));
        }
        if let Err(err) = crate::layout::write_atomic(
            &old.dir.join(crate::database::DATABASE_FILE),
            &config_bytes,
        ) {
            // The swapped-in database keeps its own (valid) configuration.
            tracing::warn!(
                target: "graphersal_server",
                "database '{key}': the old database.toml could not be put back: {err}"
            );
        }
        let config = DatabaseConfig::read(&old.dir).ok().or(old.config.clone());
        inner.entries.remove(&other);
        let entry = DatabaseEntry {
            name: key.clone(),
            dir: old.dir.clone(),
            state: DbState::at_rest(config.as_ref()),
            config,
        };
        inner.entries.insert(key.clone(), entry.clone());
        tracing::info!(
            target: "graphersal_server",
            "database '{other}' took the name '{key}' (the old one moved to {})",
            trash.display()
        );
        let out = after(&other);
        Ok((entry, trash, out))
    }

    /// Moves the database `name` out of the registry into `trash/<name>-<UTC time>/` (a
    /// counter is added when that exists) and answers its name and trash directory. Refused
    /// while it is loaded or being loaded (the caller holding the loader makes sure it stays
    /// so: [`crate::ops::Admin::delete`]), while an alias names it (no alias is removed
    /// implicitly) and for a database that layers name as their base. A failed database may be
    /// deleted.
    pub fn delete(&self, name: &str) -> Result<(String, PathBuf), ServerError> {
        self.delete_with(name, |_| ())
            .map(|(name, trash, ())| (name, trash))
    }

    /// [`Registry::delete`], then `after` with the deleted (folded) name, still under the
    /// registry's write lock (the server removes the database's grants there, so a new database
    /// or alias of that name cannot take the name before they are gone): its result beside the
    /// name and the trash path.
    pub fn delete_with<T>(
        &self,
        name: &str,
        after: impl FnOnce(&str) -> T,
    ) -> Result<(String, PathBuf, T), ServerError> {
        let key = fold(name);
        let mut inner = self.write();
        let entry = inner.entries.get(&key).ok_or_else(|| not_found(name))?;
        let refuse = |message: String| ServerError::Refused {
            name: key.clone(),
            message,
        };
        if entry.state.is_busy() {
            return Err(refuse(format!(
                "it is {}; unload it first",
                entry.state.as_str()
            )));
        }
        let aliases: Vec<&str> = inner
            .aliases
            .iter()
            .filter(|(_, target)| **target == key)
            .map(|(alias, _)| alias.as_str())
            .collect();
        if !aliases.is_empty() {
            return Err(refuse(format!(
                "the aliases {} name it; remove them or point them at another database first",
                aliases.join(", ")
            )));
        }
        let layers: Vec<&str> = inner
            .entries
            .values()
            .filter(|e| {
                e.config.as_ref().is_some_and(|c| {
                    c.kind == DatabaseKind::Layer && c.base.as_deref() == Some(key.as_str())
                })
            })
            .map(|e| e.name.as_str())
            .collect();
        if !layers.is_empty() {
            return Err(refuse(format!(
                "it is the base of the layers {}; delete them first",
                layers.join(", ")
            )));
        }
        let source = entry.dir.clone();
        let target = self.trash_target(&key)?;
        fs::rename(&source, &target).map_err(|err| ServerError::io("rename", &source, err))?;
        inner.entries.remove(&key);
        tracing::info!(
            target: "graphersal_server",
            "database '{key}' moved to {}",
            target.display()
        );
        let out = after(&key);
        Ok((key, target, out))
    }
}

/// Moves `build` into `databases/` under the held write lock.
fn commit_build(
    databases: &Path,
    inner: &mut Inner,
    build: &TmpBuild,
) -> Result<DatabaseEntry, ServerError> {
    check_new(inner, &build.name, &build.config)?;
    if !build.store_dir().is_dir() {
        return Err(ServerError::Refused {
            name: build.name.clone(),
            message: format!("the build has no Store ({STORE_DIR}/ is missing)"),
        });
    }
    fs::create_dir_all(databases)
        .map_err(|err| ServerError::io("create directory", databases, err))?;
    let target = databases.join(&build.name);
    if target.exists() {
        return Err(ServerError::NameInUse {
            name: build.name.clone(),
            holder: "directory in databases/",
        });
    }
    build.config.write(&build.dir)?;
    fs::rename(&build.dir, &target).map_err(|err| ServerError::io("rename", &build.dir, err))?;
    let entry = DatabaseEntry {
        name: build.name.clone(),
        dir: target,
        config: Some(build.config.clone()),
        state: DbState::at_rest(Some(&build.config)),
    };
    inner.entries.insert(entry.name.clone(), entry.clone());
    tracing::info!(target: "graphersal_server", "database '{}' registered", entry.name);
    Ok(entry)
}

/// The checks of a new database against the registry: its name is free, a layer's base is a
/// base database.
fn check_new(inner: &Inner, name: &str, config: &DatabaseConfig) -> Result<(), ServerError> {
    if let Some(holder) = inner.holder_of(name) {
        return Err(ServerError::NameInUse {
            name: name.to_string(),
            holder,
        });
    }
    if config.kind == DatabaseKind::Layer
        && let Some(problem) = config.base.as_deref().and_then(|b| inner.base_problem(b))
    {
        return Err(ServerError::Refused {
            name: name.to_string(),
            message: problem,
        });
    }
    Ok(())
}

/// A renamed alias ([`Registry::alias_rename`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Renamed<T> {
    /// The old name (folded).
    pub old: String,
    /// The new name.
    pub new: String,
    /// The database the alias names.
    pub target: String,
    /// What the `moved` callback returned.
    pub moved: T,
    /// The grants on the alias that moved to the new name with it ([`crate::ops::Admin::alias_rename`];
    /// 0 from the registry itself).
    pub grants: usize,
    /// The grants left on the new name (from a deleted database or alias) removed before the
    /// alias took it (the `claim` of [`Registry::alias_rename_with`]).
    pub leftover: usize,
}

/// Refuses an alias change based on another revision of the aliases than theirs now.
fn check_aliases(inner: &Inner, expected: Option<&str>) -> Result<(), ServerError> {
    if expected.is_none() {
        return Ok(());
    }
    crate::revision::check(expected, &aliases_revision(&inner.aliases), "aliases")
}

fn refuse_broken_aliases(inner: &Inner) -> Result<(), ServerError> {
    match &inner.aliases_broken {
        Some(reason) => Err(ServerError::Refused {
            name: "aliases.toml".to_string(),
            message: format!("{reason}; fix the file and restart the server"),
        }),
        None => Ok(()),
    }
}

/// Why the database with `config` in `state` cannot become a base, or `None` when it can.
pub(crate) fn promote_refusal(config: &DatabaseConfig, state: &DbState) -> Option<String> {
    if config.kind == DatabaseKind::Layer {
        return Some(
            "a layer cannot become a base (its data lives over another base); export it \
             materialized and import the file with role = base"
                .to_string(),
        );
    }
    if config.role == DatabaseRole::Base {
        return Some("it is a base already".to_string());
    }
    match state {
        DbState::Unloaded => None,
        DbState::Loading | DbState::Loaded | DbState::Maintenance { .. } => {
            Some("it is loaded; unload it first".to_string())
        }
        DbState::NeedsModel { repo } => Some(format!(
            "it needs the embedding model '{repo}'; add it first (a base is frozen, so no model \
             can be added to it later)"
        )),
        DbState::Failed { reason } => Some(format!("it is failed ({reason})")),
    }
}

fn not_found(name: &str) -> ServerError {
    ServerError::NotFound {
        name: name.to_string(),
    }
}

/// A new database being built in `tmp/` ([`Registry::prepare_tmp`]); dropping it without
/// [`Registry::commit`] removes its directory and frees its name.
#[derive(Debug)]
pub struct TmpBuild {
    registry: Arc<Registry>,
    name: String,
    config: DatabaseConfig,
    dir: PathBuf,
    done: bool,
    /// A replacement of an existing database ([`Registry::prepare_replace`]).
    replace: bool,
}

impl TmpBuild {
    /// The new database's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The configuration [`Registry::commit`] writes.
    pub fn config(&self) -> &DatabaseConfig {
        &self.config
    }

    /// The build directory in `tmp/`.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where the caller writes the new Store (`<build>/store/`, not created here).
    pub fn store_dir(&self) -> PathBuf {
        self.dir.join(STORE_DIR)
    }

    /// Changes the configuration the commit writes (a restore learns a backup's kind from
    /// the restored Store); the commit checks it again.
    pub(crate) fn config_mut(&mut self) -> &mut DatabaseConfig {
        &mut self.config
    }

    /// Whether this build replaces an existing database ([`Registry::prepare_replace`]).
    pub fn is_replacement(&self) -> bool {
        self.replace
    }
}

impl Drop for TmpBuild {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        if let Err(err) = fs::remove_dir_all(&self.dir)
            && self.dir.exists()
        {
            tracing::warn!(
                target: "graphersal_server",
                "cannot remove the build {}: {err} (removed at the next start)",
                self.dir.display()
            );
        }
        self.registry.write().pending.remove(&self.name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::tests::{add_db, server};
    use crate::test_support::TempDir;

    fn registry_with(temp: &TempDir, dbs: &[(&str, &str)]) -> Arc<Registry> {
        let dir = server(temp);
        for (name, config) in dbs {
            add_db(&dir, name, Some(config), true);
        }
        Registry::scan(&dir).unwrap()
    }

    #[test]
    fn get_resolve_and_list() {
        let temp = TempDir::new("admin-get");
        let registry = registry_with(&temp, &[("acme-2026.3", ""), ("b", "")]);
        registry.alias_set("acme", "acme-2026.3").unwrap();
        assert_eq!(registry.get("ACME-2026.3").unwrap().name, "acme-2026.3");
        assert!(matches!(
            registry.get("acme").unwrap_err(),
            ServerError::NotFound { .. }
        ));
        assert_eq!(registry.resolve("Acme").unwrap().name, "acme-2026.3");
        assert_eq!(registry.resolve("b").unwrap().name, "b");
        let err = registry.resolve("nope").unwrap_err();
        assert!(err.to_string().contains("'nope'"), "{err}");
        let names: Vec<String> = registry.list().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["acme-2026.3", "b"]);
    }

    #[test]
    fn aliases_share_the_namespace_and_name_only_databases() {
        let temp = TempDir::new("admin-alias");
        let registry = registry_with(&temp, &[("acme-2026.3", ""), ("acme-2026.4", "")]);
        registry.alias_set("acme", "acme-2026.3").unwrap();
        // A flip is one line.
        registry.alias_set("acme", "ACME-2026.4").unwrap();
        assert_eq!(registry.aliases()["acme"], "acme-2026.4");
        let text = fs::read_to_string(registry.dir().aliases_path()).unwrap();
        assert!(text.contains("acme = \"acme-2026.4\""), "{text}");

        // Case-insensitive clash with a database.
        let err = registry
            .alias_set("ACME-2026.3", "acme-2026.4")
            .unwrap_err();
        assert!(matches!(err, ServerError::InvalidName { .. }), "{err}");
        let err = registry
            .alias_set("acme-2026.3", "acme-2026.4")
            .unwrap_err();
        assert!(
            matches!(
                err,
                ServerError::NameInUse {
                    holder: "database",
                    ..
                }
            ),
            "{err}"
        );
        // Dangling, chains and cycles.
        let err = registry.alias_set("x", "gone").unwrap_err();
        assert!(matches!(err, ServerError::NotFound { .. }), "{err}");
        let err = registry.alias_set("y", "acme").unwrap_err();
        assert!(matches!(err, ServerError::Refused { .. }), "{err}");
        assert!(err.to_string().contains("another alias"), "{err}");
        let err = registry.alias_set("acme", "acme").unwrap_err();
        assert!(matches!(err, ServerError::Refused { .. }), "{err}");
        assert_eq!(registry.aliases().len(), 1);

        // A new database cannot take an alias's name, in any case.
        let err = registry
            .prepare_tmp("acme", DatabaseConfig::plain())
            .unwrap_err();
        assert!(
            matches!(
                err,
                ServerError::NameInUse {
                    holder: "alias",
                    ..
                }
            ),
            "{err}"
        );

        registry.alias_remove("ACME").unwrap();
        assert!(registry.aliases().is_empty());
        assert!(matches!(
            registry.alias_remove("acme").unwrap_err(),
            ServerError::NotFound { .. }
        ));
        // The file is the truth after a rescan.
        let again = Registry::scan(registry.dir()).unwrap();
        assert!(again.aliases().is_empty());
    }

    #[test]
    fn an_alias_rename_moves_the_name_and_keeps_the_database() {
        let temp = TempDir::new("admin-alias-rename");
        let registry = registry_with(&temp, &[("a", ""), ("b", "")]);
        registry.alias_set("prod", "a").unwrap();
        registry.alias_set("other", "b").unwrap();

        let renamed = registry
            .alias_rename("PROD", "live", |old, new| format!("{old}->{new}"))
            .unwrap();
        assert_eq!(renamed.old, "prod");
        assert_eq!(renamed.new, "live");
        assert_eq!(renamed.target, "a");
        assert_eq!(renamed.moved, "prod->live");
        assert_eq!(
            registry.aliases().get("live").map(String::as_str),
            Some("a")
        );
        assert!(!registry.aliases().contains_key("prod"));
        let again = Registry::scan(registry.dir()).unwrap();
        assert_eq!(again.aliases(), registry.aliases());

        // Refusals change nothing and never call back.
        let refused = |old: &str, new: &str| {
            registry
                .alias_rename(old, new, |_, _| panic!("called back"))
                .unwrap_err()
        };
        let err = refused("prod", "x");
        assert!(matches!(err, ServerError::NotFound { .. }), "{err}");
        let err = refused("a", "x");
        assert!(matches!(err, ServerError::Refused { .. }), "{err}");
        assert!(err.to_string().contains("not an alias"), "{err}");
        let err = refused("live", "Bad");
        assert!(matches!(err, ServerError::InvalidName { .. }), "{err}");
        for (taken, holder) in [("b", "database"), ("other", "alias"), ("live", "alias")] {
            let err = refused("live", taken);
            assert!(
                matches!(&err, ServerError::NameInUse { holder: h, .. } if *h == holder),
                "{err}"
            );
        }
        assert_eq!(registry.aliases().len(), 2);
        assert_eq!(registry.aliases()["live"], "a");
    }

    #[test]
    fn a_broken_alias_file_refuses_alias_changes() {
        let temp = TempDir::new("admin-alias-broken");
        let dir = server(&temp);
        add_db(&dir, "a", Some(""), true);
        fs::write(dir.aliases_path(), "x = [").unwrap();
        let registry = Registry::scan(&dir).unwrap();
        let err = registry.alias_set("b", "a").unwrap_err();
        assert!(matches!(err, ServerError::Refused { .. }), "{err}");
        // The hand-broken file is kept.
        assert_eq!(fs::read_to_string(dir.aliases_path()).unwrap(), "x = [");
    }

    #[test]
    fn flags_are_persisted() {
        let temp = TempDir::new("admin-flags");
        let registry = registry_with(&temp, &[("a", "description = \"kept\"")]);
        let entry = registry.set_auto_load("A", true).unwrap();
        assert!(entry.config.as_ref().unwrap().auto_load);
        registry.set_read_only("a", true).unwrap();
        let config = DatabaseConfig::read(&entry.dir).unwrap();
        assert!(config.auto_load && config.read_only);
        assert_eq!(config.description, "kept");
        assert!(matches!(
            registry.set_auto_load("nope", true).unwrap_err(),
            ServerError::NotFound { .. }
        ));
    }

    #[test]
    fn a_failed_entry_without_configuration_refuses_changes() {
        let temp = TempDir::new("admin-failed");
        let dir = server(&temp);
        add_db(&dir, "broken", Some("kind = "), true);
        let registry = Registry::scan(&dir).unwrap();
        let err = registry.set_read_only("broken", true).unwrap_err();
        assert!(matches!(err, ServerError::DatabaseFailed { .. }), "{err}");
    }

    #[test]
    fn a_base_stays_read_only_while_layers_name_it() {
        let temp = TempDir::new("admin-base");
        let registry = registry_with(&temp, &[("plain", "")]);
        let base = registry
            .prepare_tmp(
                "base",
                DatabaseConfig::plain()
                    .with_role(DatabaseRole::Base)
                    .with_read_only(false),
            )
            .unwrap();
        fs::create_dir(base.store_dir()).unwrap();
        let entry = registry.commit(base).unwrap();
        // A base is created read-only.
        assert!(entry.config.as_ref().unwrap().read_only);

        // Layers only over a base database.
        let err = registry
            .prepare_tmp("bad-layer", DatabaseConfig::layer("plain"))
            .unwrap_err();
        assert!(err.to_string().contains("not a base database"), "{err}");
        let err = registry
            .prepare_tmp("bad-layer", DatabaseConfig::layer("gone"))
            .unwrap_err();
        assert!(err.to_string().contains("not a database"), "{err}");

        // Without layers the flag may be turned off.
        registry.set_read_only("base", false).unwrap();
        registry.set_read_only("base", true).unwrap();
        let layer = registry
            .prepare_tmp("layer", DatabaseConfig::layer("base"))
            .unwrap();
        fs::create_dir(layer.store_dir()).unwrap();
        registry.commit(layer).unwrap();
        let err = registry.set_read_only("base", false).unwrap_err();
        assert!(matches!(err, ServerError::Refused { .. }), "{err}");
        assert!(err.to_string().contains("base of layers"), "{err}");
        assert!(registry.get("base").unwrap().config.unwrap().read_only);
        // Turning it on stays allowed.
        registry.set_read_only("base", true).unwrap();
    }

    #[test]
    fn builds_commit_atomically_and_debris_is_cleaned() {
        let temp = TempDir::new("admin-build");
        let registry = registry_with(&temp, &[]);
        let dir = registry.dir().clone();

        let build = registry
            .prepare_tmp("new-db", DatabaseConfig::plain().with_description("d"))
            .unwrap();
        assert!(build.dir().starts_with(dir.tmp()));
        // The name is reserved while it is built.
        let err = registry
            .prepare_tmp("NEW-DB", DatabaseConfig::plain())
            .unwrap_err();
        assert!(matches!(err, ServerError::InvalidName { .. }), "{err}");
        let err = registry
            .prepare_tmp("new-db", DatabaseConfig::plain())
            .unwrap_err();
        assert!(
            matches!(
                err,
                ServerError::NameInUse {
                    holder: "database being built",
                    ..
                }
            ),
            "{err}"
        );
        assert!(registry.list().is_empty());
        fs::create_dir(build.store_dir()).unwrap();
        fs::write(build.store_dir().join("GRAPH"), "synthetic").unwrap();
        let entry = registry.commit(build).unwrap();
        assert_eq!(entry.state, DbState::Unloaded);
        assert_eq!(entry.dir, dir.databases().join("new-db"));
        assert!(entry.store_dir().join("GRAPH").is_file());
        assert_eq!(fs::read_dir(dir.tmp()).unwrap().count(), 0);
        // A rescan finds the same entry.
        let again = Registry::scan(&dir).unwrap();
        assert_eq!(again.get("new-db").unwrap(), entry);

        // A build without a Store is refused and discarded.
        let empty = registry
            .prepare_tmp("empty", DatabaseConfig::plain())
            .unwrap();
        let err = registry.commit(empty).unwrap_err();
        assert!(err.to_string().contains("store/ is missing"), "{err}");
        assert_eq!(fs::read_dir(dir.tmp()).unwrap().count(), 0);
        assert!(registry.get("empty").is_err());
        // The name is free again.
        drop(
            registry
                .prepare_tmp("empty", DatabaseConfig::plain())
                .unwrap(),
        );
        assert_eq!(fs::read_dir(dir.tmp()).unwrap().count(), 0);

        // A crash between prepare and commit: the build is left behind (simulated by
        // forgetting it), and the next scan removes it.
        let crashed = registry
            .prepare_tmp("crashed", DatabaseConfig::plain())
            .unwrap();
        fs::create_dir(crashed.store_dir()).unwrap();
        std::mem::forget(crashed);
        assert_eq!(fs::read_dir(dir.tmp()).unwrap().count(), 1);
        let rescanned = Registry::scan(&dir).unwrap();
        assert_eq!(fs::read_dir(dir.tmp()).unwrap().count(), 0);
        assert!(rescanned.get("crashed").is_err());
        assert_eq!(rescanned.list().len(), 1);
    }

    #[test]
    fn an_existing_database_of_a_reserved_name_is_still_served_and_flipped_to() {
        // Made before `jobs` was reserved: it stays listed, loadable and an alias target;
        // only a new database or alias of that name is refused.
        let temp = TempDir::new("admin-reserved-existing");
        let registry = registry_with(&temp, &[("jobs", ""), ("a", "")]);
        assert_eq!(registry.get("jobs").unwrap().name, "jobs");
        registry.alias_set("cur", "jobs").unwrap();
        assert!(registry.alias_set("jobs", "a").is_err());
        let err = registry.alias_rename("cur", "jobs", |_, _| ()).unwrap_err();
        assert!(matches!(err, ServerError::InvalidName { .. }), "{err}");
        assert_eq!(registry.aliases()["cur"], "jobs");
    }

    #[test]
    fn invalid_configs_and_names_are_refused_before_anything_is_written() {
        let temp = TempDir::new("admin-invalid");
        let registry = registry_with(&temp, &[]);
        let err = registry
            .prepare_tmp("con", DatabaseConfig::plain())
            .unwrap_err();
        assert!(matches!(err, ServerError::InvalidName { .. }), "{err}");
        let mut config = DatabaseConfig::plain();
        config.base = Some("x".to_string());
        let err = registry.prepare_tmp("a", config).unwrap_err();
        assert!(matches!(err, ServerError::Refused { .. }), "{err}");
        assert_eq!(fs::read_dir(registry.dir().tmp()).unwrap().count(), 0);
    }
}
