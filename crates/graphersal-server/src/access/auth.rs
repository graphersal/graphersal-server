//! The authenticator chain and the browser session table.
//!
//! A request runs through the chain in order: the operator token of this run, user bearer
//! tokens (`Authorization: Bearer <token>`), the browser session cookie ([`SESSION_COOKIE`]);
//! the first link that recognises its credentials gives the [`Principal`], a link that finds
//! ITS credentials wrong rejects the request ([`AuthError`]: never a silent fall back to
//! anonymous), and a request no link claims is anonymous. A later JWT authenticator is one
//! more [`Authenticator`] in the chain; nothing else changes.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};

use graphersal_serve::Request;

use super::model::AccessFile;
use super::principal::Principal;
use super::token::TokenHash;
use crate::error::ServerError;

/// The browser session cookie's name.
pub const SESSION_COOKIE: &str = "gs_session";

/// How stale a session's `last_seen` may get before a request updates it (coarse, so a
/// persisted table is not rewritten on every request).
const LAST_SEEN_STEP: Duration = Duration::from_secs(60);

/// Why a request's credentials were refused.
///
/// `#[non_exhaustive]`: further reasons (an expired JWT) may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuthError {
    /// A bearer token (or a login's token) that no user (nor the operator) holds.
    InvalidToken,
    /// A session cookie naming no live session (unknown, logged out, or its user's token was
    /// revoked or reissued, or the user removed).
    InvalidSession,
    /// A session cannot be made (the OS's random generator, the session table).
    Unavailable(String),
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthError::InvalidToken => f.write_str("the token is not valid"),
            AuthError::InvalidSession => {
                f.write_str("the session has ended (logged out, or the token changed): log in")
            }
            AuthError::Unavailable(message) => write!(f, "cannot start a session: {message}"),
        }
    }
}

impl std::error::Error for AuthError {}

/// What one link of the chain says about a request.
///
/// `#[non_exhaustive]`: further outcomes may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuthStep {
    /// Its credentials: this principal.
    Authenticated(Principal),
    /// Not its credentials: the next link decides.
    Pass,
    /// Its credentials, but wrong: the request is refused.
    Rejected(AuthError),
}

/// One link of the authenticator chain.
pub trait Authenticator: Send + Sync {
    /// What this link says about `request` (it reads the headers only).
    fn authenticate(&self, request: &Request<'_>) -> AuthStep;
}

/// Runs `request` through `chain`: the first principal or refusal; anonymous when no link
/// claims it.
pub fn authenticate_with(
    chain: &[Box<dyn Authenticator>],
    request: &Request<'_>,
) -> Result<Principal, AuthError> {
    for link in chain {
        match link.authenticate(request) {
            AuthStep::Authenticated(principal) => return Ok(principal),
            AuthStep::Rejected(err) => return Err(err),
            AuthStep::Pass => {}
        }
    }
    Ok(Principal::anonymous())
}

/// The token of `Authorization: Bearer <token>` (scheme case-insensitive), if any.
pub fn bearer_token<'r>(request: &'r Request<'_>) -> Option<&'r str> {
    let value = request.header("Authorization")?.trim();
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
}

/// The value of the cookie `name` (every `Cookie` header looked at: HTTP/2 may split them).
pub fn cookie<'r>(request: &'r Request<'_>, name: &str) -> Option<&'r str> {
    request
        .headers()
        .iter()
        .filter(|(field, _)| field.eq_ignore_ascii_case("cookie"))
        .flat_map(|(_, value)| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.trim())
}

/// The operator token of this run (`server.run`): the operator principal.
pub struct OperatorToken {
    hash: TokenHash,
}

impl OperatorToken {
    /// The link for the operator token hashing to `hash`.
    pub fn new(hash: TokenHash) -> Self {
        Self { hash }
    }
}

impl Authenticator for OperatorToken {
    fn authenticate(&self, request: &Request<'_>) -> AuthStep {
        match bearer_token(request) {
            Some(token) if TokenHash::of(token).matches(&self.hash) => {
                AuthStep::Authenticated(Principal::operator())
            }
            _ => AuthStep::Pass,
        }
    }
}

/// User tokens of `access.toml` as bearer tokens; a bearer token no user holds is rejected.
pub struct UserTokens {
    file: Arc<RwLock<AccessFile>>,
}

impl UserTokens {
    /// The link over the (shared, live) access file.
    pub fn new(file: Arc<RwLock<AccessFile>>) -> Self {
        Self { file }
    }
}

impl Authenticator for UserTokens {
    fn authenticate(&self, request: &Request<'_>) -> AuthStep {
        let Some(token) = bearer_token(request) else {
            return AuthStep::Pass;
        };
        let file = self.file.read().unwrap_or_else(|p| p.into_inner());
        match file.user_by_token(&TokenHash::of(token)) {
            Some((name, user)) => {
                AuthStep::Authenticated(Principal::user(name, user.roles.clone()))
            }
            None => AuthStep::Rejected(AuthError::InvalidToken),
        }
    }
}

/// A browser session: the user it belongs to and the token it logged in with.
///
/// `#[non_exhaustive]`: build one with [`BrowserSession::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BrowserSession {
    /// The user's name.
    pub user: String,
    /// The hash of the token it logged in with: when the user's token changes (revoked,
    /// reissued), the session ends.
    pub token: TokenHash,
    /// When it was made.
    pub created: SystemTime,
    /// When a request last used it (updated coarsely).
    pub last_seen: SystemTime,
}

