//! The API every principal may call (beside the admin API, which is for admins only):
//! `GET /api/databases`, the databases and aliases the request's principal may reach, for the
//! web UI's "my databases" page (and its login page: anonymous sees the databases its anonymous
//! grants open).
//!
//! The answer is `{"databases": [row]}`, sorted by name, a row being `{name, state, level,
//! readOnly, description, playground, kind, aliasOf?}`:
//! - `name`: the database's name, or the alias's (then `aliasOf` names its database);
//! - `kind`: `database` (a real database) or `alias` (with `aliasOf`, its current target);
//! - `state`: `unloaded`, `loading`, `loaded`, `needs-model` or `failed` (no reason: that is
//!   the admin's);
//! - `level`: the principal's effective level, `read` or `write` (the granted level capped by
//!   the database's mode: a read-only database or a base is `read`);
//! - `readOnly`: `level == "read"`;
//! - `description`: the database's free text (`""` when none);
//! - `playground`: the path of its playground from the server root, `/db/<name>/`.
//!
//! A database the principal has no grant on is never listed (it does not exist for the
//! principal); an alias is listed when the principal has a grant on its target or on the alias
//! itself (a grant on an alias reaches the alias's CURRENT target, through the alias only: the
//! target's own row stays hidden without a grant of its own). Anonymous without anonymous grants gets an empty list.

use graphersal_serve::Response;
use serde_json::{Value, json};

use crate::access::{Access, Level, Principal, database_read_only, effective_level};
use crate::loader::Loader;
use crate::registry::DatabaseEntry;

/// The path of the listing.
pub(crate) const DATABASES: &str = "/api/databases";

/// `GET /api/databases` for `principal` (module docs).
pub(crate) fn databases(loader: &Loader, access: &Access, principal: &Principal) -> Response {
    let registry = loader.registry();
    let mut rows: Vec<Value> = registry
        .list()
        .into_iter()
        .filter_map(|entry| {
            let granted = access.granted_level(principal, &entry.name);
            (granted != Level::None).then(|| {
                let level = effective_level(granted, database_read_only(&entry));
                row(&entry.name, &entry, level, None)
            })
        })
        .collect();
    // An alias is listed when it reaches its target for `principal`: a grant on the target,
    // or one on the alias itself (which reaches the target only through the alias).
    for (alias, _) in registry.aliases() {
        if let Some((entry, granted)) = access.reach(registry, principal, &alias) {
            let level = effective_level(granted, database_read_only(&entry));
            rows.push(row(&alias, &entry, level, Some(&entry.name)));
        }
    }
    rows.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Response::json(200, &json!({ "databases": rows })).with_header("Cache-Control", "no-store")
}

/// One row (module docs): `name` is the database's name or an alias of it (`alias_of`: the
/// database).
fn row(name: &str, entry: &DatabaseEntry, level: Level, alias_of: Option<&str>) -> Value {
    let description = entry
        .config
        .as_ref()
        .map(|c| c.description.as_str())
        .unwrap_or_default();
    let mut row = json!({
        "name": name,
        "state": entry.state.as_str(),
        "level": level.as_str(),
        "readOnly": level == Level::Read,
        "description": description,
        "playground": format!("/db/{name}/"),
        "kind": if alias_of.is_some() { "alias" } else { "database" },
    });
    if let Some(target) = alias_of {
        row["aliasOf"] = Value::String(target.to_string());
    }
    row
}
