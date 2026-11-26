//! The shared models in the admin core: listing, adding, removing (`models list|add|remove`,
//! `/api/admin/models`), the adoption after every build and at server start (module docs of
//! [`crate::models`]).

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use graphersal::persist::{
    ImportModelOptions, ModelCache, ModelDownload, ModelDownloadError, ModelSource, Store,
    StoreOptions,
};
use graphersal::prelude::TraversalGraph;

use super::Admin;
use crate::audit::AuditEvent;
use crate::database::{DatabaseKind, DatabaseRole};
use crate::error::ServerError;
use crate::model_archive;
use crate::model_download::{Download, DownloadState, DownloadStatus, Downloads, ReportFacts};
use crate::models::{ModelBlob, ModelsDir, ModelsIndex, ScanEntry, catalog_users};
use crate::registry::DbState;

/// What [`Admin::model_add`] adds.
///
/// `#[non_exhaustive]`: build it with [`ModelAddSpec::path`] or [`ModelAddSpec::repo`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModelAddSpec {
    /// A Model2Vec model directory, a tar archive of one (module docs of
    /// `model_archive`: ustar, unpacked safely into `tmp/`) or a packed snapshot (`.gsnap`)
    /// carrying model blobs, on the server's machine (an upload: its file in `tmp/`).
    pub path: Option<PathBuf>,
    /// The repository (`owner/name`): of a directory that does not name it, or alone: a model
    /// `models/` already holds (the waiting databases are resolved again).
    pub repo: Option<String>,
    /// The licence (SPDX id) of a directory that does not state it.
    pub license: Option<String>,
}

impl ModelAddSpec {
    /// The models of the directory, `.tar` or `.gsnap` file `path`.
    pub fn path(path: impl Into<PathBuf>) -> Self {
        Self {
            path: Some(path.into()),
            ..Self::default()
        }
    }

    /// The repository `repo`, already in `models/`.
    pub fn repo(repo: impl Into<String>) -> Self {
        Self {
            repo: Some(repo.into()),
            ..Self::default()
        }
    }

    /// The spec with the repository of a directory that does not name it.
    pub fn with_repo(mut self, repo: Option<String>) -> Self {
        self.repo = repo;
        self
    }

    /// The spec with the licence of a directory that does not state it.
    pub fn with_license(mut self, license: Option<String>) -> Self {
        self.license = license;
        self
    }
}

/// What [`Admin::model_add`] did.
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModelAdded {
    /// The model blobs added (or found) in `models/`.
    pub added: Vec<ModelBlob>,
    /// The `needs-model` databases that have their model now (state `unloaded`).
    pub resolved: Vec<String>,
    /// The `needs-model` databases still waiting: `(database, repo)`.
    pub missing: Vec<(String, String)>,
    /// Things to know (link warnings, a step that failed).
    pub notes: Vec<String>,
}

/// What [`Admin::model_remove`] did.
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModelRemoved {
    /// The repository (or blob hash) removed.
    pub model: String,
    /// The blobs removed from `models/blobs/`.
    pub blobs: Vec<String>,
    /// Their bytes.
    pub bytes: u64,
}

/// What `Admin::model_download_start` downloads from Hugging Face.
///
/// `#[non_exhaustive]`: build it with [`ModelDownloadSpec::new`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModelDownloadSpec {
    /// The repository (`owner/name`).
    pub repo: String,
    /// The branch, tag or commit (`None`: `main`).
    pub revision: Option<String>,
    /// The endpoint instead of the environment's (the tests' fake endpoint; the admin API never
    /// sets it: module docs of `model_download`).
    pub(crate) endpoint: Option<String>,
}

impl ModelDownloadSpec {
    /// The repository `repo` at its default revision.
    pub fn new(repo: impl Into<String>) -> Self {
        Self {
            repo: repo.into(),
            ..Self::default()
        }
    }

