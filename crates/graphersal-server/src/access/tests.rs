//! Tests of the live access state: the authenticator chain, browser login/logout, the user and
//! grant changes with their audit events, the level of a database or alias, the first admin.

use std::fs;
use std::sync::Arc;

use graphersal_serve::Method;

use super::*;
use crate::audit::MemoryAudit;
use crate::registry::tests::{add_db, server};
use crate::test_support::TempDir;

fn request(headers: &[(&str, String)]) -> Request<'static> {
    headers.iter().fold(
        Request::new(Method::Get, "/", std::io::empty()),
        |r, (n, v)| r.with_header(n, v),
    )
}

fn bearer(token: &str) -> Request<'static> {
    request(&[("Authorization", format!("Bearer {token}"))])
}

fn with_cookie(id: &Secret) -> Request<'static> {
    request(&[(
        "Cookie",
        format!("theme=dark; {SESSION_COOKIE}={}", id.expose()),
    )])
}

/// A server directory (with its first admin), the access state with an operator token and a
/// memory audit, and the first admin's token.
struct Fixture {
    _temp: TempDir,
    dir: ServerDir,
    access: Access,
    operator: Secret,
    admin: Secret,
    audit: Arc<MemoryAudit>,
}

fn fixture(name: &str) -> Fixture {
    let temp = TempDir::new(name);
    let report = ServerDir::init(temp.path().join("srv")).unwrap();
    let admin = report.first_admin.unwrap().token;
    let operator = Secret::generate().unwrap();
    let audit = Arc::new(MemoryAudit::default());
    let (access, first) = Access::open(
        &report.dir,
        Some(operator.hash()),
        Audit::new(audit.clone()),
    )
    .unwrap();
    assert_eq!(first, None, "init made the first admin already");
    Fixture {
        _temp: temp,
        dir: report.dir,
        access,
        operator,
        admin,
        audit,
    }
}

#[test]
fn the_chain_finds_operator_users_and_anonymous() {
    let f = fixture("access-chain");
    let operator = f.access.authenticate(&bearer(f.operator.expose())).unwrap();
    assert_eq!(operator, Principal::operator());
    let admin = f.access.authenticate(&bearer(f.admin.expose())).unwrap();
    assert_eq!(admin.id, FIRST_ADMIN);
    assert!(admin.is_admin());
    assert_eq!(admin.identity().as_deref(), Some("user:admin"));
    // No credentials: anonymous. Wrong credentials: refused, never anonymous.
    let anonymous = f.access.authenticate(&request(&[])).unwrap();
    assert!(anonymous.is_anonymous());
    assert_eq!(
        f.access.authenticate(&bearer("not-a-token")),
        Err(AuthError::InvalidToken)
    );
    // Without an operator link the operator token is just a wrong token.
    let (plain, _) = Access::open(&f.dir, None, Audit::default()).unwrap();
    assert_eq!(
        plain.authenticate(&bearer(f.operator.expose())),
        Err(AuthError::InvalidToken)
    );
}

