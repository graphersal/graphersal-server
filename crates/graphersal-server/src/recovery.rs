//! Recovery after an unclean shutdown (round 3 B-CRASH): the server killed by force (`kill
//! -9`, a power loss, an OOM kill) leaves its databases' Stores open.
//!
//! What survives such a kill and what does not: the OS releases `server.lock` with the
//! process; `server.run` stays behind (stale); every committed transaction is in its Store's
//! WAL, and the next open of the Store replays it (and cuts an interrupted last write: the
//! engine's open report, [`OpenReport`]). What this module adds:
//!
//! - **The open set.** The running server keeps [`OPEN_FILE`] (`server.open`, TOML: `pid`,
//!   `started_at_ms`, `databases`) up to date: rewritten atomically (`OpenRecord`) on every
//!   load start, load end and unload with the databases it has loaded or is loading, and
//!   removed at a clean stop. `graphersal-server stop` on a directory without a server removes
//!   a stale `server.run`, never `server.open`.
//! - **The unclean-shutdown check.** `serve` (and `start`, which runs `serve`) finds, with the
//!   lock held, a `server.open` or a stale `server.run` ([`detect`]): the last server did not
//!   stop cleanly. It says so (a warning in the log, an audit record
//!   [`AuditEvent::UncleanShutdown`], the start block's "Unclean shutdown" section) and checks
//!   every database of the open set on a background thread: it is loaded again (whether or not
//!   `auto_load` is on: the server serves again what it served before the kill), its Store's
//!   open report is kept ([`RecoveryFacts`]: clean close, commits replayed, a torn tail cut,
//!   ...) and the open Store is verified ([`VerifyFacts`], the engine's `Store::verify`). A
//!   Store that opened in maintenance mode (damage found at the open), or whose verify found
//!   damage under the damage policy `maintenance`, is served READ-ONLY in the state
//!   `maintenance` with its damage report ([`crate::registry::DbState::Maintenance`]). Each
//!   result is logged and audited ([`AuditEvent::DatabaseChecked`]) and kept in
//!   [`RECOVERY_FILE`] (`recovery.json`, [`UncleanShutdown`]): `status`, `GET
//!   /api/admin/server` (`uncleanShutdown`), the Databases tab and the database settings show
//!   the last unclean shutdown until the next one replaces it. When `server.open` is missing
//!   (a stale `server.run` of a server older than this version), the open set is unknown: the
//!   check covers the `auto_load` databases only ([`UncleanShutdown::databases_known`] false).
//! - Repair and swap are admin operations (`crate::ops`, `db verify`, `db repair`, `db swap`).

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use graphersal::persist::{OpenReport, VerifyReport};
use serde::{Deserialize, Serialize};

use crate::audit::AuditEvent;
use crate::error::ServerError;
use crate::layout::{ServerDir, write_atomic};
use crate::loader::Loader;
use crate::ops::Admin;
use crate::registry::DbState;
use crate::run_file::RunInfo;

pub use crate::layout::{LAST_VERIFY_FILE, OPEN_FILE, RECOVERY_FILE};

/// How long the check waits for one database's load.
const LOAD_WAIT: Duration = Duration::from_secs(3600);

/// The contents of `server.open` (module docs).
///
/// `#[non_exhaustive]`: further fields may be added (readers ignore unknown keys).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct OpenFile {
    /// The process id of the server that wrote it.
    pub pid: u32,
    /// When that server started, in Unix milliseconds.
    #[serde(default)]
    pub started_at_ms: u64,
    /// The databases it had loaded or was loading (real names).
    #[serde(default)]
    pub databases: Vec<String>,
}

impl OpenFile {
    /// Reads `server.open` of `dir`: `None` when there is none.
    pub fn read(dir: &ServerDir) -> Result<Option<Self>, ServerError> {
        let path = dir.open_path();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(ServerError::io("read", &path, err)),
        };
        toml::from_str(&text)
            .map(Some)
            .map_err(|err| ServerError::Config {
                path,
                message: err.message().to_string(),
            })
    }

    /// Writes `server.open` of `dir` atomically (with the directory's fsync: it must survive a
    /// power loss).
    fn write(&self, dir: &ServerDir) -> Result<(), ServerError> {
        let path = dir.open_path();
        let body = toml::to_string(self).map_err(|err| ServerError::Config {
            path: path.clone(),
            message: err.to_string(),
        })?;
        let text = format!(
            "# Written by the running graphersal-server on every load and unload and removed at \
             a clean stop; left behind, it names the databases to check after an unclean \
             shutdown.\n{body}"
        );
        write_atomic(&path, text.as_bytes())
    }

    /// Removes `server.open` of `dir` (no error when there is none).
    pub fn remove(dir: &ServerDir) -> Result<(), ServerError> {
        let path = dir.open_path();
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(ServerError::io("remove", &path, err)),
        }
    }
}

/// The running server's `server.open` writer ([`crate::Loader`] calls [`OpenRecord::write`]
/// after every change of its open set, holding [`OpenRecord::guard`] while it reads the set,
/// so two writes never land out of order).
#[derive(Debug)]
pub(crate) struct OpenRecord {
    dir: ServerDir,
    pid: u32,
    started_at_ms: u64,
    lock: Mutex<()>,
}

