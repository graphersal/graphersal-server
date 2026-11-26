//! `graphersal-server`: the command line (EXPERIMENTAL).
//!
//! ```text
//! graphersal-server init DIR
//! graphersal-server serve --dir DIR [--listen ADDR] [--max-connections N]
//!     [--header-timeout SECS] [--idle-timeout SECS]
//! graphersal-server start --dir DIR [serve options] [--timeout SECS]
//! graphersal-server stop --dir DIR [--timeout SECS]
//! graphersal-server status --dir DIR [--json]
//! graphersal-server db list|create|delete|clone|load|unload|promote|reset-layer|read-only|auto-load|alias|backup|backups|restore|verify|repair|swap|export ... --dir DIR [--json]
//! graphersal-server import DIR SOURCE --name NAME [--base BASE] [--format F] [--description TEXT] [--role normal|base] [--mcp|--no-mcp] [--json]
//! graphersal-server import DIR --inbox FILE --name NAME [...] | import DIR --list [--json]
//! graphersal-server models list|add|remove|download ... --dir DIR [--json]
//! graphersal-server user ... --dir DIR [--json]
//! graphersal-server mcp status|on|off | mcp expose-new on|off | mcp db NAME [read-only] on|off --dir DIR [--json]
//! graphersal-server backup --dir DIR [--out FILE] [--with-databases] [--json]
//! graphersal-server restore FILE --dir NEWDIR [--force-version] [--json]
//! graphersal-server openapi --dir DIR [--db NAME] [--out FILE] [--server URL]
//!     (db, import, models, user, mcp, config, backup, stop, status also:
//!      [--token TOKEN | --token-file PATH|- [--server ADDRESS]])
//! ```
//!
//! stdout carries only results, stderr diagnostics and the log. Exit codes: `0` ok, `1` the
//! command failed, `2` invalid invocation, `3` `status` of a stopped server, `141` stdout
//! closed before the result was written (`| head`; as the graphersal CLI). The text comes from [`console`] in the graphersal CLI's
//! layout; this file only writes it ([`out`], [`err`]: never `println!`, which panics when the
//! reader of the pipe is gone).

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use graphersal_server::config::parse_listen;
use graphersal_server::console::{self, DbCommand, DbSetting, Via};
use graphersal_server::{
    CreateSpec, DatabaseKind, DatabaseRole, ExportSpec, ServeOptions, ServerDir, StopHook, daemon,
    serve,
};

const EXIT_FAILED: u8 = 1;
const EXIT_USAGE: u8 = 2;
/// stdout was closed before the result was written (128 + SIGPIPE, as the graphersal CLI).
const EXIT_CLOSED_PIPE: u8 = 141;

/// Whether a write to stdout failed (its reader went away): later writes are dropped and the
/// exit code is [`EXIT_CLOSED_PIPE`].
static STDOUT_CLOSED: AtomicBool = AtomicBool::new(false);

/// Writes `text` and a newline to stdout (the result), flushed; a failure is remembered.
fn out(text: &str) {
    if STDOUT_CLOSED.load(Ordering::Relaxed) {
        return;
    }
    let mut stdout = std::io::stdout().lock();
    if writeln!(stdout, "{text}")
        .and_then(|()| stdout.flush())
        .is_err()
    {
        STDOUT_CLOSED.store(true, Ordering::Relaxed);
    }
}

/// Writes `text` and a newline to stderr (diagnostics), best effort.
fn err(text: &str) {
    let _ = writeln!(std::io::stderr().lock(), "{text}");
}

/// `code`, or [`EXIT_CLOSED_PIPE`] for a successful run whose stdout was closed.
fn exit(code: u8) -> ExitCode {
    if code == 0 && STDOUT_CLOSED.load(Ordering::Relaxed) {
        ExitCode::from(EXIT_CLOSED_PIPE)
    } else {
        ExitCode::from(code)
    }
}

const USAGE: &str = "graphersal-server (EXPERIMENTAL): many Graphersal graph databases in one
server directory, over HTTP. Users and grants in access.toml; tokens travel in clear text
over plain HTTP: keep it on a loopback address or behind a TLS proxy.

