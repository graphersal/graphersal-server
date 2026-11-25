//! The admin API's users and grants routes (`/api/admin/users...`, `/api/admin/grants...`),
//! over [`Admin::users`] and the other user operations (`crate::ops`); the console's `user`
//! commands and the web UI's Users tab use them.
//!
//! | Method | Path | Body | Answer |
//! |---|---|---|---|
//! | GET | `users` | | `200 {"users": [user...], "grants": [grant...]}` |
//! | POST | `users` | `{"name", "admin"?}` | `201` user + `"token"` (shown once) |
//! | GET | `users/<name>` | | `200` user |
//! | DELETE | `users/<name>` | | `200 {"name", "removed": true, "grantsRemoved", "mcpSessionsEnded"}` |
//! | POST | `users/<name>/token` | | `200` user + `"token"` (shown once) + `"mcpSessionsEnded"` |
//! | DELETE | `users/<name>/token` | | `200` user (`hasToken: false`) + `"mcpSessionsEnded"` |
//! | PUT | `users/<name>/admin` | `{"admin": bool}` | `200` user |
//! | PUT | `users/<name>/roles` | `{"roles": [role...]}` | `200` user |
//! | GET | `grants` | | `200 {"grants": [grant...]}` |
//! | POST | `grants` | `{"subject", "database", "level"}` | `200` grant (set, or its level replaced) |
//! | DELETE | `grants/<subject>/<database>` | | `200 {"subject", "database", "revoked": bool}` |
//!
//! A user: `name`, `roles`, `admin` (has the `admin` role), `hasToken`, `tokenLast4` (the last
//! four characters of its token, `null` without one), `grants` (`[{"database", "level"}...]`,
//! its own `user:<name>` grants). A grant: `subject` (`user:<name>`, `anonymous`, `role:<r>`,
//! `group:<g>`; a bare user name is taken as `user:<name>`), `database` (a name or `*`),
//! `level` (`read` | `write`). Path segments may be percent-encoded (`user%3Abob`).
//!
//! Errors: `400` (invalid name, subject or level), `404` (no such user or database), `409` (a
//! name taken or reserved, a grant on an alias, the last admin).