impl OpenRecord {
    /// The record of this process, started at `started_at_ms`.
    pub(crate) fn new(dir: ServerDir, started_at_ms: u64) -> Self {
        Self {
            dir,
            pid: std::process::id(),
            started_at_ms,
            lock: Mutex::new(()),
        }
    }

    /// The writer's lock (a leaf: taken before the loader's state is read, never while it is
    /// held).
    pub(crate) fn guard(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Writes the open set `databases` (a failure is a warning: the server keeps serving).
    pub(crate) fn write(&self, databases: Vec<String>) -> Result<(), ServerError> {
        let written = OpenFile {
            pid: self.pid,
            started_at_ms: self.started_at_ms,
            databases,
        }
        .write(&self.dir);
        if let Err(err) = &written {
            tracing::warn!(
                target: "graphersal_server",
                "cannot record the open databases in {}: {err}",
                self.dir.open_path().display()
            );
        }
        written
    }
}

/// What the last server left behind when it did not stop cleanly ([`detect`]).
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Previous {
    /// Its process id (from `server.open`, else `server.run`).
    pub pid: Option<u32>,
    /// When it started (Unix milliseconds), when known.
    pub started_at_ms: Option<u64>,
    /// Its URL (from a stale `server.run`).
    pub url: Option<String>,
    /// The databases it had open (`server.open`); `None`: unknown (no `server.open`).
    pub databases: Option<Vec<String>>,
    /// Whether its `server.run` was left behind.
    pub run_file_left: bool,
}

/// Whether the last server on `dir` stopped uncleanly: a `server.open` or a `server.run` is
/// left. Call it with the server lock held, before this server writes its own files. A file
/// that cannot be read counts as left behind (with what could be read).
pub fn detect(dir: &ServerDir) -> Option<Previous> {
    let run = match RunInfo::read(dir) {
        Ok(run) => run.map(Some),
        Err(_) => Some(None),
    };
    let open = match OpenFile::read(dir) {
        Ok(open) => open.map(Some),
        Err(err) => {
            tracing::warn!(target: "graphersal_server", "{err}");
            Some(None)
        }
    };
    if run.is_none() && open.is_none() {
        return None;
    }
    let run = run.flatten();
    let open = open.flatten();
    Some(Previous {
        pid: open.as_ref().map(|o| o.pid).or(run.as_ref().map(|r| r.pid)),
        started_at_ms: open
            .as_ref()
            .map(|o| o.started_at_ms)
            .filter(|ms| *ms > 0)
            .or(run.as_ref().and_then(|r| r.started_at_ms)),
        url: run.as_ref().map(|r| r.url.clone()),
        databases: open.map(|o| o.databases),
        run_file_left: dir.run_path().exists(),
    })
}

/// What one database's check found ([`DatabaseCheck::outcome`]).
///
/// `#[non_exhaustive]`: further outcomes may be added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum CheckOutcome {
    /// Not checked yet.
    Pending,
    /// Being loaded and verified.
    Checking,
    /// Loaded; the verify found no problem.
    Ok,
    /// Damage found: served read-only in the state `maintenance` (or, under the damage policy
    /// `continue`, still writable with the problems listed).
    Damaged,
    /// It could not be loaded (the reason in [`DatabaseCheck::message`]); its files were
    /// verified without opening it.
    Failed,
    /// It needs an embedding model before it can be loaded; its files were verified.
    NeedsModel,
    /// It no longer exists.
    Missing,
}

impl CheckOutcome {
    /// The outcome as the JSON shows it (`ok`, `damaged`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            CheckOutcome::Pending => "pending",
            CheckOutcome::Checking => "checking",
            CheckOutcome::Ok => "ok",
            CheckOutcome::Damaged => "damaged",
            CheckOutcome::Failed => "failed",
            CheckOutcome::NeedsModel => "needs-model",
            CheckOutcome::Missing => "missing",
        }
    }
}

/// What the open of a Store did (from the engine's [`OpenReport`]).
///
/// `#[non_exhaustive]`: further facts may be added.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct RecoveryFacts {
    /// Whether the Store had been closed cleanly (`false` after a kill).
    pub clean_close: bool,
    /// The WAL commits replayed onto the snapshot.
    pub commits_replayed: u64,
    /// The snapshot's position.
    pub snapshot_commit_seq: u64,
    /// The position after the replay: the last commit acknowledged before the kill.
    pub commit_seq: u64,
    /// The interrupted last WAL write that was cut off (in words), if any.
    pub torn_tail: Option<String>,
    /// An interrupted multi-file operation completed (`prune`, ...).
    pub intent_completed: Option<String>,
    /// Temporary files removed (an interrupted checkpoint).
    pub tmp_removed: usize,
    /// A `GRAPH` / `GRAPH.copy` file rewritten (in words).
    pub graph_file_repaired: Option<String>,
    /// The `marks` file was rebuilt from the WAL.
    pub marks_rebuilt: bool,
    /// A refused WAL record cut now (in words).
    pub refused_cut: Option<String>,
    /// Damage of a redundant copy that did not stop the open.
    pub warnings: Vec<String>,
    /// The Store opened in maintenance mode (damage found: read-only).
    pub maintenance: bool,
}