    /// The spec at the revision `revision` (`None`: `main`).
    pub fn with_revision(mut self, revision: Option<String>) -> Self {
        self.revision = revision;
        self
    }
}

impl Admin {
    /// The shared models directory.
    pub fn models(&self) -> ModelsDir {
        #[allow(unused_mut)]
        let mut models = ModelsDir::new(self.registry.dir());
        #[cfg(test)]
        {
            models.fail_links = self.no_links;
        }
        models
    }

    /// The databases as the models scan sees them.
    fn scan_entries(&self) -> Vec<ScanEntry> {
        self.registry
            .list()
            .into_iter()
            .map(|entry| ScanEntry {
                store_dir: entry.store_dir(),
                needs_model: entry.config.as_ref().and_then(|c| c.needs_model.clone()),
                name: entry.name,
            })
            .collect()
    }

    /// The index of `models/`, rebuilt now (and written to `models/models.toml`).
    ///
    /// `Err`: [`ServerError::Io`], [`ServerError::Config`] (the index cannot be written).
    pub fn models_index(&self) -> Result<ModelsIndex, ServerError> {
        self.models().scan(&self.scan_entries())
    }

    /// Adopts the blobs of the Store `store_dir` of the build `name` (never fails: a problem
    /// is a warning, logged and answered).
    pub(crate) fn adopt_build(&self, name: &str, store_dir: &Path) -> Vec<String> {
        match self.models().adopt(store_dir) {
            Ok(adoption) => {
                if !adoption.linked.is_empty() {
                    tracing::info!(
                        target: "graphersal_server",
                        "database '{name}': {} model blob(s) shared through models/",
                        adoption.linked.len()
                    );
                }
                adoption.warnings
            }
            Err(err) => {
                let text = format!("database '{name}': its model blobs are not shared: {err}");
                tracing::warn!(target: "graphersal_server", "{text}");
                vec![text]
            }
        }
    }

    /// At server start (on a background thread): adopts the blobs of every database that is
    /// idle (online: under [`crate::Loader`]'s idle check, so no load starts meanwhile; a
    /// loading or loaded one is skipped), then writes the index. Problems are logged, never
    /// fatal.
    pub fn adopt_all(&self) {
        for entry in self.registry.list() {
            if !entry.store_dir().is_dir() {
                continue;
            }
            let adopt = || Ok(self.adopt_build(&entry.name, &entry.store_dir()));
            let adopted = match &self.loader {
                Some(loader) => loader.while_idle(&entry.name, adopt),
                None => adopt(),
            };
            if let Err(err) = adopted {
                tracing::debug!(target: "graphersal_server", "models: skipped: {err}");
            }
        }
        match self.models_index() {
            Ok(index) => tracing::info!(
                target: "graphersal_server",
                "models: {} shared model blob(s)",
                index.models.len()
            ),
            Err(err) => tracing::warn!(target: "graphersal_server", "models: {err}"),
        }
    }

    /// Adds models to `models/` (from a Model2Vec directory, a tar archive of one or a
    /// `.gsnap`, through a scratch Store in `tmp/`; or a repository `models/` holds already), then resolves every
    /// `needs-model` database whose model `models/` now has (module docs of
    /// [`crate::models`]).
    ///
    /// `Err`: [`ServerError::SourceNotFound`]; [`ServerError::InvalidInput`] (nothing given, a
    /// directory that is not a model, an archive refused by the safe unpack, a file that is
    /// neither a tar archive nor a packed snapshot or carries no model); [`ServerError::ModelNotFound`] (a repository `models/` lacks);
    /// [`ServerError::Io`].
    pub fn model_add(&self, spec: &ModelAddSpec) -> Result<ModelAdded, ServerError> {
        let mut out = ModelAdded::default();
        let wanted: BTreeSet<String> = match &spec.path {
            Some(path) => {
                let harvested = self.harvest(path, spec, &mut out.notes)?;
                harvested.into_iter().collect()
            }
            None => {
                let Some(repo) = &spec.repo else {
                    return Err(model_input(
                        "models",
                        "name a model directory or .gsnap file (path) or a repository (repo)",
                    ));
                };
                let index = self.models_index()?;
                let found: BTreeSet<String> = index.of_repo(repo).map(|m| m.blob.clone()).collect();
                if found.is_empty() {
                    return Err(ServerError::ModelNotFound { repo: repo.clone() });
                }
                found
            }
        };
        self.finish_add(&wanted, &mut out)?;
        Ok(out)
    }

