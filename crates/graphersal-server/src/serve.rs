//! `graphersal-server serve`: the HTTP listener of a server directory.
//!
//! [`start`] opens the server directory, takes its `server.lock` (a second server on the same
//! directory is refused with [`ServerError::Locked`]), binds the listen address (`--listen`
//! over `server.toml`'s `listen`; port `0` picks a free port), writes the bound address into
//! `server.run` (with a new operator token for this run, [`crate::access`]), scans the database
//! registry ([`Registry::scan`]; `/readyz` answers `503 loading` until it is done), opens the
//! access state ([`Access::open`]: a directory without `access.toml` gets its first admin
//! here, its token handed out once by [`Running::take_first_admin`]), starts loading the `auto_load` databases in the background
//! ([`Loader::auto_load`]), shares the model blobs of every idle database through `models/`
//! on a background thread ([`crate::ops::Admin::adopt_all`]), and serves HTTP/1.1 and h2c
//! (HTTP/2 over plain TCP with prior knowledge: a connection that starts with the HTTP/2
//! preface) with `hyper` server connections on a `tokio` multi-thread runtime that
//! [`Running`] owns.
//!
//! Connections: at most `[limits] max_connections` are served at once ([`ConnectionLimits`]);
//! one more is accepted, answered `503` with `Retry-After: 1` (in its own protocol) and closed,
//! so a client learns at once instead of hanging in the accept queue (beyond a further
//! [`OVERFLOW_CONNECTIONS`] being refused that way, a connection is closed unanswered). A
//! request's head must arrive within `header_timeout_secs` (HTTP/1.1: hyper's header read
//! timeout, counted from the moment the connection waits for a request; HTTP/2: the idle
//! timeout below covers a connection that never completes a request); a connection without a
//! running request is closed after `idle_timeout_secs`. A body is never cut by a whole-request
//! timeout (a large upload streams for minutes): its reads wait at most 120 s for each next
//! piece.
//!
//! The router, the session jobs and every file I/O are blocking: each request is answered on
//! the runtime's blocking pool (`spawn_blocking`), never on an async worker. The request body
//! reaches the router as an `std::io::Read` that pulls hyper's body stream one frame at a time
//! (backpressure: nothing is read before the route reads, and `Expect: 100-continue` is
//! answered only then); an export download is pumped from its file through a small bounded
//! channel. Neither is ever held whole in memory.
//!
//! A declared body (`Content-Length`) over the limit ([`MAX_DECLARED_BODY_BYTES`], or the
//! upload limit for an import or model upload) is answered `413` with the JSON error at once, without
//! reading the body (a client waiting on `Expect: 100-continue` never gets its `100`), with
//! `Connection: close`. A body the route did not read to its end closes the connection after
//! the answer (a short declared rest is drained instead, keeping the connection). A connection
//! ends with a lingering close: the server shuts its sending side (the client sees the answer,
//! then EOF) and discards what the client still sends for at most two seconds before the socket
//! is closed, so the answer is never lost to a reset. All of this is HTTP/1.1; on HTTP/2 the
//! oversize answer is the same `413` (the stream is reset after it, the connection stays), and
//! the connection ends with HTTP/2's own GOAWAY instead of a lingering close.
//!
//! [`Running::stop`] stops it: the lifecycle answers `503 stopping`, the listening socket is
//! closed, idle connections are closed and running requests get five seconds to finish,
//! every database is unloaded ([`Loader::shutdown`]: its Store closed cleanly), the runtime is
//! shut down, `server.run` is removed and the lock released.
//!
//! Safety: every route but the probes, `/api/info` and the UI files needs a principal with
//! access ([`crate::access`], enforced by the router); tokens travel in clear text over plain
//! HTTP. The default listen address is the loopback; any other address gets a warning in
//! `serve`'s start block (the console; [`Running::exposure`]
//! tells a library caller). The `Host`/`Origin` checks of `graphersal_serve` (DNS
//! rebinding, cross-site pages) run on every request but the probes.

use std::convert::Infallible;
use std::fs::File;
use std::future::Future;
use std::io::{self, ErrorKind, Read};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use graphersal_serve::{Exposure, HostCheck, Method};
use hyper::body::{Body, Bytes, Frame, Incoming, SizeHint};
use hyper::header::{CONNECTION, CONTENT_LENGTH, EXPECT, HeaderName, HeaderValue};
use hyper::server::conn::{http1, http2};
use hyper::service::service_fn;
use hyper::{HeaderMap, StatusCode, Version};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::runtime::{Handle, Runtime};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
use tokio::task::JoinHandle;

use crate::access::{Access, FirstAdmin, Secret};
use crate::audit::Audit;
use crate::config::Limits;
use crate::error::ServerError;
use crate::layout::ServerDir;
use crate::loader::Loader;
use crate::lock::ServerLock;
use crate::ops::ExportFile;
use crate::registry::Registry;
use crate::router::{Answer, Router};
use crate::run_file::RunInfo;

