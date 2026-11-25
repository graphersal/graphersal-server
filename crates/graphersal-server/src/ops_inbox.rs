//! The import inbox `imports/` of the server directory ([`Admin::inbox`],
//! [`Admin::inbox_source`]): files (and Store directories) an admin puts there by other means
//! (scp, a mounted volume) and imports by NAME, so neither the web UI nor a client has to name a
//! path on the server's machine. Part of [`Admin`] (module docs of [`crate::ops`]).
//!
//! The inbox is FLAT: an inbox name is one directory entry of `imports/`. A name that is empty,
//! starts with `.` (also `.` and `..`), or holds `/`, `\`, `:` or NUL is refused
//! ([`ServerError::InvalidInput`]); an entry that is a symbolic link resolving outside
//! `imports/` (or a link inside it to such a link) is refused ([`ServerError::Refused`]) and
//! left out of the listing. Hidden entries (a leading `.`, such as `.DS_Store` or a copy in
//! progress named `.x.part`) are not listed. `imports/` is created by `init`; an older server
//! directory gets it at its next `init` or at the first listing.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use graphersal::persist::Store;

use super::{Admin, ImportFormat};
use crate::database::utc_text;
use crate::error::ServerError;
use crate::layout::{IMPORTS_DIR, dir_bytes};

/// One entry of the import inbox ([`Admin::inbox`]).
///
/// `#[non_exhaustive]`: further details may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct InboxFile {
    /// Its name in `imports/` (what an import names).
    pub name: String,
    /// The format guessed from its name (a directory: a Store when it holds one); `None` when
    /// nothing fits (the import then detects it from the content, or refuses it).
    pub kind: Option<ImportFormat>,
    /// Whether it is a directory (a Store directory).
    pub directory: bool,
    /// Its size in bytes (a directory: every file below it).
    pub bytes: u64,
    /// Its last modification, RFC 3339 UTC (`None` when the file system does not tell).
    pub modified: Option<String>,
}

impl Admin {
    /// The entries of the import inbox `imports/`, sorted by name (module docs: hidden entries
    /// and links leading outside `imports/` are left out). A missing `imports/` (an older
    /// server directory) is created first.
    ///
    /// `Err`: [`ServerError::Io`] (`imports/` cannot be created or read).
    pub fn inbox(&self) -> Result<Vec<InboxFile>, ServerError> {
        let dir = self.registry.dir().imports();
        match fs::create_dir(&dir) {
            Ok(()) => {
                tracing::info!(target: "graphersal_server", "created {}", dir.display());
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(ServerError::io("create directory", &dir, err)),
        }
        let root = fs::canonicalize(&dir).map_err(|err| ServerError::io("read", &dir, err))?;
        let listing =
            fs::read_dir(&dir).map_err(|err| ServerError::io("read directory", &dir, err))?;
        let mut files = Vec::new();
        for item in listing {
            let item = item.map_err(|err| ServerError::io("read directory", &dir, err))?;
            let Some(name) = item.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if name_problem(&name).is_some() {
                continue;
            }
            let path = dir.join(&name);
            match inside(&root, &path) {
                Ok(true) => {}
                Ok(false) => {
                    tracing::warn!(
                        target: "graphersal_server",
                        "imports/{name} links outside {}: not listed, not importable",
                        dir.display()
                    );
                    continue;
                }
                // A dangling link or an entry removed meanwhile.
                Err(_) => continue,
            }
            let Ok(meta) = fs::metadata(&path) else {
                continue;
            };
            let directory = meta.is_dir();
            files.push(InboxFile {
                kind: guess_kind(&path, directory),
                directory,
                bytes: if directory {
                    dir_bytes(&path)
                } else {
                    meta.len()
                },
                modified: meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| utc_text(d.as_secs())),
                name,
            });
        }
        files.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(files)
    }

    /// The path of the inbox entry `name` for an import (module docs: one entry of `imports/`,
    /// links only within it; the answer is the resolved path, so a link changed afterwards does
    /// not lead elsewhere).
    ///
    /// `Err`: [`ServerError::InvalidInput`] (not a plain entry name: empty, a leading `.`, a
    /// separator); [`ServerError::SourceNotFound`] (no such entry);
    /// [`ServerError::Refused`] (a link leading outside `imports/`); [`ServerError::Io`].
    pub fn inbox_source(&self, name: &str) -> Result<PathBuf, ServerError> {
        if let Some(problem) = name_problem(name) {
            return Err(ServerError::InvalidInput {
                name: format!("{IMPORTS_DIR}/{name}"),
                message: format!(
                    "{problem}: an inbox name is one file or directory name in {IMPORTS_DIR}/ \
                     (GET /api/admin/imports lists them)"
                ),
            });
        }
        let dir = self.registry.dir().imports();
        let path = dir.join(name);
        if fs::symlink_metadata(&path).is_err() {
            return Err(ServerError::SourceNotFound { path });
        }
        let root = fs::canonicalize(&dir).map_err(|err| ServerError::io("read", &dir, err))?;
        let real = match fs::canonicalize(&path) {
            Ok(real) => real,
            // A dangling link.
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Err(ServerError::SourceNotFound { path });
            }
            Err(err) => return Err(ServerError::io("read", &path, err)),
        };
        if !real.starts_with(&root) || real == root {
            return Err(ServerError::Refused {
                name: format!("{IMPORTS_DIR}/{name}"),
                message: format!(
                    "it links to {}, outside {IMPORTS_DIR}/: only what lies in the inbox is \
                     imported by name (copy the file into {IMPORTS_DIR}/, or import it by its \
                     absolute path)",
                    real.display()
                ),
            });
        }
        Ok(real)
    }
}

