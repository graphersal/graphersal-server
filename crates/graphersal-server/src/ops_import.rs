//! The import of another database into the server directory ([`Admin::import`]): a new
//! database built in `tmp/` from a file or directory and renamed into `databases/` in one step
//! (a failure or a crash in between leaves no `databases/<name>`). Part of [`Admin`] (module
//! docs of [`crate::ops`]).
//!
//! Sources ([`ImportFormat`]):
//! - a packed snapshot (`.gsnap`; the kind is read from its manifest with
//!   [`read_snapshot_info`], never from the file name): a FULL snapshot becomes a plain database
//!   ([`Store::create_from_packed_dir`]: its embedding models come along when it carries them;
//!   a model it names without carrying it is taken from the server's shared `models/` when
//!   that holds it (hard-linked like any adopted blob), else the database gets the state
//!   `needs-model (<repo>)`, persisted as `needs_model` in its `database.toml`); a LAYER
//!   snapshot becomes a layer database over the base database with the snapshot's base
//!   identity ([`BaseRef`]): the one named by `base`, or the only base database with that
//!   identity; refused otherwise, naming the identity;
//! - a Store (a directory, or a single-file `.gstore`): copied file by file into a directory
//!   Store ([`Store::convert_with`], verified; its creation parameters come along); a layer
//!   Store is matched with its base like a layer snapshot; a model blob the source lacks is
//!   filled into the copy from `models/` (checked by its hash, then hard-linked to `models/`
//!   by the adoption; [`Imported::models_resolved`] names its model). A blob `models/` lacks
//!   too is left out of the copy (`ConvertOptions::with_skip_missing_blobs`: a Store without a
//!   model blob is a valid Store, as after a snapshot import), and the database gets the state
//!   `needs-model (<repo>)` like a snapshot (add the model to `models/`, which resolves it);
//!   a base is refused instead ([`ServerError::Refused`], as below). One `models/` holds
//!   damaged is [`ServerError::ModelDamaged`] naming the file, and a damaged blob of the source
//!   itself fails the copy's verification ([`ServerError::Store`]);
//! - GraphSON (`.json`, `.jsonl`, `.graphson`) or GraphML (`.xml`, `.graphml`): a new plain
//!   database holding the file's graph (the Store gets `server.toml`'s `[defaults]`); data
//!   GraphSON cannot hold is skipped and listed in the import's notes.
//!
//! Stored jobs (KBFIX, the import jobs check): a Store or snapshot carrying scheduled jobs that
//! write outside the server's backup root (`server.toml [jobs] backup_root`) gets them listed
//! in [`Imported::jobs_outside`] and the notes; they never run here (each load drops them),
//! unless the admin rebases jobs ([`Admin::rebasing_jobs`]): then each such backup job is
//! rewritten to `<backup root>/<database>/<job>` in the new Store before it appears.
//!
//! With [`DatabaseRole::Base`] ([`Admin::import_as`]) a PLAIN source (a full snapshot, a plain
//! Store, GraphSON, GraphML) becomes a base database (`role = "base"`, read-only, frozen for
//! good: layers are made over it); a layer source is refused (`InvalidInput`), and so is a
//! source naming an embedding model it does not carry and `models/` lacks (`Refused`: a base
//! is never written, so the model could not be added later; add the model to `models/` and
//! import again, or import it as an ordinary database, add the model, then promote it:
//! [`Admin::promote`]).
//!
//! Without a format hint, the kind is detected: a directory holding a Store; a file whose head
//! reads as a packed snapshot; a single-file Store; then by the extension; then by the first
//! character that is not white space (`{` / `[` GraphSON, `<` GraphML), which an upload in
//! `tmp/` (no extension) needs.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use graphersal::GraphSource;
use graphersal::io::{GraphSONImport, ImportReport};
use graphersal::persist::{
    BlobSource, ConvertOptions, ModelState, PersistError, ReadOptions, Store, StoreFailure,
    read_layer_snapshot, read_snapshot_info,
};
use graphersal::storage::{BaseRef, LayeredGraph};

