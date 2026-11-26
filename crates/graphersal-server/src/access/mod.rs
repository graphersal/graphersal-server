//! Access control: who sends a request ([`Principal`], the authenticator chain) and what it may
//! reach ([`Level`] per database, from `access.toml`'s grants).
//!
//! - `access.toml` ([`AccessFile`]): users with roles and the SHA-256 of their tokens (plus
//!   the token's last four characters for listings), and grants keyed by subject. Written
//!   atomically, mode `0600` on unix, hashes only.
//! - A server directory without `access.toml` is CLOSED: `init` and the first `serve` create
//!   the first admin ([`FIRST_ADMIN`], role `admin`) and hand its token back once
//!   ([`FirstAdmin`]) for the console to print; nothing else ever shows it.
//! - [`Access`]: the live access state of a running server: the file (re-written on every
//!   change), the authenticator chain (operator token of this run, user bearer tokens, the
//!   browser session cookie, else anonymous), browser login/logout, the level computation, and
//!   the user and grant changes, each recorded through the [`crate::audit`] sink.
//!
//! Tokens and session ids are 256 random bits from the OS ([`Secret`]); only their hashes are
//! kept, compared in constant time; none is ever logged.
//!
//! Enforcement on the routes is not part of this module: the server's router asks
//! [`Access`] for the principal and the level and refuses accordingly.

mod auth;
mod model;
mod principal;
mod token;

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use graphersal_serve::Request;

pub use auth::{
    AuthError, AuthStep, Authenticator, BrowserSession, MemorySessions, OperatorToken,
    SESSION_COOKIE, SessionCookie, SessionLimits, SessionTable, UserTokens, authenticate_with,
    bearer_token, cookie,
};
pub use model::{
    AccessFile, FIRST_ADMIN, Grant, Level, RESERVED_USER_NAMES, Subject, User, database_read_only,
    effective_level,
};
pub use principal::{ADMIN_ROLE, OPERATOR, Principal, PrincipalKind};
pub use token::{Secret, TokenHash};

use crate::audit::{Audit, AuditEvent};
use crate::error::ServerError;
use crate::layout::ServerDir;
use crate::registry::Registry;

/// The first admin of a server directory, made because it had no `access.toml`: its token,
/// to be shown ONCE.
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FirstAdmin {
    /// The user's name ([`FIRST_ADMIN`]).
    pub name: String,
    /// Its token, in clear text (never stored: print it once).
    pub token: Secret,
    /// The `access.toml` written.
    pub path: PathBuf,
}

/// Creates the first admin of `dir` when it has no `access.toml` (the event recorded through
/// `audit`): `None` when the file exists.
pub fn ensure_first_admin(
    dir: &ServerDir,
    audit: &Audit,
) -> Result<Option<FirstAdmin>, ServerError> {
    let Some((_, token)) = AccessFile::create_first_admin(dir)? else {
        return Ok(None);
    };
    audit.record(AuditEvent::FirstAdminCreated {
        name: FIRST_ADMIN.to_string(),
        token_last4: token.last4(),
    });
    Ok(Some(FirstAdmin {
        name: FIRST_ADMIN.to_string(),
        token,
        path: dir.access_path(),
    }))
}

/// The access state of a running server (module docs).
pub struct Access {
    dir: ServerDir,
    file: Arc<RwLock<AccessFile>>,
    sessions: Arc<dyn SessionTable>,
    limits: SessionLimits,
    chain: Vec<Box<dyn Authenticator>>,
    audit: Audit,
}

impl std::fmt::Debug for Access {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Access")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

impl Access {
    /// The access state of `dir` with the in-memory session table: its `access.toml` read
    /// (a missing file first gets the first admin, returned once); `operator`: the hash of
    /// this run's operator token (`None`: no operator link).
    ///
    /// `Err`: a broken `access.toml` ([`ServerError::Config`]: the server does not start, the
    /// directory stays closed), an I/O error, the random generator failing.
    pub fn open(
        dir: &ServerDir,
        operator: Option<TokenHash>,
        audit: Audit,
    ) -> Result<(Self, Option<FirstAdmin>), ServerError> {
        Self::open_with(dir, operator, Arc::new(MemorySessions::new()), audit)
    }

