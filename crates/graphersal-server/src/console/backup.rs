//! The console's server backup and restore (`graphersal-server backup`, `restore`;
//! [`crate::server_backup`]).
//!
//! `backup` on a stopped directory writes the archive itself (holding the server lock); on a
//! running server's directory it downloads `GET /api/admin/server-backup` with the run's
//! operator token (or a user's admin token, `--token`), so the server copies its configuration
//! under its own locks. Either way the archive is written as `<FILE>.part` (mode `0600`) and
//! renamed when complete. `restore` never goes through a server: it builds a new directory.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::render::shell_word;
use super::{Outcome, START_WAIT, Via, answer_of, bearer, server_info};
use crate::error::ServerError;
use crate::layout::ServerDir;
use crate::ops::Admin;
use crate::run_file::RunInfo;
use crate::server_backup::{RestoreReport, ServerBackedUp, dir_name, file_name};
use crate::text::{Note, bytes};

const BIN: &str = super::render::BIN;

/// The archive's default file name for `dir` now (`<dir name>-server-backup-<UTC time>.zip`, in
/// the current directory).
pub fn default_backup_file(dir: &ServerDir) -> PathBuf {
    PathBuf::from(file_name(&dir_name(dir), &crate::database::utc_now()))
}

/// The JSON answer of a server backup written by the console itself.
fn answer_json(made: &ServerBackedUp, out: &Path) -> Value {
    json!({
        "file": out.display().to_string(),
        "bytes": made.bytes,
        "withDatabases": made.manifest.with_databases,
        "databases": made.manifest.databases.iter().filter(|d| d.included).count(),
        "skipped": made.manifest.databases.iter()
            .filter(|d| !d.included && made.manifest.with_databases)
            .map(|d| json!({"name": d.name, "reason": d.reason}))
            .collect::<Vec<_>>(),
        "files": made.manifest.files.len(),
    })
}

/// `graphersal-server backup --dir DIR [--out FILE] [--with-databases]` as the directory's
/// operator (module docs).
///
/// `Err`: [`ServerError::InsecureDir`]; the errors of opening the directory; through a server
/// [`ServerError::Remote`] / [`ServerError::Connect`]; [`ServerError::Io`].
pub fn server_backup(
    dir: &ServerDir,
    out: &Path,
    with_databases: bool,
) -> Result<Outcome, ServerError> {
    crate::security::check(dir)?;
    let audit = crate::audit::Audit::default()
        .with_actor(Some(crate::access::OPERATOR.to_string()))
        .with_os_user(crate::security::os_user());
    match Admin::open_offline(dir) {
        Ok((admin, _lock)) => {
            let made = admin
                .with_audit(audit)
                .server_backup_to_file(with_databases, out)?;
            Ok(Outcome {
                body: answer_json(&made, out),
                via: Via::Directory,
                server_url: None,
            })
        }
        Err(ServerError::Locked { .. }) => download(server_info(dir)?, out, with_databases),
        Err(err) => Err(err),
    }
}

/// [`server_backup`] through the running server with the admin token `token` (a user's,
/// `--token`), at `address` or the one `dir`'s `server.run` names; no file of `dir` is read.
pub fn server_backup_with_token(
    dir: &ServerDir,
    out: &Path,
    with_databases: bool,
    token: &str,
    address: Option<SocketAddr>,
) -> Result<Outcome, ServerError> {
    let server = match address {
        Some(address) => RunInfo::new(address, format!("http://{address}/")),
        None => match RunInfo::read(dir) {
            Ok(Some(info)) => RunInfo::new(info.address, info.url),
            _ => {
                return Err(ServerError::Connect {
                    address: dir.run_path().display().to_string(),
                    message: "--token sends the backup request to a running server, and no \
                              server runs on this directory (no server.run); start it, or name \
                              its address with --server ADDRESS"
                        .to_string(),
                });
            }
        },
    };
    download(
        server.with_operator_token(crate::access::Secret::from_text(token)),
        out,
        with_databases,
    )
}

