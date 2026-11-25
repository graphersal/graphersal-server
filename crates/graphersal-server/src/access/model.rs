//! `access.toml`: users, their token hashes and roles, and the grants; the level computation.
//!
//! ```toml
//! [users.alice]
//! roles = ["admin"]
//! token_sha256 = "<64 hex digits>"
//! token_last4 = "a1b2"
//!
//! [[grant]]
//! subject = "user:bob"      # or "anonymous"; later "group:<g>", "role:<r>"
//! database = "acme"         # a database name, an alias or "*"
//! level = "read"            # or "write"
//! ```
//!
//! Grants are keyed by SUBJECT, not nested under users, so groups and roles (and a JWT
//! authenticator filling them) slot in without a format change. A principal's level on a
//! database is the highest grant of any of its subjects ([`super::Principal::subjects`]); the
//! `admin` role (and the operator) is `write` everywhere. The effective level is capped by the
//! database's mode ([`effective_level`]): a read-only database or a base gives `read` at most.
//! [`Level::None`] means the database does not exist for that principal.
//!
//! A grant may name an alias: it reaches the alias's CURRENT target, but only when the request
//! names the alias ([`AccessFile::granted_level_via`]); asked by the real name, only the grants
//! on that name (and `*`) count. An alias's rename moves its grants
//! ([`AccessFile::rename_grants`]), its removal drops them ([`AccessFile::drop_grants`]).

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::principal::Principal;
use super::token::{Secret, TokenHash};
use crate::database::DatabaseRole;
use crate::error::ServerError;
use crate::layout::ServerDir;
use crate::names::{fold, validate_name, validate_new_user_name};
use crate::registry::DatabaseEntry;

/// The name of the first admin a server directory gets.
pub const FIRST_ADMIN: &str = "admin";

/// Names no user may take: `anonymous` (the grant subject of everyone) and `operator` (the
/// console's principal), so a grant or audit line never means two things.
pub const RESERVED_USER_NAMES: [&str; 2] = ["anonymous", "operator"];

/// A principal's access level on one database, ordered `None < Read < Write`.
///
/// `#[non_exhaustive]`: a finer split may come later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Level {
    /// The database does not exist for the principal (404, not listed).
    None,
    /// Reading: queries under the reading authorizer, no write route.
    Read,
    /// Everything the database's playground can change.
    Write,
}

impl Level {
    /// `none`, `read`, `write`.
    pub fn as_str(self) -> &'static str {
        match self {
            Level::None => "none",
            Level::Read => "read",
            Level::Write => "write",
        }
    }

    /// A grant's level as written in `access.toml`: `read` or `write` (no `none`: a grant
    /// that gives nothing is revoked instead).
    pub fn parse_grant(text: &str) -> Option<Level> {
        match text {
            "read" => Some(Level::Read),
            "write" => Some(Level::Write),
            _ => None,
        }
    }
}

/// The level of a principal whose grants give `granted`, on a database that is read-only
/// (`read_only`: the flag, or a base): the lower of the two.
pub fn effective_level(granted: Level, read_only: bool) -> Level {
    if read_only {
        granted.min(Level::Read)
    } else {
        granted
    }
}

/// Whether the database of `entry` is served read-only: its flag, or a base.
pub fn database_read_only(entry: &DatabaseEntry) -> bool {
    entry
        .config
        .as_ref()
        .is_some_and(|config| config.read_only || config.role == DatabaseRole::Base)
}

/// Who a grant is for.
///
/// `#[non_exhaustive]`: further subject kinds may come.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Subject {
    /// `user:<name>`.
    User(String),
    /// `anonymous`: every request, also an authenticated one.
    Anonymous,
    /// `role:<role>` (every principal with that role).
    Role(String),
    /// `group:<group>` (filled by a later authenticator).
    Group(String),
}

