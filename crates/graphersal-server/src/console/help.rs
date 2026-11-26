//! The admin API's `help` texts as the console shows them: every admin route a help names
//! (`GET /api/admin/databases lists ...`) becomes the console command doing the same
//! (`graphersal-server db list --dir DIR lists ...`). The HTTP API keeps its texts; this is a
//! small mapping in the console layer only. A route without a console command stays as it is.
//!
//! Mapped routes: `GET databases`, `DELETE databases/<n>`, `POST databases` (a layer:
//! `{"name", "base"}`), `POST databases/<n>/load|unload`, `GET databases/<n>/backups`,
//! `POST databases/<n>/restore` (`{"backup", "name"?}`), `POST verify`, `POST
//! databases/<n>/verify|repair|swap` (`{"name"}`, `{"with"}`), `GET databases/<n>/export?...`, `GET|PUT databases/<n>/settings`, `PUT|DELETE aliases/<a>`, `GET models`, `POST models`
//! (`{"path"}` or `{"repo"}`), `POST import?name=&role=`, `GET imports`, `GET users`, `POST users`
//! (`{"name", "admin"}`), `POST|DELETE users/<n>/token`, `PUT users/<n>/admin` (`{"admin"}`).

use serde_json::Value;

use super::render::BIN;

/// The methods a help names a route with.
const METHODS: [&str; 4] = ["GET", "POST", "PUT", "DELETE"];
/// The admin API's prefix.
const PREFIX: &str = "/api/admin/";