impl BrowserSession {
    /// A session of `user`, logged in with the token hashing to `token`, made `now`.
    pub fn new(user: impl Into<String>, token: TokenHash, now: SystemTime) -> Self {
        Self {
            user: user.into(),
            token,
            created: now,
            last_seen: now,
        }
    }
}

/// The browser sessions, keyed by the SHA-256 of the session id (the id itself is never
/// kept). In memory today ([`MemorySessions`]); a persisted table implements the same trait.
pub trait SessionTable: Send + Sync {
    /// Adds a session.
    fn insert(&self, id: TokenHash, session: BrowserSession) -> Result<(), ServerError>;
    /// The session `id`.
    fn get(&self, id: &TokenHash) -> Option<BrowserSession>;
    /// Sets the session's `last_seen` (a missing session is ignored).
    fn touch(&self, id: &TokenHash, now: SystemTime) -> Result<(), ServerError>;
    /// Removes the session `id`: it, when it was there.
    fn remove(&self, id: &TokenHash) -> Result<Option<BrowserSession>, ServerError>;
    /// Keeps only the sessions `keep` accepts: how many were removed.
    fn retain(&self, keep: &dyn Fn(&BrowserSession) -> bool) -> Result<usize, ServerError>;
}

/// The in-memory session table (sessions end with the process).
#[derive(Debug, Default)]
pub struct MemorySessions {
    sessions: Mutex<HashMap<TokenHash, BrowserSession>>,
}

