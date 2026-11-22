//! `graphersal-server`: the command line (EXPERIMENTAL).
//!
//! ```text
//! graphersal-server init DIR
//! graphersal-server serve --dir DIR [--listen ADDR]
//! graphersal-server db list|create|delete|clone|load|unload|promote|read-only|auto-load|alias|backup|backups|restore|export ... --dir DIR [--json]
//! graphersal-server import DIR SOURCE --name NAME [--base BASE] [--format F] [--description TEXT] [--role normal|base] [--json]
//! graphersal-server models list|add|remove ... --dir DIR [--json]
//! ```
//!
//! stdout carries only results, stderr diagnostics and the log. Exit codes: `0` ok, `1` the
//! command failed, `2` invalid invocation.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use graphersal_server::config::parse_listen;
use graphersal_server::console::{self, DbCommand, Via};
use graphersal_server::{
    CreateSpec, DatabaseKind, DatabaseRole, ExportSpec, ServeOptions, ServerDir, serve,
};

const EXIT_FAILED: u8 = 1;
const EXIT_USAGE: u8 = 2;

const USAGE: &str = "graphersal-server (EXPERIMENTAL): many Graphersal graph databases in one \
server directory, over HTTP. No access control: keep it on a loopback address.

Usage:
  graphersal-server init DIR
      Create a server directory (server.toml, databases/, models/blobs/, backups/, trash/,
      tmp/, logs/). DIR must be missing, empty, or a server directory (then only missing
      parts are created; server.toml is kept).
  graphersal-server serve --dir DIR [--listen ADDR]
      Serve the server directory DIR. ADDR (IP:PORT, port 0 = a free port) overrides
      server.toml's listen. The bound address is written to DIR/server.run while the server
      runs. Ctrl+C or SIGTERM stops it.
  graphersal-server db COMMAND ... --dir DIR [--json]
      Administer the databases of DIR. When a server runs on DIR the command goes through
      its admin API (address from DIR/server.run), otherwise it acts on DIR directly.
        db list
        db create NAME [--base BASE] [--kind plain|layer] [--role normal|base]
                  [--schema FILE.json] [--description TEXT] [--auto-load] [--read-only]
            A new empty database; --base makes a layer over a role=base database.
        db delete NAME           an unloaded database, moved to DIR/trash/
        db clone SOURCE NAME     a fork of SOURCE's latest state (it may be loaded)
        db load NAME [--no-wait] (running server only)
        db unload NAME           (running server only)
        db promote NAME
            The unloaded plain database NAME becomes a base (role=base, read-only): the
            frozen graph layers are made over (db create L --base NAME). One way: a base
            is frozen forever; nothing turns it back into an ordinary database.
        db read-only NAME on|off
        db auto-load NAME on|off
        db alias set ALIAS DATABASE
        db alias remove ALIAS
        db backup NAME           a verified ZIP backup into DIR/backups/NAME/<UTC time>.zip
                                 (it may be loaded)
        db backups NAME          the backups of NAME (also of a deleted database)
        db restore NAME BACKUP [--name NEW]
            Restores the backup BACKUP (a name from db backups) of NAME into NAME (it must
            be unloaded; the replaced database moves to DIR/trash/) or into the new
            database NEW. A layer's backup needs its base (checked by identity).
        db export NAME --out FILE [--no-models] [--materialize]
            A packed snapshot (.gsnap) of NAME's latest state: with its embedding models
            unless --no-models; a layer as a layer snapshot (loads only over its base, no
            models) unless --materialize (a standalone full snapshot).
      --json prints the admin API's JSON answer instead of text.
  graphersal-server import DIR SOURCE --name NAME [--base BASE] [--format FORMAT]
                    [--description TEXT] [--role normal|base] [--json]
      Import SOURCE into DIR as the new database NAME: a packed snapshot (.gsnap; a layer
      snapshot needs a role=base database with its base identity, --base names it), a Store
      directory or .gstore file (copied into a directory Store), or a GraphSON (.json,
      .jsonl, .graphson) or GraphML (.xml, .graphml) file. FORMAT (snapshot, store,
      graphson, graphml) overrides the detection. A snapshot naming an embedding model it
      does not carry becomes a needs-model database. --role base makes a base (read-only,
      frozen for good) of a plain source (not of a layer, nor of a source naming a model it
      does not carry). Through a running server the server
      reads SOURCE itself (same machine).
  graphersal-server models COMMAND ... --dir DIR [--json]
      The shared embedding models of DIR (one copy in DIR/models/blobs/, hard-linked into
      every database holding it; copied with a warning where the file system has no hard
      links). Through a running server like db.
        models list              every model, its size and the databases using it
        models add SOURCE [--repo REPO] [--license SPDX]
            SOURCE: a Model2Vec model directory or a .gsnap carrying models (--repo and
            --license for a directory that does not name them), or the repository of a model
            the shared models hold. Every needs-model database whose model is then there
            gets it and can be loaded.
        models remove REPO       refused while a database uses it
  graphersal-server --help | --version

