//! Verify, repair and swap (round 3 B-CRASH, [`crate::recovery`]): the admin side of a
//! damaged Store.
//!
//! - [`Admin::verify`]: the engine's scrub of one database's Store. A loaded plain or layer
//!   database: its open Store (`Store::verify`, the context held so an unload waits); found
//!   damage under the damage policy `maintenance` makes the engine refuse writes, and the
//!   database's state becomes `maintenance`. Anything else (unloaded, failed, needs-model, a
//!   base, the console without a server): its files (`Store::verify_dir`, no lock, nothing
//!   written; a layer Store is verified without its base). Audited as
//!   [`AuditEvent::DatabaseChecked`].
//! - [`Admin::repair`]: the engine's repair (`Store::repair_to` of the open Store, else
//!   `Store::repair_dir`) into a NEW database built in `tmp/` (default name
//!   `<name>-repaired`, `-2`, ... when taken): never in place, the damaged database is not
//!   changed. The new database is a plain database with the damaged one's description and
//!   read-only flag, `auto_load` off; the engine verifies it before the build is committed. A
//!   layer or base database is refused (the engine repairs plain Stores only; a base's new
//!   identity would not match its layers): restore a backup instead.
//! - [`Admin::swap`]: the repaired copy takes the damaged database's name: both unloaded first
//!   (the old one is loaded again afterwards when it was loaded), `databases/<name>/` moves to
//!   `trash/`, `databases/<with>/` is renamed to `databases/<name>/` and gets the old
//!   `database.toml` back (description, flags, limits), the name `<with>` is gone (its grants
//!   and MCP settings removed; refused while an alias names it). Grants, aliases and MCP
//!   settings of `<name>` stay. Both must be plain normal databases.

use graphersal::persist::{DamagePolicy, RepairReport, Store};
use serde_json::{Value, json};

use super::{Admin, store_error};
use crate::audit::AuditEvent;
use crate::database::{DatabaseConfig, DatabaseKind, DatabaseRole};
use crate::error::ServerError;
use crate::loader::OpenStoreRef;
use crate::names::fold;
use crate::recovery::VerifyFacts;
use crate::registry::{DatabaseEntry, DbState};

/// How many diverged elements a repair answer lists (the count is always given).
const DIVERGED_SHOWN: usize = 100;

/// What [`Admin::verify`] found.
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Verified {
    /// The database (real name).
    pub name: String,
    /// What the verify found.
    pub facts: VerifyFacts,
    /// The database's state afterwards (`maintenance` when the verify froze its open Store).
    pub state: DbState,
}

/// What [`Admin::repair`] built.
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Repaired {
    /// The damaged database (unchanged).
    pub source: String,
    /// The new database.
    pub entry: DatabaseEntry,
    /// The engine's report.
    pub report: RepairReport,
}

/// What [`Admin::swap`] did.
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Swapped {
    /// The database under the kept name now.
    pub entry: DatabaseEntry,
    /// The name that is gone.
    pub with: String,
    /// Where the replaced database went.
    pub trash: std::path::PathBuf,
    /// Whether it was loaded again (it was loaded before).
    pub reloaded: bool,
    /// How many grants on the gone name were removed.
    pub grants_removed: usize,
}

impl Admin {
    /// Verifies the database `name` (module docs).
    ///
    /// `Err`: [`ServerError::NotFound`]; [`ServerError::Store`] when the verify itself fails
    /// (a Store that cannot be read at all).
    pub fn verify(&self, name: &str) -> Result<Verified, ServerError> {
        let entry = self.registry.get(name)?;
        let key = entry.name.clone();
        let open = self.loader.as_ref().and_then(|l| l.open_store_of(&key));
        let failed = |err: graphersal::persist::PersistError| {
            store_error(&key, &format!("cannot verify it: {err}"))
        };
        let (facts, frozen) = match open {
            Some((_context, OpenStoreRef::Plain(store))) => {
                let report = store.verify().map_err(failed)?;
                let frozen = frozen(store.damage_found().ok(), store.damage_policy().ok());
                (VerifyFacts::of(&report, "open store"), frozen)
            }
            Some((_context, OpenStoreRef::Layer(store))) => {
                let report = store.verify().map_err(failed)?;
                let frozen = frozen(store.damage_found().ok(), store.damage_policy().ok());
                (VerifyFacts::of(&report, "open store"), frozen)
            }
            None => {
                let report = Store::verify_dir(entry.store_dir().as_path()).map_err(failed)?;
                (VerifyFacts::of(&report, "files"), false)
            }
        };
        if frozen && entry.state == DbState::Loaded {
            let damage = format!(
                "verify found {} problem(s):\n{}",
                facts.problems.len(),
                facts.problems_text()
            );
            self.registry
                .set_state(&key, DbState::maintenance(damage))?;
            tracing::error!(
                target: "graphersal_server",
                "database '{key}': the verify found damage; it is read-only from now on \
                 (maintenance)"
            );
        }
        let state = self.registry.get(&key)?.state;
        let outcome = if !facts.ok || matches!(state, DbState::Maintenance { .. }) {
            "damaged"
        } else {
            "ok"
        };
        self.audit.record(AuditEvent::DatabaseChecked {
            name: key.clone(),
            outcome: outcome.to_string(),
            detail: facts.summary(),
        });
        Ok(Verified {
            name: key,
            facts,
            state,
        })
    }

