//! The server-side Hugging Face download of a Model2Vec embedding model (S1, graphersal's
//! `persist::ModelCache::download`, feature `model-download`): the state of the ONE download a
//! server runs at a time ([`Downloads`], held by the loader), its progress and its cancel. The
//! admin core runs it (`Admin::model_download_start` in `ops_models.rs`): the files are fetched
//! and checked into a staging directory in `tmp/`, imported into a scratch Store there, and the
//! model blob joins the shared `models/` like any added model (the manifest and hard-link
//! scheme of [`crate::models`]); the `needs-model` databases waiting for the repository are
//! resolved afterwards.
//!
//! Where it downloads from: `https://huggingface.co`, or the endpoint in the server process's
//! environment variable [`ENDPOINT_ENV`] (a mirror, the tests' fake endpoint); a gated or private
//! repository needs a token in [`TOKEN_ENV`] (sent to that endpoint's host only). The request
//! names only the repository and the revision: a client never chooses the endpoint (the server
//! would fetch any URL for it).
//!
//! The state: `running` (with the file being fetched and its bytes), then `done` (the model's
//! report: identity, licence, gated, base models, the licence warnings; the blob added),
//! `failed` (the error, nothing kept) or `cancelled`. The last download stays visible until the
//! next one starts (in memory only: a restart forgets it). Its mutex is a leaf lock.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use graphersal::exec::CancelToken;
use graphersal::persist::{DownloadEvent, ModelDownloadError, ModelDownloadReport};

use crate::ops::ModelAdded;

/// The environment variable naming the Hugging Face endpoint to download from (the same as the
/// graphersal CLI's `embed-model download`).
pub(crate) const ENDPOINT_ENV: &str = "GRAPHERSAL_HF_ENDPOINT";

/// The environment variable holding the Hugging Face token of gated or private repositories.
pub(crate) const TOKEN_ENV: &str = "HF_TOKEN";

/// Where a download stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum DownloadState {
    /// Fetching the files, or adding the model to `models/`.
    #[default]
    Running,
    /// The model is in `models/`.
    Done,
    /// It failed; nothing was kept.
    Failed,
    /// It was cancelled; nothing was kept.
    Cancelled,
}

impl DownloadState {
    /// `running`, `done`, `failed`, `cancelled`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            DownloadState::Running => "running",
            DownloadState::Done => "done",
            DownloadState::Failed => "failed",
            DownloadState::Cancelled => "cancelled",
        }
    }
}

/// What the download reported about the model (the engine's `ModelDownloadReport`).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ReportFacts {
    pub(crate) identity: String,
    pub(crate) commit: Option<String>,
    pub(crate) dimension: Option<u64>,
    pub(crate) license: String,
    pub(crate) license_file: bool,
    pub(crate) gated: bool,
    /// `(repository, licence)` of the models it was made from.
    pub(crate) base_models: Vec<(String, Option<String>)>,
    /// The licence warnings (an unknown or non-commercial licence, custom terms, gated).
    pub(crate) warnings: Vec<String>,
    /// `(file, bytes)` of every downloaded file.
    pub(crate) files: Vec<(String, u64)>,
}

impl ReportFacts {
    pub(crate) fn of(report: &ModelDownloadReport) -> Self {
        Self {
            identity: report.identity.clone(),
            commit: report.commit.clone(),
            dimension: report.dimension,
            license: report.license.clone(),
            license_file: report.license_file,
            gated: report.gated,
            base_models: report
                .base_models
                .iter()
                .map(|b| (b.repo.clone(), b.license.clone()))
                .collect(),
            warnings: report.warnings.clone(),
            files: report
                .files
                .iter()
                .map(|f| (f.name.clone(), f.size))
                .collect(),
        }
    }
}

