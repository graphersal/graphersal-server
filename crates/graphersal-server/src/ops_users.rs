//! The users and grants of `access.toml` ([`Admin::users`], [`Admin::user_add`], ...): part of
//! [`Admin`] (module docs of [`crate::ops`]), behind the admin API's `users` and `grants`
//! routes and the console's `user` commands.
//!
//! - Online (a running server) the changes go through the server's live [`Access`]: the file
//!   is rewritten, the browser sessions of a changed token end at once, and the MCP sessions
//!   of that user end too (the endpoint's `forget_sessions_of`).
//! - Offline (the console holding the server lock) the same [`Access`] code runs over the
//!   directory's `access.toml` and its stored sessions (`sessions/`, [`crate::sessions`]): a
//!   token change or removal drops the user's stored browser and MCP sessions from the files,
//!   so they do not come back at the next start. A directory without `access.toml` is
//!   refused: `init` or `serve` makes the first admin, whose token they print.
//!
//! The actor recorded with each change is the admin's principal ([`Admin::with_principal`];
//! default: the operator, which is also what the console on the directory acts as: it holds the
//! server lock, the operator's power). The last admin (the `admin` role and a token) cannot be
//! removed, demoted or have its token revoked ([`ServerError::LastAdmin`]).

use std::sync::Arc;

use super::Admin;
use crate::access::{ADMIN_ROLE, Access, AccessFile, Grant, Level, Principal, Secret, Subject};
use crate::error::ServerError;
use crate::names::{fold, validate_name};
use crate::sessions::{FileSessions, McpSessionFile};

/// What a token change ended besides the token itself.
///
/// `#[non_exhaustive]`: further counts may be added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct SessionsEnded {
    /// The user's MCP sessions ended (offline, or with MCP off: the stored ones removed from
    /// `sessions/mcp.json`).
    pub mcp: usize,
}

impl Admin {
    /// The admin acting with the live access state `access` of its server, as `principal` (the
    /// actor of the user and grant changes).
    pub fn with_access(mut self, access: Arc<Access>, principal: Principal) -> Self {
        self.access = Some(access);
        self.principal = principal;
        self
    }

    /// The admin acting as `principal` (the actor of the user and grant changes; default: the
    /// operator).
    pub fn with_principal(mut self, principal: Principal) -> Self {
        self.principal = principal;
        self
    }

    /// The access state the user operations work on: the server's, or offline one over the
    /// directory's `access.toml`.
    fn access_state(&self) -> Result<Arc<Access>, ServerError> {
        if let Some(access) = &self.access {
            return Ok(access.clone());
        }
        let dir = self.registry().dir();
        if self.is_online() {
            return Err(ServerError::Refused {
                name: "access.toml".to_string(),
                message: "this admin has no access state (a server bug)".to_string(),
            });
        }
        if AccessFile::read(dir)?.is_none() {
            return Err(ServerError::Refused {
                name: dir.access_path().display().to_string(),
                message: "the directory has no access.toml yet; `init` or `serve` makes the \
                          first admin and prints its token"
                    .to_string(),
            });
        }
        let limits = dir
            .load_config()
            .map(|config| config.sessions.limits())
            .unwrap_or_default();
        let sessions = Arc::new(FileSessions::open(dir));
        let (access, _) =
            Access::open_with_limits(dir, None, sessions, limits, self.audit().clone())?;
        Ok(Arc::new(access))
    }

    /// The access state whose grants follow an alias's rename or removal: the server's;
    /// offline the directory's `access.toml`; `None` when there is none to change (offline
    /// without `access.toml`, or an online admin without access state: the unit tests).
    pub(crate) fn grants_state(&self) -> Result<Option<Arc<Access>>, ServerError> {
        if let Some(access) = &self.access {
            return Ok(Some(access.clone()));
        }
        if self.is_online() || AccessFile::read(self.registry().dir())?.is_none() {
            return Ok(None);
        }
        self.access_state().map(Some)
    }