impl Subject {
    /// The subject written as `user:<name>`, `anonymous`, `role:<r>` or `group:<g>`. A user
    /// name is checked and folded to lower case.
    pub fn parse(text: &str) -> Result<Subject, String> {
        if text == "anonymous" {
            return Ok(Subject::Anonymous);
        }
        let (kind, name) = text.split_once(':').ok_or_else(|| {
            format!("subject {text:?}: expected user:<name>, anonymous, role:<r> or group:<g>")
        })?;
        if name.is_empty() {
            return Err(format!("subject {text:?}: the name after ':' is missing"));
        }
        match kind {
            "user" => {
                let name = fold(name);
                validate_name(&name).map_err(|err| format!("subject {text:?}: {err}"))?;
                Ok(Subject::User(name))
            }
            "role" => Ok(Subject::Role(name.to_string())),
            "group" => Ok(Subject::Group(name.to_string())),
            _ => Err(format!(
                "subject {text:?}: expected user:<name>, anonymous, role:<r> or group:<g>"
            )),
        }
    }
}

impl std::fmt::Display for Subject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Subject::User(name) => write!(f, "user:{name}"),
            Subject::Anonymous => f.write_str("anonymous"),
            Subject::Role(role) => write!(f, "role:{role}"),
            Subject::Group(group) => write!(f, "group:{group}"),
        }
    }
}

/// One grant: `subject` may reach `database` (a name or `*`) at `level`.
///
/// `#[non_exhaustive]`: build one with [`Grant::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Grant {
    /// Who.
    pub subject: Subject,
    /// The database name (folded) or `*` (every database).
    pub database: String,
    /// `Read` or `Write`.
    pub level: Level,
}

impl Grant {
    /// A grant; `database` is checked (a name or `*`) and folded, `level` must not be
    /// [`Level::None`].
    pub fn new(subject: Subject, database: &str, level: Level) -> Result<Grant, String> {
        if level == Level::None {
            return Err("a grant gives read or write (revoke it to give nothing)".to_string());
        }
        let database = if database == "*" {
            database.to_string()
        } else {
            let folded = fold(database);
            validate_name(&folded).map_err(|err| format!("database {database:?}: {err}"))?;
            folded
        };
        Ok(Grant {
            subject,
            database,
            level,
        })
    }

    /// Whether it covers the database `name` (folded).
    fn covers(&self, name: &str) -> bool {
        self.database == "*" || self.database == name
    }
}

/// A user of `access.toml`.
///
/// `#[non_exhaustive]`: further fields may be added.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct User {
    /// Its roles (`admin`).
    pub roles: Vec<String>,
    /// The SHA-256 of its token; `None`: it cannot authenticate (token revoked).
    pub token: Option<TokenHash>,
    /// The last four characters of its token (shown in listings).
    pub token_last4: Option<String>,
}

/// The contents of `access.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccessFile {
    users: BTreeMap<String, User>,
    grants: Vec<Grant>,
}