/// `help` with every admin route it names replaced by its console command on the server
/// directory `dir_arg` (already a shell word).
pub(crate) fn console_help(help: &str, dir_arg: &str) -> String {
    let mut out = String::new();
    let mut rest = help;
    while let Some((start, method)) = next_route(rest) {
        out.push_str(&rest[..start]);
        let after = &rest[start + method.len() + 1..];
        let path_len = after
            .find(|c: char| c.is_whitespace() || c == ',' || c == ')' || c == ';')
            .unwrap_or(after.len());
        let path = &after[..path_len];
        let mut tail = &after[path_len..];
        let mut body = None;
        if let Some(json) = tail.strip_prefix(' ').filter(|t| t.starts_with('{'))
            && let Some(end) = json.find('}')
            && let Ok(value) = serde_json::from_str::<Value>(&json[..=end])
        {
            body = Some(value);
            tail = &json[end + 1..];
        }
        match command(method, path, body.as_ref(), dir_arg) {
            Some(command) => {
                out.push_str(&command);
                // A model is added by its repository or by a path: say it in console words.
                if let Some(after_or) = tail.strip_prefix(" (or a \"path\")") {
                    out.push_str(" (or a model directory or .gsnap)");
                    tail = after_or;
                }
            }
            None => out.push_str(&rest[start..rest.len() - tail.len()]),
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// The first `METHOD /api/admin/` in `text`: its byte offset and the method.
fn next_route(text: &str) -> Option<(usize, &'static str)> {
    METHODS
        .iter()
        .filter_map(|method| {
            text.find(&format!("{method} {PREFIX}"))
                .map(|at| (at, *method))
        })
        .min_by_key(|(at, _)| *at)
}

/// The console command of the route `method path` (with its JSON `body`), `None` when the
/// console has none.
fn command(method: &str, path: &str, body: Option<&Value>, dir_arg: &str) -> Option<String> {
    let path = path.strip_prefix(PREFIX)?;
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let param = |key: &str| {
        query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
    };
    let field = |key: &str| body.and_then(|b| b[key].as_str()).map(word);
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let db = |args: String| Some(format!("{BIN} db {args} --dir {dir_arg}"));
    let models = |args: String| Some(format!("{BIN} models {args} --dir {dir_arg}"));
    let user = |args: String| Some(format!("{BIN} user {args} --dir {dir_arg}"));
    let flag = |key: &str| body.and_then(|b| b[key].as_bool());
    match (method, segments.as_slice()) {
        ("GET", ["databases"]) => db("list".to_string()),
        ("POST", ["databases"]) => {
            let name = field("name").unwrap_or_else(|| "NAME".to_string());
            match field("base") {
                Some(base) => db(format!("create {name} --base {base}")),
                None => db(format!("create {name}")),
            }
        }
        ("DELETE", ["databases", name]) => db(format!("delete {}", word(name))),
        ("POST", ["databases", name, action @ ("load" | "unload")]) => {
            db(format!("{action} {}", word(name)))
        }
        ("GET", ["databases", name, "backups"]) => db(format!("backups {}", word(name))),
        ("POST", ["databases", name, "restore"]) => {
            let backup = field("backup").unwrap_or_else(|| "BACKUP".to_string());
            match field("name") {
                Some(new) => db(format!("restore {} {backup} --name {new}", word(name))),
                None => db(format!("restore {} {backup}", word(name))),
            }
        }
        ("POST", ["verify"]) => db("verify --all".to_string()),
        ("POST", ["databases", name, "verify"]) => db(format!("verify {}", word(name))),
        ("POST", ["databases", name, "repair"]) => match field("name") {
            Some(new) => db(format!("repair {} --name {new}", word(name))),
            None => db(format!("repair {}", word(name))),
        },
        ("POST", ["databases", name, "swap"]) => {
            let with = field("with").unwrap_or_else(|| "REPAIRED".to_string());
            db(format!("swap {} {with} --yes", word(name)))
        }
        ("POST", ["databases", name, "reset-layer"]) => {
            db(format!("reset-layer {} --yes", word(name)))
        }
        ("GET", ["databases", name, "settings"]) => db(format!("info {}", word(name))),
        ("PUT", ["databases", name, "settings"]) => db(format!("set {} ...", word(name))),
        ("GET", ["databases", name, "export"]) => {
            let mut args = format!("export {} --out FILE", word(name));
            if param("models") == Some("false") {
                args.push_str(" --no-models");
            }
            if param("materialize") == Some("true") {
                args.push_str(" --materialize");
            }
            db(args)
        }
        ("PUT", ["aliases", alias]) => {
            let target = field("target").unwrap_or_else(|| "DATABASE".to_string());
            db(format!("alias set {} {target}", word(alias)))
        }
        ("DELETE", ["aliases", alias]) => db(format!("alias remove {}", word(alias))),
        ("GET", ["models"]) => models("list".to_string()),
        ("POST", ["models"]) => match (body.and_then(|b| b["repo"].as_str()), field("path")) {
            (Some(repo), _) => models(format!("add {}", word(repo))),
            (None, Some(path)) if !path.contains(' ') => models(format!("add {path}")),
            _ => models("add SOURCE".to_string()),
        },
        ("GET", ["users"]) => user("list".to_string()),
        ("POST", ["users"]) => {
            let name = field("name").unwrap_or_else(|| "NAME".to_string());
            let admin = if flag("admin") == Some(true) {
                " --admin"
            } else {
                ""
            };
            user(format!("add {name}{admin}"))
        }
        ("POST", ["users", name, "token"]) => user(format!("token {}", word(name))),
        ("DELETE", ["users", name, "token"]) => user(format!("token {} --revoke", word(name))),
        ("PUT", ["users", name, "admin"]) => {
            let on = if flag("admin") == Some(false) {
                "off"
            } else {
                "on"
            };
            user(format!("admin {} {on}", word(name)))
        }
        ("GET", ["imports"]) => Some(format!("{BIN} import {dir_arg} --list")),
        ("POST", ["import"]) => {
            let name = param("name")
                .map(word)
                .unwrap_or_else(|| "NAME".to_string());
            let mut command = format!("{BIN} import {dir_arg} FILE --name {name}");
            if let Some(role) = param("role") {
                command.push_str(&format!(" --role {}", word(role)));
            }
            Some(command)
        }
        _ => None,
    }
}

/// A value of a route as a command-line word: a placeholder `<new>` becomes `NEW`; anything
/// else stays (names follow the name rule, no quoting needed).
fn word(text: &str) -> String {
    match text.strip_prefix('<').and_then(|t| t.strip_suffix('>')) {
        Some(inner) => inner.to_uppercase(),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_inbox_listing_becomes_import_list() {
        assert_eq!(
            console_help("GET /api/admin/imports lists the import inbox", "srv"),
            "graphersal-server import srv --list lists the import inbox"
        );
    }

    /// Every help the admin API gives that names a route (`admin_api.rs`, `error_answer` and
    /// the `*_help` functions), as the console shows it.
    #[test]
    fn every_admin_route_in_a_help_becomes_its_console_command() {
        let cases = [
            (
                "GET /api/admin/databases lists the databases and aliases",
                "graphersal-server db list --dir srv lists the databases and aliases",
            ),
            (
                "GET /api/admin/models lists the shared models; add one: POST /api/admin/models \
                 {\"path\": \"<model directory or .gsnap>\"}",
                "graphersal-server models list --dir srv lists the shared models; add one: \
                 graphersal-server models add SOURCE --dir srv",
            ),
            (
                "GET /api/admin/databases/acme/backups lists its backups",
                "graphersal-server db backups acme --dir srv lists its backups",
            ),
            (
                "unload it first: POST /api/admin/databases/acme/unload",
                "unload it first: graphersal-server db unload acme --dir srv",
            ),
            (
                "unload it first: POST /api/admin/databases/acme/unload, then promote it again",
                "unload it first: graphersal-server db unload acme --dir srv, then promote it \
                 again",
            ),
            (
                "remove the alias first: DELETE /api/admin/aliases/cur, or point it at another \
                 database: PUT /api/admin/aliases/cur {\"target\": \"<database>\"}",
                "remove the alias first: graphersal-server db alias remove cur --dir srv, or \
                 point it at another database: graphersal-server db alias set cur DATABASE \
                 --dir srv",
            ),
            (
                "it is the base of the layers a, b: delete them first (DELETE \
                 /api/admin/databases/a, DELETE /api/admin/databases/b)",
                "it is the base of the layers a, b: delete them first (graphersal-server db \
                 delete a --dir srv, graphersal-server db delete b --dir srv)",
            ),
            (
                "choose another name: GET /api/admin/databases lists the taken names (databases \
                 and aliases share one namespace, case-insensitive)",
                "choose another name: graphersal-server db list --dir srv lists the taken names \
                 (databases and aliases share one namespace, case-insensitive)",
            ),
            (
                "the loaded layers l1 use it as their base: unload them first (POST \
                 /api/admin/databases/l1/unload)",
                "the loaded layers l1 use it as their base: unload them first \
                 (graphersal-server db unload l1 --dir srv)",
            ),
            (
                "export it materialized (GET /api/admin/databases/lay/export?materialize=true) \
                 and import the file as a base (POST /api/admin/import?name=<new>&role=base)",
                "export it materialized (graphersal-server db export lay --out FILE \
                 --materialize --dir srv) and import the file as a base (graphersal-server \
                 import srv FILE --name NEW --role base)",
            ),
            (
                "add the model first: POST /api/admin/models {\"repo\": \"acme/tiny\"} (or a \
                 \"path\")",
                "add the model first: graphersal-server models add acme/tiny --dir srv (or a \
                 model directory or .gsnap)",
            ),
            (
                "write into a layer over it instead: POST /api/admin/databases {\"name\": \
                 \"<new>\", \"base\": \"prod\"}",
                "write into a layer over it instead: graphersal-server db create NEW --base \
                 prod --dir srv",
            ),
            (
                "load it: POST /api/admin/databases/acme/load",
                "load it: graphersal-server db load acme --dir srv",
            ),
        ];
        for (http, console) in cases {
            assert_eq!(console_help(http, "srv"), console, "{http}");
        }
    }

    #[test]
    fn text_without_a_mapped_route_stays_as_it_is() {
        for help in [
            "the server's upload limit is [limits] upload_bytes in server.toml",
            "create it first (graphersal store create srv/databases/b/store)",
            "GET /api/admin/databases/acme answers its row",
            "see PATCH /api/admin/databases",
            "",
        ] {
            assert_eq!(console_help(help, "srv"), help);
        }
        // The directory is passed as a shell word already.
        assert_eq!(
            console_help("GET /api/admin/databases lists them", "'my srv'"),
            "graphersal-server db list --dir 'my srv' lists them"
        );
    }

    #[test]
    fn user_routes_in_a_help_become_user_commands() {
        let help = "make another admin first: POST /api/admin/users {\"name\": \"<name>\", \
                    \"admin\": true}, or PUT /api/admin/users/<user>/admin {\"admin\": true}; a \
                    lost token is replaced (not revoked) with POST /api/admin/users/<name>/token; \
                    GET /api/admin/users lists the users";
        assert_eq!(
            console_help(help, "srv"),
            "make another admin first: graphersal-server user add NAME --admin --dir srv, or \
             graphersal-server user admin USER on --dir srv; a lost token is replaced (not \
             revoked) with graphersal-server user token NAME --dir srv; graphersal-server user \
             list --dir srv lists the users"
        );
        assert_eq!(
            console_help("DELETE /api/admin/users/bob/token", "srv"),
            "graphersal-server user token bob --revoke --dir srv"
        );
    }
}