Usage:
  graphersal-server init DIR [--config FILE] [--set KEY=VALUE]...
      Create a server directory (server.toml, databases/, models/blobs/, backups/, trash/,
      tmp/). DIR must be missing, empty, or a server directory (then only missing
      parts are created; server.toml is kept). A directory without access.toml gets its
      first admin (user admin): its token is printed ONCE on stderr (the first serve of
      such a directory does the same). A new server has MCP switched off and offers no
      database to it (mcp.toml: enabled = false, expose_new = false; see mcp). DIR becomes private to its owner (mode 0700; the
      secrets access.toml, server.run, sessions/ owner only); on an existing directory
      init tightens the modes it finds too open. --config FILE takes server.toml from
      FILE (checked like config check; refused if invalid; keys it leaves out keep their
      defaults); each --set KEY=VALUE (as config set) is applied after it. Secrets are
      never in server.toml: the first admin's token is generated as always. Both only for
      a new directory.
  graphersal-server serve --dir DIR [--listen ADDR] [--max-connections N]
                         [--header-timeout SECS] [--idle-timeout SECS]
      Serve the server directory DIR (HTTP/1.1 and h2c). ADDR (IP:PORT, port 0 = a free
      port) overrides server.toml's listen; the other options override its [limits]
      max_connections (open connections served at once; one more is answered 503),
      header_timeout_secs and idle_timeout_secs. The bound address is written to DIR/server.run while the server
      runs. Ctrl+C, SIGTERM or `stop` stops it. Runs in the foreground (for systemd, launchd
      and other service managers).
  graphersal-server start --dir DIR [serve options] [--timeout SECS]
      serve in the background: a detached process (its own session on unix, no console
      window on Windows), stdout and stderr appended to DIR/logs/server.log. Waits until it
      is ready (GET /readyz; at most SECS, default 60), prints its start block (and, for a
      directory without access.toml, the first admin's token, once) and its address on
      stdout. Refused while a server runs on DIR; on failure the log's last lines.
  graphersal-server stop --dir DIR [--timeout SECS]
      Asks the server running on DIR to stop (POST /api/admin/shutdown with the operator
      token of DIR/server.run; SIGTERM when its API cannot be reached, unix) and waits until
      it stopped (at most SECS, default 60). No server: prints `not running` (exit 0) and
      removes a stale server.run.
  graphersal-server status --dir DIR [--json]
      running or stopped: pid, address, version, uptime, databases (loaded ones by name),
      memory, the log; a stale server.run is reported. Exit code 0 running, 3 stopped.
  graphersal-server db COMMAND ... --dir DIR [--json]
      Administer the databases of DIR. When a server runs on DIR the command goes through
      its admin API (address from DIR/server.run), otherwise it acts on DIR directly.
        db list
        db create NAME [--base BASE] [--kind plain|layer] [--role normal|base]
                  [--schema FILE.json] [--description TEXT] [--auto-load|--no-auto-load]
                  [--read-only|--no-read-only] [--mcp|--no-mcp]
            A new empty database; --base makes a layer over a role=base database.
            Without the auto-load/read-only flags it takes server.toml's [defaults]
            auto_load and read_only (so does an import).
            --mcp offers it to MCP (a new database or alias is hidden from MCP unless
            mcp expose-new is on; --no-mcp hides it then); also on clone, alias set,
            restore and import.
        db delete NAME           an unloaded database, moved to DIR/trash/
        db clone SOURCE NAME [--mcp|--no-mcp]
                                 a fork of SOURCE's latest state (it may be loaded)
        db load NAME [--no-wait] (running server only)
        db unload NAME           (running server only)
        db promote NAME
            The unloaded plain database NAME becomes a base (role=base, read-only): the
            frozen graph layers are made over (db create L --base NAME). One way: a base
            is frozen forever; nothing turns it back into an ordinary database.
        db reset-layer NAME --yes
            Discards every change of the layer database NAME over its base (the base and
            other layers are untouched): a new empty layer over the same base takes its
            place, the old layer moves to DIR/trash/ (recoverable by hand). Name, base,
            description, flags, query limits, grants, aliases, MCP settings and scheduled
            job definitions stay. A loaded layer is unloaded and loaded again.
        db read-only NAME on|off
        db auto-load NAME on|off
        db info NAME             everything about NAME (or the database an alias names):
                                 kind, role, state, sizes, query limits (its own overrides
                                 and the server's defaults), MCP, grants, backups and the
                                 Store's creation parameters
        db set NAME description TEXT
        db set NAME limit KEY VALUE | db set NAME limit KEY --clear
            A query limit override of NAME (database.toml [limits.query]; KEY: timeout_ms,
            max_operations, max_string_size, max_array_size, max_map_size, max_traversers,
            max_materialized_bytes, max_value_depth; 0 = no limit, for max_value_depth the
            default 128); --clear lets the server's default apply again. Applies at once
            to NAME's new runs (a run already going keeps its limits).
        db alias set ALIAS DATABASE [--mcp|--no-mcp]
        db alias remove ALIAS
        db alias rename OLD NEW  the alias OLD becomes NEW (same database); open MCP sessions
                                 that selected OLD move to NEW
        db backup NAME           a verified ZIP backup into DIR/backups/NAME/<UTC time>.zip
                                 (it may be loaded)
        db backups NAME          the backups of NAME (also of a deleted database)
        db restore NAME BACKUP [--name NEW] [--mcp|--no-mcp]
            Restores the backup BACKUP (a name from db backups) of NAME into NAME (it must
            be unloaded; the replaced database moves to DIR/trash/) or into the new
            database NEW (a restore into NAME keeps NAME's MCP settings). A layer's backup
            needs its base (checked by identity).
        db verify NAME | db verify --all
            The engine's scrub of NAME's Store (every checksum, snapshot and WAL record),
            or of every database's. A loaded database's open Store is verified (damage
            found turns it read-only: state maintenance); otherwise its files.
        db repair NAME [--name NEW]
            Repairs the damaged plain database NAME into the NEW database (default
            NAME-repaired; never in place, NAME is not changed): damaged parts rebuilt from
            older snapshots and the WAL, what is lost reported; the copy is verified. Then
            `db swap NAME NEW --yes`, or keep both. A layer or base: restore a backup.
        db swap NAME WITH --yes
            The database WITH (a repaired copy) takes the name NAME: NAME moves to
            DIR/trash/, WITH's name goes away; NAME's grants, aliases and settings stay.
            Both are unloaded first; NAME is loaded again when it was loaded.
        db export NAME --out FILE [--no-models] [--materialize]
            A packed snapshot (.gsnap) of NAME's latest state: with its embedding models
            unless --no-models; a layer as a layer snapshot (loads only over its base, no
            models) unless --materialize (a standalone full snapshot).
      --json prints the admin API's JSON answer instead of text.
  graphersal-server import DIR SOURCE --name NAME [--base BASE] [--format FORMAT]
                    [--description TEXT] [--role normal|base] [--mcp|--no-mcp] [--json]
      Import SOURCE into DIR as the new database NAME: a packed snapshot (.gsnap; a layer
      snapshot needs a role=base database with its base identity, --base names it), a Store
      directory or .gstore file (copied into a directory Store; a model blob it lacks is
      filled from the shared models, else it is refused), or a GraphSON (.json,
      .jsonl, .graphson) or GraphML (.xml, .graphml) file. FORMAT (snapshot, store,
      graphson, graphml) overrides the detection. An embedding model the source names but
      does not carry is taken from the shared models when they hold it; otherwise the
      database is needs-model until `models add`. --role base makes a base (read-only,
      frozen for good) of a plain source (not of a layer, nor of a source naming a model
      neither it nor the shared models carry). Through a running server the server reads
      SOURCE itself (same machine). --mcp offers the new database to MCP (see db create).
  graphersal-server import DIR --inbox FILE --name NAME [--base BASE] [--format FORMAT]
                    [--description TEXT] [--role normal|base] [--json]
      The same from the import inbox DIR/imports/: FILE is one file or Store directory
      name there (no path; a link leading outside the inbox is refused).
  graphersal-server import DIR --list [--json]
      The entries of the import inbox DIR/imports/ (size, kind guessed from the name).
  graphersal-server models COMMAND ... --dir DIR [--json]
      The shared embedding models of DIR (one copy in DIR/models/blobs/, hard-linked into
      every database holding it; copied with a warning where the file system has no hard
      links). Through a running server like db.
        models list              every model, its size and the databases using it
        models add SOURCE [--repo REPO] [--license SPDX]
            SOURCE: a Model2Vec model directory, a tar archive of one (.tar, or .tar.gz)
            or a .gsnap carrying models (--repo and --license for a directory that does not
            name them), or the repository of a model the shared models hold. Every needs-model database whose model is then there
            gets it and can be loaded.
        models remove REPO       refused while a database uses it
        models download REPO [--revision REV]
            Downloads the Model2Vec model REPO (owner/name; REV: branch, tag or commit,
            default main) from Hugging Face into the shared models, then resolves the
            needs-model databases waiting for it. Through a running server it runs there
            (one download at a time; the console waits for its end). The server's
            environment: GRAPHERSAL_HF_ENDPOINT (a mirror), HF_TOKEN (gated repositories).
        models download --cancel the running download of the server stops; nothing is kept
  graphersal-server user COMMAND ... --dir DIR [--json]
      The users and grants of DIR/access.toml. Through a running server like db (its
      sessions of a changed token end at once: browser and MCP).
        user list                every user: roles, its token's last 4 characters, grants
        user add NAME [--admin]  a new user; its token is printed ONCE (only a hash is kept)
        user token NAME [--revoke]
            A new token for NAME, printed once (the old one stops working); --revoke takes
            the token away without a new one.
        user remove NAME         the user and its grants
        user admin NAME on|off   give or take the admin role (every database, the admin API)
        user grant SUBJECT DATABASE read|write
            SUBJECT: a user name, or anonymous (everyone, also without a token); DATABASE: a
            database's name or '*' (every database).
        user revoke SUBJECT DATABASE
      The last admin (admin role and a token) cannot be removed, demoted or revoked.
  graphersal-server mcp COMMAND ... --dir DIR [--json]
      The MCP switches of DIR/mcp.toml (a running server follows at once).
        mcp status               on or off, each database's MCP settings, the usage
                                 statistics (open sessions, tool calls) of a running server
        mcp on | mcp off         MCP for the whole server (off: /mcp and /db/NAME/mcp
                                 answer 503; open sessions go on when it is on again);
                                 a new server starts with it off
        mcp expose-new on|off    whether a new database or alias (create, import, clone,
                                 a restore into a new name, repair, alias set) is offered
                                 to MCP when the command does not say --mcp or --no-mcp
                                 (default off; the names there keep their settings)
        mcp db NAME on|off       offer a database or alias to MCP, or hide it
        mcp db NAME read-only on|off
            MCP sessions only read NAME, whatever their grant.
  graphersal-server config COMMAND ... --dir DIR [--json] [--token TOKEN --server ADDR]
      The keys of DIR/server.toml (dotted: listen, limits.upload_bytes, limits.query.timeout_ms,
      defaults.auto_load, ...). Through a running server (like db) the keys that can apply
      at once do (upload limit, memory ceiling, session lifetimes, public scheme); query
      limits apply at each database's next load, [defaults] to new databases, the rest
      after a restart (listed as restart pending until then).
        config get [KEY]         every key (value, default, when it applies), or one
        config set KEY VALUE     one key (true/false, a number, a text; allowed_hosts:
                                 names separated by commas; memory_limit: bytes, auto, none)
        config unset KEY         back to its default
        config check [FILE]      check a server.toml (default DIR/server.toml) without a
                                 server: exit 0 valid, 1 with the parser's error
  graphersal-server backup --dir DIR [--out FILE] [--with-databases] [--json]
      One ZIP archive (mode 0600) of the server itself: server.toml, aliases.toml,
      access.toml, mcp.toml, every database's database.toml (its settings, not its
      data), the list of the shared models and manifest.json (versions, time, every
      file's SHA-256, what was left out and why). Never in it: sessions/, logs/,
      server.run/.lock/.open, recovery.json, tmp/, trash/, imports/, backups/.
      --with-databases adds a fresh verified backup of every database (its Store with
      its saved queries and backup jobs) and the shared model blobs: a full backup.
      FILE defaults to ./<dir name>-server-backup-<UTC time>.zip. Through a running
      server like db (it copies its files under its locks). The archive holds the
      users' token hashes: keep it like a password.
  graphersal-server restore FILE --dir NEWDIR [--force-version] [--json]
      Build the NEW server directory NEWDIR (missing or empty; never over an existing
      one) from the server backup FILE: every checksum and the format checked first,
      modes 0700/0600, the databases from their embedded backups (an archive without
      a database's data skips it, reported), then the configuration. Users keep their
      tokens; sessions are not restored. --force-version tries an archive of a newer
      graphersal-server. Then: graphersal-server serve --dir NEWDIR.
  graphersal-server openapi --dir DIR [--db NAME] [--out FILE] [--server URL]
      The OpenAPI 3.1 description of the server's own HTTP API (login, the caller's
      databases, the admin API), or with --db that of the database or alias NAME (its
      queries, schema, saved queries, ... below /db/NAME/), to generate HTTP clients
      from. The same documents a running server answers at /openapi.json and
      /db/NAME/openapi.json; built from DIR alone (no server needed, nothing written in
      DIR). Its servers URL: URL (http(s)://host:port; a bare host:port gets server.toml's
      [http] public_scheme), else server.toml's first [http] allowed_hosts name, else its
      listen address. FILE: written there, else printed on stdout.
  graphersal-server --help | --version

The OS user owning DIR is its operator: serve and the db, import, models, user, mcp and config commands
refuse a DIR (or access.toml, server.run, sessions/) owned by another user or open to group
or others, naming the chmod/chown that fixes it (unix; on Windows DIR's ACL is the boundary).
Other administrators send an admin token instead:
  ... --token TOKEN [--server ADDRESS]
  ... --token-file PATH [--server ADDRESS]
      db, import, models, user, mcp, config, backup, stop and status go to the running server with this user's admin token
      (--token-file: read from PATH, - for stdin; or the environment variable
      GRAPHERSAL_SERVER_TOKEN; a token on the command line shows in ps: a warning), at
      ADDRESS (IP:PORT) or the address in DIR/server.run, used only while its server holds
      DIR/server.lock and the file is the directory owner's; no file of DIR is written and
      its permissions are not checked. Refused when no server runs.

stdout carries only results, stderr diagnostics and the log (level: RUST_LOG, default
info). Exit codes: 0 ok, 1 the command failed, 2 invalid invocation, 3 status: not
running.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args = match token_args(args) {
        Ok(args) => args,
        Err(code) => return code,
    };
    match args.first().map(String::as_str) {
        None => usage_error("no command given"),
        Some("-h" | "--help" | "help") => {
            out(USAGE);
            exit(0)
        }
        Some("-V" | "--version") => {
            out(&format!("graphersal-server {}", env!("CARGO_PKG_VERSION")));
            exit(0)
        }
        Some("init") => init(&args[1..]),
        Some("serve") => serve_command(&args[1..]),
        Some("start") => start_command(&args[1..]),
        Some("stop") => stop_command(&args[1..]),
        Some("status") => status_command(&args[1..]),
        Some("db") => db_command(&args[1..]),
        Some("import") => import_command(&args[1..]),
        Some("models") => models_command(&args[1..]),
        Some("user") => user_command(&args[1..]),
        Some("mcp") => mcp_command(&args[1..]),
        Some("config") => config_command(&args[1..]),
        Some("backup") => backup_command(&args[1..]),
        Some("restore") => restore_command(&args[1..]),
        Some("openapi") => openapi_command(&args[1..]),
        Some(other) => usage_error(&format!("unknown command {other:?}")),
    }
}

fn usage_error(message: &str) -> ExitCode {
    err(&console::usage_error_text(message));
    ExitCode::from(EXIT_USAGE)
}

/// Prints the error `error` (and its help) on stderr: exit code 1.
fn failure(error: &graphersal_server::ServerError) -> ExitCode {
    err(&console::error_text(error));
    ExitCode::from(EXIT_FAILED)
}

/// Prints `Error: <message>` on stderr: exit code 1.
fn failure_text(message: &str) -> ExitCode {
    err(&format!("Error: {message}"));
    ExitCode::from(EXIT_FAILED)
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    // Colours only for a terminal: a log redirected into a file or a pipe stays plain text.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(std::io::stderr().is_terminal())
        .with_writer(std::io::stderr)
        .try_init();
}

fn init(args: &[String]) -> ExitCode {
    let mut dir: Option<PathBuf> = None;
    let mut config_file: Option<PathBuf> = None;
    let mut sets: Vec<(String, String)> = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if !arg.starts_with('-') {
            if dir.replace(PathBuf::from(arg)).is_some() {
                return usage_error("init takes exactly one directory");
            }
            continue;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_string())),
            _ => (arg.as_str(), None),
        };
        match flag {
            "--config" | "--set" => {
                let Some(value) = inline.or_else(|| rest.next().cloned()) else {
                    return usage_error(&format!("init: {flag} needs a value"));
                };
                if flag == "--config" {
                    if config_file.replace(PathBuf::from(value)).is_some() {
                        return usage_error("init: --config given twice");
                    }
                } else {
                    let Some((key, value)) = value.split_once('=') else {
                        return usage_error(&format!(
                            "init: --set takes KEY=VALUE (such as limits.upload_bytes=1073741824), \
                             not {value:?}"
                        ));
                    };
                    sets.push((key.trim().to_string(), value.to_string()));
                }
            }
            _ => return usage_error(&format!("init: unknown argument {arg:?}")),
        }
    }
    let Some(dir) = dir else {
        return usage_error("init takes exactly one argument: the directory");
    };
    let initialized = if config_file.is_none() && sets.is_empty() {
        ServerDir::init(&dir)
    } else {
        match init_config_text(config_file.as_deref(), &sets) {
            Ok(text) => ServerDir::init_configured(&dir, &text),
            Err(InitConfig::Usage(message)) => return usage_error(&message),
            Err(InitConfig::Failed(error)) => return failure(&error),
        }
    };
    match initialized {
        Ok(report) => {
            out(&console::init_text(&report, &dir));
            if let Some(first) = &report.first_admin {
                print_first_admin(first, &dir);
            }
            exit(0)
        }
        Err(error) => failure(&error),
    }
}

