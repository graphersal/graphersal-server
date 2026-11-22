//! The admin operations that move a database's data in and out of its Store: backups into
//! `backups/<name>/`, the restore of a backup (into the same name or a new one) and the export
//! as a packed snapshot (`.gsnap`). Part of [`Admin`] (module docs of [`crate::ops`]).
//!
//! - [`Admin::backup`]: a verified ZIP backup of the database's latest state (feature
//!   `persist-zip` of graphersal: every entry read back and checked) into
//!   `backups/<name>/<UTC time>.zip` (dashes instead of colons; `-2`, ... on a clash), written
//!   in `tmp/` and renamed into place, with a sidecar `<UTC time>.toml` holding the database's
//!   kind, role, base and description and, for a layer, its base's identity. A loaded database
//!   is backed up from its open Store (the context held, so an unload waits); an unloaded one
//!   from its files (no lock needed: the engine's "by another process" form). A layer's backup
//!   holds only the layer: it restores only over the same base.
//! - [`Admin::backups`]: the backups of a database name (also of a deleted database).
//! - [`Admin::restore`]: a backup into a new database (built in `tmp/`, renamed into
//!   `databases/`) or over the existing database of that name, which must be unloaded (built in
//!   `tmp/`; the old directory moves to `trash/`, then the build is renamed into its place). A
//!   layer's backup is checked against its base: the base database named by the sidecar (or,
//!   without one, any base database) must be a `role = "base"` database with the identity
//!   ([`BaseRef`]) the layer was built over; otherwise the restore is refused naming that
//!   identity.
//! - [`Admin::export`]: a packed snapshot of the latest state: a plain database (or a base) with
//!   its embedding models (default) or without them (the model definitions stay in the
//!   catalog, the blobs are left out); a layer as a LAYER snapshot (only its changes; it loads
//!   only over the same base and never carries models) or MATERIALIZED as a standalone full
//!   snapshot (portable; with the base's models unless left out).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use graphersal::persist::{
    ModelCopy, PersistError, RecoveryTarget, SnapshotInfo, Store, WriteOptions,
    write_graph_snapshot,
};
use graphersal::storage::{BaseRef, LayeredGraph};
use serde::{Deserialize, Serialize};

#[cfg(test)]
use super::FailPoint;
use super::{Admin, store_error};
use crate::database::{DatabaseConfig, DatabaseKind, DatabaseRole, utc_now};
use crate::error::ServerError;
use crate::loader::OpenStoreRef;
use crate::names::{fold, validate_name};
use crate::registry::DatabaseEntry;

/// The extension of a backup archive.
const BACKUP_EXT: &str = "zip";
/// The extension of a backup's sidecar.
const SIDECAR_EXT: &str = "toml";

/// One backup of a database ([`Admin::backup`], [`Admin::backups`]).
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BackupInfo {
    /// The database the backup was taken of.
    pub database: String,
    /// The backup's name: its UTC time with dashes (`2026-11-22T10-00-00Z`), the file stem.
    pub backup: String,
    /// The archive: `backups/<database>/<backup>.zip`.
    pub file: PathBuf,
    /// The archive's size in bytes.
    pub bytes: u64,
    /// When it was taken (RFC 3339 UTC), from the sidecar.
    pub created: Option<String>,
    /// The database's kind when it was taken (from the sidecar; `None` without one).
    pub kind: Option<DatabaseKind>,
    /// The database's role when it was taken (from the sidecar).
    pub role: Option<DatabaseRole>,
    /// A layer's base database name when it was taken (from the sidecar).
    pub base: Option<String>,
    /// A layer's base identity ([`BaseRef`] as text), from the sidecar.
    pub layer_base: Option<String>,
}

/// What [`Admin::restore`] did.
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Restored {
    /// The restored database (unloaded).
    pub entry: DatabaseEntry,
    /// The database whose backup was restored.
    pub from_database: String,
    /// The backup restored.
    pub backup: String,
    /// Where the replaced database went (`trash/<name>-<UTC time>/`), when the restore replaced
    /// an existing database.
    pub replaced: Option<PathBuf>,
}

/// What [`Admin::export`] writes.
///
/// `#[non_exhaustive]`: build it with [`ExportSpec::new`] and the `with_*` setters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExportSpec {
    /// Carry the embedding models (default `true`). A layer snapshot never carries models.
    pub models: bool,
    /// A layer as a standalone full snapshot instead of a layer snapshot (default `false`;
    /// no effect on a plain database).
    pub materialize: bool,
}