Log level: RUST_LOG (default info).";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => usage_error("no command given"),
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("-V" | "--version") => {
            println!("graphersal-server {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("init") => init(&args[1..]),
        Some("serve") => serve_command(&args[1..]),
        Some("db") => db_command(&args[1..]),
        Some("import") => import_command(&args[1..]),
        Some("models") => models_command(&args[1..]),
        Some(other) => usage_error(&format!("unknown command {other:?}")),
    }
}

fn usage_error(message: &str) -> ExitCode {
    eprintln!("Error: {message}\n\n{USAGE}");
    ExitCode::from(EXIT_USAGE)
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

fn init(args: &[String]) -> ExitCode {
    let dir = match args {
        [dir] if !dir.starts_with('-') => PathBuf::from(dir),
        _ => return usage_error("init takes exactly one argument: the directory"),
    };
    match ServerDir::init(&dir) {
        Ok(report) if report.existed && report.created.is_empty() => {
            println!(
                "{} is already a server directory; nothing to do",
                dir.display()
            );
            ExitCode::SUCCESS
        }
        Ok(report) if report.existed => {
            println!(
                "{} is already a server directory; created the missing {}",
                dir.display(),
                names(&report.created)
            );
            ExitCode::SUCCESS
        }
        Ok(_) => {
            println!(
                "created server directory {}; start it with: graphersal-server serve --dir {}",
                dir.display(),
                dir.display()
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("Error: {err}");
            ExitCode::from(EXIT_FAILED)
        }
    }
}

/// The created paths for the init message.
fn names(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn serve_command(args: &[String]) -> ExitCode {
    let mut dir = None;
    let mut listen = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_string())),
            _ => (arg.as_str(), None),
        };
        let slot = match flag {
            "--dir" => &mut dir,
            "--listen" => &mut listen,
            _ => return usage_error(&format!("serve: unknown argument {arg:?}")),
        };
        let Some(value) = inline.or_else(|| rest.next().cloned()) else {
            return usage_error(&format!("serve: {flag} needs a value"));
        };
        if slot.replace(value).is_some() {
            return usage_error(&format!("serve: {flag} given twice"));
        }
    }
    let Some(dir) = dir else {
        return usage_error("serve needs --dir DIR");
    };
    let mut options = ServeOptions::new();
    if let Some(listen) = listen {
        match parse_listen(&listen) {
            Ok(addr) => options = options.with_listen(addr),
            Err(err) => return usage_error(&format!("--listen: {err}")),
        }
    }
    init_logging();

    // SIGINT/SIGTERM: the first stops the server cleanly, a second exits at once.
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    static SIGNALLED: AtomicBool = AtomicBool::new(false);
    if let Err(err) = ctrlc::set_handler(move || {
        if SIGNALLED.swap(true, Ordering::SeqCst) {
            std::process::exit(130);
        }
        let _ = stop_tx.send(());
    }) {
        eprintln!("Error: cannot install the signal handler: {err}");
        return ExitCode::from(EXIT_FAILED);
    }

    let running = match serve::start(&dir, options) {
        Ok(running) => running,
        Err(err) => {
            eprintln!("Error: {err}");
            return ExitCode::from(EXIT_FAILED);
        }
    };
    eprintln!(
        "graphersal-server {} (EXPERIMENTAL) serves {} at {} (Ctrl+C stops it)",
        env!("CARGO_PKG_VERSION"),
        dir,
        running.url()
    );
    // Wait for a signal (a closed channel cannot happen while the handler lives).
    let _ = stop_rx.recv();
    eprintln!("stopping...");
    match running.stop() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("Error: {err}");
            ExitCode::from(EXIT_FAILED)
        }
    }
}