#[test]
fn browser_sessions_live_until_logout_or_a_token_change() {
    let f = fixture("access-sessions");
    let operator = Principal::operator();
    let bob_token = f.access.add_user(&operator, "bob", Vec::new()).unwrap();

    // The operator token does not log in; a wrong token neither.
    assert_eq!(
        f.access.login(f.operator.expose()).map(|_| ()),
        Err(AuthError::InvalidToken)
    );
    let (id, bob) = f.access.login(bob_token.expose()).unwrap();
    assert_eq!(bob.id, "bob");
    assert_eq!(f.access.authenticate(&with_cookie(&id)).unwrap(), bob);
    // An unknown session id is refused.
    let stranger = Secret::generate().unwrap();
    assert_eq!(
        f.access.authenticate(&with_cookie(&stranger)),
        Err(AuthError::InvalidSession)
    );
    // Logout ends it.
    assert!(f.access.logout(id.expose()).unwrap());
    assert!(!f.access.logout(id.expose()).unwrap());
    assert_eq!(
        f.access.authenticate(&with_cookie(&id)),
        Err(AuthError::InvalidSession)
    );

    // A new token ends the sessions of the old one at once; the old bearer token is refused.
    let (id, _) = f.access.login(bob_token.expose()).unwrap();
    let new_token = f.access.issue_token(&operator, "bob").unwrap();
    assert_eq!(
        f.access.authenticate(&with_cookie(&id)),
        Err(AuthError::InvalidSession)
    );
    assert_eq!(
        f.access.authenticate(&bearer(bob_token.expose())),
        Err(AuthError::InvalidToken)
    );
    // A revoked token, a removed user: the same.
    let (id, _) = f.access.login(new_token.expose()).unwrap();
    f.access.revoke_token(&operator, "bob").unwrap();
    assert_eq!(
        f.access.authenticate(&with_cookie(&id)),
        Err(AuthError::InvalidSession)
    );
    let newest = f.access.issue_token(&operator, "bob").unwrap();
    let (id, _) = f.access.login(newest.expose()).unwrap();
    f.access.remove_user(&operator, "bob").unwrap();
    assert_eq!(
        f.access.authenticate(&with_cookie(&id)),
        Err(AuthError::InvalidSession)
    );
    assert!(f.access.file().user("bob").is_none());
    // Every change is on disk, and only hashes are.
    let text = fs::read_to_string(f.dir.access_path()).unwrap();
    assert!(!text.contains(newest.expose()), "{text}");
    assert_eq!(AccessFile::read(&f.dir).unwrap().unwrap(), f.access.file());
}

#[test]
fn changes_and_logins_are_audited_without_secrets() {
    let f = fixture("access-audit");
    let admin = f.access.authenticate(&bearer(f.admin.expose())).unwrap();
    let token = f.access.add_user(&admin, "Bob", Vec::new()).unwrap();
    let grant = Grant::new(Subject::parse("user:bob").unwrap(), "acme", Level::Read).unwrap();
    f.access.grant(&admin, grant).unwrap();
    assert!(
        f.access
            .revoke(&admin, &Subject::User("bob".to_string()), "acme")
            .unwrap()
    );
    assert!(
        !f.access
            .revoke(&admin, &Subject::User("bob".to_string()), "acme")
            .unwrap()
    );
    f.access
        .set_roles(&Principal::operator(), "bob", vec!["admin".to_string()])
        .unwrap();
    let (id, _) = f.access.login(token.expose()).unwrap();
    f.access.logout(id.expose()).unwrap();
    let _ = f.access.login("wrong");
    let lines = f.audit.lines();
    assert_eq!(
        lines,
        [
            format!(
                "user:admin: user 'bob' added (roles [], token …{})",
                token.last4()
            ),
            "user:admin: grant user:bob read on 'acme'".to_string(),
            "user:admin: grant of user:bob on 'acme' revoked".to_string(),
            "operator: user 'bob' roles set to [admin]".to_string(),
            "user:bob: login of 'bob'".to_string(),
            "user:bob: logout of 'bob'".to_string(),
            "-: login refused: a token no user holds".to_string(),
        ]
    );
    assert!(lines.iter().all(|l| !l.contains(token.expose())));
}