    /// Verifies every database ([`Admin::verify`] each, in name order): each one's result,
    /// a failure with the database's name.
    pub fn verify_all(&self) -> Vec<(String, Result<Verified, ServerError>)> {
        self.registry
            .list()
            .into_iter()
            .map(|entry| {
                let result = self.verify(&entry.name);
                (entry.name, result)
            })
            .collect()
    }

    /// Repairs the database `name` into the new database `new_name` (default
    /// `<name>-repaired`; module docs).
    ///
    /// `Err`: [`ServerError::NotFound`]; [`ServerError::DatabaseFailed`] without a
    /// configuration; [`ServerError::Refused`] for a layer or base database; the name errors of
    /// a new database; [`ServerError::Store`] when the repair fails.
    pub fn repair(&self, name: &str, new_name: Option<&str>) -> Result<Repaired, ServerError> {
        let entry = self.registry.get(name)?;
        let key = entry.name.clone();
        let Some(config) = entry.config.clone() else {
            return Err(ServerError::DatabaseFailed {
                name: key,
                reason: entry.state.to_string(),
            });
        };
        if config.kind == DatabaseKind::Layer {
            return Err(ServerError::Refused {
                name: key,
                message: "a layer database cannot be repaired (the engine repairs plain Stores \
                          only): restore a backup of it, or rebuild the layer"
                    .to_string(),
            });
        }
        if config.role == DatabaseRole::Base {
            return Err(ServerError::Refused {
                name: key,
                message: "a base database cannot be repaired (a repaired copy has a new \
                          identity, which its layers do not match): restore a backup of it"
                    .to_string(),
            });
        }
        let target = match new_name {
            Some(given) => fold(given),
            None => self.free_repair_name(&key),
        };
        let new_config = DatabaseConfig::plain()
            .with_read_only(config.read_only)
            .with_description(config.description.clone());
        let build = self.registry.prepare_tmp(&target, new_config)?;
        let store_dir = build.store_dir();
        let open = self.loader.as_ref().and_then(|l| l.open_store_of(&key));
        let failed = |err: graphersal::persist::PersistError| {
            store_error(&key, &format!("cannot repair it: {err}"))
        };
        let report = match open {
            // Loaded: its open Store repairs (the context held makes an unload wait).
            Some((_context, OpenStoreRef::Plain(store))) => {
                store.repair_to(store_dir.as_path()).map_err(failed)?
            }
            Some((_context, OpenStoreRef::Layer(_))) => {
                return Err(ServerError::Refused {
                    name: key,
                    message: "a layer database cannot be repaired".to_string(),
                });
            }
            None => Store::repair_dir(entry.store_dir().as_path(), store_dir.as_path())
                .map_err(failed)?,
        };
        self.adopt_build(&target, &store_dir);
        let (new_entry, _leftover) = self.commit_new(build)?;
        let lossless = report.is_lossless();
        self.audit.record(AuditEvent::DatabaseRepaired {
            source: key.clone(),
            name: new_entry.name.clone(),
            lossless,
        });
        tracing::info!(
            target: "graphersal_server",
            "database '{key}' repaired into '{}' at commit {} ({})",
            new_entry.name,
            report.commit_seq,
            if lossless { "lossless" } else { "data lost: see the repair report" }
        );
        Ok(Repaired {
            source: key,
            entry: new_entry,
            report,
        })
    }