    /// The rest of an add once the blobs `wanted` are in `models/`: the index rebuilt, the
    /// waiting databases resolved, the rows of `out` as they are afterwards.
    fn finish_add(
        &self,
        wanted: &BTreeSet<String>,
        out: &mut ModelAdded,
    ) -> Result<(), ServerError> {
        let models = self.models();
        let index = models.scan(&self.scan_entries())?;
        out.added = index
            .models
            .iter()
            .filter(|m| wanted.contains(&m.blob))
            .cloned()
            .collect();
        for model in &out.added {
            tracing::info!(
                target: "graphersal_server",
                "model {} ({}) is in models/",
                model.repo.as_deref().unwrap_or("?"),
                model.blob
            );
        }
        self.resolve_waiting(&index, out);
        let index = models.scan(&self.scan_entries())?;
        out.missing = index.missing.clone();
        // Rows as they are after the resolution (users, waiting).
        out.added = index
            .models
            .iter()
            .filter(|m| wanted.contains(&m.blob))
            .cloned()
            .collect();
        Ok(())
    }

    /// The blobs of a model directory, `.tar`, `.tar.gz` or `.gsnap` `path`, adopted into `models/` through a
    /// scratch Store in `tmp/` (removed afterwards).
    fn harvest(
        &self,
        path: &Path,
        spec: &ModelAddSpec,
        notes: &mut Vec<String>,
    ) -> Result<Vec<String>, ServerError> {
        if !path.exists() {
            return Err(ServerError::SourceNotFound {
                path: path.to_path_buf(),
            });
        }
        let tmp = self.registry.dir().tmp();
        let scratch = Scratch::new(&tmp, "models-add")?;
        let label = path.display().to_string();
        if path.is_dir() {
            import_dir(scratch.path(), path, spec, &label, notes)?;
        } else if model_archive::is_tar(path) {
            // A model directory as a tar archive: unpacked safely into `tmp/` first.
            let unpacked = Scratch::new(&tmp, "models-unpack")?;
            let root = model_archive::unpack(path, unpacked.path()).map_err(|why| {
                model_input(&label, &format!("not a usable model archive: {why}"))
            })?;
            import_dir(scratch.path(), &root, spec, &label, notes)?;
        } else if model_archive::is_gzip(path) {
            // A gzip-compressed tar archive: decompressed into `tmp/` (at most the upload
            // limit: a decompression bomb is refused), then unpacked like a `.tar`.
            let limit = self.registry.dir().load_config()?.limits.upload_bytes;
            let plain = Scratch::new(&tmp, "models-gunzip")?;
            fs::create_dir_all(plain.path())
                .map_err(|err| ServerError::io("create directory", plain.path(), err))?;
            let tar = plain.path().join("model.tar");
            model_archive::gunzip(path, &tar, limit).map_err(|why| {
                model_input(&label, &format!("not a usable model archive: {why}"))
            })?;
            let unpacked = Scratch::new(&tmp, "models-unpack")?;
            let root = model_archive::unpack(&tar, unpacked.path()).map_err(|why| {
                model_input(&label, &format!("not a usable model archive: {why}"))
            })?;
            drop(plain);
            import_dir(scratch.path(), &root, spec, &label, notes)?;
        } else {
            let file = File::open(path).map_err(|err| ServerError::io("read", path, err))?;
            Store::create_from_packed_dir(
                scratch.path(),
                BufReader::new(file),
                StoreOptions::new(),
            )
            .map_err(|err| model_input(&label, &format!("not a packed snapshot: {err}")))?;
        }
        let adoption = self.models().adopt(scratch.path())?;
        notes.extend(adoption.warnings.iter().cloned());
        let mut blobs = adoption.linked;
        blobs.extend(adoption.copied);
        if blobs.is_empty() {
            return Err(model_input(&label, "it carries no model blob"));
        }
        Ok(blobs)
    }