#[test]
fn levels_follow_grants_database_modes_and_aliases() {
    let f = fixture("access-levels");
    add_db(&f.dir, "acme", Some(""), true);
    add_db(&f.dir, "frozen", Some("read_only = true"), true);
    add_db(
        &f.dir,
        "base",
        Some("role = \"base\"\nread_only = true"),
        true,
    );
    fs::write(f.dir.aliases_path(), "prod = \"acme\"\nold = \"frozen\"\n").unwrap();
    let registry = crate::registry::Registry::scan(&f.dir).unwrap();
    let operator = Principal::operator();
    f.access.add_user(&operator, "bob", Vec::new()).unwrap();
    let grant = |subject: &str, database: &str, level: Level| {
        let grant = Grant::new(Subject::parse(subject).unwrap(), database, level).unwrap();
        f.access.grant(&operator, grant).unwrap();
    };
    grant("user:bob", "acme", Level::Write);
    grant("user:bob", "frozen", Level::Write);
    grant("anonymous", "base", Level::Read);
    let bob = f.access.file().principal_of("bob").unwrap();
    let anonymous = Principal::anonymous();
    let level = |p: &Principal, name: &str| f.access.level_in(&registry, p, name);

    assert_eq!(level(&bob, "acme"), Level::Write);
    assert_eq!(level(&bob, "prod"), Level::Write); // an alias: its target's level
    assert_eq!(level(&bob, "PROD"), Level::Write);
    assert_eq!(level(&bob, "frozen"), Level::Read); // capped by the read-only flag
    assert_eq!(level(&bob, "old"), Level::Read);
    assert_eq!(level(&bob, "base"), Level::Read); // the anonymous grant counts for bob too
    assert_eq!(level(&anonymous, "base"), Level::Read);
    assert_eq!(level(&anonymous, "acme"), Level::None);
    assert_eq!(level(&anonymous, "prod"), Level::None);
    assert_eq!(level(&bob, "missing"), Level::None);
    // Admin and operator: write everywhere a database allows it.
    let admin = f.access.authenticate(&bearer(f.admin.expose())).unwrap();
    assert_eq!(level(&admin, "prod"), Level::Write);
    assert_eq!(level(&admin, "base"), Level::Read);
    assert_eq!(level(&operator, "acme"), Level::Write);
    // "*" reaches every database.
    grant("user:bob", "*", Level::Read);
    add_db(&f.dir, "later", Some(""), true);
    let registry = crate::registry::Registry::scan(&f.dir).unwrap();
    assert_eq!(f.access.level_in(&registry, &bob, "later"), Level::Read);
    assert_eq!(f.access.level_in(&registry, &bob, "acme"), Level::Write);
}

#[test]
fn a_directory_without_access_toml_gets_its_first_admin_once() {
    let temp = TempDir::new("access-first");
    let dir = server(&temp);
    // `init` made one; a directory whose file was removed is closed again at the next open.
    fs::remove_file(dir.access_path()).unwrap();
    let audit = Arc::new(MemoryAudit::default());
    let (access, first) = Access::open(&dir, None, Audit::new(audit.clone())).unwrap();
    let first = first.unwrap();
    assert_eq!(first.name, FIRST_ADMIN);
    assert_eq!(first.path, dir.access_path());
    let admin = access.authenticate(&bearer(first.token.expose())).unwrap();
    assert!(admin.is_admin());
    assert_eq!(
        audit.lines(),
        [format!(
            "-: first admin 'admin' created (token …{}, printed once)",
            first.token.last4()
        )]
    );
    let (_, again) = Access::open(&dir, None, Audit::default()).unwrap();
    assert_eq!(again, None);
    // A broken file: the server does not start (the directory stays closed).
    fs::write(dir.access_path(), "[users.admin]\ntoken_sha256 = 5\n").unwrap();
    let err = Access::open(&dir, None, Audit::default()).unwrap_err();
    assert!(matches!(err, ServerError::Config { .. }), "{err}");
}

#[test]
fn init_makes_the_first_admin_also_in_an_old_server_directory() {
    let temp = TempDir::new("access-init");
    let root = temp.path().join("srv");
    let report = ServerDir::init(&root).unwrap();
    let first = report.first_admin.clone().unwrap();
    assert_eq!(report.created.last(), Some(&first.path));
    // Again: nothing to do, no second token.
    let again = ServerDir::init(&root).unwrap();
    assert_eq!(again.first_admin, None);
    assert!(again.created.is_empty());
    // A server directory without access.toml (made before access control): init closes it.
    fs::remove_file(report.dir.access_path()).unwrap();
    let old = ServerDir::init(&root).unwrap();
    assert!(old.existed);
    let token = old.first_admin.unwrap().token;
    assert_ne!(token, first.token);
    assert_eq!(old.created, vec![report.dir.access_path()]);
    let file = AccessFile::read(&report.dir).unwrap().unwrap();
    assert!(file.user_by_token(&token.hash()).is_some());
    assert!(file.user_by_token(&first.token.hash()).is_none());
}