    /// `<name>-repaired`, else `<name>-repaired-2`, ... (the first name no database or alias
    /// has).
    fn free_repair_name(&self, key: &str) -> String {
        let taken = |n: &str| self.registry.resolve(n).is_ok();
        let first = format!("{key}-repaired");
        if !taken(&first) {
            return first;
        }
        (2..1000)
            .map(|n| format!("{key}-repaired-{n}"))
            .find(|candidate| !taken(candidate))
            .unwrap_or(first)
    }

    /// Lets the database `with` take the place of the database `name` (module docs).
    ///
    /// `Err`: [`ServerError::NotFound`]; [`ServerError::InvalidInput`] for the same name twice;
    /// [`ServerError::Refused`] for a layer or base database, while an alias names `with`, or
    /// while one of them cannot be unloaded; [`ServerError::Io`].
    pub fn swap(&self, name: &str, with: &str) -> Result<Swapped, ServerError> {
        let old = self.registry.get(name)?;
        let new = self.registry.get(with)?;
        let (key, other) = (old.name.clone(), new.name.clone());
        if key == other {
            return Err(ServerError::InvalidInput {
                name: key,
                message: "a database cannot take its own place".to_string(),
            });
        }
        let was_loaded = old.state.is_open();
        if let Some(loader) = &self.loader {
            if new.state.is_open() {
                loader.unload(&other)?;
            }
            if was_loaded {
                loader.unload(&key)?;
            }
        }
        let swap = || {
            self.registry
                .swap_in(&key, &other, |gone| self.drop_grants_on(gone))
        };
        let (entry, trash, grants) = match &self.loader {
            Some(loader) => loader.while_all_idle(&[&key, &other], swap)?,
            None => swap()?,
        };
        self.audit.record(AuditEvent::DatabaseSwapped {
            name: key.clone(),
            with: other.clone(),
            trash: trash.clone(),
        });
        let reloaded = match (&self.loader, was_loaded) {
            (Some(loader), true) => match loader.load(&key) {
                Ok(_) => true,
                Err(err) => {
                    tracing::warn!(
                        target: "graphersal_server",
                        "database '{key}': cannot load it again after the swap: {err}"
                    );
                    false
                }
            },
            _ => false,
        };
        Ok(Swapped {
            entry,
            with: other,
            trash,
            reloaded,
            grants_removed: grants.unwrap_or(0),
        })
    }
}

/// Whether found damage froze the open Store (the damage policy `maintenance`).
fn frozen(
    found: Option<Vec<graphersal::persist::FoundDamage>>,
    policy: Option<DamagePolicy>,
) -> bool {
    found.is_some_and(|found| !found.is_empty()) && policy == Some(DamagePolicy::Maintenance)
}

/// A repair report as JSON (`book/src/reference/admin_api.md`, "Verify, repair and swap").
pub(crate) fn repair_json(report: &RepairReport) -> Value {
    let element = |e: &graphersal::changes::ElementRef| {
        let kind = match e.kind {
            graphersal::storage::ElementKind::Vertex => "vertex",
            graphersal::storage::ElementKind::Edge => "edge",
        };
        format!("{kind} {}", e.id)
    };
    let diverged: Vec<Value> = report
        .diverged
        .iter()
        .take(DIVERGED_SHOWN)
        .map(|d| json!({ "element": element(&d.element), "commitSeq": d.commit_seq, "reason": d.reason }))
        .collect();
    json!({
        "commitSeq": report.commit_seq,
        "lossless": report.is_lossless(),
        "damage": report.damage.to_string().trim_end(),
        "damageItems": report.damage.damage.len(),
        "repaired": report.repaired,
        "lostCommits": report
            .lost_commits
            .iter()
            .map(|(first, last)| if *last == u64::MAX {
                json!({ "first": first, "last": null })
            } else {
                json!({ "first": first, "last": last })
            })
            .collect::<Vec<_>>(),
        "lostRanges": report.lost_ranges.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "lostElements": report.lost_elements.iter().map(element).collect::<Vec<_>>(),
        "diverged": diverged,
        "divergedCount": report.diverged.len(),
        "marksNotCarried": report.marks_not_carried.len(),
        "modelsNotCarried": report.models_not_carried,
        "blobsNotCarried": report.blobs_not_carried,
        "notCarried": report.not_carried(),
    })
}