use graphersal_serve::{HttpError, Method, Request, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{body_of, error_answer};
use crate::access::{AccessFile, Grant, Subject};
use crate::error::ServerError;
use crate::names::fold;
use crate::ops::Admin;

/// The help of a refused change of the last admin.
const LAST_ADMIN_HELP: &str = "make another admin first: POST /api/admin/users {\"name\": \
\"<name>\", \"admin\": true}, or PUT /api/admin/users/<user>/admin {\"admin\": true}; a lost \
token is replaced (not revoked) with POST /api/admin/users/<name>/token";

/// Answers `method users/...` or `grants/...` (`segments`: the endpoint split at `/`); `None`
/// when the path is none of these routes.
pub(super) fn route(
    admin: &Admin,
    method: &Method,
    segments: &[&str],
    request: &mut Request<'_>,
) -> Option<Result<Response, HttpError>> {
    let read = matches!(method, Method::Get | Method::Head);
    let decoded: Vec<String> = match segments
        .iter()
        .map(|s| crate::router::percent_decode(s))
        .collect::<Option<Vec<String>>>()
    {
        Some(decoded) => decoded,
        None => return Some(Err(HttpError::bad_request("a broken %-escape in the path"))),
    };
    let parts: Vec<&str> = decoded.iter().map(String::as_str).collect();
    let not_allowed = || {
        Err(HttpError::new(
            405,
            format!(
                "method {method} is not allowed on /api/admin/{}",
                segments.join("/")
            ),
        ))
    };
    let answer = match (method, parts.as_slice()) {
        (_, ["users"]) if read => admin.users().map(|file| {
            let users: Vec<Value> = file
                .users()
                .keys()
                .filter_map(|name| user_json(&file, name))
                .collect();
            let grants: Vec<Value> = file.grants().iter().map(grant_json).collect();
            Response::json(200, &json!({ "users": users, "grants": grants }))
        }),
        (Method::Post, ["users"]) => {
            let body: AddBody = match body_of(request) {
                Ok(body) => body,
                Err(err) => return Some(Err(err)),
            };
            admin.user_add(&body.name, body.admin).and_then(|token| {
                let mut user = one_user(admin, &body.name)?;
                user["token"] = Value::String(token.expose().to_string());
                Ok(Response::json(201, &user))
            })
        }
        (_, ["users", name]) if read && !name.is_empty() => {
            one_user(admin, name).map(|user| Response::json(200, &user))
        }
        (Method::Delete, ["users", name]) if !name.is_empty() => {
            admin.user_remove(name).map(|(grants, ended)| {
                Response::json(
                    200,
                    &json!({
                        "name": fold(name),
                        "removed": true,
                        "grantsRemoved": grants,
                        "mcpSessionsEnded": ended.mcp,
                    }),
                )
            })
        }
        (Method::Post, ["users", name, "token"]) if !name.is_empty() => {
            admin.user_token(name).and_then(|(token, ended)| {
                let mut user = one_user(admin, name)?;
                user["token"] = Value::String(token.expose().to_string());
                user["mcpSessionsEnded"] = json!(ended.mcp);
                Ok(Response::json(200, &user))
            })
        }
        (Method::Delete, ["users", name, "token"]) if !name.is_empty() => {
            admin.user_revoke_token(name).and_then(|ended| {
                let mut user = one_user(admin, name)?;
                user["mcpSessionsEnded"] = json!(ended.mcp);
                Ok(Response::json(200, &user))
            })
        }
        (Method::Put, ["users", name, "admin"]) if !name.is_empty() => {
            let body: AdminBody = match body_of(request) {
                Ok(body) => body,
                Err(err) => return Some(Err(err)),
            };
            admin
                .user_set_admin(name, body.admin)
                .and_then(|()| one_user(admin, name))
                .map(|user| Response::json(200, &user))
        }
        (Method::Put, ["users", name, "roles"]) if !name.is_empty() => {
            let body: RolesBody = match body_of(request) {
                Ok(body) => body,
                Err(err) => return Some(Err(err)),
            };
            admin
                .user_set_roles(name, body.roles)
                .and_then(|()| one_user(admin, name))
                .map(|user| Response::json(200, &user))
        }
        (_, ["grants"]) if read => admin.users().map(|file| {
            let grants: Vec<Value> = file.grants().iter().map(grant_json).collect();
            Response::json(200, &json!({ "grants": grants }))
        }),
        (Method::Post, ["grants"]) => {
            let body: GrantBody = match body_of(request) {
                Ok(body) => body,
                Err(err) => return Some(Err(err)),
            };
            admin
                .grant_set(&body.subject, &body.database, &body.level)
                .map(|grant| Response::json(200, &grant_json(&grant)))
        }
        (Method::Delete, ["grants", subject, database])
            if !subject.is_empty() && !database.is_empty() =>
        {
            admin.grant_revoke(subject, database).and_then(|revoked| {
                let subject = crate::ops::parse_subject(subject)?;
                let database = if *database == "*" {
                    database.to_string()
                } else {
                    fold(database)
                };
                Ok(Response::json(
                    200,
                    &json!({
                        "subject": subject.to_string(),
                        "database": database,
                        "revoked": revoked,
                    }),
                ))
            })
        }
        (_, ["users"])
        | (_, ["users", _])
        | (_, ["users", _, "token" | "admin" | "roles"])
        | (_, ["grants"])
        | (_, ["grants", _, _]) => return Some(not_allowed()),
        (_, ["users" | "grants", ..]) => {
            return Some(Err(HttpError::new(
                404,
                format!("unknown admin endpoint /api/admin/{}", segments.join("/")),
            )));
        }
        _ => return None,
    };
    Some(Ok(answer.unwrap_or_else(|err| {
        let help = help_of(&err);
        error_answer(&err, help)
    })))
}

/// The help of a refused user or grant change.
fn help_of(err: &ServerError) -> Option<String> {
    match err {
        ServerError::LastAdmin { .. } => Some(LAST_ADMIN_HELP.to_string()),
        ServerError::UserNotFound { .. } => {
            Some("GET /api/admin/users lists the users".to_string())
        }
        _ => None,
    }
}

/// The user `name` as JSON (module docs), `NotFound` when there is none.
fn one_user(admin: &Admin, name: &str) -> Result<Value, ServerError> {
    let file = admin.users()?;
    user_json(&file, name).ok_or_else(|| ServerError::UserNotFound { name: fold(name) })
}

/// The user `name` of `file` as JSON (module docs).
fn user_json(file: &AccessFile, name: &str) -> Option<Value> {
    let key = fold(name);
    let user = file.user(&key)?;
    let subject = Subject::User(key.clone());
    let grants: Vec<Value> = file
        .grants()
        .iter()
        .filter(|grant| grant.subject == subject)
        .map(|grant| json!({ "database": grant.database, "level": grant.level.as_str() }))
        .collect();
    Some(json!({
        "name": key,
        "roles": user.roles,
        "admin": user.roles.iter().any(|r| r == crate::access::ADMIN_ROLE),
        "hasToken": user.token.is_some(),
        "tokenLast4": user.token_last4,
        "grants": grants,
    }))
}

/// A grant as JSON (module docs).
fn grant_json(grant: &Grant) -> Value {
    json!({
        "subject": grant.subject.to_string(),
        "database": grant.database,
        "level": grant.level.as_str(),
    })
}

/// `POST users`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddBody {
    name: String,
    #[serde(default)]
    admin: bool,
}

/// `PUT users/<name>/admin`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdminBody {
    admin: bool,
}

/// `PUT users/<name>/roles`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RolesBody {
    roles: Vec<String>,
}

/// `POST grants`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantBody {
    subject: String,
    database: String,
    level: String,
}