impl Default for ExportSpec {
    fn default() -> Self {
        Self::new()
    }
}

impl ExportSpec {
    /// With models, not materialized.
    pub fn new() -> Self {
        Self {
            models: true,
            materialize: false,
        }
    }

    /// The spec with `models`.
    pub fn with_models(mut self, models: bool) -> Self {
        self.models = models;
        self
    }

    /// The spec with `materialize`.
    pub fn with_materialize(mut self, materialize: bool) -> Self {
        self.materialize = materialize;
        self
    }
}

/// What [`Admin::export`] wrote.
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Exported {
    /// The database exported.
    pub database: String,
    /// Whether the file is a LAYER snapshot (it loads only over the same base).
    pub layer: bool,
    /// The base identity of a layer snapshot ([`BaseRef`] as text).
    pub layer_base: Option<String>,
    /// Whether the write was asked to carry the models (always `false` for a layer snapshot).
    pub models: bool,
    /// The model blobs the write could not carry (the database does not hold them).
    pub blobs_not_carried: Vec<String>,
    /// The bytes written.
    pub bytes: u64,
    /// The file name a download suggests: `<database>.gsnap`, `<database>.layer.gsnap`.
    pub file_name: String,
}

/// A backup's sidecar `<backup>.toml`: what the archive does not say.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Sidecar {
    database: String,
    created: String,
    kind: DatabaseKind,
    role: DatabaseRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    layer_base: Option<String>,
}

/// A temporary file (in `tmp/`, or a `.part` beside a target), removed on drop when it still
/// exists (a completed file was renamed away).
pub(crate) struct TmpFile {
    path: PathBuf,
}

impl TmpFile {
    /// A new unique path `tmp/<prefix>-<n>.<ext>` (not created).
    pub(crate) fn new(tmp: &Path, prefix: &str, ext: &str) -> Result<Self, ServerError> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        fs::create_dir_all(tmp).map_err(|err| ServerError::io("create directory", tmp, err))?;
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        Ok(Self {
            path: tmp.join(format!("{prefix}-{}-{n}.{ext}", std::process::id())),
        })
    }

    /// The path.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TmpFile {
    fn drop(&mut self) {
        if self.path.exists()
            && let Err(err) = fs::remove_file(&self.path)
        {
            tracing::warn!(
                target: "graphersal_server",
                "cannot remove {}: {err} (removed at the next start)",
                self.path.display()
            );
        }
    }
}

/// An export written into `tmp/` ([`Admin::export_to_tmp`]): the file is removed when this is
/// dropped (after the download).
pub(crate) struct ExportFile {
    /// What was written.
    pub(crate) exported: Exported,
    file: TmpFile,
}

impl ExportFile {
    /// The file in `tmp/`.
    pub(crate) fn path(&self) -> &Path {
        self.file.path()
    }
}

/// A writer counting the bytes it passes on.
struct Counting<W> {
    inner: W,
    bytes: u64,
}

impl<W: Write> Write for Counting<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.bytes += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Serializes the choice of a backup's final name (two backups in the same second).
static NAMING: Mutex<()> = Mutex::new(());