/// `access.toml` as written.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    users: BTreeMap<String, RawUser>,
    #[serde(default, rename = "grant", skip_serializing_if = "Vec::is_empty")]
    grants: Vec<RawGrant>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUser {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    roles: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token_last4: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGrant {
    subject: String,
    database: String,
    level: String,
}

const HEADER: &str = "# graphersal-server access: users (only SHA-256 hashes of their tokens), \
roles and grants.\n# Written by the server and its console (atomically); a token is shown once, \
when it is made.\n# grant: subject = \"user:<name>\" | \"anonymous\", database = <name> | <alias> | \
\"*\", level = \"read\" | \"write\" (a grant on an alias reaches its current database, only \
through the alias).\n";

impl AccessFile {
    /// A file with no users and no grants (nobody but the operator gets in).
    pub fn new() -> Self {
        Self::default()
    }

    /// A file with the first admin ([`FIRST_ADMIN`], role `admin`) and a new token for it.
    pub fn with_first_admin() -> Result<(Self, Secret), ServerError> {
        let mut file = Self::new();
        let token = file.add_user(FIRST_ADMIN, vec![super::ADMIN_ROLE.to_string()])?;
        Ok((file, token))
    }

    /// Parses `text` (the file `path`, for the error).
    pub fn parse(text: &str, path: &Path) -> Result<Self, ServerError> {
        let config = |message: String| ServerError::Config {
            path: path.to_path_buf(),
            message,
        };
        let raw: RawFile = toml::from_str(text).map_err(|err| config(err.message().to_string()))?;
        let mut file = Self::new();
        for (name, raw_user) in raw.users {
            let key = fold(&name);
            validate_name(&key).map_err(|err| config(format!("users.{name}: {err}")))?;
            let token = match &raw_user.token_sha256 {
                None => None,
                Some(hex) => Some(TokenHash::parse_hex(hex).ok_or_else(|| {
                    config(format!(
                        "users.{name}.token_sha256: expected 64 hex digits (a SHA-256)"
                    ))
                })?),
            };
            let user = User {
                roles: raw_user.roles,
                token,
                token_last4: raw_user.token_last4,
            };
            if file.users.insert(key, user).is_some() {
                return Err(config(format!(
                    "users.{name}: the user is listed twice (names are case-insensitive)"
                )));
            }
        }
        for (i, raw_grant) in raw.grants.into_iter().enumerate() {
            let at = |message: String| config(format!("grant #{}: {message}", i + 1));
            let subject = Subject::parse(&raw_grant.subject).map_err(at)?;
            let level = Level::parse_grant(&raw_grant.level).ok_or_else(|| {
                at(format!(
                    "level {:?}: expected \"read\" or \"write\"",
                    raw_grant.level
                ))
            })?;
            let grant = Grant::new(subject, &raw_grant.database, level).map_err(at)?;
            file.grants.push(grant);
        }
        Ok(file)
    }

    /// The file's text (a comment header, then the users and the grants).
    pub fn to_text(&self) -> String {
        let raw = RawFile {
            users: self
                .users
                .iter()
                .map(|(name, user)| {
                    let raw = RawUser {
                        roles: user.roles.clone(),
                        token_sha256: user.token.map(|hash| hash.to_hex()),
                        token_last4: user.token_last4.clone(),
                    };
                    (name.clone(), raw)
                })
                .collect(),
            grants: self
                .grants
                .iter()
                .map(|grant| RawGrant {
                    subject: grant.subject.to_string(),
                    database: grant.database.clone(),
                    level: grant.level.as_str().to_string(),
                })
                .collect(),
        };
        // Plain strings, lists and tables: the serializer cannot fail on them.
        let body = toml::to_string(&raw).unwrap_or_default();
        format!("{HEADER}\n{body}")
    }

    /// Reads `access.toml` of `dir`: `None` when there is none.
    pub fn read(dir: &ServerDir) -> Result<Option<Self>, ServerError> {
        let path = dir.access_path();
        match fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text, &path).map(Some),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(ServerError::io("read", &path, err)),
        }
    }

    /// Writes `access.toml` of `dir` (atomically, mode `0600` on unix).
    pub fn write(&self, dir: &ServerDir) -> Result<(), ServerError> {
        crate::layout::write_atomic_private(&dir.access_path(), self.to_text().as_bytes())
    }

    /// Creates `access.toml` of `dir` with the first admin, unless the file exists (a server
    /// or console that raced us made it; then `None`). The file appears whole or not at all.
    pub fn create_first_admin(dir: &ServerDir) -> Result<Option<(Self, Secret)>, ServerError> {
        let path = dir.access_path();
        if path.exists() {
            return Ok(None);
        }
        let (file, token) = Self::with_first_admin()?;
        if create_new(&path, file.to_text().as_bytes())? {
            Ok(Some((file, token)))
        } else {
            Ok(None)
        }
    }

    /// The users by name.
    pub fn users(&self) -> &BTreeMap<String, User> {
        &self.users
    }

    /// The user `name` (case-insensitive).
    pub fn user(&self, name: &str) -> Option<&User> {
        self.users.get(&fold(name))
    }

    /// The grants, in file order.
    pub fn grants(&self) -> &[Grant] {
        &self.grants
    }

    /// The user whose token hashes to `hash` (each user's hash compared in constant time).
    pub fn user_by_token(&self, hash: &TokenHash) -> Option<(&str, &User)> {
        let mut found = None;
        for (name, user) in &self.users {
            if user.token.is_some_and(|token| token.matches(hash)) && found.is_none() {
                found = Some((name.as_str(), user));
            }
        }
        found
    }

    /// The principal of the user `name` (its roles), if it exists.
    pub fn principal_of(&self, name: &str) -> Option<Principal> {
        let key = fold(name);
        self.users
            .get(&key)
            .map(|user| Principal::user(key, user.roles.clone()))
    }

    /// The level `principal`'s grants give on the database `name` (a REAL name: resolve an
    /// alias first), before the database's mode: `Write` for an admin, else the highest grant
    /// of any of its subjects, else [`Level::None`].
    pub fn granted_level(&self, principal: &Principal, name: &str) -> Level {
        if principal.is_admin() {
            return Level::Write;
        }
        let name = fold(name);
        let subjects = principal.subjects();
        self.grants
            .iter()
            .filter(|grant| grant.covers(&name))
            .filter(|grant| subjects.contains(&grant.subject.to_string()))
            .map(|grant| grant.level)
            .max()
            .unwrap_or(Level::None)
    }

    /// The level `principal`'s grants give on the database `real` when the request names it
    /// as `requested` (the database's own name, or an alias of it), before the database's
    /// mode: [`AccessFile::granted_level`] on `real`, and, when `requested` is an alias, the
    /// grants on the alias too (the higher of the two). A grant on an alias therefore reaches
    /// its CURRENT target, but only through the alias's name: asked by the real name, only the
    /// grants on the real name (and `*`) count.
    pub fn granted_level_via(&self, principal: &Principal, requested: &str, real: &str) -> Level {
        let own = self.granted_level(principal, real);
        let requested = fold(requested);
        if requested == fold(real) {
            return own;
        }
        own.max(self.granted_level(principal, &requested))
    }

    /// How many grants name the database or alias `name` itself (`*` grants not counted).
    pub fn grants_on(&self, name: &str) -> usize {
        let name = fold(name);
        self.grants.iter().filter(|g| g.database == name).count()
    }

    /// Moves every grant on `old` to `new` (an alias renamed): how many moved. A grant the
    /// same subject already holds on `new` keeps the higher level.
    pub fn rename_grants(&mut self, old: &str, new: &str) -> usize {
        let (old, new) = (fold(old), fold(new));
        let moving: Vec<Grant> = self
            .grants
            .iter()
            .filter(|g| g.database == old)
            .cloned()
            .collect();
        self.grants.retain(|g| g.database != old);
        for mut grant in moving.iter().cloned() {
            grant.database = new.clone();
            match self
                .grants
                .iter_mut()
                .find(|g| g.subject == grant.subject && g.database == grant.database)
            {
                Some(existing) => existing.level = existing.level.max(grant.level),
                None => self.grants.push(grant),
            }
        }
        moving.len()
    }

    /// Removes every grant on `name` itself (an alias removed; `*` grants stay): how many.
    pub fn drop_grants(&mut self, name: &str) -> usize {
        let name = fold(name);
        let before = self.grants.len();
        self.grants.retain(|g| g.database != name);
        before - self.grants.len()
    }

    /// The effective level of `principal` on the database of `entry`: the granted level
    /// capped by the database's mode ([`effective_level`]).
    pub fn level(&self, principal: &Principal, entry: &DatabaseEntry) -> Level {
        effective_level(
            self.granted_level(principal, &entry.name),
            database_read_only(entry),
        )
    }

    /// How many users can act as an admin: the `admin` role and a token.
    pub fn usable_admins(&self) -> usize {
        self.users
            .values()
            .filter(|user| {
                user.token.is_some() && user.roles.iter().any(|r| r == super::ADMIN_ROLE)
            })
            .count()
    }

    /// Adds the user `name` with `roles` and a new token (returned: show it once).
    ///
    /// `Err`: an invalid new name ([`ServerError::InvalidName`]; the new rule of
    /// [`crate::names::validate_new_user_name`]), a reserved name
    /// ([`RESERVED_USER_NAMES`]) or a user of that name ([`ServerError::Refused`]), the OS's
    /// random generator failing.
    pub fn add_user(&mut self, name: &str, roles: Vec<String>) -> Result<Secret, ServerError> {
        let key = fold(name);
        // A new user takes the new name rule; existing users keep the old one (`names`).
        validate_new_user_name(&key)?;
        if RESERVED_USER_NAMES.contains(&key.as_str()) {
            return Err(ServerError::Refused {
                name: key,
                message: "a reserved name (a grant subject or principal of its own)".to_string(),
            });
        }
        if self.users.contains_key(&key) {
            return Err(ServerError::Refused {
                name: key,
                message: "a user of this name exists".to_string(),
            });
        }
        let token = new_token()?;
        self.users.insert(
            key,
            User {
                roles,
                token: Some(token.hash()),
                token_last4: Some(token.last4()),
            },
        );
        Ok(token)
    }

    /// Gives the user `name` a new token (returned: show it once); the old one stops working.
    pub fn issue_token(&mut self, name: &str) -> Result<Secret, ServerError> {
        let token = new_token()?;
        let user = self.user_mut(name)?;
        user.token = Some(token.hash());
        user.token_last4 = Some(token.last4());
        Ok(token)
    }

    /// Takes the user `name`'s token away: it cannot authenticate until it gets a new one.
    pub fn revoke_token(&mut self, name: &str) -> Result<(), ServerError> {
        let user = self.user_mut(name)?;
        user.token = None;
        user.token_last4 = None;
        Ok(())
    }

    /// Sets the user `name`'s roles.
    pub fn set_roles(&mut self, name: &str, roles: Vec<String>) -> Result<(), ServerError> {
        self.user_mut(name)?.roles = roles;
        Ok(())
    }

    /// Removes the user `name` and every grant to `user:<name>`.
    pub fn remove_user(&mut self, name: &str) -> Result<(), ServerError> {
        let key = fold(name);
        if self.users.remove(&key).is_none() {
            return Err(ServerError::UserNotFound { name: key });
        }
        let subject = Subject::User(key);
        self.grants.retain(|grant| grant.subject != subject);
        Ok(())
    }

    /// Sets `grant` (replacing the level of an existing grant to the same subject and
    /// database).
    pub fn grant(&mut self, grant: Grant) {
        match self
            .grants
            .iter_mut()
            .find(|g| g.subject == grant.subject && g.database == grant.database)
        {
            Some(existing) => existing.level = grant.level,
            None => self.grants.push(grant),
        }
    }

    /// Removes the grants of `subject` on `database` (a name or `*`): whether one was there.
    pub fn revoke(&mut self, subject: &Subject, database: &str) -> bool {
        let database = if database == "*" {
            database.to_string()
        } else {
            fold(database)
        };
        let before = self.grants.len();
        self.grants
            .retain(|g| !(g.subject == *subject && g.database == database));
        self.grants.len() != before
    }

    fn user_mut(&mut self, name: &str) -> Result<&mut User, ServerError> {
        let key = fold(name);
        self.users
            .get_mut(&key)
            .ok_or(ServerError::UserNotFound { name: key })
    }
}