/// `db ...`: parses the command, runs it (on the directory or through the running server) and
/// prints the answer.
fn db_command(args: &[String]) -> ExitCode {
    let mut positional: Vec<String> = Vec::new();
    let mut values: Vec<(String, String)> = Vec::new();
    let mut switches: Vec<String> = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if !arg.starts_with("--") {
            positional.push(arg.clone());
            continue;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag.to_string(), Some(value.to_string())),
            None => (arg.clone(), None),
        };
        match flag.as_str() {
            "--dir" | "--kind" | "--role" | "--base" | "--schema" | "--description" | "--out"
            | "--name" => {
                let Some(value) = inline.or_else(|| rest.next().cloned()) else {
                    return usage_error(&format!("db: {flag} needs a value"));
                };
                if values.iter().any(|(f, _)| *f == flag) {
                    return usage_error(&format!("db: {flag} given twice"));
                }
                values.push((flag, value));
            }
            "--json" | "--auto-load" | "--read-only" | "--no-wait" | "--no-models"
            | "--materialize"
                if inline.is_none() =>
            {
                switches.push(flag);
            }
            _ => return usage_error(&format!("db: unknown argument {arg:?}")),
        }
    }
    let value = |flag: &str| {
        values
            .iter()
            .find(|(f, _)| f == flag)
            .map(|(_, v)| v.clone())
    };
    let switch = |flag: &str| switches.iter().any(|s| s == flag);
    let Some(dir) = value("--dir") else {
        return usage_error("db needs --dir DIR");
    };
    let command = match parse_db(&positional, &value, &switch) {
        Ok(command) => command,
        Err(Problem::Usage(message)) => return usage_error(&format!("db: {message}")),
        Err(Problem::Failed(message)) => {
            eprintln!("Error: {message}");
            return ExitCode::from(EXIT_FAILED);
        }
    };
    let dir = match ServerDir::open(&dir) {
        Ok(dir) => dir,
        Err(err) => {
            eprintln!("Error: {err}");
            return ExitCode::from(EXIT_FAILED);
        }
    };
    print_outcome(&dir, &command, switch("--json"))
}

/// Runs `command` on `dir` and prints its answer (text, or JSON with `json`).
fn print_outcome(dir: &ServerDir, command: &DbCommand, json: bool) -> ExitCode {
    match console::run(dir, command) {
        Ok(outcome) => {
            if let Via::Server(address) = outcome.via {
                eprintln!(
                    "note: a server runs on {}; sent to its admin API at {address}",
                    dir.root().display()
                );
            }
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&outcome.body).unwrap_or_default()
                );
            } else {
                println!("{}", console::render(command, &outcome.body));
            }
            if console::failed(command, &outcome.body) {
                ExitCode::from(EXIT_FAILED)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(err) => {
            eprintln!("Error: {err}");
            if let graphersal_server::ServerError::Remote {
                help: Some(help), ..
            } = &err
            {
                eprintln!("help: {help}");
            }
            ExitCode::from(EXIT_FAILED)
        }
    }
}