    /// Moves the grants on the alias `old` to `new` (renamed): how many moved.
    pub(crate) fn move_alias_grants(&self, old: &str, new: &str) -> Result<usize, ServerError> {
        match self.grants_state()? {
            Some(access) => access.rename_grants(&self.principal, old, new),
            None => Ok(0),
        }
    }

    /// Removes the grants on the name `name` itself (an alias removed, a database deleted):
    /// how many, each audited as a revoked grant.
    pub(crate) fn drop_grants_on(&self, name: &str) -> Result<usize, ServerError> {
        match self.grants_state()? {
            Some(access) => access.drop_grants(&self.principal, name),
            None => Ok(0),
        }
    }

    /// Removes the grants left on the name `name` (a database or alias deleted while an older
    /// version ran, or whose grants could not be removed then) before a new database or alias
    /// takes that name, so the new one never inherits them: how many (audited each, and a
    /// warning in the log when any).
    pub(crate) fn drop_leftover_grants(&self, name: &str) -> Result<usize, ServerError> {
        let dropped = self.drop_grants_on(name)?;
        if dropped > 0 {
            tracing::warn!(
                target: "graphersal_server",
                "'{}': removed {dropped} grant(s) left on the name from a deleted database or \
                 alias before it was taken again",
                crate::names::fold(name)
            );
        }
        Ok(dropped)
    }

    /// The users and grants as they are now.
    pub fn users(&self) -> Result<AccessFile, ServerError> {
        Ok(self.access_state()?.file())
    }

    /// Adds the user `name` (with the `admin` role when `admin`): its token, to show once.
    pub fn user_add(&self, name: &str, admin: bool) -> Result<Secret, ServerError> {
        let roles = if admin {
            vec![ADMIN_ROLE.to_string()]
        } else {
            Vec::new()
        };
        self.access_state()?
            .edit(&self.principal, self.expected())
            .add_user(name, roles)
    }

    /// Gives the user `name` a new token (to show once); its browser and MCP sessions end.
    pub fn user_token(&self, name: &str) -> Result<(Secret, SessionsEnded), ServerError> {
        let token = self
            .access_state()?
            .edit(&self.principal, self.expected())
            .issue_token(name)?;
        Ok((token, self.end_sessions(name)))
    }

    /// Takes the user `name`'s token away; its browser and MCP sessions end.
    pub fn user_revoke_token(&self, name: &str) -> Result<SessionsEnded, ServerError> {
        self.access_state()?
            .edit(&self.principal, self.expected())
            .revoke_token(name)?;
        Ok(self.end_sessions(name))
    }

    /// Removes the user `name` with its grants; its browser and MCP sessions end. Answers the
    /// number of grants removed with it.
    pub fn user_remove(&self, name: &str) -> Result<(usize, SessionsEnded), ServerError> {
        let grants = self
            .access_state()?
            .edit(&self.principal, self.expected())
            .remove_user(name)?;
        Ok((grants, self.end_sessions(name)))
    }

    /// Sets the user `name`'s roles.
    pub fn user_set_roles(&self, name: &str, roles: Vec<String>) -> Result<(), ServerError> {
        let mut clean: Vec<String> = Vec::new();
        for role in roles {
            let role = role.trim().to_string();
            if role.is_empty() || role.contains(char::is_whitespace) {
                return Err(ServerError::InvalidInput {
                    name: name.to_string(),
                    message: format!("role {role:?}: a role is one word"),
                });
            }
            if !clean.contains(&role) {
                clean.push(role);
            }
        }
        self.access_state()?
            .edit(&self.principal, self.expected())
            .set_roles(name, clean)
    }

    /// Gives the user `name` the `admin` role (`on`) or takes it away, keeping its other roles.
    pub fn user_set_admin(&self, name: &str, on: bool) -> Result<(), ServerError> {
        self.access_state()?
            .edit(&self.principal, self.expected())
            .set_admin(name, on)
    }