/// Largest declared body (`Content-Length`) a request may name (as the graphersal CLI's dev
/// server), except an upload (`POST /api/admin/import` or `POST /api/admin/models` with a body
/// that is not JSON),
/// which may declare up to `server.toml`'s `[limits] upload_bytes` (its route streams the body
/// into `tmp/`). A request declaring more is answered `413` without its body being read, and
/// its connection is closed (module docs).
pub const MAX_DECLARED_BODY_BYTES: usize = 4 * graphersal_serve::MAX_BODY_BYTES;

/// How long [`Running::stop`] waits for running requests.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// How long a closing connection discards what its client still sends (module docs).
const LINGER: Duration = Duration::from_secs(2);

/// The largest unread rest of a declared body that is drained after the answer to keep the
/// connection; a larger rest closes it.
const DRAIN_BYTES: u64 = 256 * 1024;

/// How long a route reading the body waits for the client's next bytes before the read fails.
const BODY_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// The chunk size of a download.
const DOWNLOAD_CHUNK: usize = 64 * 1024;
/// How many chunks of a download wait for the client.
const DOWNLOAD_QUEUE: usize = 8;

/// How many connections over the cap are still answered `503` at once; more are closed
/// unanswered (a flood must not cost more than this).
pub const OVERFLOW_CONNECTIONS: usize = 64;

/// How long an over-the-cap connection may take to send its request before it is closed.
const OVERFLOW_TIMEOUT: Duration = Duration::from_secs(5);

/// The streams one HTTP/2 connection may run at once (every request runs on the blocking pool).
const H2_MAX_STREAMS: u32 = 64;

/// The HTTP/2 connection preface a client with prior knowledge starts with.
const H2_PREFACE: &[u8; 24] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

/// The connection limits a server runs with: `server.toml`'s `[limits]` with the `serve`
/// options over them (module docs).
///
/// `#[non_exhaustive]`: read it from [`Running::connection_limits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConnectionLimits {
    /// The largest number of open connections served at once.
    pub max_connections: usize,
    /// How long a request's head may take to arrive.
    pub header_timeout: Duration,
    /// How long a connection without a running request stays open.
    pub idle_timeout: Duration,
}

impl ConnectionLimits {
    /// The limits of `limits` with the options' overrides.
    fn of(limits: &Limits, options: &ServeOptions) -> Self {
        Self {
            max_connections: options.max_connections.unwrap_or(limits.max_connections),
            header_timeout: Duration::from_secs(
                options
                    .header_timeout_secs
                    .unwrap_or(limits.header_timeout_secs),
            ),
            idle_timeout: Duration::from_secs(
                options
                    .idle_timeout_secs
                    .unwrap_or(limits.idle_timeout_secs),
            ),
        }
    }
}

/// The options of [`start`].
///
/// `#[non_exhaustive]`: build it with [`ServeOptions::new`] and its setters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ServeOptions {
    /// The listen address, overriding `server.toml`'s `listen`.
    pub listen: Option<SocketAddr>,
    /// The largest number of open connections, overriding `[limits] max_connections`.
    pub max_connections: Option<usize>,
    /// The header timeout in seconds, overriding `[limits] header_timeout_secs`.
    pub header_timeout_secs: Option<u64>,
    /// The idle timeout in seconds, overriding `[limits] idle_timeout_secs`.
    pub idle_timeout_secs: Option<u64>,
}

impl ServeOptions {
    /// The defaults: everything from `server.toml`.
    pub fn new() -> Self {
        Self::default()
    }

    /// The options with a listen address overriding `server.toml`'s.
    pub fn with_listen(mut self, listen: SocketAddr) -> Self {
        self.listen = Some(listen);
        self
    }

    /// The options with the largest number of open connections (at least 1, else `start`
    /// refuses it) overriding `[limits] max_connections`.
    pub fn with_max_connections(mut self, max: usize) -> Self {
        self.max_connections = Some(max);
        self
    }

    /// The options with the header timeout in seconds (at least 1) overriding
    /// `[limits] header_timeout_secs`.
    pub fn with_header_timeout_secs(mut self, secs: u64) -> Self {
        self.header_timeout_secs = Some(secs);
        self
    }

    /// The options with the idle timeout in seconds (at least 1) overriding
    /// `[limits] idle_timeout_secs`.
    pub fn with_idle_timeout_secs(mut self, secs: u64) -> Self {
        self.idle_timeout_secs = Some(secs);
        self
    }
}

/// What the connections share.
struct Shared {
    router: Router,
    /// The requests being answered (a download until its last byte is handed over).
    in_flight: AtomicUsize,
    /// The connection limits.
    limits: ConnectionLimits,
    /// One permit per served connection (`max_connections`).
    connections: Arc<Semaphore>,
    /// One permit per connection being refused with `503` ([`OVERFLOW_CONNECTIONS`]).
    overflow: Arc<Semaphore>,
    /// Whether the cap was reached and not left since (one warning per episode).
    saturated: AtomicBool,
}

/// The listener: its runtime, the accept task and the stop signal.
struct Listener {
    runtime: Runtime,
    accept: JoinHandle<()>,
    stop: watch::Sender<bool>,
}

/// A running server; [`Running::stop`] (or dropping it) stops it.
pub struct Running {
    dir: ServerDir,
    shared: Arc<Shared>,
    listener: Option<Listener>,
    lock: Option<ServerLock>,
    registry: Arc<Registry>,
    loader: Arc<Loader>,
    address: SocketAddr,
    url: String,
    exposure: Exposure,
    access: Option<Arc<Access>>,
    first_admin: Option<FirstAdmin>,
}