#[cfg(test)]
use super::FailPoint;
use super::import_jobs::{JobOutside, JobsChecked};
use super::{Admin, store_error};
use crate::audit::AuditEvent;
use crate::database::{DatabaseConfig, DatabaseKind, DatabaseRole};
use crate::error::ServerError;
use crate::models::{ModelsDir, manifest_of};
use crate::registry::DatabaseEntry;

/// The format of an import source.
///
/// `#[non_exhaustive]`: further formats may be added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImportFormat {
    /// A packed snapshot (`.gsnap`), full or layer.
    Snapshot,
    /// A Store: a directory or a single-file `.gstore`.
    Store,
    /// GraphSON (1.0, 2.0, 3.0; line or wrapped form).
    GraphSon,
    /// GraphML.
    GraphMl,
}

impl ImportFormat {
    /// The format's name: `snapshot`, `store`, `graphson`, `graphml`.
    pub fn as_str(self) -> &'static str {
        match self {
            ImportFormat::Snapshot => "snapshot",
            ImportFormat::Store => "store",
            ImportFormat::GraphSon => "graphson",
            ImportFormat::GraphMl => "graphml",
        }
    }

    /// The format a hint names (case-insensitive): `snapshot` / `gsnap`, `store` / `gstore`,
    /// `graphson` / `json` / `jsonl`, `graphml` / `xml`; `None` for anything else.
    pub fn parse(hint: &str) -> Option<Self> {
        match hint.trim().to_ascii_lowercase().as_str() {
            "snapshot" | "gsnap" => Some(ImportFormat::Snapshot),
            "store" | "gstore" => Some(ImportFormat::Store),
            "graphson" | "json" | "jsonl" => Some(ImportFormat::GraphSon),
            "graphml" | "xml" => Some(ImportFormat::GraphMl),
            _ => None,
        }
    }
}

/// What [`Admin::import`] reads.
///
/// `#[non_exhaustive]`: build it with [`ImportSource::new`] and [`ImportSource::with_format`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ImportSource {
    /// The file or directory.
    pub path: PathBuf,
    /// The format, when the caller knows it (else detected; a snapshot is always checked).
    pub format: Option<ImportFormat>,
}

impl ImportSource {
    /// The source at `path`, its format detected.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            format: None,
        }
    }

    /// The source with a format hint.
    pub fn with_format(mut self, format: Option<ImportFormat>) -> Self {
        self.format = format;
        self
    }
}

/// What [`Admin::import`] did.
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Imported {
    /// The new database (`unloaded`, or `needs-model` when a model is missing and `models/`
    /// lacks it).
    pub entry: DatabaseEntry,
    /// The source's format.
    pub format: ImportFormat,
    /// A layer's base identity ([`BaseRef`] as text).
    pub layer_base: Option<String>,
    /// The embedding models (repositories) the database needs but does not keep (`models/`
    /// lacks them too).
    pub models_missing: Vec<String>,
    /// The embedding models (repositories) the source named without carrying them, taken
    /// from the server's shared `models/` at the import: a snapshot's model, or a model whose
    /// blob a source Store lacked (filled into the copy).
    pub models_resolved: Vec<String>,
    /// How many values, entries or elements the import skipped (GraphSON).
    pub skipped: usize,
    /// What the import noted (skipped or converted data, missing models), as sentences.
    pub notes: Vec<String>,
    /// How many grants left on the name (from a deleted database or alias) were removed
    /// before the new database took it (`Admin::commit_new`).
    pub leftover_grants: usize,
    /// The stored jobs writing outside the server's backup root (`server.toml [jobs]
    /// backup_root`): dropped at the first load unless the import rebased them
    /// ([`Admin::rebasing_jobs`], [`JobOutside::rebased_to`]).
    pub jobs_outside: Vec<JobOutside>,
}

/// What a build learned besides the Store.
#[derive(Default)]
struct Built {
    layer: Option<(BaseRef, String)>,
    models_missing: Vec<String>,
    models_resolved: Vec<String>,
    /// The repositories of the model blobs a Store import filled from `models/`.
    models_filled: Vec<String>,
    skipped: usize,
    notes: Vec<String>,
}