    /// Starts the download of `spec` from Hugging Face (module docs of `model_download`): online
    /// in a thread of its own, answering its first state at once (`running`); offline (the
    /// console holding the server lock) it runs to its end here and answers the final state.
    /// One download at a time.
    ///
    /// `Err`: [`ServerError::InvalidInput`] (a repository or revision that is not one; nothing
    /// fetched); [`ServerError::Refused`] (a download is running already: its repository).
    pub(crate) fn model_download_start(
        &self,
        spec: &ModelDownloadSpec,
    ) -> Result<DownloadStatus, ServerError> {
        let repo = spec.repo.trim().to_string();
        let revision = spec
            .revision
            .clone()
            .filter(|r| !r.trim().is_empty())
            .unwrap_or_else(|| ModelDownload::DEFAULT_REVISION.to_string());
        if !is_download_repo(&repo) {
            return Err(model_input(
                "models",
                &format!(
                    "'{repo}' is not a Hugging Face repository name: owner/name of letters, \
                     digits, '.', '_' and '-' (for example minishlab/potion-base-8M)"
                ),
            ));
        }
        if !is_revision(&revision) {
            return Err(model_input(
                "models",
                &format!(
                    "'{revision}' is not a revision: a branch, tag or commit of letters, digits, \
                     '.', '_', '-' and '/'"
                ),
            ));
        }
        let actor = self.audit.actor().unwrap_or(crate::access::OPERATOR);
        let offline = Downloads::default();
        let downloads = match &self.loader {
            Some(loader) => loader.downloads(),
            None => &offline,
        };
        let download = downloads
            .begin(&repo, &revision, actor, now_ms())
            .map_err(|running| ServerError::Refused {
                name: running.repo.clone(),
                message: format!(
                    "a download of {} ({}) is running: wait for it, or cancel it (POST \
                     /api/admin/models/download/cancel)",
                    running.repo, running.revision
                ),
            })?;
        self.audit.record(AuditEvent::ModelDownloadStarted {
            repo: repo.clone(),
            revision: revision.clone(),
        });
        let request = self.download_request(&repo, &revision, spec, &download);
        if self.loader.is_none() {
            self.run_download(&download, &request);
            return Ok(download.status());
        }
        let admin = self.clone();
        let running = download.clone();
        let spawned = std::thread::Builder::new()
            .name("graphersal-server-model-download".to_string())
            .spawn(move || admin.run_download(&running, &request));
        if let Err(err) = spawned {
            download.update(|status| {
                status.state = DownloadState::Failed;
                status.error = Some(format!("cannot start the download's thread: {err}"));
                status.error_kind = Some("io");
                status.finished_at_ms = Some(now_ms());
            });
        }
        Ok(download.status())
    }

    /// The running or last download's state (`None`: none since the start, or offline).
    pub(crate) fn model_download_status(&self) -> Option<DownloadStatus> {
        self.loader.as_ref()?.downloads().current()
    }

    /// Asks the running download to stop at its next chunk: `(true, state)` when one ran,
    /// `(false, state)` for a finished one, `None` without any.
    ///
    /// `Err`: [`ServerError::Refused`] offline (no download runs without a server).
    pub(crate) fn model_download_cancel(
        &self,
    ) -> Result<Option<(bool, DownloadStatus)>, ServerError> {
        let Some(loader) = &self.loader else {
            return Err(self.not_running("models", "cancel a model download of"));
        };
        Ok(loader.downloads().cancel())
    }