impl RecoveryFacts {
    /// The facts of `report`.
    pub fn of(report: &OpenReport) -> Self {
        let cut = |c: &graphersal::persist::CutTail| {
            format!(
                "{} bytes cut at offset {} of the WAL segment {}",
                c.bytes, c.offset, c.segment
            )
        };
        Self {
            clean_close: report.clean_close,
            commits_replayed: report.commits_replayed,
            snapshot_commit_seq: report.snapshot_commit_seq,
            commit_seq: report.commit_seq,
            torn_tail: report.torn_tail.as_ref().map(cut),
            intent_completed: report.intent_completed.clone(),
            tmp_removed: report.tmp_removed,
            graph_file_repaired: report.graph_file_repaired.clone(),
            marks_rebuilt: report.marks_rebuilt,
            refused_cut: report.refused_cut.as_ref().map(cut),
            warnings: report.warnings.clone(),
            maintenance: report.maintenance,
        }
    }

    /// The facts in one line (the log, the console).
    pub fn summary(&self) -> String {
        let mut parts = vec![if self.clean_close {
            "closed cleanly".to_string()
        } else {
            "not closed cleanly".to_string()
        }];
        parts.push(format!(
            "{} WAL commit(s) replayed onto snapshot {}, now at commit {}",
            self.commits_replayed, self.snapshot_commit_seq, self.commit_seq
        ));
        if let Some(tail) = &self.torn_tail {
            parts.push(format!("torn tail: {tail}"));
        }
        if let Some(intent) = &self.intent_completed {
            parts.push(format!("completed an interrupted {intent}"));
        }
        if self.tmp_removed > 0 {
            parts.push(format!("{} temporary file(s) removed", self.tmp_removed));
        }
        if let Some(what) = &self.graph_file_repaired {
            parts.push(format!("GRAPH file rewritten: {what}"));
        }
        if self.marks_rebuilt {
            parts.push("marks rebuilt".to_string());
        }
        if let Some(cut) = &self.refused_cut {
            parts.push(format!("refused record cut: {cut}"));
        }
        for warning in &self.warnings {
            parts.push(format!("warning: {warning}"));
        }
        if self.maintenance {
            parts.push("opened in MAINTENANCE mode (damage found)".to_string());
        }
        parts.join("; ")
    }
}

/// One problem a verify found.
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ProblemFacts {
    /// The file, relative to the Store directory.
    pub path: String,
    /// The byte offset in it, when known.
    pub offset: Option<u64>,
    /// What is wrong.
    pub reason: String,
    /// What kind of problem it is: `damage` (damage or an inconsistency of the Store's files;
    /// also a missing blob that is not a known embedding model) or `model` (the blob of an
    /// embedding model is missing: [`ProblemFacts::model`]). Older reports have none
    /// (`damage`).
    #[serde(default = "damage_kind")]
    pub kind: String,
    /// `kind` `model`: the embedding model (its repository, `owner/name`) whose blob is
    /// missing; `None` when the blob is one of several missing models ([`Self::candidates`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// `kind` `model` without a [`Self::model`]: the models the blob may belong to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<String>,
    /// `kind` `model`: the blob's SHA-256 (hex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
    /// `kind` `model`: whether the server's shared models (`models/`) hold the model (then it
    /// only has to be added to the database again: `models add <repo>`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shared: bool,
    /// What fixes it, in words (`kind` `model`: add the model).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remedy: Option<String>,
}

/// The default [`ProblemFacts::kind`].
fn damage_kind() -> String {
    KIND_DAMAGE.to_string()
}

/// [`ProblemFacts::kind`] of damage.
pub const KIND_DAMAGE: &str = "damage";
/// [`ProblemFacts::kind`] of a missing embedding model blob.
pub const KIND_MODEL: &str = "model";
/// [`VerifyFacts::verdict`]: no problem found.
pub const VERDICT_OK: &str = "ok";
/// [`VerifyFacts::verdict`]: the only problems are missing embedding model blobs.
pub const VERDICT_NEEDS_MODEL: &str = "needs-model";
/// [`VerifyFacts::verdict`]: damage found.
pub const VERDICT_DAMAGED: &str = "damaged";

impl ProblemFacts {
    /// The problem in one line: `blobs/f0/f0ed...: the blob is missing`; a missing model's
    /// blob: `the embedding model acme/tiny (blob f0ed7648) is missing`.
    pub fn text(&self) -> String {
        if self.kind == KIND_MODEL {
            let short = self.blob.as_deref().map_or("?", short_hash);
            return match (&self.model, self.candidates.as_slice()) {
                (Some(model), _) => {
                    format!("the embedding model {model} (blob {short}) is missing")
                }
                (None, candidates) => format!(
                    "the blob {short} of one of the embedding models {} is missing",
                    candidates.join(", ")
                ),
            };
        }
        match self.offset {
            Some(offset) => format!("{} at byte {offset}: {}", self.path, self.reason),
            None => format!("{}: {}", self.path, self.reason),
        }
    }
}

/// The first 8 hex digits of a blob hash.
pub fn short_hash(hash: &str) -> &str {
    hash.get(..8).unwrap_or(hash)
}