/// A `ServerError::Config`-style refusal of a `serve` option that is 0.
fn check_limits(limits: &ConnectionLimits) -> Result<(), ServerError> {
    for (name, zero) in [
        ("max_connections", limits.max_connections == 0),
        ("header_timeout_secs", limits.header_timeout.is_zero()),
        ("idle_timeout_secs", limits.idle_timeout.is_zero()),
    ] {
        if zero {
            return Err(ServerError::Config {
                path: std::path::PathBuf::from(crate::layout::CONFIG_FILE),
                message: format!("limits.{name}: must be at least 1"),
            });
        }
    }
    Ok(())
}

/// Starts serving the server directory `root` (module docs).
pub fn start(
    root: impl Into<std::path::PathBuf>,
    options: ServeOptions,
) -> Result<Running, ServerError> {
    let dir = ServerDir::open(root)?;
    let config = dir.load_config()?;
    let listen = match options.listen {
        Some(listen) => listen,
        None => config.listen_addr()?,
    };
    let limits = ConnectionLimits::of(&config.limits, &options);
    check_limits(&limits)?;
    // The lock before the bind: a second server is refused for what it is, not for its port.
    let lock = ServerLock::acquire(&dir)?;
    let bind_error = |message: String| ServerError::Bind {
        address: listen.to_string(),
        message,
    };
    let socket = std::net::TcpListener::bind(listen).map_err(|err| bind_error(err.to_string()))?;
    let address = socket
        .local_addr()
        .map_err(|err| bind_error(err.to_string()))?;
    socket
        .set_nonblocking(true)
        .map_err(|err| bind_error(err.to_string()))?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .thread_name("graphersal-server-http")
        .enable_io()
        .enable_time()
        .build()
        .map_err(|err| ServerError::Thread {
            message: err.to_string(),
        })?;
    let socket = {
        let _entered = runtime.enter();
        tokio::net::TcpListener::from_std(socket).map_err(|err| bind_error(err.to_string()))?
    };
    let exposure = if address.ip().is_loopback() {
        Exposure::Loopback
    } else {
        Exposure::Network
    };
    let url = url_of(address);
    let host_check = HostCheck::new(exposure, address.port(), &url)
        .with_hint(format!("open this server at {url}"));
    let shared = Arc::new(Shared {
        router: Router::new(host_check.clone()),
        in_flight: AtomicUsize::new(0),
        limits,
        connections: Arc::new(Semaphore::new(limits.max_connections)),
        overflow: Arc::new(Semaphore::new(OVERFLOW_CONNECTIONS)),
        saturated: AtomicBool::new(false),
    });
    // A new operator token per run: the console on this machine reads it from `server.run`.
    let operator = Secret::generate().map_err(|message| ServerError::Random { message })?;
    let operator_hash = operator.hash();
    RunInfo::new(address, url.clone())
        .with_operator_token(operator)
        .write(&dir)?;
    let (stop, stop_rx) = watch::channel(false);
    let accept = runtime.spawn(accept(socket, shared.clone(), stop_rx));
    let mcp = crate::mcp::endpoint(&config.mcp);
    let registry = Registry::empty(&dir);
    let loader = Loader::with_mcp(registry.clone(), host_check.clone(), mcp.clone());
    let mut running = Running {
        dir,
        shared,
        listener: Some(Listener {
            runtime,
            accept,
            stop,
        }),
        lock: Some(lock),
        registry,
        loader,
        address,
        url,
        exposure,
        access: None,
        first_admin: None,
    };
    // The scan with the lock held and the listener up: the probes answer meanwhile.
    match Registry::scan(&running.dir) {
        Ok(registry) => {
            running.loader = Loader::with_mcp(registry.clone(), host_check, mcp);
            running.registry = registry;
        }
        Err(err) => {
            let _ = running.shutdown();
            return Err(err);
        }
    }
    // The access state: a directory without `access.toml` is closed and gets its first admin.
    match Access::open(&running.dir, Some(operator_hash), Audit::default()) {
        Ok((access, first_admin)) => {
            running.access = Some(Arc::new(access));
            running.first_admin = first_admin;
        }
        Err(err) => {
            let _ = running.shutdown();
            return Err(err);
        }
    }
    let access = running.access.clone().ok_or_else(|| ServerError::Thread {
        message: "the access state is missing".to_string(),
    });
    let ready = access.and_then(|access| {
        running
            .shared
            .router
            .ready(running.loader.clone(), access)
            .map_err(|err| ServerError::Thread {
                message: err.to_string(),
            })
    });
    if let Err(err) = ready {
        // A first admin nobody will see must not close the directory for good.
        if let Some(first) = running.first_admin.take() {
            let _ = std::fs::remove_file(&first.path);
        }
        let _ = running.shutdown();
        return Err(err);
    }
    // The databases with `auto_load = true`, each on a background thread of its own.
    running.loader.auto_load();
    // In the background: every idle database's model blobs shared through `models/`, and
    // `models/models.toml` rebuilt (problems are logged, never fatal; a database that loads
    // meanwhile is skipped).
    let admin = crate::ops::Admin::online(running.loader.clone());
    if let Err(err) = std::thread::Builder::new()
        .name("graphersal-server-models".to_string())
        .spawn(move || admin.adopt_all())
    {
        tracing::warn!(target: "graphersal_server", "models: cannot start the adoption: {err}");
    }
    // The console prints the start block (and, on a network bind, the warning) right after
    // this returns, so a library caller only gets a debug line here: the warning must appear
    // exactly once.
    tracing::debug!(
        target: "graphersal_server",
        "serving {} on {} ({})",
        running.dir.root().display(),
        running.url,
        match exposure {
            Exposure::Loopback => "loopback",
            Exposure::Network => "reachable from the network",
        }
    );
    Ok(running)
}

