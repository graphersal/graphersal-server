//! The database registry of a server directory.
//!
//! There is NO registry file: the registry IS `databases/<name>/database.toml` (plus the Store
//! beside it in `store/`), so the registry and the data cannot disagree. Aliases are the only
//! cross-database file (`aliases.toml`, one line per alias: `acme = "acme-2026.3"`).
//!
//! [`Registry::scan`] reads it when the server starts (with the server lock held): it removes
//! the debris of interrupted builds in `tmp/`, reads every `databases/*/database.toml`, checks
//! that `store/` exists and that a layer's base is a `role = "base"` database, and reads
//! `aliases.toml`. A broken entry is listed in state [`DbState::Failed`] with the reason, a
//! broken alias is dropped and listed in [`Registry::problems`]: nothing in a database
//! directory makes the scan fail.
//!
//! The registry keeps each database's configuration and state in memory behind a
//! [`RwLock`], shared by the HTTP threads through an [`Arc`]. The admin functions (module
//! `admin`) change it: each writes its file first (atomically) and then the memory, under the
//! write lock, so the admin operations are serialized.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::database::{DatabaseConfig, DatabaseKind, DatabaseRole, STORE_DIR};
use crate::error::ServerError;
use crate::layout::{ServerDir, write_atomic};
use crate::names::{fold, validate_name};

/// The state of a database.
///
/// `#[non_exhaustive]`: further states may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DbState {
    /// Registered, not loaded (the state after the scan and after a create).
    Unloaded,
    /// Being loaded on a background thread.
    Loading,
    /// Loaded and served.
    Loaded,
    /// It cannot be loaded until the embedding model `repo` is added to the server.
    #[non_exhaustive]
    NeedsModel {
        /// The model's repository name.
        repo: String,
    },
    /// It cannot be served: its directory, configuration or Store is broken.
    #[non_exhaustive]
    Failed {
        /// Why.
        reason: String,
    },
}

impl DbState {
    /// [`DbState::NeedsModel`].
    pub fn needs_model(repo: impl Into<String>) -> Self {
        DbState::NeedsModel { repo: repo.into() }
    }

    /// The state of a database that is not loaded, by its configuration:
    /// [`DbState::NeedsModel`] when an import marked a missing model (`needs_model`), else
    /// [`DbState::Unloaded`].
    pub fn at_rest(config: Option<&DatabaseConfig>) -> Self {
        match config.and_then(|c| c.needs_model.as_deref()) {
            Some(repo) => DbState::needs_model(repo),
            None => DbState::Unloaded,
        }
    }

    /// [`DbState::Failed`].
    pub fn failed(reason: impl Into<String>) -> Self {
        DbState::Failed {
            reason: reason.into(),
        }
    }

    /// The state's name as the admin API shows it: `unloaded`, `loading`, `loaded`,
    /// `needs-model`, `failed`.
    pub fn as_str(&self) -> &'static str {
        match self {
            DbState::Unloaded => "unloaded",
            DbState::Loading => "loading",
            DbState::Loaded => "loaded",
            DbState::NeedsModel { .. } => "needs-model",
            DbState::Failed { .. } => "failed",
        }
    }
}

impl fmt::Display for DbState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbState::NeedsModel { repo } => write!(f, "needs-model ({repo})"),
            DbState::Failed { reason } => write!(f, "failed ({reason})"),
            other => f.write_str(other.as_str()),
        }
    }
}

/// One database of the registry: a snapshot (the registry hands out clones).
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DatabaseEntry {
    /// The name (= its directory's name under `databases/`).
    pub name: String,
    /// Its directory, `databases/<name>/`.
    pub dir: PathBuf,
    /// Its `database.toml`; `None` when the file is missing or does not parse (the state is
    /// then [`DbState::Failed`]).
    pub config: Option<DatabaseConfig>,
    /// Its state.
    pub state: DbState,
}

