//! The MCP switches as admin operations (round 3 B6; [`crate::mcp_control`]): the global
//! on/off and the per-name exposure and read-only of `mcp.toml`, online through the server's
//! one [`McpControl`] (the endpoints follow at once), offline on the file (the console on a
//! stopped server). Every change may be based on the settings' revision
//! ([`Admin::expecting`]) and is audited.

use std::sync::Arc;

use crate::audit::AuditEvent;
use crate::error::ServerError;
use crate::mcp_control::{McpControl, McpSettings, NameSettings};
use crate::names::fold;

use super::Admin;

impl Admin {
    /// The MCP switches: the running server's (the endpoints follow every change at once), or,
    /// offline, the directory's `mcp.toml` read now.
    ///
    /// `Err`: [`ServerError::Config`] for an `mcp.toml` that does not parse.
    pub(crate) fn mcp_control(&self) -> Result<Arc<McpControl>, ServerError> {
        if let Some(control) = self.loader.as_ref().and_then(|l| l.mcp_control()) {
            return Ok(control.clone());
        }
        McpControl::open(self.registry.dir()).map(Arc::new)
    }

    /// Whether the MCP endpoints exist at all (`server.toml [mcp] enabled`): online whether the
    /// server built them at start, offline what `server.toml` says now.
    pub(crate) fn mcp_endpoint_built(&self) -> bool {
        match &self.loader {
            Some(loader) => loader.mcp().is_some(),
            None => self
                .registry
                .dir()
                .load_config()
                .map_or(true, |config| config.mcp.enabled),
        }
    }

    /// Switches MCP on or off for the whole server (module docs): the new settings.
    ///
    /// `Err`: [`ServerError::Conflict`] for a stale revision; [`ServerError::Io`] when
    /// `mcp.toml` cannot be written (nothing changed).
    pub(crate) fn mcp_set_enabled(&self, on: bool) -> Result<McpSettings, ServerError> {
        let settings = self.mcp_control()?.change(self.expected(), |settings| {
            settings.enabled = on;
            Ok(())
        })?;
        self.audit.record(AuditEvent::McpSwitched { on });
        Ok(settings)
    }

    /// Sets whether a new database or alias is offered to MCP when its creator does not say
    /// (`mcp.toml`'s `expose_new`; names already there keep their settings): the new
    /// settings.
    ///
    /// `Err`: [`ServerError::Conflict`] for a stale revision; [`ServerError::Io`] when
    /// `mcp.toml` cannot be written (nothing changed).
    pub(crate) fn mcp_set_expose_new(&self, on: bool) -> Result<McpSettings, ServerError> {
        let settings = self.mcp_control()?.change(self.expected(), |settings| {
            settings.expose_new = on;
            Ok(())
        })?;
        self.audit.record(AuditEvent::McpExposeNewSet { on });
        Ok(settings)
    }

    /// Gives the name `name` this admin is creating (a database or an alias; under the build's
    /// reservation or the registry's write lock) its MCP settings: hidden from MCP unless
    /// [`Admin::exposing`] or `mcp.toml`'s `expose_new` says otherwise
    /// ([`McpControl::claim_new`]).
    ///
    /// `Err`: `mcp.toml` cannot be read or written (the creation is refused).
    pub(crate) fn claim_mcp_settings(&self, name: &str) -> Result<NameSettings, ServerError> {
        self.mcp_control()?.claim_new(name, self.expose)
    }

    /// Whether MCP offers the name `name` now by its own settings (`mcp.toml`; `None` when the
    /// file cannot be read).
    pub(crate) fn mcp_exposed(&self, name: &str) -> Option<bool> {
        self.mcp_control()
            .ok()
            .map(|control| control.settings().of(name).exposed)
    }