/// A new user token.
fn new_token() -> Result<Secret, ServerError> {
    Secret::generate().map_err(|message| ServerError::Random { message })
}

/// Writes `bytes` as the NEW file `path`: `false` when `path` exists (nothing written). The
/// file is written beside it and linked into place, so it appears whole and an existing file
/// is never replaced; a file system without hard links gets a rename after a check.
fn create_new(path: &Path, bytes: &[u8]) -> Result<bool, ServerError> {
    use std::io::Write;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".new-{}", std::process::id()));
    let tmp: PathBuf = path.with_file_name(name);
    let written = crate::layout::create_file(&tmp, true).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    if let Err(err) = written {
        let _ = fs::remove_file(&tmp);
        return Err(ServerError::io("write", &tmp, err));
    }
    let created = match fs::hard_link(&tmp, path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(_) if path.exists() => Ok(false),
        Err(_) => fs::rename(&tmp, path)
            .map(|()| true)
            .map_err(|err| ServerError::io("write", path, err)),
    };
    let _ = fs::remove_file(&tmp);
    created
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::DatabaseConfig;
    use crate::registry::DbState;
    use crate::test_support::TempDir;

    fn entry(name: &str, config: DatabaseConfig) -> DatabaseEntry {
        DatabaseEntry {
            name: name.to_string(),
            dir: PathBuf::from(name),
            state: DbState::at_rest(Some(&config)),
            config: Some(config),
        }
    }

    fn grant(file: &mut AccessFile, subject: &str, database: &str, level: Level) {
        file.grant(Grant::new(Subject::parse(subject).unwrap(), database, level).unwrap());
    }

    #[test]
    fn levels_take_the_highest_grant_capped_by_the_database_mode() {
        let mut file = AccessFile::new();
        file.add_user("bob", Vec::new()).unwrap();
        file.add_user("carol", Vec::new()).unwrap();
        grant(&mut file, "user:bob", "acme", Level::Read);
        grant(&mut file, "user:bob", "*", Level::Read);
        grant(&mut file, "user:bob", "acme", Level::Write); // replaces read
        grant(&mut file, "anonymous", "demo", Level::Read);
        grant(&mut file, "user:carol", "*", Level::Write);
        let bob = file.principal_of("BOB").unwrap();
        let carol = file.principal_of("carol").unwrap();
        let anonymous = Principal::anonymous();

        let acme = entry("acme", DatabaseConfig::plain());
        let other = entry("other", DatabaseConfig::plain());
        let demo = entry("demo", DatabaseConfig::plain());
        let frozen = entry("frozen", DatabaseConfig::plain().with_read_only(true));
        let base = entry(
            "base",
            DatabaseConfig::plain().with_role(DatabaseRole::Base),
        );
        assert_eq!(file.level(&bob, &acme), Level::Write);
        assert_eq!(file.level(&bob, &other), Level::Read); // through "*"
        assert_eq!(file.level(&bob, &demo), Level::Read);
        // The database's mode caps the grant.
        assert_eq!(file.level(&carol, &frozen), Level::Read);
        assert_eq!(file.level(&carol, &base), Level::Read);
        assert_eq!(file.level(&carol, &acme), Level::Write);
        // Anonymous: only its own grants; the cap never raises a level.
        assert_eq!(file.level(&anonymous, &demo), Level::Read);
        assert_eq!(file.level(&anonymous, &acme), Level::None);
        assert_eq!(file.level(&anonymous, &frozen), Level::None);
        // Admin and operator: write everywhere, read on a read-only database.
        let admin = Principal::user("root", vec!["admin".to_string()]);
        assert_eq!(file.level(&admin, &acme), Level::Write);
        assert_eq!(file.level(&admin, &base), Level::Read);
        assert_eq!(file.level(&Principal::operator(), &other), Level::Write);
        // A role grant reaches every principal with that role.
        let mut roles = AccessFile::new();
        grant(&mut roles, "role:analyst", "acme", Level::Read);
        let analyst = Principal::user("dave", vec!["analyst".to_string()]);
        assert_eq!(roles.level(&analyst, &acme), Level::Read);
        assert_eq!(roles.level(&bob, &acme), Level::None);
    }

    #[test]
    fn an_alias_grant_counts_only_through_the_alias_and_moves_or_goes_with_it() {
        let mut file = AccessFile::new();
        file.add_user("bob", Vec::new()).unwrap();
        grant(&mut file, "user:bob", "prod", Level::Read);
        grant(&mut file, "anonymous", "prod", Level::Write);
        grant(&mut file, "user:bob", "*", Level::Read);
        let bob = file.principal_of("bob").unwrap();
        let carol = Principal::user("carol", Vec::new());
        // Through the alias `prod` (-> `prod-2026-11`): the alias's grants and the target's.
        assert_eq!(
            file.granted_level_via(&bob, "PROD", "prod-2026-11"),
            Level::Write
        );
        // By the real name: only the real name's grants and `*`.
        assert_eq!(
            file.granted_level_via(&bob, "prod-2026-11", "prod-2026-11"),
            Level::Read
        );
        assert_eq!(
            file.granted_level_via(&carol, "prod", "prod-2026-11"),
            Level::Write,
            "the anonymous grant on the alias"
        );
        assert_eq!(
            file.granted_level_via(&carol, "prod-2026-11", "prod-2026-11"),
            Level::None
        );
        assert_eq!(file.grants_on("prod"), 2);
        // A rename moves them; a grant already on the new name keeps the higher level.
        grant(&mut file, "user:bob", "live", Level::Write);
        assert_eq!(file.rename_grants("Prod", "live"), 2);
        assert_eq!(file.grants_on("prod"), 0);
        assert_eq!(file.grants_on("live"), 2);
        assert_eq!(file.granted_level_via(&bob, "live", "x"), Level::Write);
        assert_eq!(file.rename_grants("nothing", "other"), 0);
        // A removal drops them; `*` stays.
        assert_eq!(file.drop_grants("LIVE"), 2);
        assert_eq!(file.grants().len(), 1);
        assert_eq!(file.grants()[0].database, "*");
    }

    #[test]
    fn effective_level_is_the_minimum() {
        assert_eq!(effective_level(Level::Write, true), Level::Read);
        assert_eq!(effective_level(Level::Write, false), Level::Write);
        assert_eq!(effective_level(Level::Read, true), Level::Read);
        assert_eq!(effective_level(Level::None, true), Level::None);
        assert!(Level::None < Level::Read && Level::Read < Level::Write);
    }

    #[test]
    fn the_file_round_trips_with_hashes_and_last4_only() {
        let temp = TempDir::new("access-round-trip");
        let dir = ServerDir::init(temp.path()).unwrap().dir;
        let mut file = AccessFile::new();
        let alice = file.add_user("alice", vec!["admin".to_string()]).unwrap();
        let bob = file.add_user("bob", Vec::new()).unwrap();
        grant(&mut file, "user:bob", "acme", Level::Read);
        grant(&mut file, "anonymous", "demo", Level::Read);
        grant(&mut file, "group:ops", "*", Level::Write);
        file.write(&dir).unwrap();
        let text = fs::read_to_string(dir.access_path()).unwrap();
        assert!(
            !text.contains(alice.expose()) && !text.contains(bob.expose()),
            "{text}"
        );
        assert!(text.contains("[users.alice]"), "{text}");
        assert!(
            text.contains(&format!("token_last4 = \"{}\"", bob.last4())),
            "{text}"
        );
        assert!(text.contains(&format!("token_sha256 = \"{}\"", bob.hash().to_hex())));
        assert!(text.contains("[[grant]]\nsubject = \"user:bob\""), "{text}");
        let read = AccessFile::read(&dir).unwrap().unwrap();
        assert_eq!(read, file);
        assert_eq!(read.user("alice").unwrap().token_last4, Some(alice.last4()));
        assert_eq!(
            read.user_by_token(&bob.hash()).map(|(name, _)| name),
            Some("bob")
        );
        assert_eq!(read.user_by_token(&TokenHash::of("nope")), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.access_path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_broken_file_is_refused_with_its_reason() {
        let path = Path::new("access.toml");
        let refuse = |text: &str, wanted: &str| {
            let err = AccessFile::parse(text, path).unwrap_err();
            assert!(matches!(err, ServerError::Config { .. }), "{err}");
            assert!(err.to_string().contains(wanted), "{err}");
        };
        refuse("[users.x]\ntoken_sha256 = \"abc\"\n", "64 hex digits");
        refuse("[users.X_]\n[users.x_]\n", "listed twice");
        refuse("[users.\"bad name\"]\n", "users.bad name");
        refuse(
            "[[grant]]\nsubject = \"user:bob\"\ndatabase = \"acme\"\nlevel = \"admin\"\n",
            "expected \"read\" or \"write\"",
        );
        refuse(
            "[[grant]]\nsubject = \"bob\"\ndatabase = \"acme\"\nlevel = \"read\"\n",
            "grant #1: subject \"bob\"",
        );
        refuse(
            "[[grant]]\nsubject = \"anonymous\"\ndatabase = \"A B\"\nlevel = \"read\"\n",
            "database \"A B\"",
        );
        // A typo is not silently ignored.
        refuse("[[grants]]\nsubject = \"anonymous\"\n", "grants");
        assert_eq!(AccessFile::parse("", path).unwrap(), AccessFile::new());
    }

    #[test]
    fn users_and_grants_change() {
        let mut file = AccessFile::new();
        let first = file.add_user("bob", Vec::new()).unwrap();
        assert!(matches!(
            file.add_user("BOB", Vec::new()),
            Err(ServerError::Refused { .. })
        ));
        assert!(matches!(
            file.add_user("no way", Vec::new()),
            Err(ServerError::InvalidName { .. })
        ));
        let second = file.issue_token("bob").unwrap();
        assert_eq!(file.user_by_token(&first.hash()), None);
        assert!(file.user_by_token(&second.hash()).is_some());
        assert_eq!(file.user("bob").unwrap().token_last4, Some(second.last4()));
        file.revoke_token("bob").unwrap();
        assert_eq!(file.user_by_token(&second.hash()), None);
        file.set_roles("bob", vec!["admin".to_string()]).unwrap();
        assert!(file.principal_of("bob").unwrap().is_admin());
        grant(&mut file, "user:bob", "acme", Level::Read);
        grant(&mut file, "anonymous", "acme", Level::Read);
        assert!(file.revoke(&Subject::Anonymous, "ACME"));
        assert!(!file.revoke(&Subject::Anonymous, "acme"));
        file.remove_user("bob").unwrap();
        assert!(file.grants().is_empty(), "{:?}", file.grants());
        assert!(matches!(
            file.remove_user("bob"),
            Err(ServerError::UserNotFound { .. })
        ));
        assert!(Grant::new(Subject::Anonymous, "acme", Level::None).is_err());
    }

    #[test]
    fn the_first_admin_is_created_once() {
        let temp = TempDir::new("access-first-admin");
        let dir = ServerDir::new(temp.path());
        let (file, token) = AccessFile::create_first_admin(&dir).unwrap().unwrap();
        let (name, user) = file.user_by_token(&token.hash()).unwrap();
        assert_eq!(name, FIRST_ADMIN);
        assert_eq!(user.roles, ["admin"]);
        assert_eq!(AccessFile::read(&dir).unwrap(), Some(file.clone()));
        // Never twice: the existing file is kept.
        assert!(AccessFile::create_first_admin(&dir).unwrap().is_none());
        assert_eq!(AccessFile::read(&dir).unwrap(), Some(file));
        // Only the file itself is left behind.
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
        assert!(
            create_new(&dir.access_path(), b"x")
                .map(|created| !created)
                .unwrap()
        );
    }
}
