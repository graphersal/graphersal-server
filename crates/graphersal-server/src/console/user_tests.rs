//! The console's `user` commands on both paths: offline on the server directory (the console
//! holds the server lock and edits `access.toml`) and online through a running server's admin
//! API with the run's operator token. Synthetic directories only.

use serde_json::Value;

use super::*;
use crate::access::{AccessFile, FIRST_ADMIN, TokenHash};
use crate::ops::CreateSpec;
use crate::registry::tests::server;
use crate::serve::{Running, ServeOptions};
use crate::test_support::TempDir;

fn user(name: &str) -> String {
    name.to_string()
}

/// Runs `command` on `dir`, expecting success: the answer and where it ran.
fn ok(dir: &ServerDir, command: DbCommand) -> (Value, Via) {
    let outcome = run(dir, &command).unwrap_or_else(|err| panic!("{command:?}: {err}"));
    (outcome.body, outcome.via)
}

/// Runs `command` on `dir`, expecting a refusal: its status and its text as the console shows
/// it (help with console commands).
fn refused(dir: &ServerDir, command: DbCommand) -> (u16, String) {
    match run(dir, &command) {
        Err(err @ ServerError::Remote { status, .. }) => {
            (status, command_error_text(&err, dir.root()))
        }
        other => panic!("{command:?}: expected a refusal, got {other:?}"),
    }
}

fn file(dir: &ServerDir) -> AccessFile {
    AccessFile::read(dir).unwrap().unwrap()
}

/// The whole `user` command set on `dir`; `online`: through a server (the answers are checked
/// to come from it).
fn exercise(dir: &ServerDir, online: Option<&Running>) {
    let via = |via: Via| match online {
        Some(running) => assert_eq!(via, Via::Server(running.address())),
        None => assert_eq!(via, Via::Directory),
    };
    // add: the token once, only its hash on disk.
    let (bob, how) = ok(
        dir,
        DbCommand::UserAdd {
            name: user("bob"),
            admin: false,
        },
    );
    via(how);
    let token = bob["token"].as_str().unwrap().to_string();
    let text = render(
        &DbCommand::UserAdd {
            name: user("bob"),
            admin: false,
        },
        &bob,
        dir.root(),
    );
    assert!(text.starts_with("Added user 'bob'\n"), "{text}");
    assert!(text.contains(&format!("token:      {token}")), "{text}");
    assert!(text.contains("shown once"), "{text}");
    let on_disk = std::fs::read_to_string(dir.access_path()).unwrap();
    assert!(!on_disk.contains(&token));
    assert_eq!(
        file(dir)
            .user_by_token(&TokenHash::of(&token))
            .map(|(n, _)| n),
        Some("bob")
    );
    let (status, text) = refused(
        dir,
        DbCommand::UserAdd {
            name: user("BOB"),
            admin: false,
        },
    );
    assert_eq!(status, 409, "{text}");

    // grant / revoke, by user name and for anonymous.
    let (grant, _) = ok(
        dir,
        DbCommand::UserGrant {
            subject: user("bob"),
            database: user("acme"),
            level: user("write"),
        },
    );
    assert_eq!(grant["subject"], "user:bob");
    ok(
        dir,
        DbCommand::UserGrant {
            subject: user("anonymous"),
            database: user("*"),
            level: user("read"),
        },
    );
    let (status, _) = refused(
        dir,
        DbCommand::UserGrant {
            subject: user("bob"),
            database: user("nope"),
            level: user("read"),
        },
    );
    assert_eq!(status, 404);

    // list: last 4 characters, grants, the other subjects.
    let (list, _) = ok(dir, DbCommand::UserList);
    let text = render(&DbCommand::UserList, &list, dir.root());
    assert!(text.contains(&format!("…{}", &token[60..])), "{text}");
    assert!(text.contains("acme write"), "{text}");
    assert!(text.contains("anonymous"), "{text}");
    assert!(text.contains("every database"), "{text}");
    assert!(!text.contains(&token), "{text}");

    let (revoked, _) = ok(
        dir,
        DbCommand::UserRevoke {
            subject: user("anonymous"),
            database: user("*"),
        },
    );
    assert_eq!(revoked["revoked"], true, "{revoked}");
    let (again, _) = ok(
        dir,
        DbCommand::UserRevoke {
            subject: user("anonymous"),
            database: user("*"),
        },
    );
    let text = render(
        &DbCommand::UserRevoke {
            subject: user("anonymous"),
            database: user("*"),
        },
        &again,
        dir.root(),
    );
    assert!(
        text.starts_with("anonymous had no grant on every database"),
        "{text}"
    );

    // admin on/off.
    let (row, _) = ok(
        dir,
        DbCommand::UserAdmin {
            name: user("bob"),
            on: true,
        },
    );
    assert_eq!(row["admin"], true);
    assert!(file(dir).principal_of("bob").unwrap().is_admin());
    ok(
        dir,
        DbCommand::UserAdmin {
            name: user("bob"),
            on: false,
        },
    );
    assert!(!file(dir).principal_of("bob").unwrap().is_admin());

    // token: a new one replaces the old; --revoke takes it away.
    let (renewed, _) = ok(
        dir,
        DbCommand::UserToken {
            name: user("bob"),
            revoke: false,
        },
    );
    let new_token = renewed["token"].as_str().unwrap();
    assert_ne!(new_token, token);
    let access = file(dir);
    assert!(access.user_by_token(&TokenHash::of(&token)).is_none());
    assert!(access.user_by_token(&TokenHash::of(new_token)).is_some());
    let (row, _) = ok(
        dir,
        DbCommand::UserToken {
            name: user("bob"),
            revoke: true,
        },
    );
    assert_eq!(row["hasToken"], false);
    let (list, _) = ok(dir, DbCommand::UserList);
    let text = render(&DbCommand::UserList, &list, dir.root());
    assert!(text.contains("none (revoked)"), "{text}");

    // The last admin stays; the help names the console's commands.
    for command in [
        DbCommand::UserRemove {
            name: user(FIRST_ADMIN),
        },
        DbCommand::UserAdmin {
            name: user(FIRST_ADMIN),
            on: false,
        },
        DbCommand::UserToken {
            name: user(FIRST_ADMIN),
            revoke: true,
        },
    ] {
        let (status, text) = refused(dir, command);
        assert_eq!(status, 409, "{text}");
        assert!(text.contains("is the last admin"), "{text}");
        assert!(
            text.contains("graphersal-server user admin USER on --dir"),
            "{text}"
        );
        assert!(
            text.contains("graphersal-server user token NAME --dir"),
            "{text}"
        );
    }

    // remove: the user and its grants.
    let (removed, _) = ok(dir, DbCommand::UserRemove { name: user("bob") });
    assert_eq!(removed["grantsRemoved"], 1, "{removed}");
    let access = file(dir);
    assert!(access.user("bob").is_none());
    assert!(access.grants().is_empty());
    let (status, text) = refused(dir, DbCommand::UserRemove { name: user("bob") });
    assert_eq!(status, 404, "{text}");
    assert!(text.contains("graphersal-server user list --dir"), "{text}");
}