impl Running {
    /// The bound address.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// The URL a browser on this machine opens (`http://127.0.0.1:<port>/`; the loopback for
    /// a wildcard address).
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Whether the server is reachable from the network (tokens travel in clear text over plain
    /// HTTP).
    pub fn exposure(&self) -> Exposure {
        self.exposure
    }

    /// The server directory.
    pub fn dir(&self) -> &ServerDir {
        &self.dir
    }

    /// The database registry (scanned at start).
    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    /// The loaded databases and their loads ([`Loader`]).
    pub fn loader(&self) -> &Arc<Loader> {
        &self.loader
    }

    /// The access state (users, grants, the authenticator chain, browser sessions): always
    /// `Some` for a server [`start`] returned.
    pub fn access(&self) -> Option<&Arc<Access>> {
        self.access.as_ref()
    }

    /// The first admin made at this start (the directory had no `access.toml`): its token, to
    /// be shown ONCE; `None` afterwards (and when the file existed).
    pub fn take_first_admin(&mut self) -> Option<FirstAdmin> {
        self.first_admin.take()
    }

    /// The connection limits the server runs with.
    pub fn connection_limits(&self) -> ConnectionLimits {
        self.shared.limits
    }

    /// Stops the server (module docs): `Err` only when `server.run` cannot be removed (the
    /// server is stopped and its lock released anyway).
    pub fn stop(mut self) -> Result<(), ServerError> {
        self.shutdown()
    }

    fn shutdown(&mut self) -> Result<(), ServerError> {
        let Some(listener) = self.listener.take() else {
            return Ok(());
        };
        self.shared.router.lifecycle.stop();
        // The accept task ends (the listening socket is closed with it); every connection
        // stops taking requests after the one it answers.
        let _ = listener.stop.send(true);
        let accept = listener.accept;
        let _ = listener
            .runtime
            .block_on(async { tokio::time::timeout(STOP_GRACE, accept).await });
        let deadline = Instant::now() + STOP_GRACE;
        while self.shared.in_flight.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let left = self.shared.in_flight.load(Ordering::SeqCst);
        if left > 0 {
            tracing::warn!(target: "graphersal_server", "stopping with {left} requests still running");
        }
        // Every database unloaded, its Store closed cleanly, before the lock is released.
        if let Err(err) = self.loader.shutdown() {
            tracing::warn!(target: "graphersal_server", "stopping: {err}");
        }
        // The connections left (idle ones, lingering closes) end with the runtime.
        listener.runtime.shutdown_timeout(Duration::from_secs(1));
        let removed = RunInfo::remove(&self.dir);
        // The lock last: the run file is gone before another server may start.
        self.lock.take();
        tracing::info!(target: "graphersal_server", "stopped serving {}", self.dir.root().display());
        removed
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// The URL of the server on `address` for a browser on this machine.
fn url_of(address: SocketAddr) -> String {
    let host = match address.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => Ipv4Addr::LOCALHOST.to_string(),
        IpAddr::V6(ip) if ip.is_unspecified() => format!("[{}]", Ipv6Addr::LOCALHOST),
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    };
    format!("http://{host}:{}/", address.port())
}

/// The accept loop: every connection a task of its own, until [`Running::stop`].
async fn accept(
    socket: tokio::net::TcpListener,
    shared: Arc<Shared>,
    mut stop: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            accepted = socket.accept() => match accepted {
                Ok((stream, _peer)) => {
                    let _ = stream.set_nodelay(true);
                    admit(stream, &shared, &stop);
                }
                Err(err) => {
                    tracing::warn!(
                        target: "graphersal_server",
                        "accepting a connection failed: {err}"
                    );
                    // Out of file descriptors, for example: no busy loop.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
            _ = stop.changed() => break,
        }
    }
}

/// Serves `stream` when a connection permit is free; else refuses it with `503` (or, past
/// [`OVERFLOW_CONNECTIONS`] refusals at once, closes it).
fn admit(stream: tokio::net::TcpStream, shared: &Arc<Shared>, stop: &watch::Receiver<bool>) {
    match shared.connections.clone().try_acquire_owned() {
        Ok(permit) => {
            shared.saturated.store(false, Ordering::Relaxed);
            tokio::spawn(connection(stream, shared.clone(), stop.clone(), permit));
        }
        Err(_) => {
            if !shared.saturated.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    target: "graphersal_server",
                    "{} connections are open, the limit ([limits] max_connections): further \
                     connections are answered 503 until one closes",
                    shared.limits.max_connections
                );
            }
            match shared.overflow.clone().try_acquire_owned() {
                Ok(permit) => {
                    tokio::spawn(refuse_connection(stream, shared.limits, permit));
                }
                Err(_) => drop(stream),
            }
        }
    }
}

