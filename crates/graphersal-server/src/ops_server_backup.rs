//! The server backup's writer ([`Admin::server_backup`], [`Admin::server_backup_to_file`],
//! [`Admin::server_backup_to_tmp`]): part of [`Admin`] (module docs of [`crate::ops`]); the
//! format and the restore are [`crate::server_backup`]'s.
//!
//! Online the configuration files are copied under their writers' locks (the registry's read
//! lock, then the access file's, then the MCP settings'), so the archive is one consistent
//! state; offline the console holds the server lock. The databases' backups (with databases)
//! are taken afterwards, one by one, through the per-database backup path
//! ([`Admin::backup_to_tmp`]); one that fails is left out and named in the manifest.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use super::{Admin, TmpFile};
use crate::archive::{Written, ZipWriter};
use crate::audit::AuditEvent;
use crate::database::{DATABASE_FILE, utc_now};
use crate::error::ServerError;
use crate::layout::{ACCESS_FILE, ALIASES_FILE, CONFIG_FILE, MCP_SETTINGS_FILE};
use crate::registry::DatabaseEntry;
use crate::server_backup::{
    DB_BACKUP, DB_SIDECAR, EXCLUDED, Excluded, FORMAT, FORMAT_VERSION, MANIFEST_FILE, MODELS_FILE,
    Manifest, ManifestDatabase, ManifestFile, ServerBackedUp, dir_name, file_name,
};

/// The configuration files as one consistent state.
struct Snapshot {
    /// (archive name, bytes), in archive order.
    files: Vec<(String, Vec<u8>)>,
    /// Every database then.
    databases: Vec<DatabaseEntry>,
}

/// A writer counting the bytes it passes on.
struct Counting<W> {
    inner: W,
    bytes: u64,
}

impl<W: Write> Write for Counting<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.bytes += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// The bytes of `path`, `None` when it does not exist.
fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, ServerError> {
    match crate::layout::read_replaced_bytes(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(ServerError::io("read", path, err)),
    }
}

impl Admin {
    /// The configuration files under their locks (module docs).
    fn config_snapshot(&self) -> Result<Snapshot, ServerError> {
        let dir = self.registry.dir().clone();
        // Lock order: registry, then the access file, then the MCP settings (a leaf).
        let registry = self.registry.read();
        let _access = self.access.as_ref().map(|access| access.hold());
        let mcp = self.loader.as_ref().and_then(|l| l.mcp_control()).cloned();
        let _mcp = mcp.as_ref().map(|control| control.hold());
        let mut files = Vec::new();
        for (name, path) in [
            (CONFIG_FILE, dir.config_path()),
            (ALIASES_FILE, dir.aliases_path()),
            (ACCESS_FILE, dir.access_path()),
            (MCP_SETTINGS_FILE, dir.mcp_settings_path()),
        ] {
            if let Some(bytes) = read_optional(&path)? {
                files.push((name.to_string(), bytes));
            }
        }
        let databases: Vec<DatabaseEntry> = registry.entries.values().cloned().collect();
        for entry in &databases {
            if let Some(bytes) = read_optional(&entry.dir.join(DATABASE_FILE))? {
                files.push((format!("databases/{}/{DATABASE_FILE}", entry.name), bytes));
            }
        }
        Ok(Snapshot { files, databases })
    }