    /// Sets the grant of `subject` (`user:<name>`, a bare user name, `anonymous`, `role:<r>`,
    /// `group:<g>`) on `database` (a database's real name, an alias or `*`) to `level`
    /// (`read`, `write`). A user subject must name a user; the database or alias must exist. A
    /// grant on an alias reaches the alias's CURRENT target, only through the alias's name
    /// (`/db/<alias>/`, MCP `use_graph <alias>`): the real name stays hidden without a grant
    /// of its own; a flip moves the access along, a rename carries the grant to the new name,
    /// a removal drops it.
    pub fn grant_set(
        &self,
        subject: &str,
        database: &str,
        level: &str,
    ) -> Result<Grant, ServerError> {
        let subject = parse_subject(subject)?;
        let level = Level::parse_grant(level).ok_or_else(|| ServerError::InvalidInput {
            name: level.to_string(),
            message: "a grant's level is \"read\" or \"write\" (revoke it to give nothing)"
                .to_string(),
        })?;
        let grant = Grant::new(subject.clone(), database, level).map_err(|message| {
            ServerError::InvalidInput {
                name: database.to_string(),
                message,
            }
        })?;
        let access = self.access_state()?;
        let set = || {
            if let Subject::User(name) = &subject
                && access.file().user(name).is_none()
            {
                return Err(ServerError::UserNotFound { name: name.clone() });
            }
            access
                .edit(&self.principal, self.expected())
                .grant(grant.clone())
        };
        if database == "*" {
            set()?;
        } else {
            // The name must exist WHILE the grant is written: under the registry's read lock,
            // no delete or alias removal runs in between (they drop the name's grants under its
            // write lock), so a grant never lands on a name that just went away.
            self.registry().while_named(database, set)?;
        }
        Ok(grant)
    }

    /// Removes the grant of `subject` (as [`Admin::grant_set`] takes it) on `database` (a name
    /// or `*`): whether there was one.
    pub fn grant_revoke(&self, subject: &str, database: &str) -> Result<bool, ServerError> {
        let subject = parse_subject(subject)?;
        if database != "*" {
            validate_name(&fold(database)).map_err(|_| ServerError::InvalidInput {
                name: database.to_string(),
                message: "a grant names a database or \"*\"".to_string(),
            })?;
        }
        self.access_state()?
            .edit(&self.principal, self.expected())
            .revoke(&subject, database)
    }

    /// Ends the MCP sessions of the user `name`: through the running endpoint (written through
    /// to its store), else (offline, or MCP off) in `sessions/mcp.json` directly.
    fn end_sessions(&self, name: &str) -> SessionsEnded {
        let identity = format!("user:{}", fold(name));
        if let Some(mcp) = self.loader.as_ref().and_then(|loader| loader.mcp()) {
            return SessionsEnded {
                mcp: mcp.forget_sessions_of(Some(&identity)),
            };
        }
        let file = McpSessionFile::new(self.registry().dir());
        let mcp = file.forget_identity(Some(&identity)).unwrap_or_else(|err| {
            tracing::warn!(target: "graphersal_server", "MCP sessions: {err}");
            0
        });
        SessionsEnded { mcp }
    }
}

/// A grant subject as the API and the console take it: `anonymous`, `user:<name>`,
/// `role:<r>`, `group:<g>`, or a bare user name.
pub(crate) fn parse_subject(text: &str) -> Result<Subject, ServerError> {
    let text = text.trim();
    let invalid = |message: String| ServerError::InvalidInput {
        name: text.to_string(),
        message,
    };
    if text.contains(':') || text == "anonymous" {
        return Subject::parse(text).map_err(invalid);
    }
    let name = fold(text);
    validate_name(&name).map_err(|err| invalid(format!("not a user name or subject: {err}")))?;
    Ok(Subject::User(name))
}
