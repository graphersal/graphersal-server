//! The server's own browser UI, built into the binary: the `server` UI profile
//! (`crates/graphersal-server/ui/profile.json`, delivered as `uiProfile` of the root `GET
//! /api/info`) and its modules (the Databases, Backups, Import, Export, Models, Runs and Users
//! tabs over the admin API, the header with the principal and its logout), served by the router
//! under [`FILES_PREFIX`] before the embedded playground. The profile depends on who asks
//! ([`View`]): an admin gets `profile.json` as it is, a user without the admin role the "My
//! databases" tab only, anonymous the "Sign in" tab only (`ui/profile-views.json` replaces the
//! keys it names), so a page never shows a tab its principal cannot use. The
//! profile names its style sheet in `styles` (the playground links it once the profile is
//! resolved), so the root serves the embedded `index.html` unchanged. `ui/tests/` stays out: the
//! `node --test` tests of the pure modules.

use serde_json::Value;

/// The path the profile's files are served under (the profile's `files` is the same path
/// relative to the page at `/`).
pub(crate) const FILES_PREFIX: &str = "/server-ui/";

/// The profile object.
const PROFILE: &str = include_str!("../ui/profile.json");

/// The keys of the profile each non-admin [`View`] replaces (`user`, `anonymous`).
const VIEWS: &str = include_str!("../ui/profile-views.json");

/// The files under [`FILES_PREFIX`]: name and bytes.
const FILES: [(&str, &[u8]); 25] = [
    ("profile.json", PROFILE.as_bytes()),
    ("profile-views.json", VIEWS.as_bytes()),
    ("access-api.js", include_bytes!("../ui/access-api.js")),
    ("header.js", include_bytes!("../ui/header.js")),
    ("login-tab.js", include_bytes!("../ui/login-tab.js")),
    ("mine-tab.js", include_bytes!("../ui/mine-tab.js")),
    ("users-tab.js", include_bytes!("../ui/users-tab.js")),
    ("server-ui.css", include_bytes!("../ui/server-ui.css")),
    ("admin-api.js", include_bytes!("../ui/admin-api.js")),
    ("databases.js", include_bytes!("../ui/databases.js")),
    ("shared.js", include_bytes!("../ui/shared.js")),
    ("fresh.js", include_bytes!("../ui/fresh.js")),
    ("sort.js", include_bytes!("../ui/sort.js")),
    ("toasts.js", include_bytes!("../ui/toasts.js")),
    ("menu.js", include_bytes!("../ui/menu.js")),
    ("databases-tab.js", include_bytes!("../ui/databases-tab.js")),
    ("backups-tab.js", include_bytes!("../ui/backups-tab.js")),
    ("import-tab.js", include_bytes!("../ui/import-tab.js")),
    ("export-tab.js", include_bytes!("../ui/export-tab.js")),
    ("models-tab.js", include_bytes!("../ui/models-tab.js")),
    ("runs-tab.js", include_bytes!("../ui/runs-tab.js")),
    ("mcp-api.js", include_bytes!("../ui/mcp-api.js")),
    ("mcp-tab.js", include_bytes!("../ui/mcp-tab.js")),
    ("settings-api.js", include_bytes!("../ui/settings-api.js")),
    ("settings-tab.js", include_bytes!("../ui/settings-tab.js")),
];

/// Which profile a principal gets (module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum View {
    /// The admin role (or the operator): every management tab and Users.
    Admin,
    /// A logged-in principal without the admin role: "My databases".
    User,
    /// No credentials: "Sign in" (with the databases anonymous grants open).
    Anonymous,
}

impl View {
    /// The view of `principal`.
    pub(crate) fn of(principal: &crate::access::Principal) -> View {
        if principal.is_admin() {
            View::Admin
        } else if principal.is_anonymous() {
            View::Anonymous
        } else {
            View::User
        }
    }

    /// Its key in `ui/profile-views.json` (`None`: the profile as it is).
    fn key(self) -> Option<&'static str> {
        match self {
            View::Admin => None,
            View::User => Some("user"),
            View::Anonymous => Some("anonymous"),
        }
    }
}

/// The admin's profile object ([`profile_for`] of [`View::Admin`]).
#[cfg(test)]
pub(crate) fn profile() -> Result<Value, String> {
    profile_for(View::Admin)
}

