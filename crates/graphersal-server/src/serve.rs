//! `graphersal-server serve`: the HTTP listener of a server directory.
//!
//! [`start`] opens the server directory, takes its `server.lock` (a second server on the same
//! directory is refused with [`ServerError::Locked`]), binds the listen address (`--listen`
//! over `server.toml`'s `listen`; port `0` picks a free port), writes the bound address into
//! `server.run`, scans the database registry ([`Registry::scan`]; `/readyz` answers
//! `503 loading` until it is done), starts loading the `auto_load` databases in the background
//! ([`Loader::auto_load`]), shares the model blobs of every idle database through `models/`
//! on a background thread ([`crate::ops::Admin::adopt_all`]), and serves HTTP/1.1 with
//! `hyper` server connections on a `tokio` multi-thread runtime that [`Running`] owns.
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
//! is closed, so the answer is never lost to a reset.
//!
//! [`Running::stop`] stops it: the lifecycle answers `503 stopping`, the listening socket is
//! closed, idle connections are closed and running requests get five seconds to finish,
//! every database is unloaded ([`Loader::shutdown`]: its Store closed cleanly), the runtime is
//! shut down, `server.run` is removed and the lock released.
//!
//! Safety: the server has no access control. The default listen address is the loopback; any
//! other address gets a warning in `serve`'s start block (the console; [`Running::exposure`]
//! tells a library caller). The `Host`/`Origin` checks of `graphersal_serve` (DNS
//! rebinding, cross-site pages) run on every request but the probes.

use std::convert::Infallible;
use std::fs::File;
use std::future::Future;
use std::io::{self, ErrorKind, Read};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use graphersal_serve::{Exposure, HostCheck, Method};
use hyper::body::{Body, Bytes, Frame, Incoming, SizeHint};
use hyper::header::{CONNECTION, CONTENT_LENGTH, EXPECT, HeaderName, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{HeaderMap, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::runtime::{Handle, Runtime};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

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

/// The options of [`start`].
///
/// `#[non_exhaustive]`: build it with [`ServeOptions::new`] and its setters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ServeOptions {
    /// The listen address, overriding `server.toml`'s `listen`.
    pub listen: Option<SocketAddr>,
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
}

/// What the connections share.
struct Shared {
    router: Router,
    /// The requests being answered (a download until its last byte is handed over).
    in_flight: AtomicUsize,
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
    });
    RunInfo::new(address, url.clone()).write(&dir)?;
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
    if let Err(err) = running.shared.router.ready(running.loader.clone()) {
        let _ = running.shutdown();
        return Err(ServerError::Thread {
            message: err.to_string(),
        });
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
            Exposure::Network => "reachable from the network, no access control",
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

    /// Whether the server is reachable from the network (no access control!).
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
                    tokio::spawn(connection(stream, shared.clone(), stop.clone()));
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

/// The boxed future of one answer (hyper wants an `Unpin` service future).
type Answering =
    Pin<Box<dyn Future<Output = Result<hyper::Response<Outgoing>, Infallible>> + Send>>;

/// Serves one connection until it ends, then closes it with a lingering close.
async fn connection(
    stream: tokio::net::TcpStream,
    shared: Arc<Shared>,
    mut stop: watch::Receiver<bool>,
) {
    let service = service_fn(move |request: hyper::Request<Incoming>| -> Answering {
        Box::pin(respond(shared.clone(), request))
    });
    let mut conn = http1::Builder::new()
        .timer(TokioTimer::new())
        .title_case_headers(true)
        .serve_connection(TokioIo::new(stream), service);
    let mut stopping = *stop.borrow();
    let mut shut = false;
    let result = loop {
        if stopping && !shut {
            shut = true;
            Pin::new(&mut conn).graceful_shutdown();
        }
        tokio::select! {
            result = std::future::poll_fn(|cx| conn.poll_without_shutdown(cx)) => break result,
            _ = stop.changed(), if !stopping => stopping = true,
        }
    };
    if let Err(err) = result {
        tracing::debug!(target: "graphersal_server", "a connection ended: {err}");
    }
    linger(conn.into_parts().io.into_inner()).await;
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

/// A request being answered: counted down however its answer ends.
struct InFlight(Arc<Shared>);

impl InFlight {
    fn new(shared: &Arc<Shared>) -> Self {
        shared.in_flight.fetch_add(1, Ordering::SeqCst);
        Self(shared.clone())
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
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
    request: hyper::Request<Incoming>,
) -> Result<hyper::Response<Outgoing>, Infallible> {
    let in_flight = InFlight::new(&shared);
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
    if reply.close {
        response
            .headers_mut()
            .insert(CONNECTION, HeaderValue::from_static("close"));
    }
    Ok(response)
}

/// The blocking part of a request: the limits, the router, the unread body.
fn answer(shared: &Shared, request: hyper::Request<Incoming>, handle: Handle) -> Reply {
    let (parts, body) = request.into_parts();
    let headers = header_list(&parts.headers);
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