impl Admin {
    /// Backs up the database `name` into `backups/<name>/<UTC time>.zip` (module docs).
    ///
    /// `Err`: [`ServerError::NotFound`]; [`ServerError::DatabaseFailed`] for a database without
    /// configuration; [`ServerError::Store`] when the backup fails (damage found included:
    /// nothing is kept); [`ServerError::Io`].
    pub fn backup(&self, name: &str) -> Result<BackupInfo, ServerError> {
        let entry = self.registry.get(name)?;
        let config = config_of(&entry)?;
        let tmp = TmpFile::new(
            &self.registry.dir().tmp(),
            &format!("backup-{}", entry.name),
            BACKUP_EXT,
        )?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(tmp.path())
            .map_err(|err| ServerError::io("create", tmp.path(), err))?;
        let failed = |err: PersistError| store_error(&entry.name, &format!("backup failed: {err}"));
        let open = self
            .loader
            .as_ref()
            .and_then(|l| l.open_store_of(&entry.name));
        match open {
            // Loaded: its open Store; the context held meanwhile makes an unload wait.
            Some((_context, OpenStoreRef::Plain(store))) => {
                store.backup_zip(&mut file).map_err(failed)?;
            }
            Some((_context, OpenStoreRef::Layer(store))) => {
                store.backup_zip(&mut file).map_err(failed)?;
            }
            None => {
                Store::backup_zip_dir(entry.store_dir().as_path(), &mut file).map_err(failed)?;
            }
        }
        file.sync_all()
            .map_err(|err| ServerError::io("write", tmp.path(), err))?;
        drop(file);
        let layer_base = match config.kind {
            DatabaseKind::Layer => Store::inspect(entry.store_dir().as_path())
                .map_err(|err| store_error(&entry.name, &format!("cannot inspect it: {err}")))?
                .layer_base
                .map(|base| base.to_string()),
            DatabaseKind::Plain => None,
        };
        let created = utc_now();
        let sidecar = Sidecar {
            database: entry.name.clone(),
            created: created.clone(),
            kind: config.kind,
            role: config.role,
            base: config.base.clone(),
            description: config.description.clone(),
            layer_base,
        };
        let dir = self.backups_dir(&entry.name);
        fs::create_dir_all(&dir).map_err(|err| ServerError::io("create directory", &dir, err))?;
        let stamp = created.replace(':', "-");
        let _naming = NAMING
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut backup = stamp.clone();
        let mut n = 1;
        while dir.join(format!("{backup}.{BACKUP_EXT}")).exists() {
            n += 1;
            backup = format!("{stamp}-{n}");
        }
        let text = toml::to_string(&sidecar).map_err(|err| ServerError::Config {
            path: dir.join(format!("{backup}.{SIDECAR_EXT}")),
            message: err.to_string(),
        })?;
        crate::layout::write_atomic(
            &dir.join(format!("{backup}.{SIDECAR_EXT}")),
            text.as_bytes(),
        )?;
        let target = dir.join(format!("{backup}.{BACKUP_EXT}"));
        fs::rename(tmp.path(), &target)
            .map_err(|err| ServerError::io("rename", tmp.path(), err))?;
        drop(_naming);
        let info = backup_info(&entry.name, &backup, &target, Some(sidecar));
        tracing::info!(
            target: "graphersal_server",
            "database '{}' backed up into {} ({} bytes)",
            entry.name,
            target.display(),
            info.bytes
        );
        Ok(info)
    }