impl DatabaseEntry {
    /// Its Store's directory, `databases/<name>/store/`.
    pub fn store_dir(&self) -> PathBuf {
        self.dir.join(STORE_DIR)
    }
}

/// The registry of a server directory (module docs).
#[derive(Debug)]
pub struct Registry {
    pub(crate) dir: ServerDir,
    pub(crate) inner: RwLock<Inner>,
}

/// The registry's state behind its lock.
#[derive(Debug, Default)]
pub(crate) struct Inner {
    /// Every database, by name.
    pub(crate) entries: BTreeMap<String, DatabaseEntry>,
    /// Alias -> database name (every target is a database of `entries`).
    pub(crate) aliases: BTreeMap<String, String>,
    /// Why `aliases.toml` cannot be read (the aliases are then refused until it is fixed).
    pub(crate) aliases_broken: Option<String>,
    /// The names of the builds in `tmp/` (reserved until they are committed or dropped).
    pub(crate) pending: BTreeSet<String>,
    /// What the scan found broken besides the failed entries.
    pub(crate) problems: Vec<String>,
}

impl Inner {
    /// What holds `name` (folded) in the one namespace, if anything.
    pub(crate) fn holder_of(&self, name: &str) -> Option<&'static str> {
        if self.entries.contains_key(name) {
            Some("database")
        } else if self.aliases.contains_key(name) {
            Some("alias")
        } else if self.pending.contains(name) {
            Some("database being built")
        } else {
            None
        }
    }

    /// Whether a layer names `base` (any entry with a configuration, also a failed one).
    pub(crate) fn has_layers_over(&self, base: &str) -> bool {
        self.entries.values().any(|entry| {
            entry
                .config
                .as_ref()
                .is_some_and(|c| c.kind == DatabaseKind::Layer && c.base.as_deref() == Some(base))
        })
    }

    /// Why `base` cannot carry a layer, if it cannot.
    pub(crate) fn base_problem(&self, base: &str) -> Option<String> {
        match self.entries.get(&fold(base)) {
            None => Some(format!(
                "its base '{base}' is not a database of this server"
            )),
            Some(entry) => match &entry.config {
                Some(config) if config.role == DatabaseRole::Base => None,
                Some(_) => Some(format!(
                    "its base '{base}' is not a base database (role = \"base\")"
                )),
                None => Some(format!("its base '{base}' is failed")),
            },
        }
    }
}

impl Registry {
    /// Scans the server directory `dir` (module docs). Call it with the server lock held: it
    /// removes everything in `tmp/`. `Err` only when `databases/` itself cannot be read; a
    /// broken database or alias never makes it fail.
    pub fn scan(dir: &ServerDir) -> Result<Arc<Registry>, ServerError> {
        let mut inner = Inner::default();
        clean_tmp(&dir.tmp(), &mut inner.problems);
        scan_databases(&dir.databases(), &mut inner)?;
        scan_aliases(&dir.aliases_path(), &mut inner);
        for problem in &inner.problems {
            tracing::warn!(target: "graphersal_server", "registry: {problem}");
        }
        for entry in inner.entries.values() {
            if let DbState::Failed { reason } = &entry.state {
                tracing::warn!(
                    target: "graphersal_server",
                    "database '{}' is failed: {reason}",
                    entry.name
                );
            }
        }
        tracing::info!(
            target: "graphersal_server",
            "registry: {} databases, {} aliases",
            inner.entries.len(),
            inner.aliases.len()
        );
        Ok(Arc::new(Registry {
            dir: dir.clone(),
            inner: RwLock::new(inner),
        }))
    }

    /// An empty registry of `dir` (before its scan).
    pub(crate) fn empty(dir: &ServerDir) -> Arc<Registry> {
        Arc::new(Registry {
            dir: dir.clone(),
            inner: RwLock::new(Inner::default()),
        })
    }

    /// The server directory.
    pub fn dir(&self) -> &ServerDir {
        &self.dir
    }