impl Admin {
    /// Imports `source` as the new database `name` (module docs). `base` names the base
    /// database of a layer snapshot or layer Store (default: the only base database with its
    /// base identity); it is refused for any other source. `description` is the new
    /// database's description.
    ///
    /// `Err`: [`ServerError::InvalidName`], [`ServerError::NameInUse`];
    /// [`ServerError::SourceNotFound`]; [`ServerError::UnsupportedSource`] (no format
    /// recognized); [`ServerError::InvalidInput`] (the source does not read as its format, a
    /// `base` for a plain source); [`ServerError::Refused`] (a layer without its base database:
    /// the message names the base identity); [`ServerError::Store`] (the Store cannot be
    /// written); [`ServerError::ModelDamaged`] (a source Store lacks a model blob that
    /// `models/` holds damaged; one `models/` lacks too makes the database `needs-model`);
    /// [`ServerError::Config`]; [`ServerError::Io`].
    pub fn import(
        &self,
        source: &ImportSource,
        name: &str,
        base: Option<&str>,
        description: Option<&str>,
    ) -> Result<Imported, ServerError> {
        self.import_as(source, name, base, description, DatabaseRole::Normal)
    }

    /// [`Admin::import`] with the new database's role: [`DatabaseRole::Base`] makes a base
    /// database (read-only, frozen) of a plain source (module docs).
    ///
    /// `Err`: as [`Admin::import`]; with [`DatabaseRole::Base`] also
    /// [`ServerError::InvalidInput`] for a layer source and [`ServerError::Refused`] for a
    /// source naming an embedding model it does not carry and `models/` lacks.
    pub fn import_as(
        &self,
        source: &ImportSource,
        name: &str,
        base: Option<&str>,
        description: Option<&str>,
        role: DatabaseRole,
    ) -> Result<Imported, ServerError> {
        let as_base = role == DatabaseRole::Base;
        let path = source.path.as_path();
        if !path.exists() {
            return Err(ServerError::SourceNotFound {
                path: path.to_path_buf(),
            });
        }
        let format = detect(path, source.format)?;
        // A new database starts with server.toml's [defaults] flags (round 4 B-SETTINGS).
        let defaults = self.registry.dir().load_config()?.defaults;
        let config = DatabaseConfig::plain()
            .with_description(description.unwrap_or_default())
            .with_auto_load(defaults.auto_load)
            .with_read_only(defaults.read_only);
        let mut build = self.registry.prepare_tmp(name, config)?;
        let store_dir = build.store_dir();
        let built = match format {
            ImportFormat::Snapshot => {
                self.import_snapshot(name, path, &store_dir, base, as_base)?
            }
            ImportFormat::Store => self.import_store(name, path, &store_dir, base, as_base)?,
            ImportFormat::GraphSon | ImportFormat::GraphMl => {
                if let Some(base) = base {
                    return Err(input(
                        name,
                        format!(
                            "base '{base}': only a layer snapshot or a layer Store is imported \
                             over a base; a {} file becomes a plain database",
                            format.as_str()
                        ),
                    ));
                }
                self.import_graph_file(name, path, &store_dir, format)?
            }
        };
        let config = build.config_mut();
        match &built.layer {
            Some((_, base)) => {
                config.kind = DatabaseKind::Layer;
                config.role = DatabaseRole::Normal;
                config.base = Some(base.clone());
            }
            None => {
                config.kind = DatabaseKind::Plain;
                config.base = None;
                if as_base {
                    config.role = DatabaseRole::Base;
                    config.read_only = true;
                }
            }
        }
        // A model it names but does not carry is taken from `models/` when that holds it
        // (the build is not served yet); the blobs it carries are shared through `models/`.
        let mut built = built;
        let held: Vec<String> = {
            let models = self.models();
            built
                .models_missing
                .iter()
                .filter(|repo| models.holds(repo))
                .cloned()
                .collect()
        };
        let mut resolution_notes = Vec::new();
        if held.is_empty() {
            resolution_notes.extend(self.adopt_build(name, &store_dir));
        } else {
            // `resolve` adopts every blob of the Store afterwards.
            let resolution = self.models().resolve(&store_dir, &held);
            resolution_notes.extend(
                resolution
                    .notes
                    .iter()
                    .map(|n| format!("database '{name}': {n}")),
            );
            resolution_notes.extend(resolution.adoption.warnings.iter().cloned());
            built
                .models_missing
                .retain(|repo| !resolution.resolved.contains(repo));
            for repo in &resolution.resolved {
                tracing::info!(
                    target: "graphersal_server",
                    "database '{name}': the embedding model '{repo}' taken from models/"
                );
            }
            built.models_resolved = resolution.resolved;
        }
        built
            .notes
            .extend(model_notes(&built.models_missing, &built.models_resolved));
        built.notes.extend(resolution_notes);
        if as_base && let Some(repo) = built.models_missing.first() {
            return Err(ServerError::Refused {
                name: name.to_string(),
                message: format!(
                    "the source names the embedding model '{repo}' without carrying it, the \
                     shared models lack it, and a base is frozen (no model can be added to it \
                     later)"
                ),
            });
        }
        // KBFIX: the stored jobs writing outside the backup root, reported (and rebased when
        // asked) before the database appears. A base never runs jobs.
        let jobs = match (&built.layer, format, as_base) {
            (_, ImportFormat::GraphSon | ImportFormat::GraphMl, _) | (None, _, true) => {
                JobsChecked::default()
            }
            (Some((_, base_name)), _, _) => {
                let base = self.base_of(base_name)?;
                self.check_import_jobs(name, &store_dir, Some(&base))?
            }
            (None, _, false) => self.check_import_jobs(name, &store_dir, None)?,
        };
        built.notes.extend(jobs.note.clone());
        built.notes.extend(jobs_notes(&jobs.outside));
        let config = build.config_mut();
        config.needs_model = built.models_missing.first().cloned();
        #[cfg(test)]
        match self.fail {
            Some(FailPoint::AfterStore) => return Err(store_error(name, "injected failure")),
            Some(FailPoint::CrashAfterStore) => {
                std::mem::forget(build);
                return Err(store_error(name, "injected crash"));
            }
            None => {}
        }
        let (entry, leftover_grants) = self.commit_new(build)?;
        self.audit().record(AuditEvent::DatabaseImported {
            name: entry.name.clone(),
            source: path.to_path_buf(),
            format: format.as_str().to_string(),
            state: entry.state.to_string(),
        });
        Ok(Imported {
            entry,
            format,
            layer_base: built.layer.map(|(identity, _)| identity.to_string()),
            models_missing: built.models_missing,
            models_resolved: dedup(
                built
                    .models_filled
                    .into_iter()
                    .chain(built.models_resolved)
                    .collect(),
            ),
            skipped: built.skipped,
            notes: built.notes,
            leftover_grants,
            jobs_outside: jobs.outside,
        })
    }

