//! `graphersal-server`: the command line (EXPERIMENTAL).
//!
//! ```text
//! graphersal-server init DIR
//! graphersal-server serve --dir DIR [--listen ADDR] [--max-connections N]
//!     [--header-timeout SECS] [--idle-timeout SECS]
//! graphersal-server start --dir DIR [serve options] [--timeout SECS]
//! graphersal-server stop --dir DIR [--timeout SECS]
//! graphersal-server status --dir DIR [--json]
//! graphersal-server db list|create|delete|clone|load|unload|promote|read-only|auto-load|alias|backup|backups|restore|export ... --dir DIR [--json]
//! graphersal-server import DIR SOURCE --name NAME [--base BASE] [--format F] [--description TEXT] [--role normal|base] [--json]
//! graphersal-server import DIR --inbox FILE --name NAME [...] | import DIR --list [--json]
//! graphersal-server models list|add|remove ... --dir DIR [--json]
//! graphersal-server user ... --dir DIR [--json]
//! graphersal-server mcp status|on|off | mcp db NAME [read-only] on|off --dir DIR [--json]
//!     (db, import, models, user, mcp, stop, status also: [--token TOKEN [--server ADDRESS]])
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
use graphersal_server::console::{self, DbCommand, Via};
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
  graphersal-server init DIR
      Create a server directory (server.toml, databases/, models/blobs/, backups/, trash/,
      tmp/). DIR must be missing, empty, or a server directory (then only missing
      parts are created; server.toml is kept). A directory without access.toml gets its
      first admin (user admin): its token is printed ONCE on stderr (the first serve of
      such a directory does the same). DIR becomes private to its owner (mode 0700; the
      secrets access.toml, server.run, sessions/ owner only); on an existing directory
      init tightens the modes it finds too open.
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
        db alias rename OLD NEW  the alias OLD becomes NEW (same database); open MCP sessions
                                 that selected OLD move to NEW
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
      directory or .gstore file (copied into a directory Store; a model blob it lacks is
      filled from the shared models, else it is refused), or a GraphSON (.json,
      .jsonl, .graphson) or GraphML (.xml, .graphml) file. FORMAT (snapshot, store,
      graphson, graphml) overrides the detection. An embedding model the source names but
      does not carry is taken from the shared models when they hold it; otherwise the
      database is needs-model until `models add`. --role base makes a base (read-only,
      frozen for good) of a plain source (not of a layer, nor of a source naming a model
      neither it nor the shared models carry). Through a running server the server reads
      SOURCE itself (same machine).
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
                                 answer 503; open sessions go on when it is on again)
        mcp db NAME on|off       offer a database or alias to MCP, or hide it
        mcp db NAME read-only on|off
            MCP sessions only read NAME, whatever their grant.
  graphersal-server --help | --version

The OS user owning DIR is its operator: serve and the db, import, models, user and mcp commands
refuse a DIR (or access.toml, server.run, sessions/) owned by another user or open to group
or others, naming the chmod/chown that fixes it (unix; on Windows DIR's ACL is the boundary).
Other administrators send an admin token instead:
  ... --token TOKEN [--server ADDRESS]
      db, import, models, user, mcp, stop and status go to the running server with this user's admin token
      (or the environment variable GRAPHERSAL_SERVER_TOKEN), at ADDRESS (IP:PORT) or the
      address in DIR/server.run; no file of DIR is written and its permissions are not
      checked. Refused when no server runs.

stdout carries only results, stderr diagnostics and the log (level: RUST_LOG, default
info). Exit codes: 0 ok, 1 the command failed, 2 invalid invocation, 3 status: not
running.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
    let dir = match args {
        [dir] if !dir.starts_with('-') => PathBuf::from(dir),
        _ => return usage_error("init takes exactly one argument: the directory"),
    };
    match ServerDir::init(&dir) {
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
                    &serde_json::to_string_pretty(&console::status_json(&status))
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
        if values
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
                }
            }
            None => DbCommand::ImportInbox {
                file: value("--inbox").unwrap_or_default(),
                name,
                role,
                base: value("--base"),
                format: value("--format"),
                description: value("--description"),
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
            "--dir" | "--repo" | "--license" | "--token" | "--server" => {
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
                 [--license L], remove REPO)",
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
                "unknown command or wrong arguments: mcp {} (status, on, off, db NAME on|off, \
                 db NAME read-only on|off)",
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