/// Why `init --config`/`--set` was refused.
enum InitConfig {
    /// A `--set` naming an unknown key or a value of the wrong type (exit code 2).
    Usage(String),
    /// The file cannot be read, or the parser refuses it or a value (exit code 1).
    Failed(graphersal_server::ServerError),
}

/// The `server.toml` text of `init --config FILE --set KEY=VALUE...`: the file's text (else
/// the default text), checked with the server's parser, then each `--set` applied in order.
fn init_config_text(
    file: Option<&std::path::Path>,
    sets: &[(String, String)],
) -> Result<String, InitConfig> {
    use graphersal_server::server_settings;
    let (text, path) = match file {
        Some(path) => (
            server_settings::read_text(path).map_err(InitConfig::Failed)?,
            path.to_path_buf(),
        ),
        None => (
            graphersal_server::config::default_config_text(),
            PathBuf::from(graphersal_server::layout::CONFIG_FILE),
        ),
    };
    server_settings::check_text(&text, &path).map_err(InitConfig::Failed)?;
    let mut changes = Vec::new();
    for (key, value) in sets {
        let value = server_settings::toml_of_text(key, value)
            .map_err(|err| InitConfig::Usage(format!("init --set: {err}")))?;
        changes.push((key.clone(), Some(value)));
    }
    if changes.is_empty() {
        return Ok(text);
    }
    server_settings::edit(&text, &path, &changes)
        .map(|edited| edited.text)
        .map_err(InitConfig::Failed)
}

/// The first admin's token, ONCE, in a framed block on stderr (styled on a terminal).
fn print_first_admin(first: &graphersal_server::FirstAdmin, dir: &std::path::Path) {
    let styled = std::io::stderr().is_terminal();
    err(&format!(
        "\n{}\n",
        console::first_admin_text(first, dir, styled)
    ));
}

/// The `serve` options of a command line (`serve`, `start`).
struct ServeArgs {
    dir: String,
    options: ServeOptions,
    /// The options as `serve` takes them (without `--dir`), for the process `start` spawns.
    passthrough: Vec<String>,
    /// `--timeout SECS` (`start` only).
    timeout: Option<std::time::Duration>,
}