    /// A packed snapshot into `store_dir`: a full one as a plain Store, a layer one as a layer
    /// Store over its base database.
    fn import_snapshot(
        &self,
        name: &str,
        path: &Path,
        store_dir: &Path,
        base: Option<&str>,
        as_base: bool,
    ) -> Result<Built, ServerError> {
        let info = read_snapshot_info(BufReader::new(open(path)?))
            .map_err(|err| input(name, format!("not a packed snapshot: {err}")))?;
        let mut built = Built::default();
        match info.layer_base {
            Some(identity) => {
                refuse_layer_as_base(name, as_base, "a layer snapshot")?;
                let base_name = self.matching_base(name, &identity, base, "snapshot")?;
                let layer_base = self.base_of(&base_name)?;
                let graph = read_layer_snapshot(
                    BufReader::new(open(path)?),
                    &ReadOptions::default(),
                    &layer_base,
                )
                .map_err(|err| input(name, format!("the layer snapshot does not read: {err}")))?;
                Store::<LayeredGraph>::create_layer_with(
                    store_dir,
                    &layer_base,
                    graph.into_inner(),
                    self.store_options()?,
                )
                .and_then(Store::close)
                .map_err(|err| {
                    store_error(name, &format!("cannot write its layer Store: {err}"))
                })?;
                built.layer = Some((identity, base_name));
            }
            None => {
                refuse_base(name, base, "a full snapshot")?;
                let report = Store::create_from_packed_dir(
                    store_dir,
                    BufReader::new(open(path)?),
                    self.store_options()?,
                )
                .map_err(|err| snapshot_error(name, err))?;
                let mut missing: Vec<String> = report
                    .models_missing
                    .iter()
                    .map(|m| m.repo().unwrap_or(m.identity()).to_string())
                    .collect();
                missing.extend(missing_blobs(name, store_dir)?);
                built.models_missing = dedup(missing);
            }
        }
        Ok(built)
    }

