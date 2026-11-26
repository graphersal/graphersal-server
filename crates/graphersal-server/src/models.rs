//! The shared embedding models of a server directory: `models/`.
//!
//! A Store keeps an embedding model as ONE content-addressed, immutable blob
//! (`store/blobs/<2 hex>/<sha256>`, graphersal's format spec 4.6: any copy with the same hash is
//! the same bytes). The server keeps one physical copy of every model blob in
//! `models/blobs/<2 hex>/<sha256>` (the same layout) and puts a HARD LINK of it into each
//! database's `store/blobs/`. Nothing changes for the engine: for a Store it is an ordinary
//! blob file; prune and delete remove only the link; an export embeds the bytes read through it.
//!
//! - [`ModelsDir::adopt`] is the dedupe pass over one database's Store: for each of its blob
//!   files, a copy in `models/blobs/` of the same name and length replaces the file by a hard
//!   link (linked to a temporary name in the Store's `blobs/`, then renamed over the file: a
//!   reader sees the old or the new name of the same bytes); a blob `models/` lacks is linked
//!   into `models/` first. When the file system refuses the link (another file system,
//!   FAT/exFAT, most SMB shares, FUSE mounts) the blob is COPIED into `models/` and the
//!   database keeps its own copy: correct, but no disk saving; the warning is logged and
//!   returned, and listings show the database's copy ([`ModelsDir::copied_blobs`]).
//!   Adoption only touches Stores that are not served: a build in `tmp/` before it is
//!   committed (create, clone, import, restore), a `needs-model` database while its model is
//!   added, and every database at server start before anything loads.
//!   An import whose source names a model without carrying it resolves it from `models/` in
//!   its build ([`ModelsDir::resolve`]) when `models/` holds it ([`ModelsDir::holds`]).
//! - [`ModelsDir::scan`] rebuilds the index `models/models.toml` (derived: one row per model
//!   blob with its repository, identity, licence and bytes from the model's own manifest inside
//!   the blob, and the databases linking it or waiting for it); it is written at server start
//!   and after every model change and listing, and never read back by the server.
//! - [`ModelsDir::resolve`] fills the models a Store needs from `models/`: a blob the Store's
//!   catalog references but lacks through `Store::restore_blobs_from` (`models/` has the blob
//!   layout); a model the schema names without a definition by `Store::import_model_dir` from
//!   the blob's files unpacked into `tmp/`.
//!
//! Server-directory backups by external tools: `rsync -H` keeps the links; `cp -r` or a zip
//! duplicates them (bigger, still correct).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use graphersal::persist::{BlobSource, ImportModelOptions, ModelSource, ModelState, Store};
use serde::Serialize;

use crate::error::ServerError;
use crate::layout::{ServerDir, write_atomic};

/// The index file of `models/`.
pub const MODELS_FILE: &str = "models.toml";
/// The model manifest inside a model blob (a ustar archive).
const MODEL_MANIFEST: &str = "graphersal-model.json";
/// The largest manifest read.
const MAX_MANIFEST_BYTES: u64 = 1 << 20;
/// The most archive entries read before giving up.
const MAX_TAR_ENTRIES: usize = 80;
/// The ustar block size.
const BLOCK: u64 = 512;
/// The prefix of temporary names (`blobs/tmp-*` is removed by a Store's open; `models/blobs`'s
/// by [`ModelsDir::scan`]).
const TMP_PREFIX: &str = "tmp-";

/// Serializes the changes of `models/blobs/` inside this process (adoption against removal);
/// two processes never work on one server directory (the server lock).
static MODELS_LOCK: Mutex<()> = Mutex::new(());

/// What [`ModelsDir::adopt`] did to one Store.
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Adoption {
    /// The blobs (hash hex) now hard-linked to `models/blobs/`.
    pub linked: Vec<String>,
    /// The blobs the database keeps as its own copy (the link failed).
    pub copied: Vec<String>,
    /// What went wrong, one text per blob (logged as warnings too).
    pub warnings: Vec<String>,
}

