//! The run administration of the loaded databases ([`Admin::runs`], [`Admin::cancel_run`]):
//! every query that waits or runs on a database, whatever its client (a browser page under
//! `/db/<name>/`, an MCP session, a console or script client), and the cancel of one by its id.
//! Part of [`Admin`] (module docs of [`crate::ops`]).
//!
//! Each loaded database's context has its own run registry (graphersal-serve's `Runs`): a run
//! id is unique within its database only, so a cancel names the database. The admin API offers
//! it under `/api/admin/` (the server's admin surface, gated like every admin route by the
//! `Host`/`Origin` checks); the databases' own mounts keep graphersal-serve's
//! `/db/<name>/api/admin/runs` off (`ContextBuilder::admin_runs` is not set), so the query
//! texts of other clients are readable only through the server's admin surface.

use std::time::{Duration, SystemTime};

use graphersal_serve::{Context, RunEntry, ServeError};

use super::Admin;
use crate::error::ServerError;
use crate::registry::DbState;

/// One queued or running run of a loaded database ([`Admin::runs`]).
#[derive(Debug, Clone)]
pub struct DatabaseRun {
    /// The database (its real name).
    pub database: String,
    /// The run registry's id of the run (unique within the database).
    pub id: u64,
    /// The client's key: `page:<client id>`, `mcp:<session>` or `mcp`, ...
    pub client: String,
    /// The client's own id of the run (a page's `runId`, an MCP request id), if it sent one.
    pub alias: Option<String>,
    /// What the run does: `query`, `layout`, `tool`, `other`.
    pub kind: &'static str,
    /// `queued` (its job waits) or `running`.
    pub state: &'static str,
    /// Whether it was cancelled (it ends at its next check, or when it starts).
    pub cancelled: bool,
    /// When it was registered.
    pub started_at: SystemTime,
    /// The time since it was registered (waiting included).
    pub duration: Duration,
    /// The first characters of its query text (graphersal-serve's `RUN_TEXT_HEAD_CHARS`), if
    /// it has one.
    pub text: Option<String>,
}

impl DatabaseRun {
    fn of(database: &str, entry: &RunEntry) -> Self {
        let info = entry.info();
        Self {
            database: database.to_string(),
            id: info.id(),
            client: info.client().to_string(),
            alias: info.alias().map(str::to_string),
            kind: info.kind().name(),
            state: entry.state().name(),
            cancelled: entry.is_cancelled(),
            started_at: info.started_at(),
            duration: entry.duration(),
            text: info.text().map(str::to_string),
        }
    }
}

impl Admin {
    /// The queued and running runs of the loaded database `name`,
    /// oldest first, or of every loaded database (`None`; by database name, then oldest first).
    /// A database that is not loaded has none.
    ///
    /// `Err`: [`ServerError::NotFound`] (no database `name`), [`ServerError::Refused`]
    /// (offline: no server runs, so nothing runs), [`ServerError::Serve`] (a run registry
    /// cannot be read).
    pub fn runs(&self, name: Option<&str>) -> Result<Vec<DatabaseRun>, ServerError> {
        let Some(loader) = &self.loader else {
            return Err(self.not_running(name.unwrap_or("runs"), "list the runs of"));
        };
        let names = match name {
            Some(name) => vec![self.registry.get(name)?.name],
            None => {
                let mut loaded = loader.loaded();
                loaded.sort();
                loaded
            }
        };
        let mut rows = Vec::new();
        for database in names {
            let Some(context) = loader.context(&database) else {
                continue;
            };
            rows.extend(
                list(&context)
                    .map_err(|err| serve_error(&database, "list its runs", err))?
                    .iter()
                    .map(|entry| DatabaseRun::of(&database, entry)),
            );
        }
        Ok(rows)
    }

    /// Cancels the run `id` of the database `name`, whatever its
    /// client: a waiting run stops when it starts, a running one at its next check (its unit
    /// rolls back). `Ok(false)` when the database has no such run (it ended, or the database is
    /// not loaded).
    ///
    /// `Err`: [`ServerError::NotFound`], [`ServerError::Refused`] (offline),
    /// [`ServerError::Serve`].
    pub fn cancel_run(&self, name: &str, id: u64) -> Result<bool, ServerError> {
        let Some(loader) = &self.loader else {
            return Err(self.not_running(name, "cancel a run of"));
        };
        let database = self.registry.get(name)?.name;
        let Some(context) = loader.context(&database) else {
            return Ok(false);
        };
        // Only a run of this database's registry (an id is unique within it only).
        let mine = list(&context)
            .map_err(|err| serve_error(&database, "list its runs", err))?
            .iter()
            .any(|entry| entry.info().id() == id);
        if !mine {
            return Ok(false);
        }
        let cancelled = context
            .runs()
            .cancel_run(id)
            .map_err(|err| serve_error(&database, "cancel the run", err))?;
        if cancelled {
            tracing::info!(
                target: "graphersal_server",
                "database '{database}': run {id} cancelled by the run administration"
            );
        }
        Ok(cancelled)
    }

    /// Whether the database `name` is loaded now (for the answer of an empty run list).
    pub(crate) fn is_loaded(&self, name: &str) -> bool {
        self.registry
            .get(name)
            .is_ok_and(|entry| entry.state == DbState::Loaded)
    }
}

/// The runs of `context`'s registry: every run of the database (each context has its own
/// registry, so nothing else is in it; runs registered without a mount, such as a host's, are
/// listed too).
fn list(context: &Context) -> Result<Vec<RunEntry>, ServeError> {
    context.runs().list()
}

fn serve_error(database: &str, what: &str, err: ServeError) -> ServerError {
    ServerError::Serve {
        name: database.to_string(),
        message: format!("cannot {what}: {err}"),
    }
}