/// What fixes a missing model blob: add the model (`shared`: `models/` holds it already).
pub fn model_remedy(model: Option<&str>, shared: bool) -> String {
    match (model, shared) {
        (Some(model), true) => format!(
            "the server's shared models hold {model}: add it to the database again (Models \
             tab, or graphersal-server models add {model}); the database is then ready to load"
        ),
        (Some(model), false) => format!(
            "download the model {model} (a Model2Vec directory) and import it on the Models tab \
             (a model directory as .tar/.tar.gz, or a .gsnap carrying it) or with \
             graphersal-server models add SOURCE; the database gets it and is ready to load"
        ),
        (None, _) => "import the missing model on the Models tab (a model directory as \
                      .tar/.tar.gz, or a .gsnap carrying it) or with graphersal-server models \
                      add SOURCE"
            .to_string(),
    }
}

/// The verify problem of a missing blob: its hash (`blobs/<2 hex>/<hash>`, the engine's text
/// "the blob is missing"), else `None`.
fn missing_blob_hash(problem: &ProblemFacts) -> Option<&str> {
    if problem.reason != "the blob is missing" {
        return None;
    }
    let rest = problem.path.strip_prefix("blobs/")?;
    let hash = rest.rsplit('/').next()?;
    (!hash.is_empty() && hash.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hash)
}

/// What a verify of a Store found (from the engine's [`VerifyReport`]).
///
/// `#[non_exhaustive]`: further facts may be added.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct VerifyFacts {
    /// Whether no problem was found.
    pub ok: bool,
    /// The damage and inconsistencies.
    pub problems: Vec<ProblemFacts>,
    /// Observations that are not damage.
    pub notes: Vec<String>,
    /// Snapshots checked (fully decoded).
    pub snapshots_checked: usize,
    /// WAL segments checked.
    pub segments_checked: usize,
    /// WAL records checked.
    pub records_checked: u64,
    /// The first commit found in the WAL (`0`: none).
    pub first_commit: u64,
    /// The last commit found in the WAL (`0`: none).
    pub last_commit: u64,
    /// Embedding models checked.
    pub models_checked: usize,
    /// Blobs checked.
    pub blobs_checked: usize,
    /// What was verified: `open store` (the loaded database's open Store) or `files` (its
    /// files without opening it: an unloaded, failed or base database).
    pub how: String,
    /// The verdict: `ok` (no problem), `needs-model` (the only problems are missing embedding
    /// model blobs: the database needs its model added, its graph data is intact) or `damaged`.
    /// Older reports have none: [`VerifyFacts::verdict`] derives it.
    #[serde(default)]
    pub verdict: String,
    /// The embedding models whose blobs are missing (repositories, sorted).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models_missing: Vec<String>,
}

impl VerifyFacts {
    /// The facts of `report`, verified as `how`.
    pub fn of(report: &VerifyReport, how: &str) -> Self {
        Self {
            ok: report.is_ok(),
            problems: report
                .problems
                .iter()
                .map(|p| ProblemFacts {
                    path: p.path.clone(),
                    offset: p.offset,
                    reason: p.reason.clone(),
                    kind: damage_kind(),
                    model: None,
                    candidates: Vec::new(),
                    blob: None,
                    shared: false,
                    remedy: None,
                })
                .collect(),
            notes: report.notes.clone(),
            snapshots_checked: report.snapshots_checked,
            segments_checked: report.segments_checked,
            records_checked: report.records_checked,
            first_commit: report.first_commit,
            last_commit: report.last_commit,
            models_checked: report.models_checked,
            blobs_checked: report.blobs_checked,
            how: how.to_string(),
            verdict: if report.is_ok() {
                VERDICT_OK
            } else {
                VERDICT_DAMAGED
            }
            .to_string(),
            models_missing: Vec::new(),
        }
    }

    /// The verdict ([`Self::verdict`], derived for a report without one).
    pub fn verdict(&self) -> &str {
        if !self.verdict.is_empty() {
            &self.verdict
        } else if self.ok {
            VERDICT_OK
        } else {
            VERDICT_DAMAGED
        }
    }

    /// Names the embedding models of the missing blobs among the problems (`kind` `model`)
    /// and sets the verdict `needs-model` when they are the only problems.
    ///
    /// - `known(hash)`: the model of a blob known exactly (the verify's own models by their
    ///   `ModelInfo::blob_hash`, else the manifest of the same blob in the server's shared
    ///   models);
    /// - `missing`: the models the engine reports missing (`ModelState::Missing` of the
    ///   verify's models): only a fallback for a missing blob no `known` names (none should be
    ///   left since graphersal 0.9.0 names every model's blob): it belongs to one of them by
    ///   elimination (one left: that one; as many blobs as models left: one of them, listed);
    /// - `shared(repo)`: whether the shared models hold `repo` (the remedy differs).
    ///
    /// A missing blob that no model can account for stays `damage` (generic text).
    pub fn name_models(
        &mut self,
        known: &dyn Fn(&str) -> Option<String>,
        missing: &[String],
        shared: &dyn Fn(&str) -> bool,
    ) {
        let mut named: BTreeSet<String> = BTreeSet::new();
        let mut unnamed: Vec<usize> = Vec::new();
        for (i, problem) in self.problems.iter_mut().enumerate() {
            let Some(hash) = missing_blob_hash(problem).map(str::to_string) else {
                continue;
            };
            match known(&hash) {
                Some(model) => {
                    problem.kind = KIND_MODEL.to_string();
                    problem.blob = Some(hash);
                    named.insert(model.clone());
                    problem.model = Some(model);
                }
                None => unnamed.push(i),
            }
        }
        let left: Vec<String> = missing
            .iter()
            .filter(|m| !named.contains(*m))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if !unnamed.is_empty() && unnamed.len() == left.len() {
            for i in unnamed {
                let problem = &mut self.problems[i];
                problem.blob = missing_blob_hash(problem).map(str::to_string);
                problem.kind = KIND_MODEL.to_string();
                if left.len() == 1 {
                    problem.model = Some(left[0].clone());
                    named.insert(left[0].clone());
                } else {
                    problem.candidates = left.clone();
                }
            }
            if left.len() > 1 {
                named.extend(left);
            }
        }
        for problem in &mut self.problems {
            if problem.kind != KIND_MODEL {
                continue;
            }
            problem.shared = problem.model.as_deref().is_some_and(shared);
            problem.remedy = Some(model_remedy(problem.model.as_deref(), problem.shared));
        }
        self.models_missing = named.into_iter().collect();
        if !self.problems.is_empty() && self.problems.iter().all(|p| p.kind == KIND_MODEL) {
            self.verdict = VERDICT_NEEDS_MODEL.to_string();
        }
    }