/// One model blob of `models/` ([`ModelsDir::scan`]).
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct ModelBlob {
    /// The blob's SHA-256 (hex): its file name.
    pub blob: String,
    /// Its length in bytes.
    pub bytes: u64,
    /// The model's repository (`owner/name`), from its manifest; `None` for a blob that is not
    /// a readable model archive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The model identity (`model2vec/<repo>@<12 hex>`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    /// The licence (SPDX id) when the manifest states one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// The databases whose Store holds the blob (linked or copied).
    pub users: Vec<String>,
    /// Those of `users` that hold their own COPY (not a link: no disk saving).
    pub copied: Vec<String>,
    /// The databases in state `needs-model` waiting for this model's repository.
    pub waiting: Vec<String>,
    /// The hard links of the file in `models/blobs/` (unix only; `1` = no other name).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<u64>,
}

/// The index of `models/` ([`ModelsDir::scan`]).
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct ModelsIndex {
    /// Every blob of `models/blobs/`, by repository then hash.
    pub models: Vec<ModelBlob>,
    /// `needs-model` databases whose repository `models/` lacks: `(database, repo)`.
    pub missing: Vec<(String, String)>,
}

impl ModelsIndex {
    /// The blobs of the repository `repo`.
    pub fn of_repo<'a>(&'a self, repo: &'a str) -> impl Iterator<Item = &'a ModelBlob> + 'a {
        self.models
            .iter()
            .filter(move |m| m.repo.as_deref() == Some(repo))
    }
}

/// A database as the scan sees it: its name, its Store directory and its `needs_model` marker.
#[derive(Debug, Clone)]
pub struct ScanEntry {
    /// The database's name.
    pub name: String,
    /// Its `store/` directory.
    pub store_dir: PathBuf,
    /// The repository its `needs_model` marker names.
    pub needs_model: Option<String>,
}

/// What [`ModelsDir::resolve`] did to one Store.
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Resolution {
    /// The repositories now present in the Store.
    pub resolved: Vec<String>,
    /// The repositories still missing.
    pub missing: Vec<String>,
    /// The adoption afterwards.
    pub adoption: Adoption,
    /// Things to know (a step that failed and why).
    pub notes: Vec<String>,
}

/// A model's manifest fields the server uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Manifest {
    pub(crate) repo: Option<String>,
    pub(crate) identity: Option<String>,
    pub(crate) license: Option<String>,
}

/// The `models/` directory of a server directory (module docs).
#[derive(Debug, Clone)]
pub struct ModelsDir {
    dir: ServerDir,
    /// Test hook: every hard link fails (the copy fallback).
    #[cfg(test)]
    pub(crate) fail_links: bool,
}

impl ModelsDir {
    /// The models of the server directory `dir`.
    pub fn new(dir: &ServerDir) -> Self {
        Self {
            dir: dir.clone(),
            #[cfg(test)]
            fail_links: false,
        }
    }

    /// `models/`.
    pub fn root(&self) -> PathBuf {
        self.dir.models()
    }

    /// `models/blobs/`.
    pub fn blobs(&self) -> PathBuf {
        self.dir.model_blobs()
    }

    /// The path of the blob `hash` in `models/blobs/`.
    pub fn blob_path(&self, hash: &str) -> PathBuf {
        self.blobs().join(&hash[..2]).join(hash)
    }

    /// The dedupe pass over the Store in `store_dir` (module docs). Call it only on a Store
    /// nothing serves. A blob that cannot be adopted stays as it is (a warning): correctness
    /// never depends on the links.
    ///
    /// `Err`: [`ServerError::Io`] when the Store's `blobs/` cannot be listed.
    pub fn adopt(&self, store_dir: &Path) -> Result<Adoption, ServerError> {
        let _guard = MODELS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut adoption = Adoption::default();
        for (hash, path, len) in blob_files(&store_dir.join("blobs"))? {
            match self.adopt_one(&hash, &path, len) {
                Ok(Adopted::Linked) => adoption.linked.push(hash),
                Ok(Adopted::Copied(why)) => {
                    let text = format!(
                        "blob {hash} of {}: copied, not linked ({why}): the file system has no \
                         hard links here, no disk saving",
                        store_dir.display()
                    );
                    tracing::warn!(target: "graphersal_server", "{text}");
                    adoption.warnings.push(text);
                    adoption.copied.push(hash);
                }
                Err(why) => {
                    let text =
                        format!("blob {hash} of {}: not adopted: {why}", store_dir.display());
                    tracing::warn!(target: "graphersal_server", "{text}");
                    adoption.warnings.push(text);
                }
            }
        }
        Ok(adoption)
    }