/// One download's state (module docs).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct DownloadStatus {
    /// Unique within this server's run.
    pub(crate) id: u64,
    pub(crate) repo: String,
    pub(crate) revision: String,
    pub(crate) state: DownloadState,
    /// `download` (fetching and checking the files) or `install` (adding the blob to
    /// `models/`, resolving the waiting databases).
    pub(crate) phase: &'static str,
    /// Who started it (the audit actor).
    pub(crate) actor: String,
    pub(crate) started_at_ms: u64,
    pub(crate) finished_at_ms: Option<u64>,
    /// The file being fetched, its bytes so far and the size the server states.
    pub(crate) file: Option<String>,
    pub(crate) file_bytes: u64,
    pub(crate) file_total: Option<u64>,
    /// The files fetched completely: `(name, bytes)`.
    pub(crate) files_done: Vec<(String, u64)>,
    /// A cancel was asked for (the download stops at its next chunk).
    pub(crate) cancel_requested: bool,
    pub(crate) report: Option<ReportFacts>,
    /// What the install added (`done`).
    pub(crate) added: Option<ModelAdded>,
    /// The failure's text (`failed`, `cancelled`).
    pub(crate) error: Option<String>,
    /// The failure's kind: `invalid-request`, `refused`, `not-found`, `transfer`, `integrity`,
    /// `not-a-model`, `io`, `cancelled`, `install`.
    pub(crate) error_kind: Option<&'static str>,
    /// The download's notes (unreadable metadata, ...).
    pub(crate) notes: Vec<String>,
}

impl DownloadStatus {
    /// The bytes received so far, every file together.
    pub(crate) fn received_bytes(&self) -> u64 {
        self.files_done.iter().map(|(_, b)| b).sum::<u64>() + self.file_bytes
    }

    /// Applies one engine event.
    pub(crate) fn on_event(&mut self, event: &DownloadEvent<'_>) {
        match *event {
            DownloadEvent::Progress {
                name, bytes, total, ..
            } => self.on_progress(name, bytes, total),
            DownloadEvent::FileDone { name, bytes, .. } => self.on_file_done(name, bytes),
            DownloadEvent::Note { text, .. } => self.notes.push(text.to_string()),
            _ => {}
        }
    }

    fn on_progress(&mut self, name: &str, bytes: u64, total: Option<u64>) {
        if self.file.as_deref() != Some(name) {
            self.file = Some(name.to_string());
        }
        self.file_bytes = bytes;
        self.file_total = total;
    }

    fn on_file_done(&mut self, name: &str, bytes: u64) {
        self.files_done.push((name.to_string(), bytes));
        self.file = None;
        self.file_bytes = 0;
        self.file_total = None;
    }

    /// Ends it with `error` (`failed`, or `cancelled` for a cancel).
    pub(crate) fn fail(&mut self, error: &ModelDownloadError, now_ms: u64) {
        self.state = match error {
            ModelDownloadError::Cancelled { .. } => DownloadState::Cancelled,
            _ => DownloadState::Failed,
        };
        self.error = Some(error.to_string());
        self.error_kind = Some(error_kind(error));
        self.file = None;
        self.finished_at_ms = Some(now_ms);
    }
}

/// The kind word of a download error (`DownloadStatus::error_kind`).
pub(crate) fn error_kind(error: &ModelDownloadError) -> &'static str {
    match error {
        ModelDownloadError::InvalidRequest { .. } => "invalid-request",
        ModelDownloadError::Refused { .. } => "refused",
        ModelDownloadError::NotFound { .. } => "not-found",
        ModelDownloadError::Transfer { .. } => "transfer",
        ModelDownloadError::Integrity { .. } => "integrity",
        ModelDownloadError::NotAModel { .. } => "not-a-model",
        ModelDownloadError::Cancelled { .. } => "cancelled",
        _ => "io",
    }
}

/// One running (or finished) download: its state and its cancel.
#[derive(Debug, Default)]
pub(crate) struct Download {
    status: Mutex<DownloadStatus>,
    cancel: CancelToken,
}