    /// The engine's request of a download: the endpoint of `spec` (tests) or the environment,
    /// the environment's token, the download's cancel.
    fn download_request(
        &self,
        repo: &str,
        revision: &str,
        spec: &ModelDownloadSpec,
        download: &Download,
    ) -> ModelDownload {
        let mut request = ModelDownload::new(repo)
            .with_revision(revision)
            .with_cancel(download.cancel_token());
        let endpoint = spec.endpoint.clone().or_else(|| {
            std::env::var(crate::model_download::ENDPOINT_ENV)
                .ok()
                .filter(|e| !e.trim().is_empty())
        });
        if let Some(endpoint) = endpoint {
            request = request.with_endpoint(endpoint);
        }
        if let Ok(token) = std::env::var(crate::model_download::TOKEN_ENV)
            && !token.trim().is_empty()
        {
            request = request.with_token(token.trim());
        }
        request
    }

    /// Runs the download of `request` to its end, the state in `download` (a failure is kept
    /// there, never panics), audited.
    fn run_download(&self, download: &Download, request: &ModelDownload) {
        let outcome = self.download_and_install(download, request);
        let status = download.status();
        match outcome {
            Ok(added) => {
                download.update(|s| {
                    s.state = DownloadState::Done;
                    s.added = Some(added);
                    s.finished_at_ms = Some(now_ms());
                });
                let report = status.report.unwrap_or_default();
                tracing::info!(
                    target: "graphersal_server",
                    "model {} ({}) downloaded: {}",
                    status.repo,
                    status.revision,
                    report.identity
                );
                self.audit.record(AuditEvent::ModelDownloaded {
                    repo: status.repo,
                    revision: status.revision,
                    identity: report.identity,
                    license: report.license,
                });
            }
            Err(failure) => {
                download.update(|s| match &failure {
                    DownloadFailure::Download(err) => s.fail(err, now_ms()),
                    DownloadFailure::Install(err) => {
                        s.state = DownloadState::Failed;
                        s.error = Some(err.to_string());
                        s.error_kind = Some("install");
                        s.finished_at_ms = Some(now_ms());
                    }
                });
                let reason = download.status().error.unwrap_or_default();
                tracing::warn!(
                    target: "graphersal_server",
                    "model download of {} ({}): {reason}",
                    status.repo,
                    status.revision
                );
                self.audit.record(AuditEvent::ModelDownloadFailed {
                    repo: status.repo,
                    revision: status.revision,
                    reason,
                });
            }
        }
    }

    /// The download into a staging directory in `tmp/`, then the model into a scratch Store
    /// there and its blob into `models/` (removed afterwards in every case).
    fn download_and_install(
        &self,
        download: &Download,
        request: &ModelDownload,
    ) -> Result<ModelAdded, DownloadFailure> {
        let tmp = self.registry.dir().tmp();
        let install = DownloadFailure::Install;
        let cache = Scratch::new(&tmp, "models-download").map_err(install)?;
        let staged = ModelCache::at(cache.path())
            .download_staged(request, |event| download.update(|s| s.on_event(event)))
            .map_err(DownloadFailure::Download)?;
        download.update(|s| {
            s.report = Some(ReportFacts::of(staged.report()));
            s.notes = staged.report().notes.clone();
            s.phase = "install";
            s.file = None;
        });
        let label = format!("{} (downloaded)", staged.report().repo);
        let scratch = Scratch::new(&tmp, "models-download-store").map_err(install)?;
        Store::create_with(scratch.path(), TraversalGraph::new(), StoreOptions::new())
            .and_then(Store::close)
            .map_err(|err| {
                install(store(
                    &label,
                    &format!("cannot create a scratch Store: {err}"),
                ))
            })?;
        let model = staged.new_model().map_err(DownloadFailure::Download)?;
        Store::import_model_dir(
            scratch.path(),
            ModelSource::Files(model),
            ImportModelOptions::new(),
        )
        .map_err(|err| {
            install(model_input(
                &label,
                &format!("not an importable model: {err}"),
            ))
        })?;
        drop(staged);
        let mut out = ModelAdded::default();
        let adoption = self.models().adopt(scratch.path()).map_err(install)?;
        out.notes.extend(adoption.warnings.iter().cloned());
        let mut blobs = adoption.linked;
        blobs.extend(adoption.copied);
        if blobs.is_empty() {
            return Err(install(model_input(&label, "it carries no model blob")));
        }
        let wanted: BTreeSet<String> = blobs.into_iter().collect();
        self.finish_add(&wanted, &mut out).map_err(install)?;
        Ok(out)
    }