    /// [`Access::open`] with the session table `sessions` (sessions without limits).
    pub fn open_with(
        dir: &ServerDir,
        operator: Option<TokenHash>,
        sessions: Arc<dyn SessionTable>,
        audit: Audit,
    ) -> Result<(Self, Option<FirstAdmin>), ServerError> {
        Self::open_with_limits(dir, operator, sessions, SessionLimits::unlimited(), audit)
    }

    /// [`Access::open_with`] with the session limits `limits`. The sessions `sessions` already
    /// holds (a table restored from the server directory) are checked first: an expired one, or
    /// one whose user is gone or holds another token now, is removed (a failing table is a
    /// warning: the request path checks every session again anyway).
    pub fn open_with_limits(
        dir: &ServerDir,
        operator: Option<TokenHash>,
        sessions: Arc<dyn SessionTable>,
        limits: SessionLimits,
        audit: Audit,
    ) -> Result<(Self, Option<FirstAdmin>), ServerError> {
        let first = ensure_first_admin(dir, &audit)?;
        let file = AccessFile::read(dir)?.ok_or_else(|| ServerError::Config {
            path: dir.access_path(),
            message: "the file disappeared while the server started".to_string(),
        })?;
        let now = SystemTime::now();
        let ended = sessions.retain(&|session| {
            auth::session_principal(&file, session).is_some() && !limits.expired(session, now)
        });
        if let Err(err) = ended {
            tracing::warn!(target: "graphersal_server", "browser sessions: {err}");
        }
        let file = Arc::new(RwLock::new(file));
        let mut chain: Vec<Box<dyn Authenticator>> = Vec::new();
        if let Some(hash) = operator {
            chain.push(Box::new(OperatorToken::new(hash)));
        }
        chain.push(Box::new(UserTokens::new(file.clone())));
        chain.push(Box::new(
            SessionCookie::new(file.clone(), sessions.clone()).with_limits(limits),
        ));
        let access = Self {
            dir: dir.clone(),
            file,
            sessions,
            limits,
            chain,
            audit,
        };
        Ok((access, first))
    }

    /// The principal of `request` (the chain of the module docs): anonymous without credentials,
    /// `Err` for credentials that are wrong.
    pub fn authenticate(&self, request: &Request<'_>) -> Result<Principal, AuthError> {
        authenticate_with(&self.chain, request)
    }

    /// A browser login with the user token `token`: a new session id (the cookie's value,
    /// handed to the browser once) and the user's principal. Only user tokens log in (the
    /// operator token is the console's).
    pub fn login(&self, token: &str) -> Result<(Secret, Principal), AuthError> {
        let hash = TokenHash::of(token);
        let user = {
            let file = self.read();
            file.user_by_token(&hash)
                .map(|(name, user)| Principal::user(name, user.roles.clone()))
        };
        let Some(principal) = user else {
            self.audit.record(AuditEvent::LoginFailed {
                reason: "a token no user holds".to_string(),
            });
            return Err(AuthError::InvalidToken);
        };
        let id = Secret::generate().map_err(AuthError::Unavailable)?;
        let session = BrowserSession::new(principal.id.clone(), hash, SystemTime::now());
        self.sessions
            .insert(id.hash(), session)
            .map_err(|err| AuthError::Unavailable(err.to_string()))?;
        self.audit
            .with_actor(principal.identity())
            .record(AuditEvent::LoginSucceeded {
                principal: principal.id.clone(),
            });
        Ok((id, principal))
    }

    /// Ends the browser session `session_id` (the cookie's value): whether it was live.
    pub fn logout(&self, session_id: &str) -> Result<bool, ServerError> {
        let Some(session) = self.sessions.remove(&TokenHash::of(session_id))? else {
            return Ok(false);
        };
        self.audit
            .with_actor(Some(format!("user:{}", session.user)))
            .record(AuditEvent::LoggedOut {
                principal: session.user,
            });
        Ok(true)
    }

    /// A copy of the access file as it is now.
    pub fn file(&self) -> AccessFile {
        self.read().clone()
    }