/// `import DIR SOURCE --name NAME ...`.
fn import_command(args: &[String]) -> ExitCode {
    let mut positional: Vec<&String> = Vec::new();
    let mut values: Vec<(String, String)> = Vec::new();
    let mut json = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if !arg.starts_with("--") {
            positional.push(arg);
            continue;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag.to_string(), Some(value.to_string())),
            None => (arg.clone(), None),
        };
        match flag.as_str() {
            "--name" | "--base" | "--format" | "--description" | "--role" => {
                let Some(value) = inline.or_else(|| rest.next().cloned()) else {
                    return usage_error(&format!("import: {flag} needs a value"));
                };
                if values.iter().any(|(f, _)| *f == flag) {
                    return usage_error(&format!("import: {flag} given twice"));
                }
                values.push((flag, value));
            }
            "--json" if inline.is_none() => json = true,
            _ => return usage_error(&format!("import: unknown argument {arg:?}")),
        }
    }
    let value = |flag: &str| {
        values
            .iter()
            .find(|(f, _)| f == flag)
            .map(|(_, v)| v.clone())
    };
    let [dir, source] = positional.as_slice() else {
        return usage_error("import takes two arguments: the server directory and the source");
    };
    let Some(name) = value("--name") else {
        return usage_error("import needs --name NAME (the new database's name)");
    };
    let source = match std::path::absolute(source) {
        Ok(source) => source,
        Err(err) => {
            eprintln!("Error: cannot resolve {source}: {err}");
            return ExitCode::from(EXIT_FAILED);
        }
    };
    let role = match value("--role").as_deref() {
        None => DatabaseRole::Normal,
        Some(word) => match DatabaseRole::parse(word) {
            Some(role) => role,
            None => return usage_error(&format!("import: --role is normal or base, not {word:?}")),
        },
    };
    let command = DbCommand::Import {
        source,
        name,
        role,
        base: value("--base"),
        format: value("--format"),
        description: value("--description"),
    };
    let dir = match ServerDir::open(dir) {
        Ok(dir) => dir,
        Err(err) => {
            eprintln!("Error: {err}");
            return ExitCode::from(EXIT_FAILED);
        }
    };
    print_outcome(&dir, &command, json)
}

/// `models list|add|remove ... --dir DIR`.
fn models_command(args: &[String]) -> ExitCode {
    let mut positional: Vec<&String> = Vec::new();
    let mut values: Vec<(String, String)> = Vec::new();
    let mut json = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if !arg.starts_with("--") {
            positional.push(arg);
            continue;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag.to_string(), Some(value.to_string())),
            None => (arg.clone(), None),
        };
        match flag.as_str() {
            "--dir" | "--repo" | "--license" => {
                let Some(value) = inline.or_else(|| rest.next().cloned()) else {
                    return usage_error(&format!("models: {flag} needs a value"));
                };
                if values.iter().any(|(f, _)| *f == flag) {
                    return usage_error(&format!("models: {flag} given twice"));
                }
                values.push((flag, value));
            }
            "--json" if inline.is_none() => json = true,
            _ => return usage_error(&format!("models: unknown argument {arg:?}")),
        }
    }
    let value = |flag: &str| {
        values
            .iter()
            .find(|(f, _)| f == flag)
            .map(|(_, v)| v.clone())
    };
    let Some(dir) = value("--dir") else {
        return usage_error("models needs --dir DIR");
    };
    let words: Vec<&str> = positional.iter().map(|s| s.as_str()).collect();
    let add_flags = value("--repo").is_some() || value("--license").is_some();
    let command = match words.as_slice() {
        ["list"] if !add_flags => DbCommand::ModelsList,
        ["add", source] => {
            let path = std::path::Path::new(source);
            if path.exists() {
                match std::path::absolute(path) {
                    Ok(path) => DbCommand::ModelsAdd {
                        path: Some(path),
                        repo: value("--repo"),
                        license: value("--license"),
                    },
                    Err(err) => {
                        eprintln!("Error: cannot resolve {source}: {err}");
                        return ExitCode::from(EXIT_FAILED);
                    }
                }
            } else if add_flags {
                eprintln!("Error: {source} does not exist");
                return ExitCode::from(EXIT_FAILED);
            } else {
                // Not a path: the repository of a model the shared models hold.
                DbCommand::ModelsAdd {
                    path: None,
                    repo: Some(source.to_string()),
                    license: None,
                }
            }
        }
        ["remove", model] if !add_flags => DbCommand::ModelsRemove {
            model: model.to_string(),
        },
        [] => return usage_error("no models command given"),
        _ => {
            return usage_error(&format!(
                "unknown command or wrong arguments: models {} (list, add SOURCE [--repo R] \
                 [--license L], remove REPO)",
                words.join(" ")
            ));
        }
    };
    let dir = match ServerDir::open(&dir) {
        Ok(dir) => dir,
        Err(err) => {
            eprintln!("Error: {err}");
            return ExitCode::from(EXIT_FAILED);
        }
    };
    print_outcome(&dir, &command, json)
}