    /// One blob of a Store: linked to `models/` (moved there first when new).
    fn adopt_one(&self, hash: &str, path: &Path, len: u64) -> Result<Adopted, String> {
        let shared = self.blob_path(hash);
        match fs::metadata(&shared) {
            Ok(meta) if meta.len() == len => {
                if same_file(&shared, path) {
                    return Ok(Adopted::Linked);
                }
                // Link the shared copy beside the file, then rename it over the file.
                let tmp = tmp_name(path.parent().and_then(Path::parent).unwrap_or(path), hash);
                match self.link(&shared, &tmp) {
                    Ok(()) => {
                        if let Err(err) = fs::rename(&tmp, path) {
                            let _ = fs::remove_file(&tmp);
                            return Err(format!("cannot rename the link into place: {err}"));
                        }
                        Ok(Adopted::Linked)
                    }
                    Err(err) => Ok(Adopted::Copied(err.to_string())),
                }
            }
            Ok(meta) => Err(format!(
                "models/blobs holds a file of that name with {} bytes, the Store's has {len}",
                meta.len()
            )),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let parent = self.blobs().join(&hash[..2]);
                fs::create_dir_all(&parent)
                    .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
                let tmp = tmp_name(&self.blobs(), hash);
                match self.link(path, &tmp) {
                    Ok(()) => {
                        fs::rename(&tmp, &shared).map_err(|err| {
                            let _ = fs::remove_file(&tmp);
                            format!("cannot rename into models/blobs: {err}")
                        })?;
                        Ok(Adopted::Linked)
                    }
                    Err(link_err) => {
                        copy_synced(path, &tmp).map_err(|err| {
                            let _ = fs::remove_file(&tmp);
                            format!("cannot copy into models/blobs: {err}")
                        })?;
                        fs::rename(&tmp, &shared).map_err(|err| {
                            let _ = fs::remove_file(&tmp);
                            format!("cannot rename into models/blobs: {err}")
                        })?;
                        Ok(Adopted::Copied(link_err.to_string()))
                    }
                }
            }
            Err(err) => Err(format!("cannot read {}: {err}", shared.display())),
        }
    }

    /// A hard link `to` -> `from` (the test hook makes it fail).
    fn link(&self, from: &Path, to: &Path) -> io::Result<()> {
        #[cfg(test)]
        if self.fail_links {
            return Err(io::Error::other("hard links disabled by the test"));
        }
        fs::hard_link(from, to)
    }

    /// The blobs (hash hex) of the Store in `store_dir` that are NOT the same file as their
    /// copy in `models/blobs/` (a copy, or not adopted). Unix only (file identity by device and
    /// inode); empty elsewhere.
    pub fn copied_blobs(&self, store_dir: &Path) -> Vec<String> {
        if !cfg!(unix) {
            return Vec::new();
        }
        blob_files(&store_dir.join("blobs"))
            .unwrap_or_default()
            .into_iter()
            .filter(|(hash, path, _)| !same_file(&self.blob_path(hash), path))
            .map(|(hash, _, _)| hash)
            .collect()
    }

    /// Rebuilds the index of `models/` from `models/blobs/` and the Stores of `databases`, and
    /// writes it to `models/models.toml` (module docs). Leftover `tmp-*` files in
    /// `models/blobs/` are removed.
    ///
    /// `Err`: [`ServerError::Io`] when `models/blobs/` cannot be listed or the index cannot be
    /// written.
    pub fn scan(&self, databases: &[ScanEntry]) -> Result<ModelsIndex, ServerError> {
        let _guard = MODELS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        remove_tmp(&self.blobs());
        let mut rows: BTreeMap<String, ModelBlob> = BTreeMap::new();
        for (hash, path, len) in blob_files(&self.blobs())? {
            let manifest = manifest_of(&path).ok().flatten();
            rows.insert(
                hash.clone(),
                ModelBlob {
                    blob: hash,
                    bytes: len,
                    repo: manifest.as_ref().and_then(|m| m.repo.clone()),
                    identity: manifest.as_ref().and_then(|m| m.identity.clone()),
                    license: manifest.and_then(|m| m.license),
                    links: link_count(&path),
                    ..ModelBlob::default()
                },
            );
        }
        let mut missing = Vec::new();
        for db in databases {
            for (hash, path, _) in blob_files(&db.store_dir.join("blobs")).unwrap_or_default() {
                if let Some(row) = rows.get_mut(&hash) {
                    row.users.push(db.name.clone());
                    if cfg!(unix) && !same_file(&self.blob_path(&hash), &path) {
                        row.copied.push(db.name.clone());
                    }
                }
            }
            if let Some(repo) = &db.needs_model {
                let mut found = false;
                for row in rows.values_mut() {
                    if row.repo.as_deref() == Some(repo.as_str()) {
                        row.waiting.push(db.name.clone());
                        found = true;
                    }
                }
                if !found {
                    missing.push((db.name.clone(), repo.clone()));
                }
            }
        }
        let mut models: Vec<ModelBlob> = rows.into_values().collect();
        models.sort_by(|a, b| (&a.repo, &a.blob).cmp(&(&b.repo, &b.blob)));
        let index = ModelsIndex { models, missing };
        self.write_index(&index)?;
        Ok(index)
    }

    /// `models/models.toml`.
    fn write_index(&self, index: &ModelsIndex) -> Result<(), ServerError> {
        #[derive(Serialize)]
        struct File<'a> {
            comment: &'a str,
            model: &'a [ModelBlob],
        }
        let path = self.root().join(MODELS_FILE);
        let text = toml::to_string(&File {
            comment: "derived by graphersal-server from models/blobs and the databases; \
                      rebuilt at every scan, edits are lost",
            model: &index.models,
        })
        .map_err(|err| ServerError::Config {
            path: path.clone(),
            message: format!("cannot write the index: {err}"),
        })?;
        // `models/` exists since `init` (not recreated: a server directory removed meanwhile
        // stays removed).
        write_atomic(&path, text.as_bytes())
    }

    /// Removes the blob `hash` from `models/blobs/` (the caller checked that no database uses
    /// it). Answers its length; `None` when it was not there.
    ///
    /// `Err`: [`ServerError::Io`].
    pub fn remove_blob(&self, hash: &str) -> Result<Option<u64>, ServerError> {
        let _guard = MODELS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let path = self.blob_path(hash);
        let len = match fs::metadata(&path) {
            Ok(meta) => meta.len(),
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(ServerError::io("read", &path, err)),
        };
        fs::remove_file(&path).map_err(|err| ServerError::io("remove", &path, err))?;
        let _ = fs::remove_dir(self.blobs().join(&hash[..2]));
        tracing::info!(target: "graphersal_server", "model blob {hash} removed from models/");
        Ok(Some(len))
    }

    /// Fills what the CLOSED Store in `store_dir` lacks of the models `repos` from `models/`
    /// (module docs), then adopts its blobs. A failing step is a note, not an error: the
    /// repository stays in [`Resolution::missing`].
    pub fn resolve(&self, store_dir: &Path, repos: &[String]) -> Resolution {
        let mut out = Resolution::default();
        let listed = match Store::models_dir(store_dir) {
            Ok(listed) => listed,
            Err(err) => {
                out.notes
                    .push(format!("cannot read the Store's models: {err}"));
                out.missing = repos.to_vec();
                return out;
            }
        };
        if listed
            .iter()
            .any(|m| matches!(m.state, ModelState::Missing))
        {
            match Store::restore_blobs_from(store_dir, BlobSource::store(self.root().as_path())) {
                Ok(report) => {
                    if !report.restored.is_empty() {
                        tracing::info!(
                            target: "graphersal_server",
                            "{}: {} model blob(s) restored from models/",
                            store_dir.display(),
                            report.restored.len()
                        );
                    }
                }
                Err(err) => out
                    .notes
                    .push(format!("cannot restore the blobs from models/: {err}")),
            }
        }
        for repo in repos {
            if listed.iter().any(|m| &m.repo == repo) {
                continue;
            }
            // The schema names the model, the catalog has no definition: import its files.
            if let Err(note) = self.import_files(store_dir, repo) {
                out.notes.push(note);
            }
        }
        match self.adopt(store_dir) {
            Ok(adoption) => out.adoption = adoption,
            Err(err) => out.notes.push(format!("adoption: {err}")),
        }
        let now = Store::models_dir(store_dir).unwrap_or_default();
        for repo in repos {
            let present = now
                .iter()
                .any(|m| &m.repo == repo && !matches!(m.state, ModelState::Missing));
            if present {
                out.resolved.push(repo.clone());
            } else {
                out.missing.push(repo.clone());
            }
        }
        out
    }

    /// The repository of the model blob `hash` (SHA-256 hex) in `models/blobs/`, from its
    /// manifest: `None` when `models/` lacks the blob or it is not a readable model archive.
    pub fn repo_of_blob(&self, hash: &str) -> Option<String> {
        if hash.len() < 3 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        manifest_of(&self.blob_path(hash)).ok().flatten()?.repo
    }

    /// Whether `models/blobs/` holds a model of the repository `repo` (read from the blobs'
    /// manifests; a directory that cannot be read holds nothing).
    pub fn holds(&self, repo: &str) -> bool {
        matches!(self.blob_of(repo), Ok(Some(_)))
    }

    /// The first blob of `models/blobs/` whose manifest names the repository `repo`.
    fn blob_of(&self, repo: &str) -> Result<Option<(PathBuf, Manifest)>, String> {
        Ok(blob_files(&self.blobs())
            .map_err(|err| err.to_string())?
            .into_iter()
            .find_map(|(_, path, _)| match manifest_of(&path) {
                Ok(Some(m)) if m.repo.as_deref() == Some(repo) => Some((path, m)),
                _ => None,
            }))
    }

    /// `Store::import_model_dir` of the repository `repo`'s model from `models/` (its blob's
    /// files unpacked into `tmp/`).
    fn import_files(&self, store_dir: &Path, repo: &str) -> Result<(), String> {
        let Some((blob, manifest)) = self.blob_of(repo)? else {
            return Err(format!("models/ has no model of '{repo}'"));
        };
        let unpacked = self.dir.tmp().join(format!(
            "{TMP_PREFIX}model-{}-{}",
            std::process::id(),
            NEXT_TMP.fetch_add(1, Ordering::SeqCst)
        ));
        let result = unpack(&blob, &unpacked)
            .map_err(|err| format!("cannot unpack the model '{repo}': {err}"))
            .and_then(|()| {
                let mut options = ImportModelOptions::new().with_repo(repo);
                if let Some(license) = manifest.license.filter(|l| l != "unknown") {
                    options = options.with_license(license);
                }
                Store::import_model_dir(store_dir, ModelSource::Dir(unpacked.clone()), options)
                    .map(drop)
                    .map_err(|err| format!("cannot import the model '{repo}': {err}"))
            });
        let _ = fs::remove_dir_all(&unpacked);
        result
    }
}