/// Why `name` is not a plain inbox entry name, or `None`.
fn name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        Some("the name is empty")
    } else if name.starts_with('.') {
        Some("the name starts with '.'")
    } else if name.contains(['/', '\\', ':', '\0']) {
        Some("the name holds a path separator")
    } else {
        None
    }
}

/// Whether `path` resolves to an entry inside `root` (the canonical `imports/`).
fn inside(root: &Path, path: &Path) -> io::Result<bool> {
    let real = fs::canonicalize(path)?;
    Ok(real.starts_with(root) && real != root)
}

/// The format of an inbox entry guessed from its name (a directory: whether it holds a Store).
fn guess_kind(path: &Path, directory: bool) -> Option<ImportFormat> {
    if directory {
        return Store::is_store(path)
            .unwrap_or(false)
            .then_some(ImportFormat::Store);
    }
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("gsnap") => Some(ImportFormat::Snapshot),
        Some("gstore") => Some(ImportFormat::Store),
        Some("json" | "jsonl" | "graphson") => Some(ImportFormat::GraphSon),
        Some("xml" | "graphml") => Some(ImportFormat::GraphMl),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Registry;
    use crate::registry::tests::server;
    use crate::test_support::TempDir;

    fn offline(temp: &TempDir) -> Admin {
        Admin::offline(Registry::scan(&server(temp)).expect("scan"))
    }

    #[test]
    fn the_inbox_lists_its_entries_with_a_kind_guess() {
        let temp = TempDir::new("inbox-list");
        let admin = offline(&temp);
        let imports = admin.registry().dir().imports();
        // An older server directory: the first listing creates `imports/`.
        let _ = fs::remove_dir_all(&imports);
        assert!(admin.inbox().unwrap().is_empty());
        assert!(imports.is_dir());
        fs::write(imports.join("b.graphml"), "<graphml/>").unwrap();
        fs::write(imports.join("a.json"), "{}").unwrap();
        fs::write(imports.join("c.GSNAP"), "x").unwrap();
        fs::write(imports.join("notes.txt"), "hi").unwrap();
        fs::write(imports.join(".DS_Store"), "hidden").unwrap();
        fs::create_dir(imports.join("folder")).unwrap();
        fs::write(imports.join("folder").join("x"), "12345").unwrap();
        let files = admin.inbox().unwrap();
        let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            ["a.json", "b.graphml", "c.GSNAP", "folder", "notes.txt"]
        );
        let kinds: Vec<Option<ImportFormat>> = files.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            [
                Some(ImportFormat::GraphSon),
                Some(ImportFormat::GraphMl),
                Some(ImportFormat::Snapshot),
                None,
                None
            ]
        );
        assert_eq!(files[0].bytes, 2);
        assert!(files[3].directory);
        assert_eq!(files[3].bytes, 5);
        assert!(
            files[0]
                .modified
                .as_deref()
                .is_some_and(|m| m.ends_with('Z'))
        );
    }

    #[test]
    fn inbox_names_are_one_plain_entry() {
        let temp = TempDir::new("inbox-names");
        let admin = offline(&temp);
        let imports = admin.registry().dir().imports();
        fs::create_dir_all(&imports).unwrap();
        fs::write(imports.join("a.json"), "{}").unwrap();
        fs::write(temp.path().join("secret.json"), "{}").unwrap();
        let real = admin.inbox_source("a.json").unwrap();
        assert_eq!(real, fs::canonicalize(imports.join("a.json")).unwrap());
        for name in [
            "",
            ".",
            "..",
            "../secret.json",
            "../../etc/passwd",
            "sub/a.json",
            "..\\secret.json",
            "/etc/passwd",
            "C:secret.json",
            ".hidden",
            "a\0b",
        ] {
            let err = admin.inbox_source(name).unwrap_err();
            assert!(
                matches!(err, ServerError::InvalidInput { .. }),
                "{name:?}: {err}"
            );
        }
        assert!(matches!(
            admin.inbox_source("missing.json").unwrap_err(),
            ServerError::SourceNotFound { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn links_leading_outside_the_inbox_are_refused_and_not_listed() {
        use std::os::unix::fs::symlink;
        let temp = TempDir::new("inbox-links");
        let admin = offline(&temp);
        let imports = admin.registry().dir().imports();
        fs::create_dir_all(&imports).unwrap();
        let outside = temp.path().join("outside.json");
        fs::write(&outside, "{}").unwrap();
        fs::write(imports.join("inside.json"), "{}").unwrap();
        symlink(&outside, imports.join("escape.json")).unwrap();
        symlink(temp.path(), imports.join("up")).unwrap();
        symlink(imports.join("escape.json"), imports.join("chain.json")).unwrap();
        symlink(imports.join("inside.json"), imports.join("alias.json")).unwrap();
        symlink(temp.path().join("nothing"), imports.join("dangling.json")).unwrap();
        let names: Vec<String> = admin.inbox().unwrap().into_iter().map(|f| f.name).collect();
        assert_eq!(names, ["alias.json", "inside.json"]);
        for name in ["escape.json", "up", "chain.json"] {
            let err = admin.inbox_source(name).unwrap_err();
            assert!(matches!(err, ServerError::Refused { .. }), "{name}: {err}");
            assert!(err.to_string().contains("outside imports/"), "{err}");
        }
        assert!(matches!(
            admin.inbox_source("dangling.json").unwrap_err(),
            ServerError::SourceNotFound { .. }
        ));
        // A link within the inbox resolves to its target.
        assert_eq!(
            admin.inbox_source("alias.json").unwrap(),
            fs::canonicalize(imports.join("inside.json")).unwrap()
        );
    }
}