/// The protocol a connection speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Protocol {
    Http1,
    Http2,
}

/// The protocol of `stream` from its first bytes, peeked (never consumed): HTTP/2 when they
/// are the HTTP/2 preface, HTTP/1.1 as soon as they differ from it (at once for every HTTP/1
/// method but a `P...` one, after a byte or two for those) or the client closes. `None` when
/// the client sends nothing decisive within `timeout`.
async fn sniff(stream: &tokio::net::TcpStream, timeout: Duration) -> Option<Protocol> {
    let decide = async {
        let mut buffer = [0u8; H2_PREFACE.len()];
        loop {
            let read = stream.peek(&mut buffer).await.ok()?;
            if read == 0 || buffer[..read] != H2_PREFACE[..read] {
                return Some(Protocol::Http1);
            }
            if read == H2_PREFACE.len() {
                return Some(Protocol::Http2);
            }
            // A prefix of the preface so far: wait for the rest (peeking again at once would
            // spin, the socket stays readable).
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    tokio::time::timeout(timeout, decide).await.ok().flatten()
}

/// The activity of one connection: its running requests and when the last one ended (the
/// idle timeout).
struct Activity {
    /// The requests of this connection being answered.
    active: AtomicUsize,
    /// When the connection became idle, in milliseconds after `epoch`.
    idle_since: AtomicU64,
    epoch: Instant,
}

impl Activity {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            active: AtomicUsize::new(0),
            idle_since: AtomicU64::new(0),
            epoch: Instant::now(),
        })
    }

    /// When the connection times out if nothing happens: `None` while a request runs.
    fn idle_deadline(&self, idle: Duration) -> Option<Instant> {
        if self.active.load(Ordering::SeqCst) > 0 {
            return None;
        }
        let since = Duration::from_millis(self.idle_since.load(Ordering::SeqCst));
        Some(self.epoch + since + idle)
    }

    fn begin(&self) {
        self.active.fetch_add(1, Ordering::SeqCst);
    }

    fn end(&self) {
        let millis = u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.idle_since.store(millis, Ordering::SeqCst);
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The boxed future of one answer (hyper wants an `Unpin` service future).
type Answering =
    Pin<Box<dyn Future<Output = Result<hyper::Response<Outgoing>, Infallible>> + Send>>;

/// Serves one connection until it ends (HTTP/1.1: then closes it with a lingering close);
/// `_permit` is its place under the connection cap.
async fn connection(
    stream: tokio::net::TcpStream,
    shared: Arc<Shared>,
    stop: watch::Receiver<bool>,
    _permit: OwnedSemaphorePermit,
) {
    let limits = shared.limits;
    let Some(protocol) = sniff(&stream, limits.header_timeout).await else {
        tracing::debug!(target: "graphersal_server", "a connection sent no request in time");
        return;
    };
    let activity = Activity::new();
    let service = {
        let activity = activity.clone();
        service_fn(move |request: hyper::Request<Incoming>| -> Answering {
            Box::pin(respond(shared.clone(), activity.clone(), request))
        })
    };
    match protocol {
        Protocol::Http1 => {
            let mut conn = http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(limits.header_timeout)
                .title_case_headers(true)
                .serve_connection(TokioIo::new(stream), service);
            let mut idle = Idle::new(stop, &activity, limits.idle_timeout);
            let result = loop {
                if idle.shut_now() {
                    Pin::new(&mut conn).graceful_shutdown();
                }
                tokio::select! {
                    result = std::future::poll_fn(|cx| conn.poll_without_shutdown(cx)) => {
                        break result;
                    }
                    () = idle.next() => {}
                }
            };
            if let Err(err) = result {
                tracing::debug!(target: "graphersal_server", "a connection ended: {err}");
            }
            linger(conn.into_parts().io.into_inner()).await;
        }
        Protocol::Http2 => {
            let conn = http2::Builder::new(TokioExecutor::new())
                .max_concurrent_streams(H2_MAX_STREAMS)
                .serve_connection(TokioIo::new(stream), service);
            let mut conn = std::pin::pin!(conn);
            let mut idle = Idle::new(stop, &activity, limits.idle_timeout);
            let result = loop {
                if idle.shut_now() {
                    conn.as_mut().graceful_shutdown();
                }
                tokio::select! {
                    result = conn.as_mut() => break result,
                    () = idle.next() => {}
                }
            };
            if let Err(err) = result {
                tracing::debug!(target: "graphersal_server", "an HTTP/2 connection ended: {err}");
            }
        }
    }
}
/// When a served connection is shut down gracefully: the server stops, or the connection was
/// idle (no running request) for the idle timeout.
struct Idle<'a> {
    stop: watch::Receiver<bool>,
    activity: &'a Activity,
    timeout: Duration,
    /// The server stops, or the idle timeout passed.
    due: bool,
    /// The graceful shutdown was started.
    shut: bool,
}

