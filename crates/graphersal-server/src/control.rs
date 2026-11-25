//! The server's own admin routes (round 3 B2): what `graphersal-server status` and `stop` ask
//! a running server.
//!
//! - `GET /api/admin/server`: the running server itself (any admin): `version`, `pid`,
//!   `address`, `url`, `directory`, `startedAtMs` (Unix milliseconds), `uptimeSecs`,
//!   `databases {total, loaded, loading}` (names), `aliases` (count), `memory {liveBytes,
//!   limitBytes, availableBytes}` (each `null` when the process does not know it: the live heap
//!   is measured only with a tracking allocator, which this binary does not install).
//! - `POST /api/admin/shutdown`: the server answers `202 {"stopping": true, "pid"}` and then
//!   stops exactly as on Ctrl+C or SIGTERM (the same graceful path in the process that owns
//!   it: [`crate::serve::StopHook`]). Only the operator (the token of `server.run`) or a user
//!   with the admin role may ask; the admin principal `everyone` of a server with access
//!   control off may not (stopping needs real credentials even then), `403`. A server started
//!   without a stop hook (a library caller that did not give one) answers `409`. The request
//!   is audited (`server shutdown requested`, with the actor).

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use graphersal_serve::{Method, Response};
use serde_json::{Value, json};

use crate::access::{Access, Principal, PrincipalKind};
use crate::audit::AuditEvent;
use crate::loader::Loader;
use crate::registry::DbState;
use crate::serve::StopHook;

/// The admin endpoint of the server itself (below `/api/admin/`).
pub(crate) const SERVER: &str = "server";
/// The admin endpoint that stops the server (below `/api/admin/`).
pub(crate) const SHUTDOWN: &str = "shutdown";

/// What the router knows about the server it routes for (set at start).
#[derive(Debug, Clone)]
pub(crate) struct ServerFacts {
    /// The bound address.
    pub(crate) address: std::net::SocketAddr,
    /// The URL a browser on this machine opens.
    pub(crate) url: String,
    /// When the server started (wall clock).
    pub(crate) started: SystemTime,
    /// When the server started (monotonic, for the uptime).
    pub(crate) started_instant: Instant,
}

impl ServerFacts {
    /// The facts of a server listening on `address` with `url`, starting now.
    pub(crate) fn now(address: std::net::SocketAddr, url: String) -> Self {
        Self {
            address,
            url,
            started: SystemTime::now(),
            started_instant: Instant::now(),
        }
    }

    /// The start time in Unix milliseconds.
    pub(crate) fn started_at_ms(&self) -> u64 {
        unix_ms(self.started)
    }
}

/// `time` in Unix milliseconds (0 before the epoch).
pub(crate) fn unix_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The answer of `method endpoint` when `endpoint` is one of this module's (module docs);
/// `None` for every other admin endpoint. `principal` is an admin (the router checked).
pub(crate) fn route(
    facts: Option<&ServerFacts>,
    hook: Option<&StopHook>,
    (loader, access): (&Loader, &Access),
    principal: &Principal,
    method: &Method,
    endpoint: &str,
) -> Option<Response> {
    let read = matches!(method, Method::Get | Method::Head);
    match endpoint {
        SERVER if read => Some(Response::json(200, &server_json(facts, loader))),
        SHUTDOWN if *method == Method::Post => Some(shutdown(hook, access, principal)),
        SERVER | SHUTDOWN => Some(Response::json(
            405,
            &json!({ "error": format!("method {method} is not allowed on /api/admin/{endpoint}") }),
        )),
        _ => None,
    }
}

/// `GET /api/admin/server` (module docs).
fn server_json(facts: Option<&ServerFacts>, loader: &Loader) -> Value {
    let registry = loader.registry();
    let entries = registry.list();
    let names = |wanted: fn(&DbState) -> bool| -> Vec<String> {
        entries
            .iter()
            .filter(|e| wanted(&e.state))
            .map(|e| e.name.clone())
            .collect()
    };
    let loaded = names(|state| matches!(state, DbState::Loaded));
    let loading = names(|state| matches!(state, DbState::Loading));
    let room = graphersal::exec::memory_room().ok();
    json!({
        "server": crate::router::SERVER_NAME,
        "version": env!("CARGO_PKG_VERSION"),
        "pid": std::process::id(),
        "address": facts.map(|f| f.address.to_string()),
        "url": facts.map(|f| f.url.clone()),
        "directory": registry.dir().root().display().to_string(),
        "startedAtMs": facts.map(ServerFacts::started_at_ms),
        "uptimeSecs": facts.map(|f| f.started_instant.elapsed().as_secs()),
        "databases": {
            "total": entries.len(),
            "loaded": loaded,
            "loading": loading,
        },
        "aliases": registry.aliases().len(),
        "memory": {
            "liveBytes": room.as_ref().and_then(|r| r.live_bytes()),
            "limitBytes": room.as_ref().and_then(|r| r.limit()),
            "availableBytes": room.as_ref().and_then(|r| r.available()),
        },
    })
}

/// `POST /api/admin/shutdown` (module docs).
fn shutdown(hook: Option<&StopHook>, access: &Access, principal: &Principal) -> Response {
    if principal.kind == PrincipalKind::Everyone {
        return Response::json(
            403,
            &json!({
                "error": "stopping the server needs the operator token or an admin user's \
                          token, also with access control off",
                "help": "run graphersal-server stop --dir DIR as the directory's owner (the \
                         operator token of server.run), or send an admin user's token",
            }),
        );
    }
    let Some(hook) = hook else {
        return Response::json(
            409,
            &json!({
                "error": "this server cannot be stopped through its API: the process that \
                          started it gave no stop hook",
                "help": "stop it in the process that runs it (Ctrl+C or SIGTERM for \
                         graphersal-server serve)",
            }),
        );
    };
    access
        .audit_as(principal)
        .record(AuditEvent::ShutdownRequested {});
    tracing::info!(
        target: "graphersal_server",
        "shutdown requested through the admin API by {}",
        principal.id
    );
    // The process that owns the server stops it (the Ctrl+C path), which lets this request
    // finish first: running requests get their grace period.
    hook.request();
    Response::json(202, &json!({ "stopping": true, "pid": std::process::id() }))
}
