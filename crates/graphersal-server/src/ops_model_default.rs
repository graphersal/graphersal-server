//! The default embedding model of a database (`models default NAME MODEL|--none`, `PUT
//! /api/admin/databases/<name>/default-model`, the Databases tab's "Default model…"): which of
//! the models its Store holds `embed()` and text queries use without a model name. Part of
//! [`Admin`] (module docs of [`crate::ops`]).
//!
//! The change is the engine's, ONE commit of the database's own Store (the default follows the
//! Store's history like the models): a LOADED database through its open Store
//! (`Store::set_default_model`: the graph's model registry is rebuilt by that commit, so its
//! next queries use the new default at once; a model already loaded stays loaded, nothing is
//! unloaded or loaded by the change), an unloaded one (also offline, the console holding the
//! server lock; also a `needs-model` or failed one) on its closed Store
//! (`Store::set_default_model_dir`, a commit appended to its WAL without loading the graph),
//! under the loader's idle check so no load runs meanwhile. Nothing changes (no commit) when
//! the model is the default already.
//!
//! Refused (`409`): a layer (its graph serves its base's models; change them in the base), a
//! base (frozen for good), a read-only database (turn read-only off first), one being loaded or
//! unloaded. A model the Store does not hold is `400`, naming the ones it holds. Audited as
//! [`AuditEvent::DefaultModelSet`].

use graphersal::persist::{ClosedCommitOptions, Store};

use super::{Admin, store_error};
use crate::audit::AuditEvent;
use crate::database::{DatabaseKind, DatabaseRole};
use crate::error::ServerError;
use crate::loader::OpenStoreRef;
use crate::registry::DatabaseEntry;

/// The principal the closed-store commit is recorded with (the Store's journal).
const PRINCIPAL: &str = "graphersal-server";

/// What [`Admin::set_default_model`] did.
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct DefaultModelSet {
    /// The database (after the change).
    pub entry: DatabaseEntry,
    /// The default model's repository now (`None`: no default).
    pub model: Option<String>,
    /// The default before the change.
    pub previous: Option<String>,
    /// Whether a commit was made (`false`: it was the default already).
    pub changed: bool,
    /// Whether the database is loaded: its open Store was changed and its next queries use
    /// the new default.
    pub loaded: bool,
    /// The repositories of the models its Store holds.
    pub models: Vec<String>,
}

impl Admin {
    /// Makes the model of the repository `model` the default model of the plain database
    /// `name`, or clears the default (`None`) (module docs).
    ///
    /// `Err`: [`ServerError::NotFound`]; [`ServerError::Refused`] for a layer, a base, a
    /// read-only database or one being loaded or unloaded; [`ServerError::InvalidInput`] for a
    /// model its Store does not hold; [`ServerError::Store`] when the Store cannot be read or
    /// written.
    pub fn set_default_model(
        &self,
        name: &str,
        model: Option<&str>,
    ) -> Result<DefaultModelSet, ServerError> {
        let entry = self.registry.get(name)?;
        let key = entry.name.clone();
        default_refusal(&entry)?;
        let open = self.loader.as_ref().and_then(|l| l.open_store_of(&key));
        let (previous, models, changed, loaded) = match open {
            Some((_context, OpenStoreRef::Plain(store))) => {
                // The context is held: an unload waits for this request.
                let held = store
                    .models()
                    .map_err(|err| store_error(&key, &format!("cannot read its models: {err}")))?;
                let models: Vec<String> = held.iter().map(|m| m.repo.clone()).collect();
                let previous = held.iter().find(|m| m.default).map(|m| m.repo.clone());
                check_held(&key, model, &models)?;
                let report = store.set_default_model(model).map_err(|err| {
                    store_error(&key, &format!("cannot set the default model: {err}"))
                })?;
                (previous, models, report.commit_seq.is_some(), true)
            }
            Some((_context, OpenStoreRef::Layer(_))) => {
                return Err(layer_refusal(&key));
            }
            None => {
                let change = || self.set_default_closed(&key, model);
                let (previous, models, changed) = match &self.loader {
                    Some(loader) => loader.while_idle(&key, change)?,
                    None => change()?,
                };
                (previous, models, changed, false)
            }
        };
        let now = model.map(str::to_string);
        if changed {
            self.audit.record(AuditEvent::DefaultModelSet {
                name: key.clone(),
                model: now.clone(),
                previous: previous.clone(),
            });
        }
        Ok(DefaultModelSet {
            entry: self.registry.get(&key)?,
            model: now,
            previous,
            changed,
            loaded,
            models,
        })
    }

    /// The change on the closed Store of the database `key` nothing serves: `(previous,
    /// models, changed)`.
    fn set_default_closed(
        &self,
        key: &str,
        model: Option<&str>,
    ) -> Result<(Option<String>, Vec<String>, bool), ServerError> {
        let entry = self.registry.get(key)?;
        // Checked again: the database may have changed before the loader's lock was taken.
        default_refusal(&entry)?;
        let dir = entry.store_dir();
        let info = Store::inspect(dir.as_path())
            .map_err(|err| store_error(key, &format!("cannot read its Store: {err}")))?;
        let models: Vec<String> = info.models.iter().map(|m| m.repo.clone()).collect();
        check_held(key, model, &models)?;
        let report = Store::set_default_model_dir(
            dir.as_path(),
            model,
            ClosedCommitOptions::new().with_principal(PRINCIPAL),
        )
        .map_err(|err| store_error(key, &format!("cannot set the default model: {err}")))?;
        Ok((info.default_model, models, report.commit_seq.is_some()))
    }
}

/// Why the default model of `entry` cannot be changed (the refusal).
fn default_refusal(entry: &DatabaseEntry) -> Result<(), ServerError> {
    let refuse = |message: &str| ServerError::Refused {
        name: entry.name.clone(),
        message: message.to_string(),
    };
    let Some(config) = entry.config.as_ref() else {
        return Err(refuse(
            "its database.toml cannot be read; fix or delete the database first",
        ));
    };
    if config.kind == DatabaseKind::Layer {
        return Err(layer_refusal(&entry.name));
    }
    if config.role == DatabaseRole::Base {
        return Err(refuse(
            "it is a base: frozen for good, its models and their default cannot change",
        ));
    }
    if config.read_only {
        return Err(refuse(
            "it is read-only: turn read-only off first, set the default, then turn it on again",
        ));
    }
    Ok(())
}

/// The refusal of a layer database: its models are its base's.
fn layer_refusal(name: &str) -> ServerError {
    ServerError::Refused {
        name: name.to_string(),
        message: "it is a layer: it keeps no embedding model of its own (its base's models \
                  serve it, with the base's default)"
            .to_string(),
    }
}

/// `InvalidInput` when `model` is not among the `held` repositories.
fn check_held(key: &str, model: Option<&str>, held: &[String]) -> Result<(), ServerError> {
    let Some(model) = model else {
        return Ok(());
    };
    if held.iter().any(|repo| repo == model) {
        return Ok(());
    }
    let message = if held.is_empty() {
        format!("its Store holds no embedding model at all (asked for '{model}')")
    } else {
        format!(
            "its Store holds no embedding model '{model}'; it holds: {}",
            held.join(", ")
        )
    };
    Err(ServerError::InvalidInput {
        name: key.to_string(),
        message,
    })
}
