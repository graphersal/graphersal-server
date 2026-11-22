//! The admin API over HTTP (`/api/admin/...`; the `Host`/`Origin` checks ran before).
//!
//! This version answers the listing only:
//! - `GET /api/admin/databases`: `{"databases": [entry...], "aliases": {alias: database},
//!   "problems": [text...]}`, the databases and the aliases sorted by name;
//! - `GET /api/admin/databases/<name>`: one entry (`<name>` may be an alias).
//!
//! An entry: `name`, `state` (`unloaded` | `loading` | `loaded` | `needs-model` | `failed`),
//! `kind` (`plain` | `layer`), `role` (`normal` | `base`), `readOnly` (a base is always
//! read-only), `autoLoad`, `description`, and when they apply `base` (a layer's base), `created`,
//! `reason` (`failed`: why; `needs-model`: the missing model), `model` (`needs-model`: the
//! model's repository), `aliasOf` (an alias row: the database it names; its other fields are
//! that database's). A database whose `database.toml` cannot be read has no `kind`, `role`,
//! `autoLoad` or `description`. The field names are those of the playground's landing view.

use graphersal_serve::{HttpError, Method, Response};
use serde_json::{Map, Value, json};

use crate::database::DatabaseRole;
use crate::names::fold;
use crate::registry::{DatabaseEntry, DbState, Registry};

/// Answers a request below `/api/admin/` (`endpoint` = the path after `/api/admin/`).
pub(crate) fn route(
    registry: &Registry,
    method: &Method,
    endpoint: &str,
) -> Result<Response, HttpError> {
    let read = matches!(method, Method::Get | Method::Head);
    let not_allowed = || {
        HttpError::new(
            405,
            format!("method {method} is not allowed on /api/admin/{endpoint}"),
        )
    };
    if endpoint == "databases" {
        return if read {
            Ok(Response::json(200, &list(registry)))
        } else {
            Err(not_allowed())
        };
    }
    if let Some(name) = endpoint.strip_prefix("databases/")
        && !name.is_empty()
        && !name.contains('/')
    {
        if !read {
            return Err(not_allowed());
        }
        return one(registry, name)
            .map(|entry| Response::json(200, &entry))
            .ok_or_else(|| HttpError::new(404, format!("no database or alias '{name}'")));
    }
    Err(HttpError::new(
        404,
        format!("unknown admin endpoint /api/admin/{endpoint}"),
    ))
}

/// `GET /api/admin/databases`.
fn list(registry: &Registry) -> Value {
    let entries = registry.list();
    let aliases = registry.aliases();
    let mut databases: Vec<Value> = entries.iter().map(|e| entry_json(e, None)).collect();
    for (alias, target) in &aliases {
        if let Some(entry) = entries.iter().find(|e| &e.name == target) {
            databases.push(entry_json(entry, Some(alias)));
        }
    }
    databases.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    json!({
        "databases": databases,
        "aliases": aliases,
        "problems": registry.problems(),
    })
}

/// `GET /api/admin/databases/<name>`: the database or alias `name`, `None` when unknown.
fn one(registry: &Registry, name: &str) -> Option<Value> {
    if let Ok(entry) = registry.get(name) {
        return Some(entry_json(&entry, None));
    }
    let alias = fold(name);
    registry.aliases().get(&alias)?;
    let entry = registry.resolve(&alias).ok()?;
    Some(entry_json(&entry, Some(&alias)))
}