    /// Resolves every `needs-model` plain database whose repository `index` has (a base is
    /// skipped: it is frozen).
    fn resolve_waiting(&self, index: &ModelsIndex, out: &mut ModelAdded) {
        for entry in self.registry.list() {
            let Some(config) = &entry.config else {
                continue;
            };
            let Some(repo) = config.needs_model.clone() else {
                continue;
            };
            // A base is never written (only a hand-edited `database.toml` could mark one).
            if config.kind != DatabaseKind::Plain
                || config.role == DatabaseRole::Base
                || index.of_repo(&repo).next().is_none()
            {
                continue;
            }
            let name = entry.name.clone();
            // A quick refusal while it is loading (the Store's lock refuses a load meanwhile).
            if let Some(loader) = &self.loader
                && let Err(err) = loader.while_idle(&name, || Ok(()))
            {
                out.notes
                    .push(format!("database '{name}': not resolved now: {err}"));
                continue;
            }
            let resolution = self
                .models()
                .resolve(&entry.store_dir(), std::slice::from_ref(&repo));
            out.notes.extend(
                resolution
                    .notes
                    .iter()
                    .map(|n| format!("database '{name}': {n}")),
            );
            out.notes
                .extend(resolution.adoption.warnings.iter().cloned());
            if !resolution.missing.is_empty() {
                continue;
            }
            let clear = || {
                self.registry.clear_needs_model(&name)?;
                self.registry.set_state(&name, DbState::Unloaded)
            };
            let cleared = match &self.loader {
                Some(loader) => loader.while_idle(&name, clear),
                None => clear(),
            };
            match cleared {
                Ok(()) => {
                    tracing::info!(
                        target: "graphersal_server",
                        "database '{name}' has its model '{repo}' now (unloaded)"
                    );
                    out.resolved.push(name);
                }
                Err(err) => out.notes.push(format!("database '{name}': {err}")),
            }
        }
    }

    /// Removes the repository `model` (or the blob of that hash) from `models/`: refused while
    /// any database holds one of its blobs, waits for it (`needs-model`) or names it in its
    /// Store's catalog.
    ///
    /// `Err`: [`ServerError::ModelNotFound`]; [`ServerError::Refused`] (the message names the
    /// databases); [`ServerError::Io`].
    pub fn model_remove(&self, model: &str) -> Result<ModelRemoved, ServerError> {
        let entries = self.scan_entries();
        let index = self.models().scan(&entries)?;
        let blobs: Vec<&ModelBlob> = index
            .models
            .iter()
            .filter(|m| m.repo.as_deref() == Some(model) || m.blob == model)
            .collect();
        let repos: BTreeSet<&str> = blobs.iter().filter_map(|m| m.repo.as_deref()).collect();
        let catalog = catalog_users(&entries);
        let mut users: BTreeSet<String> = blobs
            .iter()
            .flat_map(|m| m.users.iter().chain(&m.waiting).cloned())
            .collect();
        for repo in repos.iter().copied().chain(std::iter::once(model)) {
            if let Some(names) = catalog.get(repo) {
                users.extend(names.iter().cloned());
            }
        }
        if blobs.is_empty() && users.is_empty() {
            return Err(ServerError::ModelNotFound {
                repo: model.to_string(),
            });
        }
        if !users.is_empty() {
            return Err(ServerError::Refused {
                name: model.to_string(),
                message: format!(
                    "the databases {} use this model; delete them or remove the model from \
                     their Stores first",
                    users.into_iter().collect::<Vec<_>>().join(", ")
                ),
            });
        }
        let models = self.models();
        let mut removed = ModelRemoved {
            model: model.to_string(),
            ..ModelRemoved::default()
        };
        for blob in blobs {
            if let Some(bytes) = models.remove_blob(&blob.blob)? {
                removed.blobs.push(blob.blob.clone());
                removed.bytes += bytes;
            }
        }
        models.scan(&self.scan_entries())?;
        tracing::info!(
            target: "graphersal_server",
            "model '{model}' removed from models/ ({} blob(s), {} bytes)",
            removed.blobs.len(),
            removed.bytes
        );
        Ok(removed)
    }
}