/// Why a `db` command line was refused.
enum Problem {
    /// An invalid invocation (exit code 2).
    Usage(String),
    /// A file named by it cannot be used (exit code 1).
    Failed(String),
}

/// The `db` command of the positional arguments and the flags.
fn parse_db(
    positional: &[String],
    value: &dyn Fn(&str) -> Option<String>,
    switch: &dyn Fn(&str) -> bool,
) -> Result<DbCommand, Problem> {
    let usage = |message: &str| Problem::Usage(message.to_string());
    let words: Vec<&str> = positional.iter().map(String::as_str).collect();
    let on_off = |word: &str| match word {
        "on" | "true" | "yes" => Ok(true),
        "off" | "false" | "no" => Ok(false),
        other => Err(Problem::Usage(format!("expected on or off, not {other:?}"))),
    };
    let only_create_flags = || {
        for flag in ["--kind", "--role", "--base", "--schema", "--description"] {
            if value(flag).is_some() {
                return Err(Problem::Usage(format!("{flag} belongs to db create")));
            }
        }
        for flag in ["--auto-load", "--read-only"] {
            if switch(flag) {
                return Err(Problem::Usage(format!("{flag} belongs to db create")));
            }
        }
        Ok(())
    };
    let command = match words.as_slice() {
        ["list"] => DbCommand::List,
        ["create", name] => {
            return create_command(name, value, switch);
        }
        ["delete", name] => DbCommand::Delete {
            name: name.to_string(),
        },
        ["clone", source, name] => DbCommand::Clone {
            source: source.to_string(),
            name: name.to_string(),
        },
        ["load", name] => DbCommand::Load {
            name: name.to_string(),
            wait: !switch("--no-wait"),
        },
        ["unload", name] => DbCommand::Unload {
            name: name.to_string(),
        },
        ["promote", name] => DbCommand::Promote {
            name: name.to_string(),
        },
        ["read-only", name, word] => DbCommand::ReadOnly {
            name: name.to_string(),
            on: on_off(word)?,
        },
        ["auto-load", name, word] => DbCommand::AutoLoad {
            name: name.to_string(),
            on: on_off(word)?,
        },
        ["alias", "set", alias, target] => DbCommand::AliasSet {
            alias: alias.to_string(),
            target: target.to_string(),
        },
        ["alias", "remove", alias] => DbCommand::AliasRemove {
            alias: alias.to_string(),
        },
        ["backup", name] => DbCommand::Backup {
            name: name.to_string(),
        },
        ["backups", name] => DbCommand::Backups {
            name: name.to_string(),
        },
        ["restore", name, backup] => DbCommand::Restore {
            name: name.to_string(),
            backup: backup.to_string(),
            target: value("--name"),
        },
        ["export", name] => {
            let Some(out) = value("--out") else {
                return Err(usage("db export needs --out FILE"));
            };
            DbCommand::Export {
                name: name.to_string(),
                spec: ExportSpec::new()
                    .with_models(!switch("--no-models"))
                    .with_materialize(switch("--materialize")),
                out: PathBuf::from(out),
            }
        }
        [] => return Err(usage("no db command given")),
        [other, ..] => {
            return Err(Problem::Usage(format!(
                "unknown command or wrong arguments: db {other} {}",
                words[1..].join(" ")
            )));
        }
    };
    only_create_flags()?;
    if switch("--no-wait") && !matches!(command, DbCommand::Load { .. }) {
        return Err(usage("--no-wait belongs to db load"));
    }
    if value("--name").is_some() && !matches!(command, DbCommand::Restore { .. }) {
        return Err(usage("--name belongs to db restore"));
    }
    let export = matches!(command, DbCommand::Export { .. });
    if !export && value("--out").is_some() {
        return Err(usage("--out belongs to db export"));
    }
    for flag in ["--no-models", "--materialize"] {
        if !export && switch(flag) {
            return Err(Problem::Usage(format!("{flag} belongs to db export")));
        }
    }
    Ok(command)
}