#[test]
fn the_first_serve_without_access_toml_makes_the_first_admin() {
    let temp = TempDir::new("access-serve");
    let dir = server(&temp);
    fs::remove_file(dir.access_path()).unwrap();
    let options = crate::ServeOptions::new().with_listen("127.0.0.1:0".parse().unwrap());
    let mut running = crate::serve::start(dir.root(), options.clone()).unwrap();
    let first = running.take_first_admin().unwrap();
    assert_eq!(running.take_first_admin(), None, "handed out once");
    let access = running.access().unwrap().clone();
    assert!(
        access
            .authenticate(&bearer(first.token.expose()))
            .unwrap()
            .is_admin()
    );
    // The operator token of this run is in server.run (for its owner only) and authenticates.
    let info = crate::RunInfo::read(&dir).unwrap().unwrap();
    let operator = info.operator_token.unwrap();
    assert_eq!(
        access.authenticate(&bearer(operator.expose())).unwrap(),
        Principal::operator()
    );
    running.stop().unwrap();
    // The next start keeps the file: no new admin; a new operator token.
    let mut running = crate::serve::start(dir.root(), options).unwrap();
    assert_eq!(running.take_first_admin(), None);
    let next = crate::RunInfo::read(&dir).unwrap().unwrap();
    assert_ne!(next.operator_token, Some(operator.clone()));
    assert_eq!(
        running
            .access()
            .unwrap()
            .authenticate(&bearer(operator.expose())),
        Err(AuthError::InvalidToken)
    );
    running.stop().unwrap();
}

/// The access state of `f`'s directory over its stored browser sessions with `limits`.
fn reopened(f: &Fixture, limits: SessionLimits) -> (Access, Arc<crate::sessions::FileSessions>) {
    let table = Arc::new(crate::sessions::FileSessions::open(&f.dir));
    let (access, _) = Access::open_with_limits(
        &f.dir,
        Some(f.operator.hash()),
        table.clone(),
        limits,
        Audit::default(),
    )
    .unwrap();
    (access, table)
}

#[test]
fn stored_browser_sessions_survive_a_reopen_and_end_with_their_token() {
    let f = fixture("access-stored");
    let operator = Principal::operator();
    let (access, _) = reopened(&f, SessionLimits::unlimited());
    let bob_token = access.add_user(&operator, "bob", Vec::new()).unwrap();
    let carol_token = access.add_user(&operator, "carol", Vec::new()).unwrap();
    let (bob_id, bob) = access.login(bob_token.expose()).unwrap();
    let (carol_id, _) = access.login(carol_token.expose()).unwrap();
    drop(access);

    // A restart: the cookie still works.
    let (access, table) = reopened(&f, SessionLimits::unlimited());
    assert_eq!(table.len(), 2);
    assert_eq!(access.authenticate(&with_cookie(&bob_id)).unwrap(), bob);
    // A revoked token ends the session in memory and on disk at once.
    access.revoke_token(&operator, "bob").unwrap();
    assert_eq!(
        access.authenticate(&with_cookie(&bob_id)),
        Err(AuthError::InvalidSession)
    );
    assert_eq!(crate::sessions::FileSessions::open(&f.dir).len(), 1);
    // Logout too.
    assert!(access.logout(carol_id.expose()).unwrap());
    assert!(crate::sessions::FileSessions::open(&f.dir).is_empty());
    drop(access);

    // A token changed while no server ran (the file edited): the stored session is dropped at
    // the next open.
    let (access, _) = reopened(&f, SessionLimits::unlimited());
    let (carol_id, _) = access.login(carol_token.expose()).unwrap();
    drop(access);
    let mut file = AccessFile::read(&f.dir).unwrap().unwrap();
    file.issue_token("carol").unwrap();
    file.write(&f.dir).unwrap();
    let (access, table) = reopened(&f, SessionLimits::unlimited());
    assert!(table.is_empty());
    assert_eq!(
        access.authenticate(&with_cookie(&carol_id)),
        Err(AuthError::InvalidSession)
    );
}