    /// The problems in words, one per line ([`ProblemFacts::text`]).
    pub fn problems_text(&self) -> String {
        self.problems
            .iter()
            .map(ProblemFacts::text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The facts in one line.
    pub fn summary(&self) -> String {
        if self.verdict() == VERDICT_NEEDS_MODEL {
            return format!(
                "verify: the graph data is intact; the blob of the embedding model(s) {} is \
                 missing (needs-model)",
                self.models_missing.join(", ")
            );
        }
        if self.ok {
            format!(
                "verify ok ({} snapshot(s), {} WAL record(s), commits {}..{})",
                self.snapshots_checked, self.records_checked, self.first_commit, self.last_commit
            )
        } else {
            format!(
                "verify found {} problem(s), first: {}",
                self.problems.len(),
                self.problems_text().lines().next().unwrap_or_default()
            )
        }
    }
}

/// The most problems [`LastVerify`] keeps (the count is always kept).
const LAST_VERIFY_PROBLEMS: usize = 100;

/// The last verify of a database (B-VERIFY), kept in its directory as [`LAST_VERIFY_FILE`]
/// (`databases/<name>/verify.json`, JSON, written atomically after every verify: the admin
/// verify, `db verify`, Verify all, the check after an unclean shutdown). It moves and goes
/// with the database's directory (a delete or a swap takes it along; a new database has
/// none: "never verified"). A missing file is "never verified"; an unreadable one is a warning
/// and "never verified" too, overwritten by the next verify.
///
/// `#[non_exhaustive]`: further fields may be added (readers ignore unknown keys).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct LastVerify {
    /// The file format's version (`1`).
    pub version: u32,
    /// When the verify ended, in Unix milliseconds.
    pub at_ms: u64,
    /// [`VerifyFacts::verdict`]: `ok`, `needs-model` or `damaged`; `failed` when the verify
    /// itself failed ([`Self::error`]).
    pub verdict: String,
    /// What was verified (`open store` | `files`).
    #[serde(default)]
    pub how: String,
    /// How many problems it found.
    #[serde(default)]
    pub problem_count: usize,
    /// The first problems (at most 100).
    #[serde(default)]
    pub problems: Vec<ProblemFacts>,
    /// The embedding models whose blobs were missing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models_missing: Vec<String>,
    /// Why the verify failed (verdict `failed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl LastVerify {
    /// The last verify of `facts`, ended at `at_ms`.
    pub fn of(facts: &VerifyFacts, at_ms: u64) -> Self {
        Self {
            version: 1,
            at_ms,
            verdict: facts.verdict().to_string(),
            how: facts.how.clone(),
            problem_count: facts.problems.len(),
            problems: facts
                .problems
                .iter()
                .take(LAST_VERIFY_PROBLEMS)
                .cloned()
                .collect(),
            models_missing: facts.models_missing.clone(),
            error: None,
        }
    }

    /// A verify that failed with `error` at `at_ms`.
    pub fn failed(error: &str, at_ms: u64) -> Self {
        Self {
            version: 1,
            at_ms,
            verdict: "failed".to_string(),
            how: String::new(),
            problem_count: 0,
            problems: Vec::new(),
            models_missing: Vec::new(),
            error: Some(error.to_string()),
        }
    }

    /// Reads the last verify of the database directory `db_dir`: `None` when it was never
    /// verified (or the file cannot be read: a warning).
    pub fn read(db_dir: &std::path::Path) -> Option<Self> {
        let path = db_dir.join(LAST_VERIFY_FILE);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
            Err(err) => {
                tracing::warn!(target: "graphersal_server", "cannot read {}: {err}", path.display());
                return None;
            }
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(last) if last.version == 1 => Some(last),
            Ok(last) => {
                tracing::warn!(
                    target: "graphersal_server",
                    "{}: version {} is not known: ignored",
                    path.display(),
                    last.version
                );
                None
            }
            Err(err) => {
                tracing::warn!(target: "graphersal_server", "cannot read {}: {err}", path.display());
                None
            }
        }
    }

