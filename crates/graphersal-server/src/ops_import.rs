//! The import of another database into the server directory ([`Admin::import`]): a new
//! database built in `tmp/` from a file or directory and renamed into `databases/` in one step
//! (a failure or a crash in between leaves no `databases/<name>`). Part of [`Admin`] (module
//! docs of [`crate::ops`]).
//!
//! Sources ([`ImportFormat`]):
//! - a packed snapshot (`.gsnap`; the kind is read from its manifest with
//!   [`read_snapshot_info`], never from the file name): a FULL snapshot becomes a plain database
//!   ([`Store::create_from_packed_dir`]: its embedding models come along when it carries them;
//!   a snapshot that names a model without carrying it becomes a database in the state
//!   `needs-model (<repo>)`, persisted as `needs_model` in its `database.toml`); a LAYER
//!   snapshot becomes a layer database over the base database with the snapshot's base
//!   identity ([`BaseRef`]): the one named by `base`, or the only base database with that
//!   identity; refused otherwise, naming the identity;
//! - a Store (a directory, or a single-file `.gstore`): copied file by file into a directory
//!   Store ([`Store::convert`], verified; its creation parameters come along); a layer Store is
//!   matched with its base like a layer snapshot; a missing model blob marks `needs-model`;
//! - GraphSON (`.json`, `.jsonl`, `.graphson`) or GraphML (`.xml`, `.graphml`): a new plain
//!   database holding the file's graph (the Store gets `server.toml`'s `[defaults]`); data
//!   GraphSON cannot hold is skipped and listed in the import's notes.
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
    ModelState, PersistError, ReadOptions, Store, read_layer_snapshot, read_snapshot_info,
};
use graphersal::storage::{BaseRef, LayeredGraph};

#[cfg(test)]
use super::FailPoint;
use super::{Admin, store_error};
use crate::database::{DatabaseConfig, DatabaseKind, DatabaseRole};
use crate::error::ServerError;
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
    /// The new database (`unloaded`, or `needs-model` when a model is missing).
    pub entry: DatabaseEntry,
    /// The source's format.
    pub format: ImportFormat,
    /// A layer's base identity ([`BaseRef`] as text).
    pub layer_base: Option<String>,
    /// The embedding models (repositories) the database needs but does not keep.
    pub models_missing: Vec<String>,
    /// How many values, entries or elements the import skipped (GraphSON).
    pub skipped: usize,
    /// What the import noted (skipped or converted data, missing models), as sentences.
    pub notes: Vec<String>,
}

/// What a build learned besides the Store.
#[derive(Default)]
struct Built {
    layer: Option<(BaseRef, String)>,
    models_missing: Vec<String>,
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
    /// written); [`ServerError::Config`]; [`ServerError::Io`].
    pub fn import(
        &self,
        source: &ImportSource,
        name: &str,
        base: Option<&str>,
        description: Option<&str>,
    ) -> Result<Imported, ServerError> {
        let path = source.path.as_path();
        if !path.exists() {
            return Err(ServerError::SourceNotFound {
                path: path.to_path_buf(),
            });
        }
        let format = detect(path, source.format)?;
        let config = DatabaseConfig::plain().with_description(description.unwrap_or_default());
        let mut build = self.registry.prepare_tmp(name, config)?;
        let store_dir = build.store_dir();
        let built = match format {
            ImportFormat::Snapshot => self.import_snapshot(name, path, &store_dir, base)?,
            ImportFormat::Store => self.import_store(name, path, &store_dir, base)?,
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
            }
        }
        config.needs_model = built.models_missing.first().cloned();
        // The model blobs it carries are shared through `models/`. A model it names but does
        // not carry leaves it `needs-model` even when `models/` holds it (the V7 contract):
        // `models add <repo>` resolves it from there.
        let mut built = built;
        built.notes.extend(self.adopt_build(name, &store_dir));
        #[cfg(test)]
        match self.fail {
            Some(FailPoint::AfterStore) => return Err(store_error(name, "injected failure")),
            Some(FailPoint::CrashAfterStore) => {
                std::mem::forget(build);
                return Err(store_error(name, "injected crash"));
            }
            None => {}
        }
        let entry = self.registry.commit(build)?;
        tracing::info!(
            target: "graphersal_server",
            "database '{}' imported from {} ({}, {})",
            entry.name,
            path.display(),
            format.as_str(),
            entry.state
        );
        Ok(Imported {
            entry,
            format,
            layer_base: built.layer.map(|(identity, _)| identity.to_string()),
            models_missing: built.models_missing,
            skipped: built.skipped,
            notes: built.notes,
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
    ) -> Result<Built, ServerError> {
        let info = read_snapshot_info(BufReader::new(open(path)?))
            .map_err(|err| input(name, format!("not a packed snapshot: {err}")))?;
        let mut built = Built::default();
        match info.layer_base {
            Some(identity) => {
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
        built.notes = model_notes(&built.models_missing);
        Ok(built)
    }

    /// A Store directory or `.gstore` copied into the directory Store `store_dir`.
    fn import_store(
        &self,
        name: &str,
        path: &Path,
        store_dir: &Path,
        base: Option<&str>,
    ) -> Result<Built, ServerError> {
        Store::convert(path, store_dir)
            .map_err(|err| store_error(name, &format!("cannot copy the Store: {err}")))?;
        let info = Store::inspect(store_dir)
            .map_err(|err| store_error(name, &format!("cannot inspect the copy: {err}")))?;
        let mut built = Built::default();
        match info.layer_base {
            Some(identity) => {
                let base_name = self.matching_base(name, &identity, base, "store")?;
                built.layer = Some((identity, base_name));
            }
            None => {
                refuse_base(name, base, "a plain Store")?;
                built.models_missing = dedup(missing_blobs(name, store_dir)?);
            }
        }
        built.notes = model_notes(&built.models_missing);
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
            skipped: report.skipped(),
            notes,
        })
    }
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

fn dedup(mut repos: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    repos.retain(|repo| seen.insert(repo.clone()));
    repos
}

fn model_notes(missing: &[String]) -> Vec<String> {
    missing
        .iter()
        .map(|repo| {
            format!(
                "the embedding model '{repo}' is named but not carried: the database is \
                 needs-model until it is added"
            )
        })
        .collect()
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