/// Parses the `serve` options of `command` (`serve` or `start`; `start` also takes
/// `--timeout SECS`); `Err`: the usage error's exit code.
fn parse_serve(command: &str, args: &[String]) -> Result<ServeArgs, ExitCode> {
    let mut dir = None;
    let mut listen = None;
    let mut max_connections = None;
    let mut header_timeout = None;
    let mut idle_timeout = None;
    let mut timeout = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_string())),
            _ => (arg.as_str(), None),
        };
        let slot = match flag {
            "--dir" => &mut dir,
            "--listen" => &mut listen,
            "--max-connections" => &mut max_connections,
            "--header-timeout" => &mut header_timeout,
            "--idle-timeout" => &mut idle_timeout,
            "--timeout" if command == "start" => &mut timeout,
            _ => return Err(usage_error(&format!("{command}: unknown argument {arg:?}"))),
        };
        let Some(value) = inline.or_else(|| rest.next().cloned()) else {
            return Err(usage_error(&format!("{command}: {flag} needs a value")));
        };
        if slot.replace(value).is_some() {
            return Err(usage_error(&format!("{command}: {flag} given twice")));
        }
    }
    let Some(dir) = dir else {
        return Err(usage_error(&format!("{command} needs --dir DIR")));
    };
    let mut options = ServeOptions::new();
    let mut passthrough = Vec::new();
    if let Some(listen) = listen {
        match parse_listen(&listen) {
            Ok(addr) => options = options.with_listen(addr),
            Err(err) => return Err(usage_error(&format!("--listen: {err}"))),
        }
        passthrough.extend(["--listen".to_string(), listen]);
    }
    // A whole number of at least 1.
    let positive = |flag: &str, value: &str| match value.trim().parse::<u64>() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err(usage_error(&format!(
            "{flag}: {value:?} is not a whole number of at least 1"
        ))),
    };
    if let Some(value) = max_connections {
        let n = positive("--max-connections", &value)?;
        options = options.with_max_connections(usize::try_from(n).unwrap_or(usize::MAX));
        passthrough.extend(["--max-connections".to_string(), n.to_string()]);
    }
    if let Some(value) = header_timeout {
        let n = positive("--header-timeout", &value)?;
        options = options.with_header_timeout_secs(n);
        passthrough.extend(["--header-timeout".to_string(), n.to_string()]);
    }
    if let Some(value) = idle_timeout {
        let n = positive("--idle-timeout", &value)?;
        options = options.with_idle_timeout_secs(n);
        passthrough.extend(["--idle-timeout".to_string(), n.to_string()]);
    }
    let timeout = match timeout {
        Some(value) => Some(std::time::Duration::from_secs(positive(
            "--timeout",
            &value,
        )?)),
        None => None,
    };
    Ok(ServeArgs {
        dir,
        options,
        passthrough,
        timeout,
    })
}

fn serve_command(args: &[String]) -> ExitCode {
    let ServeArgs { dir, options, .. } = match parse_serve("serve", args) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    init_logging();

    // SIGINT/SIGTERM: the first stops the server cleanly, a second exits at once. A shutdown
    // request of the admin API (`graphersal-server stop`) takes the same way.
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let hook_tx = stop_tx.clone();
    let options = options.with_stop_hook(StopHook::new(move || {
        let _ = hook_tx.send(());
    }));
    static SIGNALLED: AtomicBool = AtomicBool::new(false);
    if let Err(err) = ctrlc::set_handler(move || {
        if SIGNALLED.swap(true, Ordering::SeqCst) {
            std::process::exit(130);
        }
        let _ = stop_tx.send(());
    }) {
        return failure_text(&format!("cannot install the signal handler: {err}"));
    }

    let mut running = match serve::start(&dir, options) {
        Ok(running) => running,
        Err(error) => return failure(&error),
    };
    // The start block on stderr, the address alone on stdout (scripts read it).
    err(&console::start_block(&running));
    if let Some(first) = running.take_first_admin() {
        print_first_admin(&first, running.dir().root());
    }
    out(&console::address_line(&running));
    // Wait for a signal or a shutdown request (a closed channel cannot happen while the
    // handler lives).
    let _ = stop_rx.recv();
    err(&console::stopping_text());
    let root = running.dir().root().to_path_buf();
    match running.stop() {
        Ok(()) => {
            err(&console::stopped_text(&root));
            // A closed stdout does not fail a server that served.
            ExitCode::SUCCESS
        }
        Err(error) => failure(&error),
    }
}

/// The console's log only when RUST_LOG asks for it; stderr stays the result's diagnostics
/// else.
fn init_console_logging() {
    if std::env::var_os("RUST_LOG").is_some() {
        init_logging();
    }
}

/// `start --dir DIR [serve options] [--timeout SECS]`: `serve` in the background.
fn start_command(args: &[String]) -> ExitCode {
    let parsed = match parse_serve("start", args) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    init_console_logging();
    let program = match std::env::current_exe() {
        Ok(program) => program,
        Err(error) => return failure_text(&format!("cannot find this program's file: {error}")),
    };
    let dir = PathBuf::from(&parsed.dir);
    let timeout = parsed.timeout.unwrap_or(daemon::START_TIMEOUT);
    match daemon::start(&dir, &program, &parsed.passthrough, timeout) {
        Ok(started) => {
            let (block, address) = console::started_output(&started);
            if !block.is_empty() {
                err(&block);
            }
            if let Some(first) = &started.first_admin {
                print_first_admin(first, &dir);
            }
            err(&format!("\n{}", console::started_note(&started, &dir)));
            if let Some(address) = address {
                out(&address);
            }
            exit(0)
        }
        Err(error) => failure(&error),
    }
}

/// The values of `--dir`, `--token`, `--server`, `--timeout` and the switch `--json` of
/// `stop` and `status` (`allowed`: the flags this command takes).
struct ServiceArgs {
    dir: String,
    token: Option<String>,
    server: Option<String>,
    timeout: Option<String>,
    json: bool,
}

fn parse_service(
    command: &str,
    args: &[String],
    allowed: &[&str],
) -> Result<ServiceArgs, ExitCode> {
    let (mut dir, mut token, mut server, mut timeout) = (None, None, None, None);
    let mut json = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_string())),
            _ => (arg.as_str(), None),
        };
        if !allowed.contains(&flag) {
            return Err(usage_error(&format!("{command}: unknown argument {arg:?}")));
        }
        if flag == "--json" && inline.is_none() {
            json = true;
            continue;
        }
        let slot = match flag {
            "--dir" => &mut dir,
            "--token" => &mut token,
            "--server" => &mut server,
            "--timeout" => &mut timeout,
            _ => return Err(usage_error(&format!("{command}: unknown argument {arg:?}"))),
        };
        let Some(value) = inline.or_else(|| rest.next().cloned()) else {
            return Err(usage_error(&format!("{command}: {flag} needs a value")));
        };
        if slot.replace(value).is_some() {
            return Err(usage_error(&format!("{command}: {flag} given twice")));
        }
    }
    let Some(dir) = dir else {
        return Err(usage_error(&format!("{command} needs --dir DIR")));
    };
    Ok(ServiceArgs {
        dir,
        token,
        server,
        timeout,
        json,
    })
}

/// `stop --dir DIR [--timeout SECS] [--token TOKEN [--server ADDRESS]]`.
fn stop_command(args: &[String]) -> ExitCode {
    let parsed = match parse_service("stop", args, &["--dir", "--timeout", "--token", "--server"]) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    let timeout = match parsed
        .timeout
        .as_deref()
        .map(str::trim)
        .map(str::parse::<u64>)
    {
        None => daemon::STOP_TIMEOUT,
        Some(Ok(secs)) if secs > 0 => std::time::Duration::from_secs(secs),
        Some(_) => return usage_error("--timeout: not a whole number of at least 1"),
    };
    let remote = match Remote::of(parsed.token, parsed.server) {
        Ok(remote) => remote,
        Err(code) => return code,
    };
    init_console_logging();
    let dir = PathBuf::from(&parsed.dir);
    let result = match remote {
        Some(remote) => daemon::stop_with_token(&dir, &remote.token, remote.address, timeout),
        None => daemon::stop(&dir, timeout),
    };
    match result {
        Ok(stopped) => {
            out(&console::stop_text(&stopped, &dir));
            exit(0)
        }
        Err(error) => failure(&error),
    }
}

/// `status` of a stopped server (the LSB init script convention: 3 = not running).
const EXIT_STOPPED: u8 = 3;