/// One entry of the listing (module docs); `alias`: the row of that alias of `entry`.
fn entry_json(entry: &DatabaseEntry, alias: Option<&str>) -> Value {
    let mut out = Map::new();
    out.insert(
        "name".into(),
        Value::String(alias.unwrap_or(&entry.name).to_string()),
    );
    if alias.is_some() {
        out.insert("aliasOf".into(), Value::String(entry.name.clone()));
    }
    out.insert("state".into(), Value::String(entry.state.as_str().into()));
    match &entry.state {
        DbState::Failed { reason } => {
            out.insert("reason".into(), Value::String(reason.clone()));
        }
        DbState::NeedsModel { repo } => {
            out.insert(
                "reason".into(),
                Value::String(format!("the embedding model '{repo}' is missing")),
            );
            out.insert("model".into(), Value::String(repo.clone()));
        }
        DbState::Unloaded | DbState::Loading | DbState::Loaded => {}
    }
    if let Some(config) = &entry.config {
        out.insert("kind".into(), Value::String(config.kind.as_str().into()));
        out.insert("role".into(), Value::String(config.role.as_str().into()));
        out.insert(
            "readOnly".into(),
            Value::Bool(config.read_only || config.role == DatabaseRole::Base),
        );
        out.insert("autoLoad".into(), Value::Bool(config.auto_load));
        out.insert(
            "description".into(),
            Value::String(config.description.clone()),
        );
        if let Some(base) = &config.base {
            out.insert("base".into(), Value::String(base.clone()));
        }
        if let Some(created) = &config.created {
            out.insert("created".into(), Value::String(created.clone()));
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::tests::{add_db, server};
    use crate::test_support::TempDir;

    fn json_of(response: &Response) -> Value {
        serde_json::from_slice(response.body()).unwrap()
    }

    #[test]
    fn the_listing_shows_databases_aliases_and_states() {
        let temp = TempDir::new("admin-api-list");
        let dir = server(&temp);
        add_db(
            &dir,
            "acme",
            Some("description = \"Acme\"\nauto_load = true"),
            true,
        );
        add_db(&dir, "prod", Some("role = \"base\""), true);
        add_db(&dir, "l1", Some("kind = \"layer\"\nbase = \"prod\""), true);
        add_db(&dir, "broken", Some("bogus = 1"), true);
        let registry = Registry::scan(&dir).unwrap();
        registry.alias_set("current", "acme").unwrap();
        registry
            .set_state("l1", DbState::needs_model("org/m"))
            .unwrap();
        let listing = json_of(&route(&registry, &Method::Get, "databases").unwrap());
        let names: Vec<&str> = listing["databases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["acme", "broken", "current", "l1", "prod"]);
        let rows = &listing["databases"];
        assert_eq!(rows[0]["state"], "unloaded");
        assert_eq!(rows[0]["kind"], "plain");
        assert_eq!(rows[0]["role"], "normal");
        assert_eq!(rows[0]["readOnly"], false);
        assert_eq!(rows[0]["autoLoad"], true);
        assert_eq!(rows[0]["description"], "Acme");
        assert!(rows[0].get("aliasOf").is_none());
        assert_eq!(rows[1]["state"], "failed");
        assert!(rows[1]["reason"].as_str().unwrap().contains("bogus"));
        assert!(rows[1].get("kind").is_none());
        assert_eq!(rows[2]["aliasOf"], "acme");
        assert_eq!(rows[2]["description"], "Acme");
        assert_eq!(rows[3]["state"], "needs-model");
        assert_eq!(rows[3]["model"], "org/m");
        assert_eq!(rows[3]["base"], "prod");
        assert_eq!(rows[4]["role"], "base");
        assert_eq!(rows[4]["readOnly"], true, "a base is always read-only");
        assert_eq!(listing["aliases"], json!({"current": "acme"}));

        let one = json_of(&route(&registry, &Method::Get, "databases/ACME").unwrap());
        assert_eq!(one["name"], "acme");
        let alias = json_of(&route(&registry, &Method::Get, "databases/current").unwrap());
        assert_eq!(alias["aliasOf"], "acme");
        assert_eq!(
            route(&registry, &Method::Get, "databases/none")
                .err()
                .unwrap()
                .status(),
            404
        );
        assert_eq!(
            route(&registry, &Method::Post, "databases")
                .err()
                .unwrap()
                .status(),
            405
        );
        assert_eq!(
            route(&registry, &Method::Get, "nothing")
                .err()
                .unwrap()
                .status(),
            404
        );
    }
}