/// The download of `GET /api/admin/server-backup` into `<out>.part` (mode `0600`), renamed
/// into `out`.
fn download(server: RunInfo, out: &Path, with_databases: bool) -> Result<Outcome, ServerError> {
    let mut part_name = out.file_name().unwrap_or_default().to_os_string();
    part_name.push(".part");
    let part = out.with_file_name(part_name);
    let url = format!(
        "/api/admin/server-backup{}",
        if with_databases {
            "?withDatabases=1"
        } else {
            ""
        }
    );
    let deadline = Instant::now() + START_WAIT;
    let answer = loop {
        let _ = std::fs::remove_file(&part);
        let mut file = crate::layout::create_new_file(&part, true)
            .map_err(|err| ServerError::io("create", &part, err))?;
        let answer = crate::client::download(server.address, bearer(&server), &url, &mut file)
            .and_then(|answer| {
                file.sync_all()
                    .map_err(|err| ServerError::io("write", &part, err))?;
                Ok(answer)
            });
        match answer {
            Ok(answer) if answer.status == 503 && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            other => break other,
        }
    };
    let answer = match answer {
        Ok(answer) => answer,
        Err(err) => {
            let _ = std::fs::remove_file(&part);
            return Err(err);
        }
    };
    if answer.status != 200 {
        let _ = std::fs::remove_file(&part);
        answer_of(answer.status, answer.error)?;
        return Err(ServerError::Remote {
            status: answer.status,
            message: format!(
                "the server answered {} instead of the archive",
                answer.status
            ),
            help: None,
        });
    }
    std::fs::rename(&part, out).map_err(|err| {
        let _ = std::fs::remove_file(&part);
        ServerError::io("rename", &part, err)
    })?;
    let skipped: Vec<Value> = answer
        .header("X-Graphersal-Skipped")
        .unwrap_or_default()
        .split(',')
        .filter(|name| !name.is_empty())
        .map(|name| json!({"name": name, "reason": Value::Null}))
        .collect();
    let databases = answer
        .header("X-Graphersal-Databases")
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(0);
    let address = server.address;
    Ok(Outcome {
        body: json!({
            "file": out.display().to_string(),
            "bytes": answer.bytes,
            "withDatabases": with_databases,
            "databases": databases,
            "skipped": skipped,
        }),
        via: Via::Server(address),
        server_url: Some(server.url),
    })
}

/// The text of a server backup's answer (`answer`: [`server_backup`]'s JSON).
pub fn server_backup_text(answer: &Value) -> String {
    let file = answer["file"].as_str().unwrap_or_default();
    let with = answer["withDatabases"].as_bool().unwrap_or(false);
    let mut note = Note::new(format!("Server backup written to {file}"))
        .detail("size", bytes(answer["bytes"].as_u64().unwrap_or(0)));
    note = if with {
        note.detail(
            "databases",
            format!(
                "{} included (verified backups) with the shared model blobs",
                answer["databases"].as_u64().unwrap_or(0)
            ),
        )
    } else {
        note.detail(
            "databases",
            "settings only (database.toml); add --with-databases for their data",
        )
    };
    let skipped: Vec<String> = answer["skipped"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| match row["reason"].as_str() {
                    Some(reason) => format!("{}: {reason}", row["name"].as_str().unwrap_or("")),
                    None => row["name"].as_str().unwrap_or("").to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    if !skipped.is_empty() {
        note = note.detail("LEFT OUT", skipped.join("\n"));
    }
    note.detail(
        "secret",
        "the archive holds the users' token hashes: keep it like a password (mode 0600)",
    )
    .command(
        "restore it into a new directory (another machine included)",
        format!("{BIN} restore {} --dir NEWDIR", shell_word(Path::new(file))),
    )
    .text()
}

/// The JSON of a restore's report.
pub fn restore_json(report: &RestoreReport) -> Value {
    json!({
        "dir": report.dir.display().to_string(),
        "sourceDir": report.manifest.source_dir,
        "serverVersion": report.manifest.server_version,
        "createdAt": report.manifest.created_at,
        "withDatabases": report.manifest.with_databases,
        "files": report.files,
        "databases": report.databases,
        "skipped": report.skipped.iter()
            .map(|(name, reason)| json!({"name": name, "reason": reason}))
            .collect::<Vec<_>>(),
        "modelBlobs": report.model_blobs,
        "notes": report.notes,
    })
}

/// The text of a restore's report, with the next command.
pub fn restore_text(report: &RestoreReport) -> String {
    let dir = shell_word(&report.dir);
    let mut note = Note::new(format!(
        "Restored the server backup of '{}' into {}",
        report.manifest.source_dir,
        report.dir.display()
    ))
    .detail(
        "archive",
        format!(
            "written {} by graphersal-server {}",
            report.manifest.created_at, report.manifest.server_version
        ),
    )
    .detail("files", report.files.join("\n"));
    note = note.detail(
        "databases",
        if report.databases.is_empty() {
            "none restored".to_string()
        } else {
            report.databases.join(", ")
        },
    );
    if report.model_blobs > 0 {
        note = note.detail("model blobs", report.model_blobs.to_string());
    }
    if !report.skipped.is_empty() {
        note = note.detail(
            "SKIPPED",
            report
                .skipped
                .iter()
                .map(|(name, reason)| format!("{name}: {reason}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    if !report.notes.is_empty() {
        note = note.detail("notes", report.notes.join("\n"));
    }
    note.command("serve it", format!("{BIN} serve --dir {dir}"))
        .text()
}
