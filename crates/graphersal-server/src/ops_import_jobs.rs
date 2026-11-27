//! The stored backup jobs of an imported database (KBFIX, part of [`Admin::import`]): a Store
//! or snapshot made elsewhere (by the graphersal CLI's dev server, another server) may carry
//! scheduled jobs that write to a path outside this server's backup root (`server.toml [jobs]
//! backup_root`, by default `backups/jobs/`), typically an ABSOLUTE path of another machine.
//! The server never runs such a job: every load drops it before the database serves (module
//! docs of [`crate::Loader`], "Backup root"), with a warning in the log. The import reports them
//! up front, so the operator learns it when importing, not from a log line later:
//!
//! - every job writing outside the root ([`JobOutside`]: its name, the path, why) is listed in
//!   the import's answer (`jobsOutside`), the console's output and the web UI's outcome, with
//!   whether it will run;
//! - with the import option "rebase jobs" (`--rebase-jobs`, `"rebaseJobs": true`,
//!   `rebaseJobs=true`) each such BACKUP job is rewritten in the new database's Store before
//!   the database appears (one unit): its path becomes `<backup root>/<database>/<job>` (the
//!   job name with every character but `A-Z a-z 0-9 . _ -` as `_`), the directory a job defined
//!   now with the relative path `<database>/<job>` would write; its other fields (schedule,
//!   enabled, limits, description, the effects' actions) stay, so it runs there from the first
//!   load on (`rebasedTo`). A job of another action that writes outside the root (a saved
//!   query's file effect) is never rewritten: it is dropped at load like before.
//!
//! The jobs are read only when the Store's catalog holds any (`Store::inspect`); then the new
//! Store is opened (read-only, or for writing to rebase), so a large import with jobs takes one
//! more load of its graph. A Store that cannot be opened here (an embedding model its schema
//! needs is missing: `needs-model`) is not checked: a note says so (its jobs are checked, and
//! those outside the root dropped, at its first load). A base imports no check: a base never
//! runs jobs.

use std::path::{Path, PathBuf};

use graphersal::auth::{AccessRequest, Resource};
use graphersal::catalog::{Definition, DefinitionKind, JobDefinition, JobTask};
use graphersal::persist::{RecoveryTarget, Store, StoreOptions};
use graphersal::prelude::{Graph, GraphStorage, Transactional};
use graphersal::storage::{LayerBase, LayeredGraph};

use super::{Admin, store_error};
use crate::error::ServerError;

/// A stored job of an imported database that writes outside the backup root.
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct JobOutside {
    /// The job's name.
    pub job: String,
    /// The path it writes, as stored.
    pub path: PathBuf,
    /// Why the path is refused.
    pub reason: String,
    /// The path inside the backup root it was rewritten to (the import's "rebase jobs"); `None`:
    /// not rewritten, the job is dropped at the database's first load and never runs.
    pub rebased_to: Option<PathBuf>,
}

/// What the check of an import's jobs found.
#[derive(Debug, Default)]
pub(super) struct JobsChecked {
    /// The jobs writing outside the root.
    pub(super) outside: Vec<JobOutside>,
    /// A note when the jobs could not be checked.
    pub(super) note: Option<String>,
}

