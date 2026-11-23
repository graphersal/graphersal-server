//! The server's own browser UI, built into the binary: the `server` UI profile
//! (`crates/graphersal-server/ui/profile.json`, delivered as `uiProfile` of the root `GET
//! /api/info`) and its modules (the Databases, Backups, Import, Export and Models tabs over the
//! admin API), served by the router under [`FILES_PREFIX`] before the embedded playground. The
//! profile names its style sheet in `styles` (the playground links it once the profile is
//! resolved), so the root serves the embedded `index.html` unchanged. `ui/tests/` stays out: the
//! `node --test` tests of the pure modules.

use serde_json::Value;

/// The path the profile's files are served under (the profile's `files` is the same path
/// relative to the page at `/`).
pub(crate) const FILES_PREFIX: &str = "/server-ui/";

/// The profile object.
const PROFILE: &str = include_str!("../ui/profile.json");

/// The files under [`FILES_PREFIX`]: name and bytes.
const FILES: [(&str, &[u8]); 11] = [
    ("profile.json", PROFILE.as_bytes()),
    ("server-ui.css", include_bytes!("../ui/server-ui.css")),
    ("admin-api.js", include_bytes!("../ui/admin-api.js")),
    ("databases.js", include_bytes!("../ui/databases.js")),
    ("shared.js", include_bytes!("../ui/shared.js")),
    ("databases-tab.js", include_bytes!("../ui/databases-tab.js")),
    ("backups-tab.js", include_bytes!("../ui/backups-tab.js")),
    ("import-tab.js", include_bytes!("../ui/import-tab.js")),
    ("export-tab.js", include_bytes!("../ui/export-tab.js")),
    ("models-tab.js", include_bytes!("../ui/models-tab.js")),
    ("runs-tab.js", include_bytes!("../ui/runs-tab.js")),
];

/// The profile object (the engine's playground validates it), with this binary's version as
/// its `version`: the page's header shows graphersal-server's own version.
pub(crate) fn profile() -> Result<Value, String> {
    let mut profile: Value = serde_json::from_str(PROFILE)
        .map_err(|err| format!("the built-in UI profile is invalid: {err}"))?;
    if let Some(object) = profile.as_object_mut() {
        object.insert(
            "version".to_string(),
            Value::String(env!("CARGO_PKG_VERSION").to_string()),
        );
    }
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
        assert_eq!(tabs.len(), 6);
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
}