    /// The effective level of `principal` on the database or alias `name` of `registry`:
    /// [`Access::reach`]'s granted level capped by the database's mode; [`Level::None`] for an
    /// unknown name.
    pub fn level_in(&self, registry: &Registry, principal: &Principal, name: &str) -> Level {
        match self.reach(registry, principal, name) {
            Some((entry, granted)) => effective_level(granted, database_read_only(&entry)),
            None => Level::None,
        }
    }

    /// The database `name` (a database's name or an alias, resolved NOW) names for
    /// `principal`, with the level granted on it through that name before the database's mode
    /// ([`AccessFile::granted_level_via`]: an alias's own grants count only when `name` is the
    /// alias). `None`: unknown, or not granted (the name does not exist for `principal`).
    pub fn reach(
        &self,
        registry: &Registry,
        principal: &Principal,
        name: &str,
    ) -> Option<(crate::registry::DatabaseEntry, Level)> {
        let entry = registry.resolve(name).ok()?;
        let granted = self.granted_level_via(principal, name, &entry.name);
        (granted != Level::None).then_some((entry, granted))
    }

    /// [`AccessFile::granted_level_via`] on the access file as it is now.
    pub fn granted_level_via(&self, principal: &Principal, requested: &str, real: &str) -> Level {
        self.read().granted_level_via(principal, requested, real)
    }

    /// Moves the grants on the alias `old` to its new name `new` (the alias was renamed): how
    /// many moved.
    pub fn rename_grants(
        &self,
        actor: &Principal,
        old: &str,
        new: &str,
    ) -> Result<usize, ServerError> {
        self.change(actor, None, |file| {
            let grants = file.rename_grants(old, new);
            let events = if grants == 0 {
                Vec::new()
            } else {
                vec![AuditEvent::GrantsMoved {
                    from: crate::names::fold(old),
                    to: crate::names::fold(new),
                    grants,
                }]
            };
            Ok((grants, events))
        })
    }

    /// Removes the grants on `name` itself (an alias removed): how many; each recorded as a
    /// revoked grant.
    pub fn drop_grants(&self, actor: &Principal, name: &str) -> Result<usize, ServerError> {
        self.change(actor, None, |file| {
            let key = crate::names::fold(name);
            let events: Vec<AuditEvent> = file
                .grants()
                .iter()
                .filter(|grant| grant.database == key)
                .map(|grant| AuditEvent::GrantRevoked {
                    subject: grant.subject.to_string(),
                    database: key.clone(),
                })
                .collect();
            let dropped = file.drop_grants(&key);
            Ok((dropped, events))
        })
    }

    /// The level granted to `principal` on the database `name` asked by that real name
    /// (an alias's grants do not count: [`Access::reach`] for a name that may be an alias),
    /// before the database's mode caps it: admin and operator [`Level::Write`], else the highest grant
    /// of any of its subjects. [`Level::None`]: the database does not exist for `principal`.
    pub fn granted_level(&self, principal: &Principal, name: &str) -> Level {
        self.read().granted_level(principal, name)
    }

    /// The audit handle of this server recording as `principal` (its identity; anonymous: no
    /// actor).
    pub fn audit_as(&self, principal: &Principal) -> Audit {
        self.audit.with_actor(principal.identity())
    }

    /// Adds the user `name` with `roles`: its token (show it once). `actor`: who did it.
    pub fn add_user(
        &self,
        actor: &Principal,
        name: &str,
        roles: Vec<String>,
    ) -> Result<Secret, ServerError> {
        self.edit(actor, None).add_user(name, roles)
    }

    /// Gives the user `name` a new token (show it once); its browser sessions end.
    pub fn issue_token(&self, actor: &Principal, name: &str) -> Result<Secret, ServerError> {
        self.edit(actor, None).issue_token(name)
    }

    /// Takes the user `name`'s token away; its browser sessions end. Refused for the last admin
    /// (`ServerError::LastAdmin`).
    pub fn revoke_token(&self, actor: &Principal, name: &str) -> Result<(), ServerError> {
        self.edit(actor, None).revoke_token(name)
    }

    /// Sets the user `name`'s roles. Refused when it takes the admin role from the last admin
    /// (`ServerError::LastAdmin`).
    pub fn set_roles(
        &self,
        actor: &Principal,
        name: &str,
        roles: Vec<String>,
    ) -> Result<(), ServerError> {
        self.edit(actor, None).set_roles(name, roles)
    }