impl Admin {
    /// The jobs of the new Store `store_dir` (database `name`; `layer`: a layer Store over that
    /// base) writing outside the backup root, rewritten under it when `self` rebases jobs
    /// (module docs).
    ///
    /// `Err`: [`ServerError::Config`] / [`ServerError::Io`] (the backup root cannot be made);
    /// [`ServerError::Store`] when the rewrite fails (the import fails, nothing is left).
    pub(super) fn check_import_jobs(
        &self,
        name: &str,
        store_dir: &Path,
        layer: Option<&LayerBase>,
    ) -> Result<JobsChecked, ServerError> {
        let jobs = Store::inspect(store_dir)
            .ok()
            .and_then(|info| info.definitions_by_kind.get("job").copied())
            .unwrap_or_default();
        if jobs == 0 {
            return Ok(JobsChecked::default());
        }
        let dir = self.registry.dir();
        let root = dir.ensure_backup_root(&dir.backup_root(&dir.load_config()?))?;
        let rebase = self.rebase_jobs;
        let not_checked = |err: &dyn std::fmt::Display| JobsChecked {
            outside: Vec::new(),
            note: Some(format!(
                "its {jobs} stored job(s) could not be checked against the backup root ({err}); \
                 a job writing outside it is dropped at the database's first load{}",
                if rebase { ", so none was rebased" } else { "" }
            )),
        };
        let checked = match (layer, rebase) {
            (None, false) => match Store::open_read_only(store_dir, RecoveryTarget::Latest) {
                Ok(recovered) => outside_of(name, &recovered.into_graph(), &root, false),
                Err(err) => return Ok(not_checked(&err)),
            },
            (Some(base), false) => {
                match Store::<LayeredGraph>::open_layer_read_only(
                    store_dir,
                    base,
                    RecoveryTarget::Latest,
                ) {
                    Ok(recovered) => outside_of(name, &recovered.into_graph(), &root, false),
                    Err(err) => return Ok(not_checked(&err)),
                }
            }
            (None, true) => match Store::open(store_dir, StoreOptions::new()) {
                Ok(store) => {
                    let out = outside_of(name, store.graph(), &root, true);
                    close(name, store.close())?;
                    out
                }
                Err(err) => return Ok(not_checked(&err)),
            },
            (Some(base), true) => {
                match Store::<LayeredGraph>::open_layer(store_dir, base, StoreOptions::new()) {
                    Ok(store) => {
                        let out = outside_of(name, store.graph(), &root, true);
                        close(name, store.close())?;
                        out
                    }
                    Err(err) => return Ok(not_checked(&err)),
                }
            }
        };
        let outside = checked?;
        for job in &outside {
            match &job.rebased_to {
                Some(to) => tracing::info!(
                    target: "graphersal_server",
                    "import '{name}': the backup job '{}' wrote '{}' outside the backup root; \
                     rebased to '{}'",
                    job.job,
                    job.path.display(),
                    to.display()
                ),
                None => tracing::warn!(
                    target: "graphersal_server",
                    "import '{name}': the job '{}' writes '{}' outside the backup root ({}); it \
                     is dropped at the first load",
                    job.job,
                    job.path.display(),
                    job.reason
                ),
            }
        }
        Ok(JobsChecked {
            outside,
            note: None,
        })
    }
}

/// The jobs of `graph` writing outside `root`; with `rebase` each backup job among them is
/// rewritten under `root` in ONE unit.
fn outside_of<S: GraphStorage>(
    name: &str,
    graph: &Graph<S>,
    root: &Path,
    rebase: bool,
) -> Result<Vec<JobOutside>, ServerError> {
    let failed = |what: &str, err: &dyn std::fmt::Display| {
        store_error(name, &format!("cannot {what} its stored jobs: {err}"))
    };
    let definitions: Vec<Definition> = {
        let read = graph.read_checked().map_err(|err| failed("read", &err))?;
        read.definitions()
            .map_err(|err| failed("read", &err))?
            .filter(|d| d.kind() == DefinitionKind::JOB)
            .cloned()
            .collect()
    };
    let mut found = graphersal_serve::jobs_outside_file_root(&definitions, root);
    found.dedup_by(|a, b| a.job == b.job);
    let mut outside: Vec<JobOutside> = found
        .into_iter()
        .map(|job| JobOutside {
            job: job.job,
            path: job.path,
            reason: job.reason,
            rebased_to: None,
        })
        .collect();
    if !rebase {
        return Ok(outside);
    }
    let mut rewritten = Vec::new();
    for job in &mut outside {
        let Some(definition) = definitions.iter().find(|d| d.name() == job.job) else {
            continue;
        };
        let target = root.join(path_word(name)).join(path_word(&job.job));
        if let Some(new) = rebased(definition, &target) {
            rewritten.push(new);
            job.rebased_to = Some(target);
        }
    }
    if !rewritten.is_empty() {
        let mut write = graph
            .write_checked()
            .map_err(|err| failed("rebase", &err))?;
        write
            .transaction(|g| g.set_definitions(rewritten))
            .map_err(|err| failed("rebase", &err))?;
    }
    Ok(outside)
}

