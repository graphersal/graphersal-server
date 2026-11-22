//! `graphersal-server`: the command line (EXPERIMENTAL).
//!
//! ```text
//! graphersal-server init DIR
//! graphersal-server serve --dir DIR [--listen ADDR]
//! ```
//!
//! stdout carries only results, stderr diagnostics and the log. Exit codes: `0` ok, `1` the
//! command failed, `2` invalid invocation.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use graphersal_server::config::parse_listen;
use graphersal_server::{ServeOptions, ServerDir, serve};

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