impl<'a> Idle<'a> {
    fn new(stop: watch::Receiver<bool>, activity: &'a Activity, timeout: Duration) -> Self {
        let due = *stop.borrow();
        Self {
            stop,
            activity,
            timeout,
            due,
            shut: false,
        }
    }

    /// Whether the graceful shutdown starts now (once).
    fn shut_now(&mut self) -> bool {
        if self.due && !self.shut {
            self.shut = true;
            return true;
        }
        false
    }

    /// Resolves when the server stops or the idle deadline passes (never once due); the
    /// caller then asks [`Self::shut_now`].
    async fn next(&mut self) {
        if self.due {
            return std::future::pending().await;
        }
        let deadline = self.activity.idle_deadline(self.timeout);
        tokio::select! {
            _ = self.stop.changed() => self.due = true,
            () = sleep_until(deadline) => {
                // A request may have begun or ended meanwhile: due only when still idle past
                // the deadline.
                if self
                    .activity
                    .idle_deadline(self.timeout)
                    .is_some_and(|deadline| Instant::now() >= deadline)
                {
                    tracing::debug!(target: "graphersal_server", "closing an idle connection");
                    self.due = true;
                }
            }
        }
    }
}

/// Sleeps until `deadline`; `None` (a request runs) re-checks after a second.
async fn sleep_until(deadline: Option<Instant>) {
    let deadline = deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(1));
    tokio::time::sleep_until(deadline.into()).await;
}

/// Answers an over-the-cap connection `503` with `Retry-After: 1` in its own protocol, then
/// closes it (`_permit` is its place among [`OVERFLOW_CONNECTIONS`]).
async fn refuse_connection(
    stream: tokio::net::TcpStream,
    limits: ConnectionLimits,
    _permit: OwnedSemaphorePermit,
) {
    let refuse = async move {
        let protocol = sniff(&stream, OVERFLOW_TIMEOUT).await?;
        let service = service_fn(move |request: hyper::Request<Incoming>| {
            std::future::ready(Ok::<_, Infallible>(over_capacity(
                limits,
                request.version(),
            )))
        });
        match protocol {
            Protocol::Http1 => {
                let mut conn = http1::Builder::new()
                    .timer(TokioTimer::new())
                    .header_read_timeout(OVERFLOW_TIMEOUT)
                    .keep_alive(false)
                    .title_case_headers(true)
                    .serve_connection(TokioIo::new(stream), service);
                let _ = std::future::poll_fn(|cx| conn.poll_without_shutdown(cx)).await;
                Some(conn.into_parts().io.into_inner())
            }
            Protocol::Http2 => {
                let conn = http2::Builder::new(TokioExecutor::new())
                    .max_concurrent_streams(1)
                    .serve_connection(TokioIo::new(stream), service);
                let mut conn = std::pin::pin!(conn);
                // GOAWAY at once: the request the client sent with its preface is still
                // answered, then the connection ends.
                conn.as_mut().graceful_shutdown();
                let _ = conn.await;
                None
            }
        }
    };
    if let Ok(Some(stream)) = tokio::time::timeout(OVERFLOW_TIMEOUT * 2, refuse).await {
        linger(stream).await;
    }
}

/// The `503` of a connection over the cap.
fn over_capacity(limits: ConnectionLimits, version: Version) -> hyper::Response<Outgoing> {
    let body = serde_json::json!({
        "error": format!(
            "the server is at its connection limit ({} open connections); try again shortly",
            limits.max_connections
        ),
        "help": "the limit is [limits] max_connections in server.toml (serve --max-connections N)",
    });
    let mut response = build(
        503,
        &[
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Retry-After".to_string(), "1".to_string()),
        ],
        Outgoing::full(body.to_string().into_bytes()),
    );
    if version != Version::HTTP_2 {
        response
            .headers_mut()
            .insert(CONNECTION, HeaderValue::from_static("close"));
    }
    response
}

/// Shuts the sending side (the client reads the answer, then EOF), then discards what the
/// client still sends for at most two seconds before the socket is closed.
async fn linger(mut stream: tokio::net::TcpStream) {
    if stream.shutdown().await.is_err() {
        return;
    }
    let _ = tokio::time::timeout(LINGER, async {
        let mut sink = vec![0u8; 16 * 1024];
        while let Ok(read) = stream.read(&mut sink).await {
            if read == 0 {
                break;
            }
        }
    })
    .await;
}

/// A request being answered: counted down (on the server and on its connection) however its
/// answer ends.
struct InFlight(Arc<Shared>, Arc<Activity>);