/// The backup job `definition` writing into `target` instead (its path and every file effect;
/// every other field kept); `None` for a job of another action or one this version keeps
/// opaque.
fn rebased(definition: &Definition, target: &Path) -> Option<Definition> {
    let job = definition.as_job()?;
    if job.unserved_reason().is_some() || !matches!(job.action(), JobTask::Backup { .. }) {
        return None;
    }
    let path = target.to_string_lossy().to_string();
    let effects = job
        .effects()
        .iter()
        .map(|effect| {
            let request = effect.as_request();
            match request.resource {
                Resource::File { .. } => {
                    AccessRequest::new(request.action, Resource::File { path: target })
                        .to_owned_request()
                }
                _ => effect.clone(),
            }
        })
        .collect();
    // Through the payload, so fields this version does not know stay as they are.
    let template = JobDefinition::new(job.schedule(), JobTask::Backup { path: path.clone() })
        .with_effects(effects)
        .to_payload();
    let mut payload = job.to_payload();
    payload.insert("path", path.as_str().into());
    payload.insert("effects", template.get("effects")?.clone());
    let new = JobDefinition::from_payload(&payload).ok()?;
    Some(Definition::job(definition.name(), new))
}

/// `text` as one path component: every character but `A-Z a-z 0-9 . _ -` as `_`, a name of
/// only dots as `_`.
fn path_word(text: &str) -> String {
    let word: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if word.is_empty() || word.chars().all(|c| c == '.') {
        "_".to_string()
    } else {
        word
    }
}

/// The close of a Store opened for the check.
fn close(
    name: &str,
    closed: Result<(), graphersal::persist::PersistError>,
) -> Result<(), ServerError> {
    closed.map_err(|err| store_error(name, &format!("cannot close its Store: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_name_becomes_one_path_component() {
        assert_eq!(path_word("nightly"), "nightly");
        assert_eq!(path_word("a/b c"), "a_b_c");
        assert_eq!(path_word(".."), "_");
        assert_eq!(path_word(""), "_");
        assert_eq!(path_word("v1.2-x_y"), "v1.2-x_y");
    }

    #[test]
    fn a_rebased_backup_job_keeps_its_fields_and_moves_its_file_effect() {
        use graphersal::auth::Action;
        let old = Path::new("/elsewhere/kb-backups");
        let job = JobDefinition::new(
            "0 3 * * *",
            JobTask::Backup {
                path: old.display().to_string(),
            },
        )
        .with_enabled(false)
        .with_description("nightly")
        .with_effects(vec![
            AccessRequest::new(Action::Create, Resource::File { path: old }).to_owned_request(),
        ]);
        let target = Path::new("/srv/backups/jobs/kb/nightly");
        let new = rebased(&Definition::job("nightly", job), target).unwrap();
        let new_job = new.as_job().unwrap();
        assert_eq!(new.name(), "nightly");
        assert_eq!(
            new_job.action(),
            &JobTask::Backup {
                path: target.display().to_string()
            }
        );
        assert!(!new_job.enabled());
        assert_eq!(new_job.description(), "nightly");
        assert_eq!(new_job.schedule(), "0 3 * * *");
        let effect = new_job.effects()[0].as_request();
        assert!(matches!(effect.action, Action::Create));
        assert!(matches!(effect.resource, Resource::File { path } if path == target));
        // Another action is not rewritten.
        let checkpoint =
            Definition::job("cp", JobDefinition::new("0 3 * * *", JobTask::Checkpoint));
        assert!(rebased(&checkpoint, target).is_none());
    }
}