    /// Writes it into the database directory `db_dir` atomically.
    ///
    /// `Err`: [`ServerError::Io`] (the directory is gone, the disk refuses).
    pub fn write(&self, db_dir: &std::path::Path) -> Result<(), ServerError> {
        let path = db_dir.join(LAST_VERIFY_FILE);
        let text = serde_json::to_string_pretty(self).map_err(|err| ServerError::Config {
            path: path.clone(),
            message: err.to_string(),
        })?;
        write_atomic(&path, text.as_bytes())
    }

    /// The last verify in words: `Last verify 2026-11-26 10:00:00 UTC: no problem found`.
    pub fn headline(&self) -> String {
        let when = crate::database::utc_text(self.at_ms / 1000);
        match self.verdict.as_str() {
            VERDICT_OK => format!("Last verify {when}: no problem found"),
            VERDICT_NEEDS_MODEL => format!(
                "Last verify {when}: the graph data is intact; the embedding model(s) {} \
                 missing (needs-model)",
                self.models_missing.join(", ")
            ),
            "failed" => format!(
                "Last verify {when}: the verify failed: {}",
                self.error.as_deref().unwrap_or_default()
            ),
            _ => format!(
                "Last verify {when}: {} problem(s), DAMAGED",
                self.problem_count
            ),
        }
    }
}

/// One database's check after an unclean shutdown.
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct DatabaseCheck {
    /// The database (real name).
    pub name: String,
    /// What the check found.
    pub outcome: CheckOutcome,
    /// Its state after the check (`loaded`, `maintenance`, `failed (...)`, ...).
    pub state: Option<String>,
    /// What the open of its Store did (a loaded plain or layer database).
    pub recovery: Option<RecoveryFacts>,
    /// What the verify found.
    pub verify: Option<VerifyFacts>,
    /// The damage report in words (a database in maintenance).
    pub damage: Option<String>,
    /// Why it failed, or a note.
    pub message: Option<String>,
    /// When it was checked (Unix milliseconds).
    pub checked_at_ms: Option<u64>,
}

impl DatabaseCheck {
    fn pending(name: &str) -> Self {
        Self {
            name: name.to_string(),
            outcome: CheckOutcome::Pending,
            state: None,
            recovery: None,
            verify: None,
            damage: None,
            message: None,
            checked_at_ms: None,
        }
    }
}

/// The last unclean shutdown and its check (`recovery.json`, module docs).
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct UncleanShutdown {
    /// When this server found it (Unix milliseconds).
    pub detected_at_ms: u64,
    /// The process id of the server that did not stop cleanly.
    pub previous_pid: Option<u32>,
    /// When that server had started (Unix milliseconds).
    pub previous_started_at_ms: Option<u64>,
    /// Whether its `server.run` was left behind.
    pub run_file_left: bool,
    /// Whether its open set was known (`server.open`); `false`: the `auto_load` databases
    /// were checked instead.
    pub databases_known: bool,
    /// One check per database.
    pub databases: Vec<DatabaseCheck>,
    /// Whether every check ended.
    pub done: bool,
}

impl UncleanShutdown {
    /// The check of `name`, if it was one of the databases checked.
    pub fn database(&self, name: &str) -> Option<&DatabaseCheck> {
        self.databases.iter().find(|d| d.name == name)
    }

    /// The headline in words (the start block, the log).
    pub fn headline(&self) -> String {
        format!(
            "the last server{} did not stop cleanly (killed, crashed or the machine lost \
             power){}",
            self.previous_pid
                .map(|pid| format!(" (pid {pid})"))
                .unwrap_or_default(),
            if self.databases_known {
                String::new()
            } else {
                "; the databases it had open are unknown, the auto-load ones are checked"
                    .to_string()
            }
        )
    }
}

/// Reads `recovery.json` of `dir`: the last unclean shutdown, `None` when there was none (or
/// the file cannot be read: a warning).
pub fn read_report(dir: &ServerDir) -> Option<UncleanShutdown> {
    let path = dir.recovery_path();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
        Err(err) => {
            tracing::warn!(target: "graphersal_server", "cannot read {}: {err}", path.display());
            return None;
        }
    };
    match serde_json::from_str(&text) {
        Ok(report) => Some(report),
        Err(err) => {
            tracing::warn!(target: "graphersal_server", "cannot read {}: {err}", path.display());
            None
        }
    }
}

/// The last unclean shutdown in memory and in `recovery.json` (held by the [`Loader`]).
#[derive(Debug)]
pub(crate) struct Recovery {
    dir: ServerDir,
    report: Mutex<Option<UncleanShutdown>>,
}