#[test]
fn user_commands_edit_access_toml_on_the_directory() {
    let temp = TempDir::new("console-users-offline");
    let dir = server(&temp);
    crate::ops::Admin::offline(crate::registry::Registry::scan(&dir).unwrap())
        .create("acme", &CreateSpec::plain())
        .unwrap();
    exercise(&dir, None);
}

#[test]
fn user_commands_go_through_a_running_server() {
    let temp = TempDir::new("console-users-online");
    let dir = server(&temp);
    crate::ops::Admin::offline(crate::registry::Registry::scan(&dir).unwrap())
        .create("acme", &CreateSpec::plain())
        .unwrap();
    let running = crate::serve::start(
        dir.root(),
        ServeOptions::new().with_listen("127.0.0.1:0".parse().unwrap()),
    )
    .expect("the server starts");
    exercise(&dir, Some(&running));
    // The server's live state is the file's: a new user's token works over HTTP at once.
    let (carol, _) = ok(
        &dir,
        DbCommand::UserAdd {
            name: user("carol"),
            admin: false,
        },
    );
    let token = carol["token"].as_str().unwrap();
    let (status, info) =
        crate::client::request(running.address(), Some(token), "GET", "/api/info", None).unwrap();
    assert_eq!(status, 200);
    assert_eq!(info["principal"]["id"], "carol", "{info}");
    ok(
        &dir,
        DbCommand::UserRemove {
            name: user("carol"),
        },
    );
    let (status, _) =
        crate::client::request(running.address(), Some(token), "GET", "/api/info", None).unwrap();
    assert_eq!(status, 401);
    running.stop().unwrap();
}

#[test]
fn a_directory_without_access_toml_is_refused_offline() {
    let temp = TempDir::new("console-users-no-file");
    let dir = server(&temp);
    std::fs::remove_file(dir.access_path()).unwrap();
    let (status, text) = refused(&dir, DbCommand::UserList);
    assert_eq!(status, 409, "{text}");
    assert!(text.contains("no access.toml"), "{text}");
    assert!(
        !dir.access_path().exists(),
        "nothing was made behind the user's back"
    );
}

#[test]
fn user_requests_name_the_admin_routes() {
    let cases = [
        (DbCommand::UserList, Method::Get, "users"),
        (
            DbCommand::UserToken {
                name: user("bob"),
                revoke: true,
            },
            Method::Delete,
            "users/bob/token",
        ),
        (
            DbCommand::UserRevoke {
                subject: user("user:bob"),
                database: user("*"),
            },
            Method::Delete,
            "grants/user%3Abob/%2A",
        ),
    ];
    for (command, method, path) in cases {
        let (m, p, _) = command.request();
        assert_eq!((m, p.as_str()), (method, path));
    }
}