/// The outcome of one blob's adoption.
enum Adopted {
    Linked,
    /// Copied: the link failed for this reason.
    Copied(String),
}

static NEXT_TMP: AtomicUsize = AtomicUsize::new(0);

/// A fresh temporary name in `dir` for the blob `hash`.
fn tmp_name(dir: &Path, hash: &str) -> PathBuf {
    dir.join(format!(
        "{TMP_PREFIX}srv-{}-{}-{}",
        &hash[..16],
        std::process::id(),
        NEXT_TMP.fetch_add(1, Ordering::SeqCst)
    ))
}

/// Removes `tmp-*` files directly in `dir`.
fn remove_tmp(dir: &Path) {
    let Ok(listing) = fs::read_dir(dir) else {
        return;
    };
    for item in listing.flatten() {
        if item.file_name().to_string_lossy().starts_with(TMP_PREFIX) {
            let _ = fs::remove_file(item.path());
        }
    }
}

/// Copies `from` to `to` (new) and syncs it.
fn copy_synced(from: &Path, to: &Path) -> io::Result<()> {
    let mut source = File::open(from)?;
    let mut target = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)?;
    io::copy(&mut source, &mut target)?;
    target.sync_all()
}

/// Whether `name` is a SHA-256 blob file name (64 lowercase hex digits).
fn is_hash(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The SHA-256 blob files under the blob directory `blobs` (`<2 hex>/<64 hex>`): hash, path,
/// length. A missing directory has none; other algorithms (`algo-*`) and `tmp-*` are skipped.
pub(crate) fn blob_files(blobs: &Path) -> Result<Vec<(String, PathBuf, u64)>, ServerError> {
    let listing = match fs::read_dir(blobs) {
        Ok(listing) => listing,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(ServerError::io("read directory", blobs, err)),
    };
    let mut out = Vec::new();
    for item in listing.flatten() {
        let prefix = item.file_name().to_string_lossy().into_owned();
        if prefix.len() != 2 || !item.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(files) = fs::read_dir(item.path()) else {
            continue;
        };
        for file in files.flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            if !is_hash(&name) || !name.starts_with(&prefix) {
                continue;
            }
            if let Ok(meta) = file.metadata()
                && meta.is_file()
            {
                out.push((name, file.path(), meta.len()));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Whether `a` and `b` are the same file (unix: device and inode). Elsewhere `false`.
fn same_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (fs::metadata(a), fs::metadata(b)) {
            (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        false
    }
}

/// The hard links of `path` (unix only).
fn link_count(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(path).ok().map(|meta| meta.nlink())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// One regular-file entry of a ustar archive: its name, data offset and size.
struct TarEntry {
    name: String,
    offset: u64,
    size: u64,
}

/// The entries of the ustar archive `file` (headers only; bounded, never panics).
fn tar_entries(file: &mut File) -> io::Result<Vec<TarEntry>> {
    let total = file.metadata()?.len();
    let mut entries = Vec::new();
    let mut at = 0u64;
    let mut header = [0u8; BLOCK as usize];
    while at + BLOCK <= total && entries.len() < MAX_TAR_ENTRIES {
        file.seek(SeekFrom::Start(at))?;
        file.read_exact(&mut header)?;
        if header.iter().all(|&b| b == 0) {
            break;
        }
        if &header[257..262] != b"ustar" {
            return Err(io::Error::other(format!("no ustar header at byte {at}")));
        }
        let name_end = header[..100].iter().position(|&b| b == 0).unwrap_or(100);
        let name = String::from_utf8_lossy(&header[..name_end]).into_owned();
        let size_text: String = header[124..136]
            .iter()
            .take_while(|&&b| b != 0)
            .map(|&b| b as char)
            .collect();
        let size = u64::from_str_radix(size_text.trim(), 8)
            .map_err(|_| io::Error::other(format!("a broken size at byte {at}")))?;
        let offset = at + BLOCK;
        if offset.saturating_add(size) > total {
            return Err(io::Error::other(format!(
                "an entry past the end at byte {at}"
            )));
        }
        if matches!(header[156], b'0' | 0) {
            entries.push(TarEntry { name, offset, size });
        }
        at = offset + size.div_ceil(BLOCK) * BLOCK;
    }
    Ok(entries)
}

/// The manifest of the model blob at `path`; `Ok(None)` when the blob is not a model archive
/// with a readable manifest.
pub(crate) fn manifest_of(path: &Path) -> io::Result<Option<Manifest>> {
    let mut file = File::open(path)?;
    let Ok(entries) = tar_entries(&mut file) else {
        return Ok(None);
    };
    let Some(entry) = entries.iter().find(|e| e.name == MODEL_MANIFEST) else {
        return Ok(None);
    };
    if entry.size > MAX_MANIFEST_BYTES {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(entry.offset))?;
    let mut bytes = Vec::new();
    file.take(entry.size).read_to_end(&mut bytes)?;
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Ok(None);
    };
    let text = |key: &str| value.get(key).and_then(|v| v.as_str()).map(str::to_string);
    Ok(Some(Manifest {
        repo: text("repo").filter(|r| !r.is_empty()),
        identity: text("identity"),
        license: text("license"),
    }))
}

/// Unpacks the model blob at `blob` into the new directory `dir`: every file but the manifest
/// (plain names only).
fn unpack(blob: &Path, dir: &Path) -> io::Result<()> {
    let mut file = File::open(blob)?;
    let entries = tar_entries(&mut file)?;
    fs::create_dir_all(dir)?;
    for entry in entries {
        let plain = !entry.name.is_empty()
            && !entry.name.contains(['/', '\\'])
            && entry.name != "."
            && entry.name != ".."
            && entry.name != MODEL_MANIFEST;
        if !plain {
            continue;
        }
        file.seek(SeekFrom::Start(entry.offset))?;
        let mut out = File::create(dir.join(&entry.name))?;
        let copied = io::copy(&mut (&mut file).take(entry.size), &mut out)?;
        if copied != entry.size {
            return Err(io::Error::other(format!("{} is truncated", entry.name)));
        }
    }
    Ok(())
}

/// The users of every repository named by a Store's catalog (`Store::models_dir`), for the
/// refusal of a removal: `repo -> databases`.
pub(crate) fn catalog_users(databases: &[ScanEntry]) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for db in databases {
        if let Ok(models) = Store::models_dir(db.store_dir.as_path()) {
            for model in models {
                out.entry(model.repo).or_default().insert(db.name.clone());
            }
        }
        if let Some(repo) = &db.needs_model {
            out.entry(repo.clone()).or_default().insert(db.name.clone());
        }
    }
    out
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