impl InFlight {
    fn new(shared: &Arc<Shared>, activity: &Arc<Activity>) -> Self {
        shared.in_flight.fetch_add(1, Ordering::SeqCst);
        activity.begin();
        Self(shared.clone(), activity.clone())
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.1.end();
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

/// What the blocking part of a request produced.
struct Reply {
    answer: Answer,
    /// The connection closes after the answer (the body was not read to its end).
    close: bool,
}

/// Answers one request: the router on the blocking pool, then the answer as hyper writes it.
async fn respond(
    shared: Arc<Shared>,
    activity: Arc<Activity>,
    request: hyper::Request<Incoming>,
) -> Result<hyper::Response<Outgoing>, Infallible> {
    let in_flight = InFlight::new(&shared, &activity);
    let version = request.version();
    let handle = Handle::current();
    let blocking = handle.clone();
    let reply = tokio::task::spawn_blocking(move || answer(&shared, request, blocking)).await;
    let reply = match reply {
        Ok(reply) => reply,
        Err(err) => {
            tracing::warn!(target: "graphersal_server", "a request handler failed: {err}");
            Reply {
                answer: Answer::Response(graphersal_serve::Response::json(
                    500,
                    &serde_json::json!({ "error": "the request handler failed" }),
                )),
                close: true,
            }
        }
    };
    let mut response = match reply.answer {
        Answer::Response(answer) => {
            drop(in_flight);
            let (status, headers, body) = answer.into_parts();
            build(status, &headers, Outgoing::full(body))
        }
        Answer::Download(download) => {
            let headers = download.headers();
            let length = download.length();
            let (file, guard) = download.into_parts();
            let (tx, rx) = mpsc::channel(DOWNLOAD_QUEUE);
            handle.spawn_blocking(move || pump(file, guard, &tx, in_flight));
            build(200, &headers, Outgoing::stream(rx, length))
        }
    };
    // HTTP/2 has no `Connection` header (the stream ends on its own).
    if reply.close && version != Version::HTTP_2 {
        response
            .headers_mut()
            .insert(CONNECTION, HeaderValue::from_static("close"));
    }
    Ok(response)
}

/// The blocking part of a request: the limits, the router, the unread body.
fn answer(shared: &Shared, request: hyper::Request<Incoming>, handle: Handle) -> Reply {
    let (parts, body) = request.into_parts();
    let mut headers = header_list(&parts.headers);
    // HTTP/2 names the host in `:authority`, not `Host`: the Host check reads it as `Host`.
    if parts.version == Version::HTTP_2
        && !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("Host"))
        && let Some(authority) = parts.uri.authority()
    {
        headers.push(("host".to_string(), authority.as_str().to_string()));
    }
    let declared = parts
        .headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok());
    let expects_continue = parts
        .headers
        .get(EXPECT)
        .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"100-continue"));
    let method = Method::parse(parts.method.as_str());
    let url = parts
        .uri
        .path_and_query()
        .map_or_else(|| parts.uri.to_string(), |p| p.as_str().to_string());
    let content_type = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("Content-Type"))
        .map(|(_, value)| value.as_str());
    let path = url.split_once('?').map_or(url.as_str(), |(path, _)| path);
    let upload = crate::admin_api::is_upload(&method, path, content_type);
    // An upload (an import or a model) may declare up to the upload limit (its route streams
    // the body into a file); every other request up to MAX_DECLARED_BODY_BYTES.
    let limit = if upload {
        shared.router.upload_limit()
    } else {
        MAX_DECLARED_BODY_BYTES as u64
    };
    if let Some(len) = declared.filter(|len| *len > limit) {
        return Reply {
            answer: Answer::Response(refuse_oversize(shared, &url, &headers, upload, limit, len)),
            close: true,
        };
    }
    let mut reader = BodyReader::new(body, handle);
    let answer = {
        let mut converted = graphersal_serve::Request::new(method, &url, &mut reader)
            .with_body_length(declared.and_then(|len| usize::try_from(len).ok()));
        for (name, value) in &headers {
            converted = converted.with_header(name, value);
        }
        shared.router.answer(converted)
    };
    // A short unread rest is drained (the connection stays); a client waiting on
    // `Expect: 100-continue` is not asked for a body nobody reads.
    if !reader.done && !expects_continue {
        let rest = declared.map(|len| len.saturating_sub(reader.read));
        if rest.is_some_and(|rest| rest <= DRAIN_BYTES) {
            let _ = io::copy(&mut (&mut reader).take(DRAIN_BYTES + 1), &mut io::sink());
        }
    }
    Reply {
        answer,
        close: !reader.done,
    }
}

/// The request headers as the serve boundary takes them (a value that is not UTF-8 lossy).
fn header_list(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect()
}

/// The answer to a request declaring a body over its limit: `413` with the JSON error (`403`
/// when the `Host`/`Origin` checks refuse it), its body never read.
fn refuse_oversize(
    shared: &Shared,
    url: &str,
    headers: &[(String, String)],
    upload: bool,
    limit: u64,
    declared: u64,
) -> graphersal_serve::Response {
    let mut probe = graphersal_serve::Request::new(Method::Post, url, io::empty());
    for (name, value) in headers {
        probe = probe.with_header(name, value);
    }
    let error = format!("the body is larger than {limit} bytes (it declares {declared})");
    let answer = match shared.router.host_check.check(&probe) {
        Err(refused) => graphersal_serve::Response::json(
            refused.status(),
            &serde_json::json!({ "error": refused.message() }),
        ),
        Ok(()) if upload => graphersal_serve::Response::json(
            413,
            &serde_json::json!({
                "error": error,
                "help": "the server's upload limit is [limits] upload_bytes in server.toml",
            }),
        ),
        Ok(()) => graphersal_serve::Response::json(413, &serde_json::json!({ "error": error })),
    };
    tracing::warn!(
        target: "graphersal_server",
        "a request declared a body of {declared} bytes, over its limit of {limit}; refused \
         with {} without reading it",
        answer.status()
    );
    answer
}