    /// A Store directory or `.gstore` copied into the directory Store `store_dir`.
    fn import_store(
        &self,
        name: &str,
        path: &Path,
        store_dir: &Path,
        base: Option<&str>,
        as_base: bool,
    ) -> Result<Built, ServerError> {
        // A model blob the source lacks is filled from `models/` (laid out as
        // `blobs/<2 hex>/<hash>`) into the copy, checked by its hash; the filled file is
        // replaced by a hard link to `models/` afterwards (the adoption in `import_as`). A blob
        // `models/` lacks too is left out: `missing_blobs` below names its model, and the
        // database becomes `needs-model` (a base is refused in `import_as`).
        let models = self.models();
        let report = Store::convert_with(
            path,
            store_dir,
            ConvertOptions::new()
                .with_fill_missing_blobs(BlobSource::store(models.root()))
                .with_skip_missing_blobs(),
        )
        .map_err(|err| convert_error(name, path, &models, err))?;
        let info = Store::inspect(store_dir)
            .map_err(|err| store_error(name, &format!("cannot inspect the copy: {err}")))?;
        let mut built = Built::default();
        match info.layer_base {
            Some(identity) => {
                refuse_layer_as_base(name, as_base, "a layer Store")?;
                let base_name = self.matching_base(name, &identity, base, "store")?;
                built.layer = Some((identity, base_name));
            }
            None => {
                refuse_base(name, base, "a plain Store")?;
                built.models_missing = dedup(missing_blobs(name, store_dir)?);
            }
        }
        for blob in &report.blobs_filled {
            let model = filled_model(&models, blob);
            tracing::info!(
                target: "graphersal_server",
                "database '{name}': the blob of {model} filled from models/"
            );
            built.notes.push(format!(
                "the source Store lacks the blob of {model}: filled from the shared models"
            ));
            if let Some(repo) = model_repo(&models, blob) {
                built.models_filled.push(repo);
            }
        }
        built.models_filled = dedup(std::mem::take(&mut built.models_filled));
        Ok(built)
    }

    /// A GraphSON or GraphML file loaded into a new graph, written as a plain Store.
    fn import_graph_file(
        &self,
        name: &str,
        path: &Path,
        store_dir: &Path,
        format: ImportFormat,
    ) -> Result<Built, ServerError> {
        let reader = BufReader::with_capacity(128 * 1024, open(path)?);
        let (graph, report) = match format {
            ImportFormat::GraphMl => GraphSource::from_graphml_reader(reader)
                .map(|graph| (graph, ImportReport::default()))
                .map_err(|err| input(name, format!("the GraphML file does not read: {err}")))?,
            _ => GraphSONImport::new()
                .load(reader)
                .map_err(|err| input(name, format!("the GraphSON file does not read: {err}")))?,
        };
        Store::create_with(store_dir, graph.into_inner(), self.store_options()?)
            .and_then(Store::close)
            .map_err(|err| store_error(name, &format!("cannot create its Store: {err}")))?;
        let notes = report
            .notes
            .iter()
            .map(|note| {
                let mut text = format!("{}: {}", note.kind, note.count);
                if let Some(sample) = note.samples.first() {
                    text.push_str(&format!(" (first: {sample})"));
                }
                text
            })
            .collect();
        Ok(Built {
            layer: None,
            models_missing: Vec::new(),
            models_resolved: Vec::new(),
            models_filled: Vec::new(),
            skipped: report.skipped(),
            notes,
        })
    }
}