    /// Removes the user `name`, its grants and its browser sessions. Refused for the last admin
    /// (`ServerError::LastAdmin`).
    pub fn remove_user(&self, actor: &Principal, name: &str) -> Result<(), ServerError> {
        self.edit(actor, None).remove_user(name).map(drop)
    }

    /// Sets `grant`.
    pub fn grant(&self, actor: &Principal, grant: Grant) -> Result<(), ServerError> {
        self.edit(actor, None).grant(grant)
    }

    /// Removes the grant of `subject` on `database`: whether there was one.
    pub fn revoke(
        &self,
        actor: &Principal,
        subject: &Subject,
        database: &str,
    ) -> Result<bool, ServerError> {
        self.edit(actor, None).revoke(subject, database)
    }

    /// The revision of the users and grants as they are now ([`crate::revision`]).
    pub fn revision(&self) -> String {
        self.read().revision()
    }

    /// The user and grant changes made by `actor`, each refused with
    /// [`ServerError::Conflict`] when the file is no longer at the revision `expected` (`None`:
    /// no check). The check and the change run under the file's lock in one step.
    pub fn edit<'a>(&'a self, actor: &'a Principal, expected: Option<&'a str>) -> AccessEdit<'a> {
        AccessEdit {
            access: self,
            actor,
            expected,
        }
    }

    /// Applies `change` to a copy of the file, writes it, then makes it live; browser sessions
    /// that are no longer live (or expired) end, in memory and in a persisted table at once; the events are recorded as `actor`.
    fn change<T>(
        &self,
        actor: &Principal,
        expected: Option<&str>,
        change: impl FnOnce(&mut AccessFile) -> Result<(T, Vec<AuditEvent>), ServerError>,
    ) -> Result<T, ServerError> {
        let mut live = self.file.write().unwrap_or_else(|p| p.into_inner());
        if expected.is_some() {
            crate::revision::check(expected, &live.revision(), "users and grants")?;
        }
        let mut next = live.clone();
        let (value, events) = change(&mut next)?;
        if events.is_empty() {
            return Ok(value);
        }
        next.write(&self.dir)?;
        *live = next;
        let now = SystemTime::now();
        let ended = self.sessions.retain(&|session| {
            auth::session_principal(&live, session).is_some() && !self.limits.expired(session, now)
        });
        drop(live);
        if let Err(err) = ended {
            tracing::warn!(target: "graphersal_server", "browser sessions: {err}");
        }
        let audit = self.audit.with_actor(actor.identity());
        for event in events {
            audit.record(event);
        }
        Ok(value)
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, AccessFile> {
        self.file.read().unwrap_or_else(|p| p.into_inner())
    }

    /// The access file's read lock: no user or grant change writes `access.toml` while it is
    /// held (the server backup copies the file under it, [`crate::server_backup`]). Lock
    /// order: take the registry's lock first, if at all.
    pub(crate) fn hold(&self) -> std::sync::RwLockReadGuard<'_, AccessFile> {
        self.read()
    }
}

/// The user and grant changes of one actor based on one revision ([`Access::edit`]).
pub struct AccessEdit<'a> {
    access: &'a Access,
    actor: &'a Principal,
    expected: Option<&'a str>,
}

impl std::fmt::Debug for AccessEdit<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccessEdit")
            .field("actor", &self.actor.identity())
            .field("expected", &self.expected)
            .finish_non_exhaustive()
    }
}