#[test]
fn browser_sessions_expire_by_lifetime_and_idle_time() {
    use std::time::Duration;
    let f = fixture("access-expiry");
    let operator = Principal::operator();
    let token = f.access.add_user(&operator, "dave", Vec::new()).unwrap();
    let hour = Duration::from_secs(3600);
    let now = SystemTime::now();
    let table = crate::sessions::FileSessions::open(&f.dir);
    // Four stored sessions: fresh, logged in two days ago (seen now), idle for two hours, and
    // one whose clock is in the future (nothing expires on a clock that went back).
    let mut sessions = Vec::new();
    for (created, last_seen) in [
        (now, now),
        (now - 48 * hour, now),
        (now - 3 * hour, now - 2 * hour),
        (now + hour, now + hour),
    ] {
        let id = Secret::generate().unwrap();
        let mut session = BrowserSession::new("dave", token.hash(), created);
        session.last_seen = last_seen;
        table.insert(id.hash(), session).unwrap();
        sessions.push(id);
    }
    drop(table);
    let alive = |access: &Access| -> Vec<bool> {
        sessions
            .iter()
            .map(|id| access.authenticate(&with_cookie(id)).is_ok())
            .collect()
    };
    // Unlimited (the default): every session is kept (not used here: a request would update
    // `last_seen` of the idle one).
    let (access, table) = reopened(&f, SessionLimits::unlimited());
    assert_eq!(table.len(), 4);
    drop(access);
    // An idle timeout of one hour ends the idle one (removed at the open, from the file too).
    let idle = SessionLimits::unlimited().with_idle_timeout(Some(hour));
    let (access, table) = reopened(&f, idle);
    assert_eq!(table.len(), 3);
    assert_eq!(alive(&access), [true, true, false, true]);
    drop(access);
    // A lifetime of one day ends the old one.
    let lifetime = SessionLimits::unlimited().with_lifetime(Some(24 * hour));
    let (access, table) = reopened(&f, lifetime);
    assert_eq!(table.len(), 2);
    assert_eq!(alive(&access), [true, false, false, true]);
    assert_eq!(crate::sessions::FileSessions::open(&f.dir).len(), 2);

    // A session that expires while the server runs is refused on its next request.
    let (id, _) = access.login(token.expose()).unwrap();
    let table = Arc::new(MemorySessions::new());
    let (short, _) = Access::open_with_limits(
        &f.dir,
        None,
        table.clone(),
        SessionLimits::unlimited().with_idle_timeout(Some(hour)),
        Audit::default(),
    )
    .unwrap();
    let mut old = BrowserSession::new("dave", token.hash(), now - 2 * hour);
    old.last_seen = now - 2 * hour;
    table.insert(id.hash(), old).unwrap();
    assert_eq!(
        short.authenticate(&with_cookie(&id)),
        Err(AuthError::InvalidSession)
    );
    assert_eq!(
        table.get(&id.hash()),
        None,
        "the expired session is removed"
    );
}