/// `status --dir DIR [--json] [--token TOKEN [--server ADDRESS]]`: exit 0 running, 3 stopped.
fn status_command(args: &[String]) -> ExitCode {
    let parsed = match parse_service("status", args, &["--dir", "--json", "--token", "--server"]) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    let remote = match Remote::of(parsed.token, parsed.server) {
        Ok(remote) => remote,
        Err(code) => return code,
    };
    init_console_logging();
    let dir = PathBuf::from(&parsed.dir);
    let result = match remote {
        Some(remote) => daemon::status_with_token(&dir, &remote.token, remote.address),
        None => daemon::status(&dir),
    };
    match result {
        Ok(status) => {
            if parsed.json {
                out(
                    &serde_json::to_string_pretty(&console::status_json_of(&status, &dir))
                        .unwrap_or_default(),
                );
            } else {
                out(&console::status_text(&status, &dir));
            }
            if status.running {
                exit(0)
            } else {
                ExitCode::from(EXIT_STOPPED)
            }
        }
        Err(error) => failure(&error),
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
            | "--name" | "--token" | "--server" => {
                let Some(value) = inline.or_else(|| rest.next().cloned()) else {
                    return usage_error(&format!("db: {flag} needs a value"));
                };
                if values.iter().any(|(f, _)| *f == flag) {
                    return usage_error(&format!("db: {flag} given twice"));
                }
                values.push((flag, value));
            }
            "--json" | "--auto-load" | "--read-only" | "--no-auto-load" | "--no-read-only"
            | "--no-wait" | "--no-models" | "--materialize" | "--clear" | "--all" | "--yes"
            | "--mcp" | "--no-mcp"
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
        Err(Problem::Failed(message)) => return failure_text(&message),
    };
    let remote = match Remote::of(value("--token"), value("--server")) {
        Ok(remote) => remote,
        Err(code) => return code,
    };
    print_outcome(&dir, &command, switch("--json"), remote)
}

/// The environment variable holding an admin token for the console (as `--token`).
const TOKEN_ENV: &str = "GRAPHERSAL_SERVER_TOKEN";

/// The commands that take `--token` (and so `--token-file`).
const TOKEN_COMMANDS: &[&str] = &[
    "db", "import", "models", "user", "mcp", "config", "backup", "stop", "status",
];

/// `args` with `--token-file PATH` (`-`: standard input) replaced by `--token <its first
/// line>`, for the commands that take a token (SEC INFO: a token on the command line shows in
/// `ps`); a `--token` given on the command line itself gets a warning on stderr. `Err`: the
/// usage error's exit code (both given, an unreadable or empty file).
fn token_args(args: Vec<String>) -> Result<Vec<String>, ExitCode> {
    if !args
        .first()
        .is_some_and(|command| TOKEN_COMMANDS.contains(&command.as_str()))
    {
        return Ok(args);
    }
    let mut out = Vec::with_capacity(args.len());
    let mut file: Option<String> = None;
    let mut argv_token = false;
    let mut rest = args.into_iter();
    while let Some(arg) = rest.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => {
                (flag.to_string(), Some(value.to_string()))
            }
            _ => (arg.clone(), None),
        };
        match flag.as_str() {
            "--token-file" => {
                let Some(path) = inline.or_else(|| rest.next()) else {
                    return Err(usage_error("--token-file needs a path (or - for stdin)"));
                };
                if file.replace(path).is_some() {
                    return Err(usage_error("--token-file given twice"));
                }
            }
            "--token" => {
                argv_token = true;
                out.push(arg);
            }
            _ => out.push(arg),
        }
    }
    if let Some(path) = file {
        if argv_token {
            return Err(usage_error("give --token or --token-file, not both"));
        }
        out.push("--token".to_string());
        out.push(read_token_file(&path)?);
    } else if argv_token {
        err(&format!(
            "warning: a token on the command line is visible to other users of this machine \
             (ps); prefer --token-file PATH (- for stdin) or {TOKEN_ENV}"
        ));
    }
    Ok(out)
}

/// The token in the file `path` (`-`: standard input): its first non-empty line, trimmed.
fn read_token_file(path: &str) -> Result<String, ExitCode> {
    use std::io::Read;
    let mut text = String::new();
    let read = if path == "-" {
        std::io::stdin().read_to_string(&mut text).map(|_| ())
    } else {
        std::fs::read_to_string(path).map(|t| text = t)
    };
    if let Err(error) = read {
        return Err(usage_error(&format!("--token-file {path}: {error}")));
    }
    match text.lines().map(str::trim).find(|line| !line.is_empty()) {
        Some(token) => Ok(token.to_string()),
        None => Err(usage_error(&format!(
            "--token-file {path}: the file holds no token"
        ))),
    }
}

/// How a console command reaches the server directory: `None` = as the operator (offline on
/// the directory, or through its running server with the operator token of `server.run`),
/// else through the running server with a user's admin token (`--token`, or
/// `GRAPHERSAL_SERVER_TOKEN`) at `--server ADDRESS` or the address in `server.run`.
struct Remote {
    token: String,
    address: Option<std::net::SocketAddr>,
}

impl Remote {
    /// The remote of the `--token` and `--server` values (the token falls back to
    /// [`TOKEN_ENV`]); `Err`: the usage error's exit code.
    fn of(token: Option<String>, server: Option<String>) -> Result<Option<Remote>, ExitCode> {
        let token = token.or_else(|| std::env::var(TOKEN_ENV).ok());
        let token = match token {
            Some(token) if token.trim().is_empty() => {
                return Err(usage_error("--token needs a token (it is empty)"));
            }
            other => other,
        };
        let address = match server {
            None => None,
            Some(text) => {
                let trimmed = text
                    .trim()
                    .trim_start_matches("http://")
                    .trim_end_matches('/');
                match parse_listen(trimmed) {
                    Ok(address) => Some(address),
                    Err(err) => return Err(usage_error(&format!("--server: {err}"))),
                }
            }
        };
        match (token, address) {
            (Some(token), address) => Ok(Some(Remote {
                token: token.trim().to_string(),
                address,
            })),
            (None, Some(_)) => Err(usage_error(&format!(
                "--server needs --token TOKEN (or {TOKEN_ENV}): without a token the console \
                 acts as the directory's operator"
            ))),
            (None, None) => Ok(None),
        }
    }
}

/// Runs `command` on the server directory `dir` (or, with `remote`, through its running
/// server with a token) and prints its answer (text, or JSON with `json`).
fn print_outcome(dir: &str, command: &DbCommand, json: bool, remote: Option<Remote>) -> ExitCode {
    // The console's log only when RUST_LOG asks for it (the audit records of offline actions:
    // `RUST_LOG=graphersal_server::audit=info`); stderr stays the result's diagnostics else.
    if std::env::var_os("RUST_LOG").is_some() {
        init_logging();
    }
    let (dir, result) = match remote {
        Some(remote) => {
            // No file of the directory is opened: it may not even be readable to this user.
            let dir = ServerDir::new(dir);
            let result = console::run_with_token(&dir, command, &remote.token, remote.address);
            (dir, result)
        }
        None => {
            // The permission check before the open: a directory of another user is refused
            // for that, not for a file it cannot read.
            let unchecked = ServerDir::new(dir);
            if let Err(error) = graphersal_server::security::check(&unchecked) {
                return failure(&error);
            }
            let dir = match ServerDir::open(dir) {
                Ok(dir) => dir,
                Err(error) => return failure(&error),
            };
            let result = console::run(&dir, command);
            (dir, result)
        }
    };
    match result {
        Ok(outcome) => {
            if let Via::Server(address) = outcome.via {
                err(&format!(
                    "note: a server runs on {}; sent to its admin API at {address}",
                    dir.root().display()
                ));
            }
            if json {
                out(&serde_json::to_string_pretty(&outcome.body).unwrap_or_default());
            } else {
                out(&console::render_outcome(command, &outcome, dir.root()));
            }
            if console::failed(command, &outcome.body) {
                ExitCode::from(EXIT_FAILED)
            } else {
                exit(0)
            }
        }
        Err(error) => {
            err(&console::command_error_text(&error, dir.root()));
            ExitCode::from(EXIT_FAILED)
        }
    }
}