    /// Writes the server backup ([`crate::server_backup`]) into `out`: its manifest and size.
    /// `with_databases`: every database's data too (a fresh verified backup of each; one that
    /// fails is left out and named in the manifest) and the shared model blobs.
    ///
    /// `Err`: [`ServerError::Io`] (reading the directory, writing `out`); a database's backup
    /// never fails the whole backup.
    pub fn server_backup(
        &self,
        with_databases: bool,
        out: impl Write,
    ) -> Result<(Manifest, u64), ServerError> {
        let dir = self.registry.dir().clone();
        let snapshot = self.config_snapshot()?;
        let failed =
            |err: io::Error| ServerError::io("write", PathBuf::from("the server backup"), err);
        let mut zip = ZipWriter::new(Counting {
            inner: out,
            bytes: 0,
        });
        let mut written: Vec<Written> = Vec::new();
        for (name, bytes) in &snapshot.files {
            written.push(zip.add_bytes(name, bytes).map_err(failed)?);
        }
        // The models list (always), from the shared models' own manifests.
        let mut excluded = Vec::new();
        let models = match self.models_index() {
            Ok(index) => serde_json::json!({
                "models": index.models.iter().map(|m| serde_json::json!({
                    "repo": m.repo,
                    "identity": m.identity,
                    "license": m.license,
                    "blob": m.blob,
                    "bytes": m.bytes,
                    "databases": m.users,
                    "waiting": m.waiting,
                })).collect::<Vec<_>>(),
                "blobsIncluded": with_databases,
            }),
            Err(err) => {
                excluded.push(Excluded::new(
                    MODELS_FILE,
                    format!("the shared models could not be listed: {err}"),
                ));
                serde_json::json!({ "models": [], "error": err.to_string() })
            }
        };
        let text = serde_json::to_vec_pretty(&models).unwrap_or_default();
        written.push(zip.add_bytes(MODELS_FILE, &text).map_err(failed)?);
        // The databases.
        let mut databases = Vec::new();
        for entry in &snapshot.databases {
            let mut row = ManifestDatabase {
                name: entry.name.clone(),
                kind: entry
                    .config
                    .as_ref()
                    .map(|c| c.kind.as_str().to_string())
                    .unwrap_or_default(),
                role: entry
                    .config
                    .as_ref()
                    .map(|c| c.role.as_str().to_string())
                    .unwrap_or_default(),
                base: entry.config.as_ref().and_then(|c| c.base.clone()),
                state: entry.state.as_str().to_string(),
                included: false,
                reason: None,
            };
            if !with_databases {
                row.reason =
                    Some("a configuration-only backup (--with-databases includes it)".into());
                excluded.push(Excluded::new(
                    format!("databases/{}/store/", entry.name),
                    "the database's data and its catalog (saved queries, backup job \
                     definitions): only with --with-databases, as a verified backup",
                ));
                databases.push(row);
                continue;
            }
            match self.backup_to_tmp(entry) {
                Ok((tmp, sidecar)) => {
                    let base = format!("databases/{}", entry.name);
                    written.push(
                        zip.add_file(&format!("{base}/{DB_BACKUP}"), tmp.path())
                            .map_err(failed)?,
                    );
                    let text = toml::to_string(&sidecar).unwrap_or_default();
                    written.push(
                        zip.add_bytes(&format!("{base}/{DB_SIDECAR}"), text.as_bytes())
                            .map_err(failed)?,
                    );
                    row.included = true;
                }
                Err(err) => {
                    tracing::warn!(
                        target: "graphersal_server",
                        "server backup: database '{}' left out: {err}",
                        entry.name
                    );
                    row.reason = Some(format!("its backup failed: {err}"));
                }
            }
            databases.push(row);
        }
        // The shared model blobs (with databases).
        if with_databases {
            let blobs = crate::models::blob_files(&dir.model_blobs())?;
            for (hash, path, _) in blobs {
                let name = format!("models/blobs/{}/{hash}", &hash[..2]);
                match zip.add_file(&name, &path) {
                    Ok(done) => written.push(done),
                    // Removed meanwhile (`models remove`): not in the backup.
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                    Err(err) => return Err(failed(err)),
                }
            }
        } else if dir.model_blobs().is_dir() {
            excluded.push(Excluded::new(
                "models/blobs/",
                "the shared model blobs: only with --with-databases (models.json lists them)",
            ));
        }
        for (path, reason) in EXCLUDED {
            let rel = path.trim_end_matches('/');
            let mut full = dir.root().to_path_buf();
            for part in rel.split('/') {
                full = full.join(part);
            }
            if full.exists() {
                excluded.push(Excluded::new(*path, *reason));
            }
        }
        let manifest = Manifest {
            format: FORMAT.to_string(),
            format_version: FORMAT_VERSION,
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            created_at: utc_now(),
            source_dir: dir_name(&dir),
            with_databases,
            files: written
                .into_iter()
                .map(|w| ManifestFile {
                    path: w.name,
                    bytes: w.bytes,
                    sha256: w.sha256,
                })
                .collect(),
            databases,
            excluded,
        };
        let text = serde_json::to_vec_pretty(&manifest).unwrap_or_default();
        zip.add_bytes(MANIFEST_FILE, &text).map_err(failed)?;
        let counting = zip.finish().map_err(failed)?;
        Ok((manifest, counting.bytes))
    }

    /// [`Admin::server_backup`] into `path`: written as `<path>.part` (mode `0600`) and renamed
    /// when complete; audited.
    pub fn server_backup_to_file(
        &self,
        with_databases: bool,
        path: &Path,
    ) -> Result<ServerBackedUp, ServerError> {
        let mut part_name = path.file_name().unwrap_or_default().to_os_string();
        part_name.push(".part");
        let part = TmpFile::at(path.with_file_name(part_name));
        let made = self.write_backup_file(with_databases, part.path())?;
        fs::rename(part.path(), path).map_err(|err| ServerError::io("rename", part.path(), err))?;
        self.audit().record(AuditEvent::ServerBackedUp {
            with_databases,
            databases: made
                .manifest
                .databases
                .iter()
                .filter(|d| d.included)
                .count(),
            path: Some(path.to_path_buf()),
            bytes: made.bytes,
        });
        Ok(made)
    }

    /// [`Admin::server_backup`] into a new file in `tmp/` (an HTTP download serves it, then
    /// drops it); audited.
    pub(crate) fn server_backup_to_tmp(
        &self,
        with_databases: bool,
    ) -> Result<(TmpFile, ServerBackedUp), ServerError> {
        let tmp = TmpFile::new(&self.registry.dir().tmp(), "server-backup", "zip")?;
        let made = self.write_backup_file(with_databases, tmp.path())?;
        self.audit().record(AuditEvent::ServerBackedUp {
            with_databases,
            databases: made
                .manifest
                .databases
                .iter()
                .filter(|d| d.included)
                .count(),
            path: None,
            bytes: made.bytes,
        });
        Ok((tmp, made))
    }

    /// The backup written into the NEW file `path` (mode `0600`), flushed to the disk.
    fn write_backup_file(
        &self,
        with_databases: bool,
        path: &Path,
    ) -> Result<ServerBackedUp, ServerError> {
        if path.exists() {
            fs::remove_file(path).map_err(|err| ServerError::io("remove", path, err))?;
        }
        let file: File = crate::layout::create_new_file(path, true)
            .map_err(|err| ServerError::io("create", path, err))?;
        let mut writer = BufWriter::new(file);
        let (manifest, bytes) = self.server_backup(with_databases, &mut writer)?;
        let file = writer
            .into_inner()
            .map_err(|err| ServerError::io("write", path, err.into_error()))?;
        file.sync_all()
            .map_err(|err| ServerError::io("write", path, err))?;
        let file_name = file_name(&manifest.source_dir, &manifest.created_at);
        Ok(ServerBackedUp {
            manifest,
            bytes,
            file_name,
        })
    }
}