/// The profile object of `view` (the engine's playground validates it), with this binary's
/// version as its `version`: the page's header shows graphersal-server's own version.
pub(crate) fn profile_for(view: View) -> Result<Value, String> {
    let mut profile: Value = serde_json::from_str(PROFILE)
        .map_err(|err| format!("the built-in UI profile is invalid: {err}"))?;
    let object = profile
        .as_object_mut()
        .ok_or_else(|| "the built-in UI profile is not an object".to_string())?;
    if let Some(key) = view.key() {
        let views: Value = serde_json::from_str(VIEWS)
            .map_err(|err| format!("the built-in UI profile views are invalid: {err}"))?;
        let replaced = views
            .get(key)
            .and_then(Value::as_object)
            .ok_or_else(|| format!("the built-in UI profile views have no '{key}'"))?;
        for (name, value) in replaced {
            object.insert(name.clone(), value.clone());
        }
    }
    object.insert(
        "version".to_string(),
        Value::String(env!("CARGO_PKG_VERSION").to_string()),
    );
    Ok(profile)
}

/// The bytes and content type of the profile file `name` (the path after [`FILES_PREFIX`]).
pub(crate) fn file(name: &str) -> Option<(&'static [u8], &'static str)> {
    FILES
        .iter()
        .find(|(file, _)| *file == name)
        .map(|(file, bytes)| (*bytes, content_type(file)))
}

/// The content type of a profile file by its extension.
fn content_type(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or_default() {
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_profile_names_its_files_and_every_module_is_built_in() {
        let profile = profile().unwrap();
        assert_eq!(profile["name"], "server");
        assert_eq!(profile["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(
            format!("/{}", profile["files"].as_str().unwrap()),
            FILES_PREFIX
        );
        let styles = profile["styles"].as_array().unwrap();
        assert_eq!(styles.len(), 1);
        for style in styles {
            let style = style.as_str().unwrap();
            assert_eq!(file(style).map(|f| f.1), Some("text/css; charset=utf-8"));
        }
        let tabs = profile["tabs"].as_array().unwrap();
        assert_eq!(tabs.len(), 9);
        for tab in tabs {
            let module = tab["module"].as_str().unwrap();
            assert!(file(module).is_some(), "{module} is not built in");
            assert!(tab.get("builtin").is_none(), "no workspace at the root");
        }
        // Every file in ui/ (but the tests) is built in.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ui");
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_file() {
                let name = entry.file_name().into_string().unwrap();
                assert!(file(&name).is_some(), "ui/{name} is not built in");
            }
        }
        assert_eq!(
            file("shared.js").unwrap().1,
            "text/javascript; charset=utf-8"
        );
        assert_eq!(file("server-ui.css").unwrap().1, "text/css; charset=utf-8");
        assert!(file("../x.js").is_none());
        assert!(file("tests/profile.test.mjs").is_none());
    }

    #[test]
    fn each_view_gets_its_own_tabs_and_the_same_header() {
        let ids = |view| -> Vec<String> {
            let profile = profile_for(view).unwrap();
            assert_eq!(profile["name"], "server");
            assert_eq!(profile["version"], env!("CARGO_PKG_VERSION"));
            assert_eq!(profile["header"]["module"], "header.js");
            let tabs = profile["tabs"].as_array().unwrap().clone();
            for tab in &tabs {
                let module = tab["module"].as_str().unwrap();
                assert!(file(module).is_some(), "{module} is not built in");
            }
            let start = profile["startTab"].as_str().unwrap();
            assert!(tabs.iter().any(|t| t["id"] == start), "{start}");
            tabs.iter()
                .map(|t| t["id"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(
            ids(View::Admin),
            [
                "databases",
                "backups",
                "import",
                "export",
                "models",
                "runs",
                "users",
                "mcp",
                "settings"
            ]
        );
        assert_eq!(ids(View::User), ["mine"]);
        assert_eq!(ids(View::Anonymous), ["login"]);
        use crate::access::Principal;
        assert_eq!(View::of(&Principal::operator()), View::Admin);
        assert_eq!(View::of(&Principal::anonymous()), View::Anonymous);
        assert_eq!(View::of(&Principal::user("bob", Vec::new())), View::User);
        let admin = Principal::user("alice", vec!["admin".to_string()]);
        assert_eq!(View::of(&admin), View::Admin);
    }
}