    /// The backups of the database name `name`, oldest first (module docs). The database need
    /// not exist any more; [`ServerError::NotFound`] when neither it nor a backup directory
    /// exists.
    pub fn backups(&self, name: &str) -> Result<Vec<BackupInfo>, ServerError> {
        let key = fold(name);
        validate_name(&key)?;
        let dir = self.backups_dir(&key);
        if !dir.is_dir() {
            return match self.registry.get(&key) {
                Ok(_) => Ok(Vec::new()),
                Err(err) => Err(err),
            };
        }
        let read = fs::read_dir(&dir).map_err(|err| ServerError::io("read", &dir, err))?;
        let mut out = Vec::new();
        for item in read {
            let item = item.map_err(|err| ServerError::io("read", &dir, err))?;
            let path = item.path();
            if path.extension().and_then(|e| e.to_str()) != Some(BACKUP_EXT) || !path.is_file() {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let sidecar = read_sidecar(&dir.join(format!("{stem}.{SIDECAR_EXT}")));
            out.push(backup_info(&key, stem, &path, sidecar));
        }
        out.sort_by(|a, b| a.backup.cmp(&b.backup));
        Ok(out)
    }

    /// Restores the backup `backup` (its name, or `<name>.zip`) of the database name `source`
    /// into the database `target` (default: `source`), module docs. Into a new name: a new
    /// database (`auto_load` off; an existing other database is [`ServerError::NameInUse`]).
    /// Over the existing database `source` itself: it must be unloaded; it keeps its flags and
    /// description, takes the backup's kind and base, and the old directory moves to `trash/`.
    ///
    /// `Err`: [`ServerError::BackupNotFound`]; [`ServerError::InvalidName`];
    /// [`ServerError::NameInUse`];
    /// [`ServerError::Refused`] (the target is loaded, is a base that layers name, a layer's
    /// base does not match); [`ServerError::Store`] (the archive does not restore);
    /// [`ServerError::Io`].
    pub fn restore(
        &self,
        source: &str,
        backup: &str,
        target: Option<&str>,
    ) -> Result<Restored, ServerError> {
        let source = fold(source);
        validate_name(&source)?;
        let backup = backup
            .strip_suffix(&format!(".{BACKUP_EXT}"))
            .unwrap_or(backup)
            .to_string();
        let dir = self.backups_dir(&source);
        let archive = dir.join(format!("{backup}.{BACKUP_EXT}"));
        let plain_name = !backup.is_empty()
            && !backup.starts_with('.')
            && backup
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if !plain_name || !archive.is_file() {
            return Err(ServerError::BackupNotFound {
                database: source,
                backup,
            });
        }
        let sidecar = read_sidecar(&dir.join(format!("{backup}.{SIDECAR_EXT}")));
        // The kind and the base are decided from the restored Store (a layer's base by its
        // identity): the build starts as a plain database.
        let named_base = sidecar.as_ref().and_then(|s| s.base.clone());
        let role = sidecar.as_ref().map(|s| s.role).unwrap_or_default();
        let target = fold(target.unwrap_or(&source));
        // Only a database's own backups replace it; another name must be new.
        let existing = self.registry.get(&target).ok().filter(|_| target == source);
        let mut build = match &existing {
            Some(entry) => {
                // A quick refusal before the work (the swap checks again).
                if let Some(loader) = &self.loader {
                    loader.while_idle(&entry.name, || Ok(()))?;
                }
                let mut config = entry.config.clone().unwrap_or_else(DatabaseConfig::plain);
                config.kind = DatabaseKind::Plain;
                config.base = None;
                config.role = role;
                config.needs_model = None;
                self.registry.prepare_replace(&target, config)?
            }
            None => {
                let mut config = DatabaseConfig::plain().with_role(role);
                if let Some(sidecar) = &sidecar {
                    config.description = sidecar.description.clone();
                }
                self.registry.prepare_tmp(&target, config)?
            }
        };
        let reader = File::open(&archive).map_err(|err| ServerError::io("read", &archive, err))?;
        Store::restore_zip(io::BufReader::new(reader), build.store_dir().as_path())
            .map_err(|err| store_error(&target, &format!("cannot restore {backup}: {err}")))?;
        let info = Store::inspect(build.store_dir().as_path()).map_err(|err| {
            store_error(
                &target,
                &format!("cannot inspect the restored Store: {err}"),
            )
        })?;
        match info.layer_base {
            Some(identity) => {
                let base =
                    self.matching_base(&target, &identity, named_base.as_deref(), "backup")?;
                let config = build.config_mut();
                config.kind = DatabaseKind::Layer;
                config.role = DatabaseRole::Normal;
                config.base = Some(base);
            }
            None => {
                let config = build.config_mut();
                config.kind = DatabaseKind::Plain;
                config.base = None;
            }
        }
        self.adopt_build(&target, &build.store_dir());
        #[cfg(test)]
        match self.fail {
            Some(FailPoint::AfterStore) => return Err(store_error(&target, "injected failure")),
            Some(FailPoint::CrashAfterStore) => {
                std::mem::forget(build);
                return Err(store_error(&target, "injected crash"));
            }
            None => {}
        }
        let (entry, replaced) = if build.is_replacement() {
            let swap = || self.registry.commit_replace(build);
            let (entry, trash) = match &self.loader {
                Some(loader) => loader.while_idle(&target, swap)?,
                None => swap()?,
            };
            (entry, Some(trash))
        } else {
            (self.registry.commit(build)?, None)
        };
        tracing::info!(
            target: "graphersal_server",
            "backup {backup} of '{source}' restored into '{}'{}",
            entry.name,
            if replaced.is_some() { " (replaced)" } else { "" }
        );
        Ok(Restored {
            entry,
            from_database: source,
            backup,
            replaced,
        })
    }

    /// Exports the database `name` as a packed snapshot into `writer` (module docs).
    ///
    /// `Err`: [`ServerError::NotFound`]; [`ServerError::DatabaseFailed`];
    /// [`ServerError::Store`] when the Store cannot be read or the write fails.
    pub fn export(
        &self,
        name: &str,
        spec: ExportSpec,
        writer: impl Write,
    ) -> Result<Exported, ServerError> {
        let entry = self.registry.get(name)?;
        let config = config_of(&entry)?;
        let layer = config.kind == DatabaseKind::Layer;
        let layer_snapshot = layer && !spec.materialize;
        let models = if spec.models && !layer_snapshot {
            ModelCopy::Include
        } else {
            ModelCopy::Exclude
        };
        let options = WriteOptions::new().with_materialize(layer && spec.materialize);
        let mut out = Counting {
            inner: io::BufWriter::new(writer),
            bytes: 0,
        };
        let failed = |err: PersistError| store_error(&entry.name, &format!("export failed: {err}"));
        let open = self
            .loader
            .as_ref()
            .and_then(|l| l.open_store_of(&entry.name));
        let info: SnapshotInfo = match open {
            // Loaded: the open graph (its latest commit, the WAL after the last snapshot
            // included), under its read lock; the context held makes an unload wait.
            Some((_context, OpenStoreRef::Plain(store))) => {
                write_graph_snapshot(store.graph(), &mut out, &options, models).map_err(failed)?
            }
            Some((_context, OpenStoreRef::Layer(store))) => {
                write_graph_snapshot(store.graph(), &mut out, &options, models).map_err(failed)?
            }
            None if layer => {
                let base_name = config.base.clone().unwrap_or_default();
                let base = self.base_of(&base_name)?;
                let recovered = Store::<LayeredGraph>::open_layer_read_only(
                    entry.store_dir().as_path(),
                    &base,
                    RecoveryTarget::Latest,
                )
                .map_err(failed)?;
                write_graph_snapshot(&recovered.into_graph(), &mut out, &options, models)
                    .map_err(failed)?
            }
            None => {
                let recovered =
                    Store::open_read_only(entry.store_dir().as_path(), RecoveryTarget::Latest)
                        .map_err(failed)?;
                write_graph_snapshot(&recovered.into_graph(), &mut out, &options, models)
                    .map_err(failed)?
            }
        };
        out.flush()
            .map_err(|err| store_error(&entry.name, &format!("export failed: {err}")))?;
        let exported = Exported {
            database: entry.name.clone(),
            layer: info.layer_base.is_some(),
            layer_base: info.layer_base.map(|b| b.to_string()),
            models: models == ModelCopy::Include,
            blobs_not_carried: info.blobs_not_carried.clone(),
            bytes: out.bytes,
            file_name: if info.layer_base.is_some() {
                format!("{}.layer.gsnap", entry.name)
            } else {
                format!("{}.gsnap", entry.name)
            },
        };
        tracing::info!(
            target: "graphersal_server",
            "database '{}' exported ({} bytes, {})",
            entry.name,
            exported.bytes,
            if exported.layer { "layer snapshot" } else { "full snapshot" }
        );
        Ok(exported)
    }

    /// [`Admin::export`] into the file `path`, written beside it as `<path>.part` and renamed
    /// when complete (a failed export leaves no file).
    pub fn export_to_file(
        &self,
        name: &str,
        spec: ExportSpec,
        path: &Path,
    ) -> Result<Exported, ServerError> {
        let mut part_name = path.file_name().unwrap_or_default().to_os_string();
        part_name.push(".part");
        let part = TmpFile {
            path: path.with_file_name(part_name),
        };
        let file =
            File::create(part.path()).map_err(|err| ServerError::io("create", part.path(), err))?;
        let exported = self.export(name, spec, &file)?;
        file.sync_all()
            .map_err(|err| ServerError::io("write", part.path(), err))?;
        drop(file);
        fs::rename(part.path(), path).map_err(|err| ServerError::io("rename", part.path(), err))?;
        Ok(exported)
    }

    /// [`Admin::export`] into a new file in `tmp/` (an HTTP download serves it, then drops it).
    pub(crate) fn export_to_tmp(
        &self,
        name: &str,
        spec: ExportSpec,
    ) -> Result<ExportFile, ServerError> {
        let tmp = TmpFile::new(
            &self.registry.dir().tmp(),
            &format!("export-{}", fold(name)),
            "gsnap",
        )?;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(tmp.path())
            .map_err(|err| ServerError::io("create", tmp.path(), err))?;
        let exported = self.export(name, spec, &file)?;
        Ok(ExportFile {
            exported,
            file: tmp,
        })
    }

    /// `backups/<name>/`.
    fn backups_dir(&self, name: &str) -> PathBuf {
        self.registry.dir().backups().join(name)
    }

    /// The base database for a layer over `identity` (`what`: "backup", "snapshot", "store",
    /// for the messages): `named` (a backup's sidecar base, an import's `base`) when it is a
    /// base database with that identity; without a name, the one base database with it.
    /// Refused otherwise, naming the identity.
    pub(super) fn matching_base(
        &self,
        target: &str,
        identity: &BaseRef,
        named: Option<&str>,
        what: &str,
    ) -> Result<String, ServerError> {
        let refuse = |message: String| ServerError::Refused {
            name: target.to_string(),
            message,
        };
        let bases: Vec<DatabaseEntry> = self
            .registry
            .list()
            .into_iter()
            .filter(|e| {
                e.config
                    .as_ref()
                    .is_some_and(|c| c.role == DatabaseRole::Base)
            })
            .collect();
        let matching = || {
            bases
                .iter()
                .filter(|e| self.base_identity(e).ok().as_ref() == Some(identity))
                .map(|e| e.name.clone())
                .collect::<Vec<_>>()
        };
        let hint = |found: Vec<String>| match found.as_slice() {
            [] => "no base database of this server has it; restore or import that base first"
                .to_string(),
            names => format!("the base databases {} have it", names.join(", ")),
        };
        match named {
            Some(name) => {
                let key = fold(name);
                let Some(entry) = bases.iter().find(|e| e.name == key) else {
                    return Err(refuse(format!(
                        "the {what} is a layer over the base {identity}, which names '{key}' as \
                         its base; '{key}' is not a base database of this server ({})",
                        hint(matching())
                    )));
                };
                let actual = self.base_identity(entry)?;
                if actual == *identity {
                    Ok(key)
                } else {
                    Err(refuse(format!(
                        "the {what} is a layer over the base {identity}, but the base database \
                         '{key}' is {actual} ({})",
                        hint(matching())
                    )))
                }
            }
            None => match matching().as_slice() {
                [one] => Ok(one.clone()),
                [] => Err(refuse(format!(
                    "the {what} is a layer over the base {identity}; {}",
                    hint(Vec::new())
                ))),
                several => Err(refuse(format!(
                    "the {what} is a layer over the base {identity}, which the base databases \
                     {} all have; name the base to use",
                    several.join(", ")
                ))),
            },
        }
    }

    /// The identity ([`BaseRef`]) of the base database `entry`: its pooled base's when it is
    /// in the pool, else read from its Store's files (lineage id, last commit and its time).
    fn base_identity(&self, entry: &DatabaseEntry) -> Result<BaseRef, ServerError> {
        if let Some(identity) = self
            .loader
            .as_ref()
            .and_then(|l| l.pooled_base(&entry.name))
            .and_then(|base| base.base_ref().copied())
        {
            return Ok(identity);
        }
        let info = Store::inspect(entry.store_dir().as_path())
            .map_err(|err| store_error(&entry.name, &format!("cannot inspect the base: {err}")))?;
        Ok(BaseRef::new(
            info.graph_id,
            info.commit_seq,
            info.last_commit_at,
        ))
    }
}

/// The configuration of `entry`, or [`ServerError::DatabaseFailed`].
fn config_of(entry: &DatabaseEntry) -> Result<DatabaseConfig, ServerError> {
    match (&entry.config, &entry.state) {
        (Some(config), _) => Ok(config.clone()),
        (None, state) => Err(ServerError::DatabaseFailed {
            name: entry.name.clone(),
            reason: state.to_string(),
        }),
    }
}

/// The sidecar at `path`, when it exists and reads (a broken one is logged and ignored: the
/// archive itself is what is restored).
fn read_sidecar(path: &Path) -> Option<Sidecar> {
    let text = fs::read_to_string(path).ok()?;
    match toml::from_str(&text) {
        Ok(sidecar) => Some(sidecar),
        Err(err) => {
            tracing::warn!(
                target: "graphersal_server",
                "ignoring the backup sidecar {}: {err}",
                path.display()
            );
            None
        }
    }
}

fn backup_info(database: &str, backup: &str, file: &Path, sidecar: Option<Sidecar>) -> BackupInfo {
    let bytes = fs::metadata(file).map(|m| m.len()).unwrap_or_default();
    let (created, kind, role, base, layer_base) = match sidecar {
        Some(s) => (
            Some(s.created),
            Some(s.kind),
            Some(s.role),
            s.base,
            s.layer_base,
        ),
        None => (None, None, None, None, None),
    };
    BackupInfo {
        database: database.to_string(),
        backup: backup.to_string(),
        file: file.to_path_buf(),
        bytes,
        created,
        kind,
        role,
        base,
        layer_base,
    }
}