/// `db create NAME ...`.
fn create_command(
    name: &str,
    value: &dyn Fn(&str) -> Option<String>,
    switch: &dyn Fn(&str) -> bool,
) -> Result<DbCommand, Problem> {
    if switch("--no-wait") {
        return Err(Problem::Usage("--no-wait belongs to db load".to_string()));
    }
    for flag in ["--no-models", "--materialize"] {
        if switch(flag) {
            return Err(Problem::Usage(format!("{flag} belongs to db export")));
        }
    }
    for flag in ["--out", "--name"] {
        if value(flag).is_some() {
            return Err(Problem::Usage(format!(
                "{flag} is not an option of db create"
            )));
        }
    }
    let base = value("--base");
    let kind = match value("--kind").as_deref() {
        None if base.is_some() => DatabaseKind::Layer,
        None | Some("plain") => DatabaseKind::Plain,
        Some("layer") => DatabaseKind::Layer,
        Some(other) => {
            return Err(Problem::Usage(format!(
                "--kind is plain or layer, not {other:?}"
            )));
        }
    };
    let role = match value("--role").as_deref() {
        None | Some("normal") => DatabaseRole::Normal,
        Some("base") => DatabaseRole::Base,
        Some(other) => {
            return Err(Problem::Usage(format!(
                "--role is normal or base, not {other:?}"
            )));
        }
    };
    let mut spec = match (kind, base) {
        (DatabaseKind::Layer, Some(base)) => CreateSpec::layer(base),
        (DatabaseKind::Layer, None) => {
            return Err(Problem::Usage(
                "a layer needs --base BASE (a role=base database)".to_string(),
            ));
        }
        (_, Some(_)) => {
            return Err(Problem::Usage(
                "--base makes a layer: drop --kind plain".to_string(),
            ));
        }
        (_, None) => CreateSpec::plain(),
    }
    .with_role(role)
    .with_auto_load(switch("--auto-load"))
    .with_read_only(switch("--read-only"));
    if let Some(description) = value("--description") {
        spec = spec.with_description(description);
    }
    if let Some(file) = value("--schema") {
        let text = std::fs::read_to_string(&file)
            .map_err(|err| Problem::Failed(format!("cannot read {file}: {err}")))?;
        let schema: serde_json::Value = serde_json::from_str(&text)
            .map_err(|err| Problem::Failed(format!("{file} is not JSON: {err}")))?;
        spec = spec.with_schema(schema);
    }
    Ok(DbCommand::Create {
        name: name.to_string(),
        spec,
    })
}