/// `import DIR SOURCE --name NAME ...`, `import DIR --inbox FILE --name NAME ...`,
/// `import DIR --list`.
fn import_command(args: &[String]) -> ExitCode {
    let mut positional: Vec<&String> = Vec::new();
    let mut values: Vec<(String, String)> = Vec::new();
    let mut json = false;
    let mut list = false;
    let mut mcp: Option<bool> = None;
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
            "--name" | "--base" | "--format" | "--description" | "--role" | "--inbox"
            | "--token" | "--server" => {
                let Some(value) = inline.or_else(|| rest.next().cloned()) else {
                    return usage_error(&format!("import: {flag} needs a value"));
                };
                if values.iter().any(|(f, _)| *f == flag) {
                    return usage_error(&format!("import: {flag} given twice"));
                }
                values.push((flag, value));
            }
            "--json" if inline.is_none() => json = true,
            "--list" if inline.is_none() => list = true,
            "--mcp" | "--no-mcp" if inline.is_none() => {
                if mcp.replace(flag == "--mcp").is_some() {
                    return usage_error("import: give --mcp or --no-mcp once");
                }
            }
            _ => return usage_error(&format!("import: unknown argument {arg:?}")),
        }
    }
    let value = |flag: &str| {
        values
            .iter()
            .find(|(f, _)| f == flag)
            .map(|(_, v)| v.clone())
    };
    let (dir, command) = if list {
        let [dir] = positional.as_slice() else {
            return usage_error("import --list takes one argument: the server directory");
        };
        if mcp.is_some()
            || values
                .iter()
                .any(|(flag, _)| flag != "--token" && flag != "--server")
        {
            return usage_error("import --list takes no other option than --json");
        }
        (dir, DbCommand::InboxList)
    } else {
        let role = match value("--role").as_deref() {
            None => DatabaseRole::Normal,
            Some(word) => match DatabaseRole::parse(word) {
                Some(role) => role,
                None => {
                    return usage_error(&format!("import: --role is normal or base, not {word:?}"));
                }
            },
        };
        let (dir, source) = match (positional.as_slice(), value("--inbox")) {
            ([dir], Some(_)) => (dir, None),
            ([_, _], Some(_)) => {
                return usage_error("import: give either SOURCE or --inbox FILE, not both");
            }
            ([dir, source], None) => (dir, Some(source)),
            _ => {
                return usage_error(
                    "import takes two arguments: the server directory and the source (or the \
                     server directory and --inbox FILE)",
                );
            }
        };
        let Some(name) = value("--name") else {
            return usage_error("import needs --name NAME (the new database's name)");
        };
        let command = match source {
            Some(source) => {
                let source = match console::source_path(std::path::Path::new(source)) {
                    Ok(source) => source,
                    Err(err) => return failure_text(&format!("cannot resolve {source}: {err}")),
                };
                DbCommand::Import {
                    source,
                    name,
                    role,
                    base: value("--base"),
                    format: value("--format"),
                    description: value("--description"),
                    mcp,
                }
            }
            None => DbCommand::ImportInbox {
                file: value("--inbox").unwrap_or_default(),
                name,
                role,
                base: value("--base"),
                format: value("--format"),
                description: value("--description"),
                mcp,
            },
        };
        (dir, command)
    };
    let remote = match Remote::of(value("--token"), value("--server")) {
        Ok(remote) => remote,
        Err(code) => return code,
    };
    print_outcome(dir, &command, json, remote)
}

/// `models list|add|remove|download ... --dir DIR`.
fn models_command(args: &[String]) -> ExitCode {
    let mut positional: Vec<&String> = Vec::new();
    let mut values: Vec<(String, String)> = Vec::new();
    let mut json = false;
    let mut cancel = false;
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
            "--dir" | "--repo" | "--license" | "--revision" | "--token" | "--server" => {
                let Some(value) = inline.or_else(|| rest.next().cloned()) else {
                    return usage_error(&format!("models: {flag} needs a value"));
                };
                if values.iter().any(|(f, _)| *f == flag) {
                    return usage_error(&format!("models: {flag} given twice"));
                }
                values.push((flag, value));
            }
            "--json" if inline.is_none() => json = true,
            "--cancel" if inline.is_none() => cancel = true,
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
    let revision = value("--revision");
    if (revision.is_some() || cancel) && words.first() != Some(&"download") {
        return usage_error("--revision and --cancel belong to models download");
    }
    let command = match words.as_slice() {
        ["download"] if cancel && !add_flags && revision.is_none() => {
            DbCommand::ModelsDownloadCancel
        }
        ["download", repo] if !cancel && !add_flags => DbCommand::ModelsDownload {
            repo: repo.to_string(),
            revision,
        },
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
                        return failure_text(&format!("cannot resolve {source}: {err}"));
                    }
                }
            } else if add_flags {
                return failure_text(&format!("{source} does not exist"));
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
                 [--license L], remove REPO, download REPO [--revision REV], download --cancel)",
                words.join(" ")
            ));
        }
    };
    let remote = match Remote::of(value("--token"), value("--server")) {
        Ok(remote) => remote,
        Err(code) => return code,
    };
    print_outcome(&dir, &command, json, remote)
}

/// `user COMMAND ... --dir DIR [--json]`.
fn user_command(args: &[String]) -> ExitCode {
    let mut positional: Vec<&str> = Vec::new();
    let mut dir: Option<String> = None;
    let mut token: Option<String> = None;
    let mut server: Option<String> = None;
    let (mut json, mut admin, mut revoke) = (false, false, false);
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if !arg.starts_with("--") {
            positional.push(arg);
            continue;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag, Some(value.to_string())),
            None => (arg.as_str(), None),
        };
        match flag {
            "--dir" | "--token" | "--server" => {
                let Some(value) = inline.or_else(|| rest.next().cloned()) else {
                    return usage_error(&format!("user: {flag} needs a value"));
                };
                let slot = match flag {
                    "--dir" => &mut dir,
                    "--token" => &mut token,
                    _ => &mut server,
                };
                if slot.replace(value).is_some() {
                    return usage_error(&format!("user: {flag} given twice"));
                }
            }
            "--json" if inline.is_none() => json = true,
            "--admin" if inline.is_none() => admin = true,
            "--revoke" if inline.is_none() => revoke = true,
            _ => return usage_error(&format!("user: unknown argument {arg:?}")),
        }
    }
    let Some(dir) = dir else {
        return usage_error("user needs --dir DIR");
    };
    let owned = |s: &str| s.to_string();
    let command = match positional.as_slice() {
        ["list"] if !admin && !revoke => DbCommand::UserList,
        ["add", name] if !revoke => DbCommand::UserAdd {
            name: owned(name),
            admin,
        },
        ["token", name] if !admin => DbCommand::UserToken {
            name: owned(name),
            revoke,
        },
        ["remove", name] if !admin && !revoke => DbCommand::UserRemove { name: owned(name) },
        ["admin", name, on @ ("on" | "off")] if !admin && !revoke => DbCommand::UserAdmin {
            name: owned(name),
            on: *on == "on",
        },
        ["grant", subject, database, level @ ("read" | "write")] if !admin && !revoke => {
            DbCommand::UserGrant {
                subject: owned(subject),
                database: owned(database),
                level: owned(level),
            }
        }
        ["revoke", subject, database] if !admin && !revoke => DbCommand::UserRevoke {
            subject: owned(subject),
            database: owned(database),
        },
        [] => return usage_error("no user command given"),
        _ => {
            return usage_error(&format!(
                "unknown command or wrong arguments: user {} (list, add NAME [--admin], token \
                 NAME [--revoke], remove NAME, admin NAME on|off, grant SUBJECT DATABASE \
                 read|write, revoke SUBJECT DATABASE)",
                positional.join(" ")
            ));
        }
    };
    let remote = match Remote::of(token, server) {
        Ok(remote) => remote,
        Err(code) => return code,
    };
    print_outcome(&dir, &command, json, remote)
}

/// `mcp status|on|off`, `mcp db NAME [read-only] on|off` with `--dir DIR [--json]`.
fn mcp_command(args: &[String]) -> ExitCode {
    let mut positional: Vec<&str> = Vec::new();
    let mut dir: Option<String> = None;
    let mut token: Option<String> = None;
    let mut server: Option<String> = None;
    let mut json = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if !arg.starts_with("--") {
            positional.push(arg);
            continue;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag, Some(value.to_string())),
            None => (arg.as_str(), None),
        };
        match flag {
            "--dir" | "--token" | "--server" => {
                let Some(value) = inline.or_else(|| rest.next().cloned()) else {
                    return usage_error(&format!("mcp: {flag} needs a value"));
                };
                let slot = match flag {
                    "--dir" => &mut dir,
                    "--token" => &mut token,
                    _ => &mut server,
                };
                if slot.replace(value).is_some() {
                    return usage_error(&format!("mcp: {flag} given twice"));
                }
            }
            "--json" if inline.is_none() => json = true,
            _ => return usage_error(&format!("mcp: unknown argument {arg:?}")),
        }
    }
    let Some(dir) = dir else {
        return usage_error("mcp needs --dir DIR");
    };
    let command = match positional.as_slice() {
        ["status"] => DbCommand::McpStatus,
        [on @ ("on" | "off")] => DbCommand::McpSwitch { on: *on == "on" },
        ["expose-new", on @ ("on" | "off")] => DbCommand::McpExposeNew { on: *on == "on" },
        ["db", name, on @ ("on" | "off")] => DbCommand::McpDatabase {
            name: name.to_string(),
            exposed: Some(*on == "on"),
            read_only: None,
        },
        ["db", name, "read-only", on @ ("on" | "off")] => DbCommand::McpDatabase {
            name: name.to_string(),
            exposed: None,
            read_only: Some(*on == "on"),
        },
        [] => return usage_error("no mcp command given"),
        _ => {
            return usage_error(&format!(
                "unknown command or wrong arguments: mcp {} (status, on, off, expose-new \
                 on|off, db NAME on|off, db NAME read-only on|off)",
                positional.join(" ")
            ));
        }
    };
    let remote = match Remote::of(token, server) {
        Ok(remote) => remote,
        Err(code) => return code,
    };
    print_outcome(&dir, &command, json, remote)
}