/// One note per job writing outside the backup root (KBFIX).
fn jobs_notes(outside: &[JobOutside]) -> Vec<String> {
    outside
        .iter()
        .map(|job| match &job.rebased_to {
            Some(to) => format!(
                "the backup job '{}' wrote '{}' outside the backup root: rebased to '{}', it runs \
                 there",
                job.job,
                job.path.display(),
                to.display()
            ),
            None => format!(
                "the job '{}' writes '{}' outside the backup root ({}): it never runs here and \
                 is dropped at the first load (import again with the jobs rebased, or define it \
                 again with a path inside the root)",
                job.job,
                job.path.display(),
                job.reason
            ),
        })
        .collect()
}

/// The format of the source at `path` (module docs); `hint` is trusted except for a snapshot
/// (always read) and a Store (always checked).
fn detect(path: &Path, hint: Option<ImportFormat>) -> Result<ImportFormat, ServerError> {
    let unsupported = |message: &str| ServerError::UnsupportedSource {
        path: path.to_path_buf(),
        message: message.to_string(),
    };
    let is_store = Store::is_store(path).unwrap_or(false);
    if path.is_dir() {
        return match hint {
            None | Some(ImportFormat::Store) if is_store => Ok(ImportFormat::Store),
            None | Some(ImportFormat::Store) => Err(unsupported("the directory holds no Store")),
            Some(other) => Err(unsupported(&format!(
                "a directory is not a {} file",
                other.as_str()
            ))),
        };
    }
    match hint {
        Some(ImportFormat::Store) if is_store => return Ok(ImportFormat::Store),
        Some(ImportFormat::Store) => return Err(unsupported("the file is not a .gstore Store")),
        Some(format) => return Ok(format),
        None => {}
    }
    let file = File::open(path).map_err(|err| ServerError::io("read", path, err))?;
    if read_snapshot_info(BufReader::new(file)).is_ok() {
        return Ok(ImportFormat::Snapshot);
    }
    if is_store {
        return Ok(ImportFormat::Store);
    }
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("json" | "jsonl" | "graphson") => return Ok(ImportFormat::GraphSon),
        Some("xml" | "graphml") => return Ok(ImportFormat::GraphMl),
        Some("gsnap") => {
            return Err(unsupported(
                "the file is named .gsnap but does not read as a packed snapshot",
            ));
        }
        _ => {}
    }
    match first_character(path)? {
        Some(b'{' | b'[') => Ok(ImportFormat::GraphSon),
        Some(b'<') => Ok(ImportFormat::GraphMl),
        _ => Err(unsupported("the format is not recognized")),
    }
}

/// The first byte of the file at `path` that is not white space (a UTF-8 byte order mark
/// skipped), from its first 64 KiB.
fn first_character(path: &Path) -> Result<Option<u8>, ServerError> {
    let mut head = Vec::new();
    open(path)?
        .take(64 * 1024)
        .read_to_end(&mut head)
        .map_err(|err| ServerError::io("read", path, err))?;
    let head = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&head);
    Ok(head.iter().copied().find(|b| !b.is_ascii_whitespace()))
}

fn open(path: &Path) -> Result<File, ServerError> {
    File::open(path).map_err(|err| ServerError::io("read", path, err))
}

/// The repositories of the models the Store in `dir` defines without their blobs.
fn missing_blobs(name: &str, dir: &Path) -> Result<Vec<String>, ServerError> {
    Ok(Store::models_dir(dir)
        .map_err(|err| store_error(name, &format!("cannot list its models: {err}")))?
        .into_iter()
        .filter(|model| matches!(model.state, ModelState::Missing))
        .map(|model| model.repo)
        .collect())
}