impl AccessEdit<'_> {
    fn change<T>(
        &self,
        change: impl FnOnce(&mut AccessFile) -> Result<(T, Vec<AuditEvent>), ServerError>,
    ) -> Result<T, ServerError> {
        self.access.change(self.actor, self.expected, change)
    }

    /// [`Access::add_user`].
    pub fn add_user(&self, name: &str, roles: Vec<String>) -> Result<Secret, ServerError> {
        self.change(|file| {
            let token = file.add_user(name, roles.clone())?;
            let event = AuditEvent::UserAdded {
                name: crate::names::fold(name),
                roles,
                token_last4: token.last4(),
            };
            Ok((token, vec![event]))
        })
    }

    /// [`Access::issue_token`].
    pub fn issue_token(&self, name: &str) -> Result<Secret, ServerError> {
        self.change(|file| {
            let token = file.issue_token(name)?;
            let event = AuditEvent::TokenIssued {
                name: crate::names::fold(name),
                token_last4: token.last4(),
            };
            Ok((token, vec![event]))
        })
    }

    /// [`Access::revoke_token`].
    pub fn revoke_token(&self, name: &str) -> Result<(), ServerError> {
        self.change(|file| {
            refuse_last_admin(file, name, |file| file.revoke_token(name))?;
            let name = crate::names::fold(name);
            Ok(((), vec![AuditEvent::TokenRevoked { name }]))
        })
    }

    /// [`Access::set_roles`].
    pub fn set_roles(&self, name: &str, roles: Vec<String>) -> Result<(), ServerError> {
        self.change(|file| {
            refuse_last_admin(file, name, |file| file.set_roles(name, roles.clone()))?;
            let name = crate::names::fold(name);
            Ok(((), vec![AuditEvent::RolesSet { name, roles }]))
        })
    }

    /// Gives the user `name` the `admin` role (`on`, first) or takes it away, keeping its other
    /// roles: read and changed in ONE step under the file's lock (a concurrent role change is
    /// never undone). Refused as [`Access::set_roles`].
    pub fn set_admin(&self, name: &str, on: bool) -> Result<(), ServerError> {
        self.change(|file| {
            let user = file.user(name).ok_or_else(|| ServerError::UserNotFound {
                name: crate::names::fold(name),
            })?;
            let mut roles: Vec<String> = user
                .roles
                .iter()
                .filter(|role| *role != ADMIN_ROLE)
                .cloned()
                .collect();
            if on {
                roles.insert(0, ADMIN_ROLE.to_string());
            }
            refuse_last_admin(file, name, |file| file.set_roles(name, roles.clone()))?;
            let name = crate::names::fold(name);
            Ok(((), vec![AuditEvent::RolesSet { name, roles }]))
        })
    }

    /// [`Access::remove_user`]: the number of grants of the user removed with it (counted in
    /// the same step).
    pub fn remove_user(&self, name: &str) -> Result<usize, ServerError> {
        self.change(|file| {
            let subject = Subject::User(crate::names::fold(name));
            let grants = file
                .grants()
                .iter()
                .filter(|grant| grant.subject == subject)
                .count();
            refuse_last_admin(file, name, |file| file.remove_user(name))?;
            let name = crate::names::fold(name);
            Ok((grants, vec![AuditEvent::UserRemoved { name }]))
        })
    }

    /// [`Access::grant`].
    pub fn grant(&self, grant: Grant) -> Result<(), ServerError> {
        self.change(|file| {
            let event = AuditEvent::GrantSet {
                subject: grant.subject.to_string(),
                database: grant.database.clone(),
                level: grant.level.as_str().to_string(),
            };
            file.grant(grant);
            Ok(((), vec![event]))
        })
    }

    /// [`Access::revoke`].
    pub fn revoke(&self, subject: &Subject, database: &str) -> Result<bool, ServerError> {
        self.change(|file| {
            if !file.revoke(subject, database) {
                return Ok((false, Vec::new()));
            }
            let event = AuditEvent::GrantRevoked {
                subject: subject.to_string(),
                database: database.to_string(),
            };
            Ok((true, vec![event]))
        })
    }
}

/// Applies `change` (of the user `name`) to `file`, refused when it leaves no usable admin
/// ([`AccessFile::usable_admins`]: the `admin` role and a token) where there was one: the last
/// admin cannot be removed, demoted or locked out (its token reissued instead, or another admin
/// made first). `file` is a copy: a refusal leaves it half changed, never written.
pub(crate) fn refuse_last_admin(
    file: &mut AccessFile,
    name: &str,
    change: impl FnOnce(&mut AccessFile) -> Result<(), ServerError>,
) -> Result<(), ServerError> {
    let before = file.usable_admins();
    change(file)?;
    if before > 0 && file.usable_admins() == 0 {
        return Err(ServerError::LastAdmin {
            name: crate::names::fold(name),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