/// `config get [KEY] | set KEY VALUE | unset KEY | check [FILE]` (round 4 B-SETTINGS): the
/// keys of `server.toml`, offline on the directory or through its running server (which
/// applies the `now` keys at once); `check` only parses a file.
fn config_command(args: &[String]) -> ExitCode {
    let mut positional: Vec<&str> = Vec::new();
    let mut dir: Option<String> = None;
    let mut token: Option<String> = None;
    let mut server: Option<String> = None;
    let mut json = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if !arg.starts_with("--") {
            positional.push(arg);
            continue;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag, Some(value.to_string())),
            None => (arg.as_str(), None),
        };
        match flag {
            "--dir" | "--token" | "--server" => {
                let Some(value) = inline.or_else(|| rest.next().cloned()) else {
                    return usage_error(&format!("config: {flag} needs a value"));
                };
                let slot = match flag {
                    "--dir" => &mut dir,
                    "--token" => &mut token,
                    _ => &mut server,
                };
                if slot.replace(value).is_some() {
                    return usage_error(&format!("config: {flag} given twice"));
                }
            }
            "--json" if inline.is_none() => json = true,
            _ => return usage_error(&format!("config: unknown argument {arg:?}")),
        }
    }
    if let ["check", rest @ ..] = positional.as_slice() {
        let path = match (rest, &dir) {
            ([file], _) => PathBuf::from(file),
            ([], Some(dir)) => PathBuf::from(dir).join(graphersal_server::layout::CONFIG_FILE),
            ([], None) => return usage_error("config check needs FILE or --dir DIR"),
            _ => return usage_error("config check takes at most one FILE"),
        };
        return match console::config_check_file(&path) {
            Ok(_) => {
                out(&console::config_check_text(&path));
                exit(0)
            }
            Err(error) => failure(&error),
        };
    }
    let Some(dir) = dir else {
        return usage_error("config needs --dir DIR");
    };
    let text_value = |key: &str, text: &str| {
        graphersal_server::server_settings::toml_of_text(key, text)
            .map(|value| json_of_toml(&value))
            .map_err(|error| error.to_string())
    };
    let command = match positional.as_slice() {
        ["get"] => DbCommand::ConfigGet { key: None },
        ["get", key] => {
            if graphersal_server::server_settings::key_of(key).is_none() {
                return usage_error(&format!(
                    "config: unknown setting '{key}' (config get lists them)"
                ));
            }
            DbCommand::ConfigGet {
                key: Some(key.to_string()),
            }
        }
        ["set", key, value] => match text_value(key, value) {
            Ok(value) => DbCommand::ConfigSet {
                changes: vec![(key.to_string(), value)],
            },
            Err(message) => return usage_error(&format!("config set: {message}")),
        },
        ["unset", key] => {
            if graphersal_server::server_settings::key_of(key).is_none() {
                return usage_error(&format!(
                    "config: unknown setting '{key}' (config get lists them)"
                ));
            }
            DbCommand::ConfigSet {
                changes: vec![(key.to_string(), serde_json::Value::Null)],
            }
        }
        [] => return usage_error("no config command given"),
        _ => {
            return usage_error(&format!(
                "unknown command or wrong arguments: config {} (get [KEY], set KEY VALUE, unset \
                 KEY, check [FILE])",
                positional.join(" ")
            ));
        }
    };
    let remote = match Remote::of(token, server) {
        Ok(remote) => remote,
        Err(code) => return code,
    };
    print_outcome(&dir, &command, json, remote)
}