    /// Changes the MCP settings of the database or alias `name` (`None` keeps a value): its
    /// folded name, its settings and the new settings of the file. The name must be a database
    /// or an alias, or have settings already (one left on a name that is gone, to reset it);
    /// it is held while the file is written.
    ///
    /// `Err`: [`ServerError::NotFound`]; [`ServerError::Conflict`] for a stale revision;
    /// [`ServerError::Io`].
    pub(crate) fn mcp_set_name(
        &self,
        name: &str,
        exposed: Option<bool>,
        read_only: Option<bool>,
    ) -> Result<(String, NameSettings, McpSettings), ServerError> {
        let control = self.mcp_control()?;
        let key = fold(name);
        let change = || {
            control.change(self.expected(), |settings| {
                let mut value = settings.of(&key);
                if let Some(exposed) = exposed {
                    value.exposed = exposed;
                }
                if let Some(read_only) = read_only {
                    value.read_only = read_only;
                }
                settings.databases.insert(key.clone(), value);
                Ok(())
            })
        };
        let settings = match self.registry.while_named(&key, change) {
            Err(ServerError::NotFound { .. }) if control.has_own(&key) => change()?,
            other => other?,
        };
        let value = settings.of(&key);
        self.audit.record(AuditEvent::McpDatabaseSet {
            name: key.clone(),
            exposed: value.exposed,
            read_only: value.read_only,
        });
        Ok((key, value, settings))
    }

    /// Sets (`Some`) or clears (`None`) the MCP profile of the database or alias `name`, or with
    /// `name` `None` the server-wide default profile ([`crate::mcp_profiles`]): the folded name
    /// and the new settings. The profile is checked first (graphersal-mcp's message, the key
    /// named); a name must be a database or an alias, or have settings already (to clear what a
    /// gone name left), and is held while the file is written. The endpoints serve the new
    /// profile from their next request on.
    ///
    /// `Err`: [`ServerError::InvalidInput`] (the profile); [`ServerError::NotFound`];
    /// [`ServerError::Conflict`] for a stale revision; [`ServerError::Io`].
    pub(crate) fn mcp_profile_set(
        &self,
        name: Option<&str>,
        profile: Option<serde_json::Value>,
    ) -> Result<(Option<String>, McpSettings), ServerError> {
        let profile = profile
            .map(|profile| {
                crate::mcp_profiles::check(profile)
                    .map(|(stored, _)| stored)
                    .map_err(|message| ServerError::InvalidInput {
                        name: "MCP profile".to_string(),
                        message,
                    })
            })
            .transpose()?;
        let control = self.mcp_control()?;
        let set = profile.is_some();
        let Some(name) = name else {
            let settings = control.change(self.expected(), |settings| {
                settings.default_profile = profile;
                Ok(())
            })?;
            self.audit
                .record(AuditEvent::McpProfileSet { name: None, set });
            return Ok((None, settings));
        };
        let key = fold(name);
        let change = || {
            control.change(self.expected(), |settings| {
                let mut value = settings.of(&key);
                value.profile = profile.clone();
                settings.databases.insert(key.clone(), value);
                Ok(())
            })
        };
        let settings = match self.registry.while_named(&key, change) {
            Err(ServerError::NotFound { .. }) if control.has_own(&key) => change()?,
            other => other?,
        };
        self.audit.record(AuditEvent::McpProfileSet {
            name: Some(key.clone()),
            set,
        });
        Ok((Some(key), settings))
    }

    /// The loaded context of the database `name` (`None`: not loaded, or offline), for the MCP
    /// profile previews and the saved-query picker.
    pub(crate) fn loaded_context(&self, name: &str) -> Option<Arc<graphersal_serve::Context>> {
        self.loader.as_ref().and_then(|loader| loader.context(name))
    }

    /// Removes the MCP settings of `name` (a database deleted, an alias removed; under the
    /// registry's write lock, with its grants). A failure only warns.
    pub(crate) fn forget_mcp_settings(&self, name: &str) {
        match self.mcp_control() {
            Ok(control) => {
                control.forget_name(name);
            }
            Err(err) => tracing::warn!(target: "graphersal_server", "MCP settings: {err}"),
        }
    }

    /// Moves the MCP settings of the alias `old` to `new` (a rename). A failure only warns.
    pub(crate) fn move_mcp_settings(&self, old: &str, new: &str) {
        match self.mcp_control() {
            Ok(control) => control.rename(old, new),
            Err(err) => tracing::warn!(target: "graphersal_server", "MCP settings: {err}"),
        }
    }
}