    /// What the scan found broken besides the failed databases (a broken `aliases.toml`, an
    /// alias dropped, debris in `tmp/` that could not be removed).
    pub fn problems(&self) -> Vec<String> {
        self.read().problems.clone()
    }

    /// Sets the state of the database `name` (for the loader). `Err(NotFound)` when there is
    /// no such database.
    pub fn set_state(&self, name: &str, state: DbState) -> Result<(), ServerError> {
        let mut inner = self.write();
        let entry = inner
            .entries
            .get_mut(&fold(name))
            .ok_or_else(|| ServerError::NotFound {
                name: name.to_string(),
            })?;
        entry.state = state;
        Ok(())
    }

    /// The read lock (a poisoned lock is taken over: every change leaves the state consistent
    /// before anything that could panic).
    pub(crate) fn read(&self) -> RwLockReadGuard<'_, Inner> {
        self.inner.read().unwrap_or_else(|p| p.into_inner())
    }

    /// The write lock (see [`Self::read`]).
    pub(crate) fn write(&self) -> RwLockWriteGuard<'_, Inner> {
        self.inner.write().unwrap_or_else(|p| p.into_inner())
    }
}

/// Removes every entry of `tmp/` (debris of builds interrupted by a crash).
fn clean_tmp(tmp: &Path, problems: &mut Vec<String>) {
    let entries = match fs::read_dir(tmp) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return,
        Err(err) => {
            problems.push(format!("cannot read {}: {err}", tmp.display()));
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let removed = match entry.file_type() {
            Ok(kind) if kind.is_dir() => fs::remove_dir_all(&path),
            _ => fs::remove_file(&path),
        };
        match removed {
            Ok(()) => tracing::info!(
                target: "graphersal_server",
                "removed the debris {} of an interrupted build",
                path.display()
            ),
            Err(err) => problems.push(format!("cannot remove {}: {err}", path.display())),
        }
    }
}

/// Reads every `databases/<name>/` into `inner.entries`.
fn scan_databases(databases: &Path, inner: &mut Inner) -> Result<(), ServerError> {
    let listing = match fs::read_dir(databases) {
        Ok(listing) => listing,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(ServerError::io("read directory", databases, err)),
    };
    for item in listing {
        let item = item.map_err(|err| ServerError::io("read directory", databases, err))?;
        let path = item.path();
        if !item.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let Some(name) = item.file_name().to_str().map(str::to_string) else {
            inner.problems.push(format!(
                "{} is skipped: its name is not UTF-8",
                path.display()
            ));
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let entry = scan_entry(name, path);
        inner.entries.insert(entry.name.clone(), entry);
    }
    // The bases of the layers (all entries are known now).
    let problems: Vec<(String, String)> = inner
        .entries
        .values()
        .filter(|entry| entry.state == DbState::Unloaded)
        .filter_map(|entry| {
            let config = entry.config.as_ref()?;
            let base = config.base.as_deref()?;
            let problem = inner.base_problem(base)?;
            Some((entry.name.clone(), problem))
        })
        .collect();
    for (name, problem) in problems {
        if let Some(entry) = inner.entries.get_mut(&name) {
            entry.state = DbState::failed(problem);
        }
    }
    Ok(())
}

/// One database directory: its name, configuration and Store checked.
fn scan_entry(name: String, dir: PathBuf) -> DatabaseEntry {
    let config = DatabaseConfig::read(&dir);
    let state = if let Err(err) = validate_name(&name) {
        DbState::failed(err.to_string())
    } else if let Err(err) = &config {
        DbState::failed(err.to_string())
    } else if !dir.join(STORE_DIR).is_dir() {
        DbState::failed(format!("it has no Store ({STORE_DIR}/ is missing)"))
    } else {
        DbState::at_rest(config.as_ref().ok())
    };
    DatabaseEntry {
        name,
        dir,
        config: config.ok(),
        state,
    }
}

/// Reads `aliases.toml` into `inner.aliases`: an alias whose name is invalid or taken by a
/// database, or whose target is not a database, is dropped (listed in `inner.problems`).
fn scan_aliases(path: &Path, inner: &mut Inner) {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return,
        Err(err) => {
            let reason = format!("cannot read {}: {err}", path.display());
            inner.problems.push(reason.clone());
            inner.aliases_broken = Some(reason);
            return;
        }
    };
    let parsed: BTreeMap<String, String> = match toml::from_str(&text) {
        Ok(parsed) => parsed,
        Err(err) => {
            let reason = format!("{}: {}", path.display(), err.message());
            inner.problems.push(reason.clone());
            inner.aliases_broken = Some(reason);
            return;
        }
    };
    for (alias, target) in parsed {
        let target = fold(&target);
        let problem = if let Err(err) = validate_name(&alias) {
            Some(err.to_string())
        } else if inner.entries.contains_key(&alias) {
            Some("a database has this name".to_string())
        } else if !inner.entries.contains_key(&target) {
            Some(format!("its target '{target}' is not a database"))
        } else {
            None
        };
        match problem {
            Some(problem) => inner
                .problems
                .push(format!("alias '{alias}' is ignored: {problem}")),
            None => {
                inner.aliases.insert(alias, target);
            }
        }
    }
}