/// A NEW user takes the rule of new names (0.3.0); a user and a database named before it
/// (a trailing '_', 40 characters) still log in and are granted.
#[test]
fn new_user_names_follow_the_new_rule_and_old_ones_keep_working() {
    let f = fixture("access-new-names");
    let operator = Principal::operator();
    let too_long = "u".repeat(33);
    for (name, reason) in [
        (too_long.as_str(), "at most 32 characters"),
        ("bob-", "ends with a lower-case letter or a digit"),
        ("bob_", "ends with a lower-case letter or a digit"),
        ("b..b", "cannot hold '..'"),
    ] {
        let err = f.access.add_user(&operator, name, Vec::new()).unwrap_err();
        assert!(matches!(err, ServerError::InvalidName { .. }), "{err}");
        assert!(err.to_string().contains(reason), "{name}: {err}");
    }
    // `jobs` is reserved for databases only.
    f.access.add_user(&operator, "jobs", Vec::new()).unwrap();
    // A user made by an older server: `bob` renamed to `bob_` in access.toml.
    let token = f.access.add_user(&operator, "bob", Vec::new()).unwrap();
    let path = f.dir.access_path();
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("[users.bob]"), "{text}");
    fs::write(&path, text.replace("[users.bob]", "[users.bob_]")).unwrap();
    let (access, _) = Access::open(&f.dir, None, Audit::default()).unwrap();
    let bob = access.authenticate(&bearer(token.expose())).unwrap();
    assert_eq!(bob.identity().as_deref(), Some("user:bob_"));
    let long = "a-database-name-of-forty-characters-0040";
    add_db(&f.dir, long, Some(""), true);
    add_db(&f.dir, "legacy-", Some(""), true);
    for database in [long, "legacy-"] {
        let grant =
            Grant::new(Subject::parse("user:bob_").unwrap(), database, Level::Read).unwrap();
        access.grant(&operator, grant).unwrap();
        assert_eq!(
            access.granted_level(&bob, database),
            Level::Read,
            "{database}"
        );
    }
    // The file with these names reads back.
    assert_eq!(AccessFile::read(&f.dir).unwrap().unwrap(), access.file());
}

/// SEC L3: a login loop cannot grow the browser session table without bound: a user holds at
/// most [`MAX_BROWSER_SESSIONS_PER_USER`] sessions, a new login drops the oldest, and another
/// user's sessions are untouched.
#[test]
fn a_users_browser_sessions_are_capped_and_the_oldest_dropped() {
    let f = fixture("access-session-cap");
    let operator = Principal::operator();
    let (access, table) = reopened(&f, SessionLimits::unlimited());
    let bob_token = access.add_user(&operator, "bob", Vec::new()).unwrap();
    let carol_token = access.add_user(&operator, "carol", Vec::new()).unwrap();
    let (carol_id, carol) = access.login(carol_token.expose()).unwrap();
    let ids: Vec<Secret> = (0..MAX_BROWSER_SESSIONS_PER_USER + 3)
        .map(|_| access.login(bob_token.expose()).unwrap().0)
        .collect();
    assert_eq!(table.len(), MAX_BROWSER_SESSIONS_PER_USER + 1);
    assert_eq!(
        crate::sessions::FileSessions::open(&f.dir).len(),
        MAX_BROWSER_SESSIONS_PER_USER + 1,
        "the file holds the capped table"
    );
    assert_eq!(
        access.authenticate(&with_cookie(&ids[0])),
        Err(AuthError::InvalidSession),
        "the oldest session was dropped"
    );
    let newest = ids.last().unwrap();
    assert_eq!(access.authenticate(&with_cookie(newest)).unwrap().id, "bob");
    assert_eq!(access.authenticate(&with_cookie(&carol_id)).unwrap(), carol);
    // The in-memory table follows the same cap.
    let memory = MemorySessions::new();
    let now = SystemTime::now();
    for i in 0..5u64 {
        let session = BrowserSession::new(
            "bob",
            TokenHash::of("t"),
            now + std::time::Duration::from_secs(i),
        );
        memory
            .insert_capped(TokenHash::of(&format!("s{i}")), session, 3)
            .unwrap();
    }
    assert!(memory.get(&TokenHash::of("s1")).is_none());
    assert!(memory.get(&TokenHash::of("s2")).is_some());
    assert!(memory.get(&TokenHash::of("s4")).is_some());
}