/// The Model2Vec model directory `dir` imported into a new scratch Store `scratch`.
fn import_dir(
    scratch: &Path,
    dir: &Path,
    spec: &ModelAddSpec,
    label: &str,
    notes: &mut Vec<String>,
) -> Result<(), ServerError> {
    Store::create_with(scratch, TraversalGraph::new(), StoreOptions::new())
        .and_then(Store::close)
        .map_err(|err| store(label, &format!("cannot create a scratch Store: {err}")))?;
    let mut options = ImportModelOptions::new();
    if let Some(repo) = &spec.repo {
        options = options.with_repo(repo.clone());
    }
    if let Some(license) = &spec.license {
        options = options.with_license(license.clone());
    }
    let report = Store::import_model_dir(scratch, ModelSource::Dir(dir.to_path_buf()), options)
        .map_err(|err| model_input(label, &format!("not an importable model: {err}")))?;
    notes.extend(report.warnings);
    Ok(())
}

/// A scratch directory in `tmp/`, removed on drop.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new(tmp: &Path, prefix: &str) -> Result<Self, ServerError> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        fs::create_dir_all(tmp).map_err(|err| ServerError::io("create directory", tmp, err))?;
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        Ok(Self {
            path: tmp.join(format!("{prefix}-{}-{n}", std::process::id())),
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if self.path.exists()
            && let Err(err) = fs::remove_dir_all(&self.path)
        {
            tracing::warn!(
                target: "graphersal_server",
                "cannot remove {}: {err} (removed at the next start)",
                self.path.display()
            );
        }
    }
}

fn model_input(name: &str, message: &str) -> ServerError {
    ServerError::InvalidInput {
        name: name.to_string(),
        message: message.to_string(),
    }
}

fn store(name: &str, message: &str) -> ServerError {
    ServerError::Store {
        name: name.to_string(),
        message: message.to_string(),
    }
}

/// Why a download ended without a model.
enum DownloadFailure {
    /// The engine's download failed (or was cancelled).
    Download(ModelDownloadError),
    /// The downloaded model could not be added to `models/`.
    Install(ServerError),
}

/// Now in Unix milliseconds.
fn now_ms() -> u64 {
    crate::control::unix_ms(std::time::SystemTime::now())
}

/// Whether `repo` names a Hugging Face repository the download takes: `owner/name` of
/// letters, digits, `.`, `_` and `-` (the engine's rule, checked here first so a bad request is
/// refused before a thread starts).
fn is_download_repo(repo: &str) -> bool {
    let part_ok = |part: &str| {
        !part.is_empty()
            && part.len() <= 96
            && !part.starts_with('.')
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    match repo.split_once('/') {
        Some((owner, name)) => !name.contains('/') && part_ok(owner) && part_ok(name),
        None => false,
    }
}

/// Whether `revision` is a branch, tag or commit name (the engine's rule).
fn is_revision(revision: &str) -> bool {
    !revision.is_empty()
        && !revision.starts_with('/')
        && !revision.contains("..")
        && revision
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
}