impl Download {
    fn lock(&self) -> MutexGuard<'_, DownloadStatus> {
        self.status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Its state now.
    pub(crate) fn status(&self) -> DownloadStatus {
        self.lock().clone()
    }

    /// Changes its state.
    pub(crate) fn update(&self, change: impl FnOnce(&mut DownloadStatus)) {
        change(&mut self.lock());
    }

    /// The token the engine checks between chunks.
    pub(crate) fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }
}

/// The server's download slot (module docs): at most one download runs.
#[derive(Debug, Default)]
pub(crate) struct Downloads {
    current: Mutex<Option<Arc<Download>>>,
    next: AtomicU64,
}

impl Downloads {
    fn lock(&self) -> MutexGuard<'_, Option<Arc<Download>>> {
        self.current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A new download of `repo` at `revision` started by `actor`, unless one is running
    /// (then `Err` with its state).
    pub(crate) fn begin(
        &self,
        repo: &str,
        revision: &str,
        actor: &str,
        now_ms: u64,
    ) -> Result<Arc<Download>, Box<DownloadStatus>> {
        let mut current = self.lock();
        if let Some(running) = current.as_ref()
            && running.status().state == DownloadState::Running
        {
            return Err(Box::new(running.status()));
        }
        let download = Arc::new(Download {
            status: Mutex::new(DownloadStatus {
                id: self.next.fetch_add(1, Ordering::SeqCst) + 1,
                repo: repo.to_string(),
                revision: revision.to_string(),
                phase: "download",
                actor: actor.to_string(),
                started_at_ms: now_ms,
                ..DownloadStatus::default()
            }),
            cancel: CancelToken::new(),
        });
        *current = Some(download.clone());
        Ok(download)
    }

    /// The running or last download's state.
    pub(crate) fn current(&self) -> Option<DownloadStatus> {
        self.lock().as_ref().map(|d| d.status())
    }

    /// Asks the running download to stop: `Some((true, state))` when one was running,
    /// `Some((false, state))` for a finished one, `None` without any.
    pub(crate) fn cancel(&self) -> Option<(bool, DownloadStatus)> {
        let current = self.lock();
        let download = current.as_ref()?;
        let mut status = download.lock();
        if status.state != DownloadState::Running {
            return Some((false, status.clone()));
        }
        download.cancel.cancel();
        status.cancel_requested = true;
        Some((true, status.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_download_at_a_time_and_its_cancel() {
        let downloads = Downloads::default();
        assert!(downloads.current().is_none());
        assert!(downloads.cancel().is_none());
        let first = downloads.begin("acme/tiny", "main", "operator", 1).unwrap();
        assert_eq!(first.status().id, 1);
        let refused = downloads
            .begin("acme/other", "main", "operator", 2)
            .unwrap_err();
        assert_eq!(refused.repo, "acme/tiny");
        let (stopped, status) = downloads.cancel().unwrap();
        assert!(stopped && status.cancel_requested);
        assert!(first.cancel_token().is_cancelled());
        first.update(|s| s.state = DownloadState::Cancelled);
        assert_eq!(downloads.current().unwrap().state, DownloadState::Cancelled);
        assert!(!downloads.cancel().unwrap().0, "nothing runs any more");
        let second = downloads
            .begin("acme/other", "v1", "user:alice", 4)
            .unwrap();
        assert_eq!(second.status().id, 2);
        assert_eq!(downloads.current().unwrap().revision, "v1");
    }

    #[test]
    fn progress_counts_the_bytes_of_every_file() {
        let mut status = DownloadStatus::default();
        status.on_progress("config.json", 10, Some(20));
        assert_eq!(status.file.as_deref(), Some("config.json"));
        assert_eq!(status.received_bytes(), 10);
        status.on_file_done("config.json", 20);
        status.on_progress("model.safetensors", 5, None);
        assert_eq!(status.received_bytes(), 25);
        assert_eq!(status.files_done, [("config.json".to_string(), 20)]);
    }
}