/// The error of a Store copy: a model blob neither the source nor `models/` holds
/// ([`ServerError::ModelUnavailable`]; not expected since the copy skips such a blob, kept for
/// a Store failure of that kind), one `models/` holds damaged
/// ([`ServerError::ModelDamaged`]), anything else the Store's.
fn convert_error(name: &str, source: &Path, models: &ModelsDir, err: PersistError) -> ServerError {
    match err {
        PersistError::Store {
            cause: StoreFailure::BlobUnavailable,
            reason,
            ..
        } => ServerError::ModelUnavailable {
            name: name.to_string(),
            // The source's catalog names the models whose blob it lacks (no lock taken).
            repos: Store::models_dir(source)
                .map(|models| {
                    dedup(
                        models
                            .into_iter()
                            .filter(|m| matches!(m.state, ModelState::Missing))
                            .map(|m| m.repo)
                            .collect(),
                    )
                })
                .unwrap_or_default(),
            message: reason,
        },
        PersistError::Store {
            cause: StoreFailure::BlobDamaged,
            reason,
            ..
        } => ServerError::ModelDamaged {
            name: name.to_string(),
            blob: blob_named(&reason).map(|hash| models.blob_path(hash)),
            message: reason,
        },
        other => store_error(name, &format!("cannot copy the Store: {other}")),
    }
}

/// The hash of the first `blobs/<2 hex>/<64 hex>` a Store's reason names.
fn blob_named(reason: &str) -> Option<&str> {
    reason.split_whitespace().find_map(|word| {
        let hash = word.strip_prefix("blobs/")?.split('/').nth(1)?;
        let hash = hash.get(..64)?;
        hash.bytes().all(|b| b.is_ascii_hexdigit()).then_some(hash)
    })
}

/// The repository of the model blob `blob` (`blobs/<2 hex>/<hash>`) as `models/` holds it.
fn model_repo(models: &ModelsDir, blob: &str) -> Option<String> {
    let hash = blob.rsplit('/').next().filter(|h| h.len() == 64)?;
    manifest_of(&models.blob_path(hash)).ok().flatten()?.repo
}

/// The model blob `blob` in words: `the embedding model '<repo>'`, else `blob <12 hex>`.
fn filled_model(models: &ModelsDir, blob: &str) -> String {
    match model_repo(models, blob) {
        Some(repo) => format!("the embedding model '{repo}'"),
        None => {
            let hash = blob.rsplit('/').next().unwrap_or(blob);
            format!("the blob {}", hash.get(..12).unwrap_or(hash))
        }
    }
}

fn dedup(mut repos: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    repos.retain(|repo| seen.insert(repo.clone()));
    repos
}

fn model_notes(missing: &[String], resolved: &[String]) -> Vec<String> {
    let missing = missing.iter().map(|repo| {
        format!(
            "the embedding model '{repo}' is named but not carried, and the shared models lack \
             it: the database is needs-model until it is added"
        )
    });
    let resolved = resolved.iter().map(|repo| {
        format!(
            "the embedding model '{repo}' is named but not carried: taken from the shared \
             models"
        )
    });
    missing.chain(resolved).collect()
}

/// `base` refused for a source that is not a layer.
fn refuse_base(name: &str, base: Option<&str>, what: &str) -> Result<(), ServerError> {
    match base {
        Some(base) => Err(input(
            name,
            format!(
                "base '{base}': the source is {what}, not a layer; only a layer snapshot or a \
                 layer Store is imported over a base"
            ),
        )),
        None => Ok(()),
    }
}

/// A layer source refused as a base (a base is a whole graph).
fn refuse_layer_as_base(name: &str, as_base: bool, what: &str) -> Result<(), ServerError> {
    if as_base {
        return Err(input(
            name,
            format!(
                "role base: the source is {what}, not a whole graph; a base holds a whole \
                 graph (import a full snapshot, a plain Store, GraphSON or GraphML, or export \
                 the layer materialized first)"
            ),
        ));
    }
    Ok(())
}

/// A snapshot that does not create a Store: a broken file is the input's fault, anything else
/// the Store's.
fn snapshot_error(name: &str, err: PersistError) -> ServerError {
    match err {
        PersistError::Io { .. } => store_error(name, &format!("cannot create its Store: {err}")),
        other => input(name, format!("the snapshot does not import: {other}")),
    }
}

fn input(name: &str, message: String) -> ServerError {
    ServerError::InvalidInput {
        name: name.to_string(),
        message,
    }
}