/// A hyper response of `status` with `headers` (a header hyper cannot carry is left out; the
/// length comes from the body).
fn build(status: u16, headers: &[(String, String)], body: Outgoing) -> hyper::Response<Outgoing> {
    let mut response = hyper::Response::new(body);
    *response.status_mut() =
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let map = response.headers_mut();
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("Content-Length") {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            map.append(name, value);
        }
    }
    response
}

/// Streams an export file into the download's channel; the export is removed once the file
/// is closed (the client got it, or went away).
fn pump(
    mut file: File,
    guard: ExportFile,
    tx: &mpsc::Sender<io::Result<Bytes>>,
    in_flight: InFlight,
) {
    let mut buffer = vec![0u8; DOWNLOAD_CHUNK];
    loop {
        let chunk = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => Ok(Bytes::copy_from_slice(&buffer[..read])),
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(err) => Err(err),
        };
        if let Err(err) = &chunk {
            tracing::warn!(target: "graphersal_server", "an export download broke off: {err}");
        }
        let failed = chunk.is_err();
        if tx.blocking_send(chunk).is_err() {
            tracing::warn!(
                target: "graphersal_server",
                "an export download broke off: the client went away"
            );
            break;
        }
        if failed {
            break;
        }
    }
    drop(file);
    drop(guard);
    drop(in_flight);
}

/// The request body as the `std::io::Read` the serve boundary takes: each read pulls hyper's
/// body stream for its next frame (on a blocking-pool thread), so the body is read only as
/// fast as the route consumes it.
struct BodyReader {
    body: Incoming,
    handle: Handle,
    chunk: Bytes,
    /// The body was read to its end.
    done: bool,
    /// The bytes handed to the route.
    read: u64,
}

impl BodyReader {
    fn new(body: Incoming, handle: Handle) -> Self {
        let done = body.is_end_stream();
        Self {
            body,
            handle,
            chunk: Bytes::new(),
            done,
            read: 0,
        }
    }
}

impl Read for BodyReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        loop {
            if !self.chunk.is_empty() {
                let n = out.len().min(self.chunk.len());
                out[..n].copy_from_slice(&self.chunk.split_to(n));
                self.read += n as u64;
                return Ok(n);
            }
            if self.done || out.is_empty() {
                return Ok(0);
            }
            let body = &mut self.body;
            let next = self.handle.block_on(tokio::time::timeout(
                BODY_IDLE_TIMEOUT,
                std::future::poll_fn(|cx| Pin::new(&mut *body).poll_frame(cx)),
            ));
            match next {
                Err(_) => {
                    return Err(io::Error::new(
                        ErrorKind::TimedOut,
                        "the client sent nothing for too long",
                    ));
                }
                Ok(None) => self.done = true,
                Ok(Some(Ok(frame))) => {
                    if let Ok(data) = frame.into_data() {
                        self.chunk = data;
                    }
                }
                Ok(Some(Err(err))) => return Err(io::Error::other(err)),
            }
        }
    }
}

/// An answer's body: in memory, or the chunks of a download with its known length.
enum Outgoing {
    Full(Option<Bytes>),
    Stream {
        rx: mpsc::Receiver<io::Result<Bytes>>,
        remaining: u64,
    },
}

impl Outgoing {
    fn full(body: Vec<u8>) -> Self {
        Self::Full(Some(Bytes::from(body)).filter(|b| !b.is_empty()))
    }

    fn stream(rx: mpsc::Receiver<io::Result<Bytes>>, length: u64) -> Self {
        Self::Stream {
            rx,
            remaining: length,
        }
    }
}

impl Body for Outgoing {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        match self.get_mut() {
            Self::Full(body) => Poll::Ready(body.take().map(|b| Ok(Frame::data(b)))),
            Self::Stream { rx, remaining } => match rx.poll_recv(cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    *remaining = remaining.saturating_sub(chunk.len() as u64);
                    Poll::Ready(Some(Ok(Frame::data(chunk))))
                }
                Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(err))),
                Poll::Ready(None) => Poll::Ready(None),
                Poll::Pending => Poll::Pending,
            },
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            Self::Full(body) => body.is_none(),
            Self::Stream { remaining, .. } => *remaining == 0,
        }
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            Self::Full(body) => SizeHint::with_exact(body.as_ref().map_or(0, |b| b.len() as u64)),
            Self::Stream { remaining, .. } => SizeHint::with_exact(*remaining),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_name_the_loopback_for_wildcards() {
        assert_eq!(
            url_of("0.0.0.0:80".parse().unwrap()),
            "http://127.0.0.1:80/"
        );
        assert_eq!(url_of("[::]:80".parse().unwrap()), "http://[::1]:80/");
        assert_eq!(url_of("[::1]:81".parse().unwrap()), "http://[::1]:81/");
        assert_eq!(
            url_of("192.168.1.5:82".parse().unwrap()),
            "http://192.168.1.5:82/"
        );
    }
}

#[cfg(test)]
#[path = "serve_tests.rs"]
mod connection_tests;