/// A TOML value of a `config set` as the JSON the admin API takes.
fn json_of_toml(value: &toml::Value) -> serde_json::Value {
    match value {
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Integer(n) => serde_json::Value::from(*n),
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(json_of_toml).collect())
        }
        other => serde_json::Value::String(other.to_string()),
    }
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
    // `--mcp` / `--no-mcp`: the MCP exposure of a new name (create, clone, alias set, restore).
    let mcp_flag = || match (switch("--mcp"), switch("--no-mcp")) {
        (true, true) => Err(Problem::Usage(
            "give either --mcp or --no-mcp, not both".to_string(),
        )),
        (true, false) => Ok(Some(true)),
        (false, true) => Ok(Some(false)),
        (false, false) => Ok(None),
    };
    let only_create_flags = || {
        for flag in ["--kind", "--role", "--base", "--schema", "--description"] {
            if value(flag).is_some() {
                return Err(Problem::Usage(format!("{flag} belongs to db create")));
            }
        }
        for flag in [
            "--auto-load",
            "--read-only",
            "--no-auto-load",
            "--no-read-only",
        ] {
            if switch(flag) {
                return Err(Problem::Usage(format!("{flag} belongs to db create")));
            }
        }
        Ok(())
    };
    let command = match words.as_slice() {
        ["list"] => DbCommand::List,
        ["create", name] => {
            let mcp = mcp_flag()?;
            return create_command(name, value, switch).map(|command| match command {
                DbCommand::Create { name, spec } => DbCommand::Create {
                    name,
                    spec: spec.with_mcp(mcp),
                },
                other => other,
            });
        }
        ["delete", name] => DbCommand::Delete {
            name: name.to_string(),
        },
        ["clone", source, name] => DbCommand::Clone {
            source: source.to_string(),
            name: name.to_string(),
            mcp: mcp_flag()?,
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
        ["info", name] => DbCommand::Info {
            name: name.to_string(),
        },
        ["set", name, "description", text] => DbCommand::Set {
            name: name.to_string(),
            setting: DbSetting::Description(text.to_string()),
        },
        ["set", name, "limit", key] if switch("--clear") => DbCommand::Set {
            name: name.to_string(),
            setting: DbSetting::limit(key, None).map_err(Problem::Usage)?,
        },
        ["set", name, "limit", key, value] if !switch("--clear") => DbCommand::Set {
            name: name.to_string(),
            setting: DbSetting::limit(key, Some(value)).map_err(Problem::Usage)?,
        },
        ["set", ..] => {
            return Err(usage(
                "db set takes NAME description TEXT, NAME limit KEY VALUE or NAME limit KEY \
                 --clear",
            ));
        }
        ["verify", name] => DbCommand::Verify {
            name: Some(name.to_string()),
        },
        ["verify"] if switch("--all") => DbCommand::Verify { name: None },
        ["verify", ..] => return Err(usage("db verify takes NAME or --all")),
        ["repair", name] => DbCommand::Repair {
            name: name.to_string(),
            target: value("--name"),
        },
        ["swap", name, with] => {
            if !switch("--yes") {
                return Err(usage(&format!(
                    "db swap moves the database '{name}' to trash/ and gives its name to                      '{with}': confirm it with --yes"
                )));
            }
            DbCommand::Swap {
                name: name.to_string(),
                with: with.to_string(),
            }
        }
        ["reset-layer", name] => {
            if !switch("--yes") {
                return Err(usage(&format!(
                    "db reset-layer discards every change of the layer '{name}' over its base \
                     (the old layer moves to trash/): confirm it with --yes"
                )));
            }
            DbCommand::ResetLayer {
                name: name.to_string(),
            }
        }
        ["alias", "set", alias, target] => DbCommand::AliasSet {
            alias: alias.to_string(),
            target: target.to_string(),
            mcp: mcp_flag()?,
        },
        ["alias", "remove", alias] => DbCommand::AliasRemove {
            alias: alias.to_string(),
        },
        ["alias", "rename", alias, name] => DbCommand::AliasRename {
            alias: alias.to_string(),
            name: name.to_string(),
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
            mcp: mcp_flag()?,
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
            return Err(Problem::Usage(
                format!(
                    "unknown command or wrong arguments: db {other} {}",
                    words[1..].join(" ")
                )
                .trim_end()
                .to_string(),
            ));
        }
    };
    only_create_flags()?;
    if (switch("--mcp") || switch("--no-mcp"))
        && !matches!(
            command,
            DbCommand::Clone { .. } | DbCommand::AliasSet { .. } | DbCommand::Restore { .. }
        )
    {
        return Err(usage(
            "--mcp and --no-mcp belong to db create, db clone, db alias set and db restore",
        ));
    }
    if switch("--no-wait") && !matches!(command, DbCommand::Load { .. }) {
        return Err(usage("--no-wait belongs to db load"));
    }
    if switch("--clear") && !matches!(command, DbCommand::Set { .. }) {
        return Err(usage("--clear belongs to db set NAME limit KEY"));
    }
    if value("--name").is_some()
        && !matches!(
            command,
            DbCommand::Restore { .. } | DbCommand::Repair { .. }
        )
    {
        return Err(usage("--name belongs to db restore and db repair"));
    }
    if switch("--all") && !matches!(command, DbCommand::Verify { name: None }) {
        return Err(usage("--all belongs to db verify"));
    }
    if switch("--yes")
        && !matches!(
            command,
            DbCommand::Swap { .. } | DbCommand::ResetLayer { .. }
        )
    {
        return Err(usage("--yes belongs to db swap and db reset-layer"));
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
    // `--auto-load` / `--no-auto-load` (and the read-only pair): given, or left to server.toml's
    // [defaults].
    let yes_no = |yes: &str, no: &str| match (switch(yes), switch(no)) {
        (true, true) => Err(Problem::Usage(format!(
            "give either {yes} or {no}, not both"
        ))),
        (true, false) => Ok(Some(true)),
        (false, true) => Ok(Some(false)),
        (false, false) => Ok(None),
    };
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
    .with_auto_load_opt(yes_no("--auto-load", "--no-auto-load")?)
    .with_read_only_opt(yes_no("--read-only", "--no-read-only")?);
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

/// The flags and positional arguments of `backup` and `restore`: (positional, values,
/// switches); `Err`: the usage error's exit code.
#[allow(clippy::type_complexity)]
fn parse_simple(
    command: &str,
    args: &[String],
    value_flags: &[&str],
    switch_flags: &[&str],
) -> Result<(Vec<String>, Vec<(String, String)>, Vec<String>), ExitCode> {
    let mut positional = Vec::new();
    let mut values: Vec<(String, String)> = Vec::new();
    let mut switches = Vec::new();
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
        if value_flags.contains(&flag.as_str()) {
            let Some(value) = inline.or_else(|| rest.next().cloned()) else {
                return Err(usage_error(&format!("{command}: {flag} needs a value")));
            };
            if values.iter().any(|(f, _)| *f == flag) {
                return Err(usage_error(&format!("{command}: {flag} given twice")));
            }
            values.push((flag, value));
        } else if switch_flags.contains(&flag.as_str()) && inline.is_none() {
            switches.push(flag);
        } else {
            return Err(usage_error(&format!("{command}: unknown argument {arg:?}")));
        }
    }
    Ok((positional, values, switches))
}

/// `backup --dir DIR [--out FILE] [--with-databases] [--json] [--token T [--server A]]`.
fn backup_command(args: &[String]) -> ExitCode {
    let (positional, values, switches) = match parse_simple(
        "backup",
        args,
        &["--dir", "--out", "--token", "--server"],
        &["--with-databases", "--json"],
    ) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    if let Some(extra) = positional.first() {
        return usage_error(&format!("backup: unexpected argument {extra:?}"));
    }
    let value = |flag: &str| {
        values
            .iter()
            .find(|(f, _)| f == flag)
            .map(|(_, v)| v.clone())
    };
    let Some(dir) = value("--dir") else {
        return usage_error("backup needs --dir DIR");
    };
    let with_databases = switches.iter().any(|s| s == "--with-databases");
    let json = switches.iter().any(|s| s == "--json");
    let remote = match Remote::of(value("--token"), value("--server")) {
        Ok(remote) => remote,
        Err(code) => return code,
    };
    init_console_logging();
    let unchecked = ServerDir::new(&dir);
    let file = value("--out")
        .map(PathBuf::from)
        .unwrap_or_else(|| console::default_backup_file(&unchecked));
    let result = match remote {
        Some(remote) => console::server_backup_with_token(
            &unchecked,
            &file,
            with_databases,
            &remote.token,
            remote.address,
        ),
        None => {
            if let Err(error) = graphersal_server::security::check(&unchecked) {
                return failure(&error);
            }
            match ServerDir::open(&dir) {
                Ok(dir) => console::server_backup(&dir, &file, with_databases),
                Err(error) => return failure(&error),
            }
        }
    };
    match result {
        Ok(outcome) => {
            if let Via::Server(address) = outcome.via {
                err(&format!(
                    "note: a server runs on {dir}; the backup came from its admin API at {address}"
                ));
            }
            if json {
                out(&serde_json::to_string_pretty(&outcome.body).unwrap_or_default());
            } else {
                out(&console::server_backup_text(&outcome.body));
            }
            exit(0)
        }
        Err(error) => failure(&error),
    }
}

/// `openapi --dir DIR [--db NAME] [--out FILE] [--server URL]`: the OpenAPI description of the
/// server, or of a database, built from DIR alone (`graphersal_server::openapi::offline_spec`).
fn openapi_command(args: &[String]) -> ExitCode {
    let (positional, values, _) = match parse_simple(
        "openapi",
        args,
        &["--dir", "--db", "--out", "--server"],
        &[],
    ) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    if let Some(extra) = positional.first() {
        return usage_error(&format!("openapi: unexpected argument {extra:?}"));
    }
    let value = |flag: &str| {
        values
            .iter()
            .find(|(f, _)| f == flag)
            .map(|(_, v)| v.clone())
    };
    let Some(dir) = value("--dir") else {
        return usage_error("openapi needs --dir DIR");
    };
    init_console_logging();
    let dir = match ServerDir::open(&dir) {
        Ok(dir) => dir,
        Err(error) => return failure(&error),
    };
    let spec = match graphersal_server::openapi::offline_spec(
        &dir,
        value("--db").as_deref(),
        value("--server").as_deref(),
    ) {
        Ok(spec) => spec,
        Err(error) => return failure(&error),
    };
    let text = match serde_json::to_string_pretty(&spec) {
        Ok(text) => text,
        Err(error) => return failure_text(&format!("cannot write the description: {error}")),
    };
    match value("--out") {
        Some(file) => match std::fs::write(&file, format!("{text}\n")) {
            Ok(()) => {
                out(&format!("wrote {file}"));
                exit(0)
            }
            Err(error) => failure_text(&format!("cannot write {file}: {error}")),
        },
        None => {
            out(&text);
            exit(0)
        }
    }
}

/// `restore FILE --dir NEWDIR [--force-version] [--json]`.
fn restore_command(args: &[String]) -> ExitCode {
    let (positional, values, switches) =
        match parse_simple("restore", args, &["--dir"], &["--force-version", "--json"]) {
            Ok(parsed) => parsed,
            Err(code) => return code,
        };
    let file = match positional.as_slice() {
        [file] => PathBuf::from(file),
        [] => return usage_error("restore needs the backup FILE"),
        _ => return usage_error("restore takes one backup FILE"),
    };
    let Some(dir) = values
        .iter()
        .find(|(f, _)| f == "--dir")
        .map(|(_, v)| PathBuf::from(v))
    else {
        return usage_error("restore needs --dir NEWDIR (a new or empty directory)");
    };
    init_console_logging();
    let options = graphersal_server::server_backup::RestoreOptions::new()
        .with_force_version(switches.iter().any(|s| s == "--force-version"));
    match graphersal_server::server_backup::restore(&file, &dir, options) {
        Ok(report) => {
            if switches.iter().any(|s| s == "--json") {
                out(
                    &serde_json::to_string_pretty(&console::restore_json(&report))
                        .unwrap_or_default(),
                );
            } else {
                out(&console::restore_text(&report));
            }
            exit(0)
        }
        Err(error) => failure(&error),
    }
}