impl MemorySessions {
    /// An empty table.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<TokenHash, BrowserSession>> {
        self.sessions.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl SessionTable for MemorySessions {
    fn insert(&self, id: TokenHash, session: BrowserSession) -> Result<(), ServerError> {
        self.lock().insert(id, session);
        Ok(())
    }

    fn get(&self, id: &TokenHash) -> Option<BrowserSession> {
        self.lock().get(id).cloned()
    }

    fn touch(&self, id: &TokenHash, now: SystemTime) -> Result<(), ServerError> {
        if let Some(session) = self.lock().get_mut(id) {
            session.last_seen = now;
        }
        Ok(())
    }

    fn remove(&self, id: &TokenHash) -> Result<Option<BrowserSession>, ServerError> {
        Ok(self.lock().remove(id))
    }

    fn retain(&self, keep: &dyn Fn(&BrowserSession) -> bool) -> Result<usize, ServerError> {
        let mut sessions = self.lock();
        let before = sessions.len();
        sessions.retain(|_, session| keep(session));
        Ok(before - sessions.len())
    }
}

/// The browser session cookie: a live session of a user whose token is still the one it
/// logged in with.
pub struct SessionCookie {
    file: Arc<RwLock<AccessFile>>,
    sessions: Arc<dyn SessionTable>,
}

impl SessionCookie {
    /// The link over the access file and the session table.
    pub fn new(file: Arc<RwLock<AccessFile>>, sessions: Arc<dyn SessionTable>) -> Self {
        Self { file, sessions }
    }
}

/// The principal of `session` when it is still live in `file`.
pub(crate) fn session_principal(file: &AccessFile, session: &BrowserSession) -> Option<Principal> {
    let user = file.user(&session.user)?;
    user.token
        .is_some_and(|token| token.matches(&session.token))
        .then(|| Principal::user(session.user.clone(), user.roles.clone()))
}

impl Authenticator for SessionCookie {
    fn authenticate(&self, request: &Request<'_>) -> AuthStep {
        let Some(id) = cookie(request, SESSION_COOKIE) else {
            return AuthStep::Pass;
        };
        let id = TokenHash::of(id);
        let Some(session) = self.sessions.get(&id) else {
            return AuthStep::Rejected(AuthError::InvalidSession);
        };
        let principal = {
            let file = self.file.read().unwrap_or_else(|p| p.into_inner());
            session_principal(&file, &session)
        };
        let Some(principal) = principal else {
            // Its user is gone or holds another token now: the session is over.
            let _ = self.sessions.remove(&id);
            return AuthStep::Rejected(AuthError::InvalidSession);
        };
        let now = SystemTime::now();
        let stale = now
            .duration_since(session.last_seen)
            .is_ok_and(|age| age >= LAST_SEEN_STEP);
        if stale && let Err(err) = self.sessions.touch(&id, now) {
            tracing::warn!(target: "graphersal_server", "browser sessions: {err}");
        }
        AuthStep::Authenticated(principal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphersal_serve::Method;

    fn request(headers: &[(&str, &str)]) -> Request<'static> {
        headers.iter().fold(
            Request::new(Method::Get, "/", std::io::empty()),
            |r, (n, v)| r.with_header(n, v),
        )
    }

    #[test]
    fn credentials_are_read_from_the_headers() {
        let r = request(&[("authorization", "bearer  abc ")]);
        assert_eq!(bearer_token(&r), Some("abc"));
        assert_eq!(
            bearer_token(&request(&[("Authorization", "Basic abc")])),
            None
        );
        assert_eq!(
            bearer_token(&request(&[("Authorization", "Bearer ")])),
            None
        );
        let r = request(&[("Cookie", "a=1; gs_session=xyz"), ("cookie", "b=2")]);
        assert_eq!(cookie(&r, SESSION_COOKIE), Some("xyz"));
        assert_eq!(cookie(&r, "b"), Some("2"));
        assert_eq!(cookie(&r, "gs"), None);
    }

    #[test]
    fn the_memory_table_keeps_hashed_ids() {
        let table = MemorySessions::new();
        let id = TokenHash::of("id");
        let now = SystemTime::now();
        table
            .insert(id, BrowserSession::new("bob", TokenHash::of("t"), now))
            .unwrap();
        assert_eq!(table.get(&id).unwrap().user, "bob");
        let later = now + Duration::from_secs(5);
        table.touch(&id, later).unwrap();
        assert_eq!(table.get(&id).unwrap().last_seen, later);
        assert_eq!(table.retain(&|s| s.user != "bob").unwrap(), 1);
        assert_eq!(table.get(&id), None);
        assert_eq!(table.remove(&id).unwrap(), None);
    }
}