/// Writes `aliases` as `aliases.toml` (atomically).
pub(crate) fn write_aliases(
    path: &Path,
    aliases: &BTreeMap<String, String>,
) -> Result<(), ServerError> {
    let mut text = String::from("# alias = \"database\" (written by graphersal-server)\n");
    let body = toml::to_string(aliases).map_err(|err| ServerError::Config {
        path: path.to_path_buf(),
        message: err.to_string(),
    })?;
    text.push_str(&body);
    write_atomic(path, text.as_bytes())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::database::DATABASE_FILE;
    use crate::test_support::TempDir;

    /// A new server directory in `temp`.
    pub(crate) fn server(temp: &TempDir) -> ServerDir {
        ServerDir::init(temp.path().join("srv")).unwrap().dir
    }

    /// A database directory written by hand: `config` (the text of `database.toml`, `None` for
    /// no file) and a `store/` when `store`.
    pub(crate) fn add_db(dir: &ServerDir, name: &str, config: Option<&str>, store: bool) {
        let db = dir.databases().join(name);
        fs::create_dir_all(&db).unwrap();
        if let Some(text) = config {
            fs::write(db.join(DATABASE_FILE), text).unwrap();
        }
        if store {
            fs::create_dir_all(db.join(STORE_DIR)).unwrap();
        }
    }

    fn state_of(registry: &Registry, name: &str) -> DbState {
        registry.read().entries[name].state.clone()
    }

    #[test]
    fn scan_lists_good_and_broken_entries_and_never_fails() {
        let temp = TempDir::new("registry-scan");
        let dir = server(&temp);
        add_db(&dir, "good", Some("description = \"fine\""), true);
        add_db(&dir, "empty-config", Some(""), true);
        add_db(&dir, "no-store", Some(""), false);
        add_db(&dir, "no-config", None, true);
        add_db(&dir, "bad-toml", Some("kind = "), true);
        add_db(&dir, "unknown-key", Some("colour = \"red\""), true);
        add_db(&dir, "Upper", Some(""), true);
        add_db(
            &dir,
            "base",
            Some("role = \"base\"\nread_only = true"),
            true,
        );
        add_db(
            &dir,
            "layer",
            Some("kind = \"layer\"\nbase = \"base\""),
            true,
        );
        add_db(
            &dir,
            "orphan",
            Some("kind = \"layer\"\nbase = \"gone\""),
            true,
        );
        add_db(
            &dir,
            "over-normal",
            Some("kind = \"layer\"\nbase = \"good\""),
            true,
        );
        // Ignored: a file and a hidden directory.
        fs::write(dir.databases().join("notes.txt"), "x").unwrap();
        fs::create_dir(dir.databases().join(".cache")).unwrap();

        let registry = Registry::scan(&dir).unwrap();
        let inner = registry.read();
        assert_eq!(inner.entries.len(), 11);
        for name in ["good", "empty-config", "base", "layer"] {
            assert_eq!(inner.entries[name].state, DbState::Unloaded, "{name}");
        }
        assert_eq!(
            inner.entries["good"].config.as_ref().unwrap().description,
            "fine"
        );
        drop(inner);
        for (name, needle) in [
            ("no-store", "store/ is missing"),
            ("no-config", "cannot read"),
            ("bad-toml", "database.toml"),
            ("unknown-key", "unknown field"),
            ("Upper", "invalid name"),
            ("orphan", "'gone' is not a database"),
            ("over-normal", "not a base database"),
        ] {
            let state = state_of(&registry, name);
            assert_eq!(state.as_str(), "failed", "{name}");
            assert!(state.to_string().contains(needle), "{name}: {state}");
        }
        assert!(registry.read().entries["no-config"].config.is_none());
        assert!(registry.problems().is_empty(), "{:?}", registry.problems());
    }

    #[test]
    fn scan_of_a_missing_databases_directory_is_empty() {
        let temp = TempDir::new("registry-missing");
        let dir = server(&temp);
        fs::remove_dir(dir.databases()).unwrap();
        let registry = Registry::scan(&dir).unwrap();
        assert!(registry.read().entries.is_empty());
    }

    #[test]
    fn scan_drops_broken_aliases() {
        let temp = TempDir::new("registry-aliases");
        let dir = server(&temp);
        add_db(&dir, "acme-2026.3", Some(""), true);
        add_db(&dir, "other", Some(""), true);
        fs::write(
            dir.aliases_path(),
            "acme = \"ACME-2026.3\"\nother = \"acme-2026.3\"\ndangling = \"gone\"\n\"Bad\" = \"other\"\n",
        )
        .unwrap();
        let registry = Registry::scan(&dir).unwrap();
        let inner = registry.read();
        assert_eq!(inner.aliases.len(), 1);
        assert_eq!(inner.aliases["acme"], "acme-2026.3");
        assert_eq!(inner.problems.len(), 3, "{:?}", inner.problems);
        assert!(inner.aliases_broken.is_none());
        drop(inner);

        fs::write(dir.aliases_path(), "acme = 5").unwrap();
        let registry = Registry::scan(&dir).unwrap();
        assert!(registry.read().aliases.is_empty());
        assert!(registry.read().aliases_broken.is_some());
        assert_eq!(registry.problems().len(), 1);
    }

    #[test]
    fn scan_removes_tmp_debris() {
        let temp = TempDir::new("registry-debris");
        let dir = server(&temp);
        let debris = dir.tmp().join("build-x-0");
        fs::create_dir_all(debris.join(STORE_DIR)).unwrap();
        fs::write(debris.join(DATABASE_FILE), "").unwrap();
        fs::write(dir.tmp().join("upload.part"), "partial").unwrap();
        let registry = Registry::scan(&dir).unwrap();
        assert_eq!(fs::read_dir(dir.tmp()).unwrap().count(), 0);
        assert!(registry.read().entries.is_empty());
    }

    #[test]
    fn states_show_their_details() {
        assert_eq!(DbState::Unloaded.to_string(), "unloaded");
        assert_eq!(DbState::Loading.as_str(), "loading");
        assert_eq!(DbState::Loaded.as_str(), "loaded");
        assert_eq!(
            DbState::needs_model("minishlab/potion").to_string(),
            "needs-model (minishlab/potion)"
        );
        assert_eq!(DbState::failed("boom").to_string(), "failed (boom)");
    }
}