impl Recovery {
    /// The recovery state of `dir`: the last report of `recovery.json`, if any.
    pub(crate) fn open(dir: &ServerDir) -> Self {
        Self {
            dir: dir.clone(),
            report: Mutex::new(read_report(dir)),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Option<UncleanShutdown>> {
        self.report.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The last unclean shutdown.
    pub(crate) fn current(&self) -> Option<UncleanShutdown> {
        self.lock().clone()
    }

    /// Starts the report of a new unclean shutdown: `previous` with `names` to check.
    pub(crate) fn begin(&self, previous: &Previous, names: &[String]) -> UncleanShutdown {
        let report = UncleanShutdown {
            detected_at_ms: crate::control::unix_ms(std::time::SystemTime::now()),
            previous_pid: previous.pid,
            previous_started_at_ms: previous.started_at_ms,
            run_file_left: previous.run_file_left,
            databases_known: previous.databases.is_some(),
            databases: names.iter().map(|n| DatabaseCheck::pending(n)).collect(),
            done: names.is_empty(),
        };
        let mut guard = self.lock();
        *guard = Some(report.clone());
        self.save(&report);
        report
    }

    /// Replaces the check of `check.name` (and saves the file).
    fn update(&self, check: DatabaseCheck) {
        let mut guard = self.lock();
        if let Some(report) = guard.as_mut() {
            if let Some(slot) = report.databases.iter_mut().find(|d| d.name == check.name) {
                *slot = check;
            }
            let report = report.clone();
            self.save(&report);
        }
    }

    /// Marks the report done.
    fn finish(&self) {
        let mut guard = self.lock();
        if let Some(report) = guard.as_mut() {
            report.done = true;
            let report = report.clone();
            self.save(&report);
        }
    }

    fn save(&self, report: &UncleanShutdown) {
        let path = self.dir.recovery_path();
        let text = match serde_json::to_string_pretty(report) {
            Ok(text) => text,
            Err(err) => {
                tracing::warn!(target: "graphersal_server", "cannot write {}: {err}", path.display());
                return;
            }
        };
        if let Err(err) = write_atomic(&path, text.as_bytes()) {
            tracing::warn!(target: "graphersal_server", "{err}");
        }
    }
}

/// The databases to check after `previous`: its open set, else (unknown) the `auto_load`
/// databases of the registry; each name once, in order.
pub(crate) fn names_to_check(previous: &Previous, loader: &Loader) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let names: Vec<String> = match &previous.databases {
        Some(names) => names.clone(),
        None => loader
            .registry()
            .list()
            .into_iter()
            .filter(|e| e.config.as_ref().is_some_and(|c| c.auto_load))
            .map(|e| e.name)
            .collect(),
    };
    names
        .into_iter()
        .map(|n| crate::names::fold(&n))
        .filter(|n| seen.insert(n.clone()))
        .collect()
}

/// Starts the unclean-shutdown check (module docs) on a background thread: announces it (log
/// + audit), loads every database of `names` and checks it. Answers the report as begun.
pub(crate) fn start_check(
    loader: &Arc<Loader>,
    recovery: &Arc<Recovery>,
    previous: &Previous,
    names: Vec<String>,
    admin: Admin,
) -> UncleanShutdown {
    let report = recovery.begin(previous, &names);
    tracing::warn!(
        target: "graphersal_server",
        "unclean shutdown: {}; checking {}",
        report.headline(),
        if names.is_empty() {
            "no database (none was open)".to_string()
        } else {
            names.join(", ")
        }
    );
    admin.audit().record(AuditEvent::UncleanShutdown {
        pid: previous.pid,
        databases: names.clone(),
    });
    // Each database starts loading now (beside the auto-load ones), so they load in parallel.
    for name in &names {
        if loader.registry().get(name).is_ok()
            && let Err(err) = loader.load(name)
        {
            tracing::warn!(target: "graphersal_server", "unclean shutdown: cannot load '{name}': {err}");
        }
    }
    if names.is_empty() {
        return report;
    }
    let (loader, recovery) = (loader.clone(), recovery.clone());
    let spawned = std::thread::Builder::new()
        .name("graphersal-server-recovery".to_string())
        .spawn(move || {
            for name in &names {
                let mut checking = DatabaseCheck::pending(name);
                checking.outcome = CheckOutcome::Checking;
                recovery.update(checking);
                let check = check_one(&loader, &admin, name);
                log_check(&check);
                admin.audit().record(AuditEvent::DatabaseChecked {
                    name: check.name.clone(),
                    outcome: check.outcome.as_str().to_string(),
                    detail: check_detail(&check),
                });
                recovery.update(check);
            }
            recovery.finish();
            tracing::info!(target: "graphersal_server", "unclean shutdown: every check ended");
        });
    if let Err(err) = spawned {
        tracing::warn!(target: "graphersal_server", "unclean shutdown: cannot start the check: {err}");
    }
    report
}

/// The check of one database (module docs).
fn check_one(loader: &Arc<Loader>, admin: &Admin, name: &str) -> DatabaseCheck {
    let mut check = DatabaseCheck::pending(name);
    check.outcome = CheckOutcome::Checking;
    let now = || Some(crate::control::unix_ms(std::time::SystemTime::now()));
    let state = match loader.wait(name, LOAD_WAIT) {
        Ok(state) => state,
        Err(err) => {
            check.outcome = CheckOutcome::Missing;
            check.message = Some(err.to_string());
            check.checked_at_ms = now();
            return check;
        }
    };
    check.recovery = loader.open_report_of(name).map(|r| RecoveryFacts::of(&r));
    match admin.verify(name) {
        Ok(verified) => {
            check.verify = Some(verified.facts);
            check.state = Some(verified.state.to_string());
            check.outcome = match &verified.state {
                DbState::Maintenance { damage } => {
                    check.damage = Some(damage.clone());
                    CheckOutcome::Damaged
                }
                DbState::Failed { reason } => {
                    check.message = Some(reason.clone());
                    CheckOutcome::Failed
                }
                DbState::NeedsModel { repo } => {
                    check.message = Some(format!("it needs the embedding model '{repo}'"));
                    CheckOutcome::NeedsModel
                }
                _ if check
                    .verify
                    .as_ref()
                    .is_some_and(|v| v.verdict() == VERDICT_NEEDS_MODEL) =>
                {
                    let models = check
                        .verify
                        .as_ref()
                        .map(|v| v.models_missing.join(", "))
                        .unwrap_or_default();
                    check.message = Some(format!("it needs the embedding model '{models}'"));
                    CheckOutcome::NeedsModel
                }
                _ if check.verify.as_ref().is_some_and(|v| !v.ok) => CheckOutcome::Damaged,
                _ => CheckOutcome::Ok,
            };
        }
        Err(err) => {
            check.state = Some(state.to_string());
            check.message = Some(format!("the verify failed: {err}"));
            check.outcome = match state {
                DbState::Failed { .. } => CheckOutcome::Failed,
                _ => CheckOutcome::Damaged,
            };
        }
    }
    check.checked_at_ms = now();
    check
}

/// The check's detail for the audit record.
fn check_detail(check: &DatabaseCheck) -> String {
    let mut parts = Vec::new();
    if let Some(recovery) = &check.recovery {
        parts.push(recovery.summary());
    }
    if let Some(verify) = &check.verify {
        parts.push(verify.summary());
    }
    if let Some(message) = &check.message {
        parts.push(message.clone());
    }
    parts.join("; ")
}

/// Logs the check of one database.
fn log_check(check: &DatabaseCheck) {
    let detail = check_detail(check);
    let name = &check.name;
    match check.outcome {
        CheckOutcome::Ok => tracing::info!(
            target: "graphersal_server",
            "unclean shutdown: database '{name}' is ok: {detail}"
        ),
        CheckOutcome::Damaged => tracing::error!(
            target: "graphersal_server",
            "unclean shutdown: database '{name}' is DAMAGED: {detail}; served read-only; repair \
             it into a new database (graphersal-server db repair {name}) or restore a backup \
             (graphersal-server db backups {name})"
        ),
        _ => tracing::warn!(
            target: "graphersal_server",
            "unclean shutdown: database '{name}' is {}: {detail}",
            check.outcome.as_str()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn the_open_file_round_trips_and_marks_an_unclean_shutdown() {
        let temp = TempDir::new("recovery-open");
        let dir = ServerDir::init(temp.path()).unwrap().dir;
        assert_eq!(detect(&dir), None);
        let record = OpenRecord::new(dir.clone(), 42);
        record.write(vec!["acme".into(), "beta".into()]).unwrap();
        let read = OpenFile::read(&dir).unwrap().unwrap();
        assert_eq!(read.databases, ["acme", "beta"]);
        assert_eq!(read.started_at_ms, 42);
        let previous = detect(&dir).unwrap();
        assert_eq!(previous.databases, Some(vec!["acme".into(), "beta".into()]));
        assert_eq!(previous.pid, Some(std::process::id()));
        assert!(!previous.run_file_left);
        OpenFile::remove(&dir).unwrap();
        assert_eq!(detect(&dir), None);
    }

    #[test]
    fn a_stale_run_file_alone_is_an_unclean_shutdown_with_an_unknown_open_set() {
        let temp = TempDir::new("recovery-run");
        let dir = ServerDir::init(temp.path()).unwrap().dir;
        RunInfo::new(
            "127.0.0.1:4242".parse().unwrap(),
            "http://127.0.0.1:4242/".into(),
        )
        .with_started_at_ms(7)
        .write(&dir)
        .unwrap();
        let previous = detect(&dir).unwrap();
        assert_eq!(previous.databases, None);
        assert!(previous.run_file_left);
        assert_eq!(previous.started_at_ms, Some(7));
        assert_eq!(previous.url.as_deref(), Some("http://127.0.0.1:4242/"));
    }

    #[test]
    fn the_report_is_kept_in_recovery_json() {
        let temp = TempDir::new("recovery-report");
        let dir = ServerDir::init(temp.path()).unwrap().dir;
        let recovery = Recovery::open(&dir);
        assert_eq!(recovery.current(), None);
        let previous = Previous {
            pid: Some(9),
            started_at_ms: None,
            url: None,
            databases: Some(vec!["acme".into()]),
            run_file_left: true,
        };
        let report = recovery.begin(&previous, &["acme".to_string()]);
        assert!(!report.done);
        assert_eq!(report.databases[0].outcome, CheckOutcome::Pending);
        let mut check = DatabaseCheck::pending("acme");
        check.outcome = CheckOutcome::Ok;
        recovery.update(check);
        recovery.finish();
        let read = read_report(&dir).unwrap();
        assert!(read.done);
        assert_eq!(read.database("acme").unwrap().outcome, CheckOutcome::Ok);
        assert_eq!(Recovery::open(&dir).current(), Some(read));
        let json = serde_json::to_value(recovery.current().unwrap()).unwrap();
        assert_eq!(json["databases"][0]["outcome"], "ok");
        assert_eq!(json["previousPid"], 9);
    }
}
