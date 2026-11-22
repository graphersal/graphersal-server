//! `graphersal-server serve`: the HTTP listener of a server directory.
//!
//! [`start`] opens the server directory, takes its `server.lock` (a second server on the same
//! directory is refused with [`ServerError::Locked`]), binds the listen address (`--listen`
//! over `server.toml`'s `listen`; port `0` picks a free port), writes the bound address into
//! `server.run`, scans the database registry ([`Registry::scan`]; `/readyz` answers
//! `503 loading` until it is done), starts loading the `auto_load` databases in the background
//! ([`Loader::auto_load`]), and serves on a `tiny_http` listener (adapted from the graphersal CLI's dev
//! server): one accept thread, one thread per request. [`Running::stop`] stops it: the
//! lifecycle answers `503 stopping`, the accept loop ends, running requests get a moment to
//! finish, every database is unloaded ([`Loader::shutdown`]: its Store closed cleanly),
//! `server.run` is removed and the lock released.
//!
//! Safety: the server has no access control. The default listen address is the loopback; any
//! other address logs a warning. The `Host`/`Origin` checks of `graphersal_serve` (DNS
//! rebinding, cross-site pages) run on every request but the probes.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use graphersal_serve::{Exposure, HostCheck, Method};

use crate::error::ServerError;
use crate::layout::ServerDir;
use crate::loader::Loader;
use crate::lock::ServerLock;
use crate::registry::Registry;
use crate::router::{Answer, Router};
use crate::run_file::RunInfo;

/// Largest declared body (`Content-Length`) a request may name (as the graphersal CLI's dev
/// server), except an import upload (`POST /api/admin/import` with a body that is not JSON),
/// which may declare up to `server.toml`'s `[limits] upload_bytes` (its route reads the whole
/// body into `tmp/`); an upload declaring more is answered `413` without being read and its
/// connection abandoned. `tiny_http` 0.12 skips an unread body when the request is dropped by allocating ONE
/// zeroed buffer of the remaining declared length, so a request declaring, say, 2^62 bytes would
/// abort the process. Such a request is never dropped: it gets no answer and its connection is
/// leaked (logged). A smaller declared body that was not read is drained in small reads before
/// the answer.
pub const MAX_DECLARED_BODY_BYTES: usize = 4 * graphersal_serve::MAX_BODY_BYTES;

/// How long [`Running::stop`] waits for running requests.
const STOP_GRACE: Duration = Duration::from_secs(5);

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

/// What the listener threads share.
struct Shared {
    router: Router,
    /// Set by [`Running::stop`] before the accept loop is unblocked.
    stopping: AtomicBool,
    /// The requests being handled.
    in_flight: AtomicUsize,
}

/// A running server; [`Running::stop`] (or dropping it) stops it.
pub struct Running {
    dir: ServerDir,
    shared: Arc<Shared>,
    http: Option<Arc<tiny_http::Server>>,
    accept: Option<JoinHandle<()>>,
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
    let http = tiny_http::Server::http(listen).map_err(|err| ServerError::Bind {
        address: listen.to_string(),
        message: err.to_string(),
    })?;
    let address = http
        .server_addr()
        .to_ip()
        .ok_or_else(|| ServerError::Bind {
            address: listen.to_string(),
            message: "the listener has no IP address".to_string(),
        })?;
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
        stopping: AtomicBool::new(false),
        in_flight: AtomicUsize::new(0),
    });
    let http = Arc::new(http);
    RunInfo::new(address, url.clone()).write(&dir)?;
    let accept = {
        let shared = shared.clone();
        let http = http.clone();
        std::thread::Builder::new()
            .name("graphersal-server-accept".to_string())
            .spawn(move || accept(&http, &shared))
    };
    let accept = match accept {
        Ok(handle) => handle,
        Err(err) => {
            let _ = RunInfo::remove(&dir);
            return Err(ServerError::Thread {
                message: err.to_string(),
            });
        }
    };
    let registry = Registry::empty(&dir);
    let loader = Loader::new(registry.clone(), host_check.clone());
    let mut running = Running {
        dir,
        shared,
        http: Some(http),
        accept: Some(accept),
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
            running.loader = Loader::new(registry.clone(), host_check);
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
    if exposure == Exposure::Network {
        tracing::warn!(
            target: "graphersal_server",
            "graphersal-server listens on {address} and is reachable from the network: it has \
             NO access control, anyone who reaches this port can read and change every \
             database; listen on 127.0.0.1 for this machine only"
        );
    }
    tracing::info!(
        target: "graphersal_server",
        "serving {} on {}",
        running.dir.root().display(),
        running.url
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
        let Some(http) = self.http.take() else {
            return Ok(());
        };
        self.shared.router.lifecycle.stop();
        self.shared.stopping.store(true, Ordering::SeqCst);
        http.unblock();
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
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
        // Closes the listening socket (the last reference: the accept thread has ended).
        drop(http);
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

/// The accept loop: every request on a thread of its own, until [`Running::stop`].
fn accept(http: &tiny_http::Server, shared: &Arc<Shared>) {
    loop {
        let request = match http.recv() {
            Ok(request) => request,
            Err(_) if shared.stopping.load(Ordering::SeqCst) => break,
            Err(err) => {
                tracing::warn!(target: "graphersal_server", "accepting a request failed: {err}");
                continue;
            }
        };
        shared.in_flight.fetch_add(1, Ordering::SeqCst);
        let worker = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("graphersal-server-http".to_string())
            .spawn(move || {
                let _done = InFlight(worker.clone());
                handle(&worker, request);
            });
        if let Err(err) = spawned {
            // The closure (and with it the request and its counter guard) is gone.
            tracing::warn!(target: "graphersal_server", "cannot start a request thread: {err}");
        }
    }
}

/// A request being handled: counted down however its handler ends.
struct InFlight(Arc<Shared>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Converts one `tiny_http` request, answers it through the router, and writes the answer.
fn handle(shared: &Shared, mut request: tiny_http::Request) {
    let declared = request.body_length();
    let headers: Vec<(String, String)> = request
        .headers()
        .iter()
        .map(|h| (h.field.as_str().to_string(), h.value.as_str().to_string()))
        .collect();
    let method = Method::parse(request.method().as_str());
    let url = request.url().to_string();
    let content_type = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("Content-Type"))
        .map(|(_, value)| value.as_str());
    let path = url.split_once('?').map_or(url.as_str(), |(path, _)| path);
    let upload = crate::admin_api::is_upload(&method, path, content_type);
    // An import upload may declare up to the upload limit (its route reads the whole body);
    // every other request up to MAX_DECLARED_BODY_BYTES.
    let limit = if upload {
        usize::try_from(shared.router.upload_limit())
            .unwrap_or(usize::MAX)
            .max(MAX_DECLARED_BODY_BYTES)
    } else {
        MAX_DECLARED_BODY_BYTES
    };
    if declared.is_some_and(|len| len > limit) {
        let len = declared.unwrap_or_default();
        if upload {
            refuse_oversize_upload(shared, request, &url, &headers, len);
        } else {
            tracing::warn!(
                target: "graphersal_server",
                "a request declared a body of {len} bytes; it is ignored (no answer)"
            );
            std::mem::forget(request);
        }
        return;
    }
    let response = {
        let mut converted = graphersal_serve::Request::new(method, &url, request.as_reader())
            .with_body_length(declared);
        for (name, value) in &headers {
            converted = converted.with_header(name, value);
        }
        shared.router.answer(converted)
    };
    // The unread rest of a declared body, in small reads (see MAX_DECLARED_BODY_BYTES).
    if let Some(len) = declared {
        let limit = u64::try_from(len).unwrap_or(u64::MAX);
        let _ = io::copy(&mut request.as_reader().take(limit), &mut io::sink());
    }
    match response {
        Answer::Response(response) => {
            let _ = request.respond(tiny_answer(response));
        }
        Answer::Download(download) => {
            let headers = download.headers();
            let length = usize::try_from(download.length()).unwrap_or(usize::MAX);
            let (file, guard) = download.into_parts();
            let mut response = tiny_http::Response::new(
                tiny_http::StatusCode(200),
                Vec::new(),
                file,
                Some(length),
                None,
            )
            // `Content-Length` (a progress bar), never chunked.
            .with_chunked_threshold(usize::MAX);
            for (name, value) in &headers {
                if let Ok(header) = tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes())
                {
                    response = response.with_header(header);
                }
            }
            if let Err(err) = request.respond(response) {
                tracing::warn!(target: "graphersal_server", "an export download broke off: {err}");
            }
            // The file is closed (the response is gone): now the export is removed.
            drop(guard);
        }
    }
}

/// Answers an import upload that declares more than the upload limit with `413` (`403` when
/// the `Host`/`Origin` checks refuse it) WITHOUT reading its body, then abandons the
/// connection: `tiny_http` 0.12 would allocate the unread declared rest in one buffer when the
/// request is dropped (see [`MAX_DECLARED_BODY_BYTES`]), so the answer is written through
/// `Request::upgrade` (which adds `Connection: upgrade` and `Upgrade` headers, and leaves out
/// `Content-Length`: the JSON body follows the head and the connection stays open) and the
/// stream is leaked, never dropped (logged). A client checks the size against the limit first
/// (`uploadBytes` of `GET /api/info`).
fn refuse_oversize_upload(
    shared: &Shared,
    request: tiny_http::Request,
    url: &str,
    headers: &[(String, String)],
    declared: usize,
) {
    let mut probe = graphersal_serve::Request::new(Method::Post, url, io::empty());
    for (name, value) in headers {
        probe = probe.with_header(name, value);
    }
    let answer = match shared.router.host_check.check(&probe) {
        Err(refused) => graphersal_serve::Response::json(
            refused.status(),
            &serde_json::json!({ "error": refused.message() }),
        ),
        Ok(()) => graphersal_serve::Response::json(
            413,
            &serde_json::json!({
                "error": format!(
                    "the body is larger than {} bytes (it declares {declared})",
                    shared.router.upload_limit()
                ),
                "help": "the server's upload limit is [limits] upload_bytes in server.toml",
            }),
        ),
    };
    tracing::warn!(
        target: "graphersal_server",
        "an import upload declared {declared} bytes, over the upload limit; refused with {} \
         without reading it (its connection is abandoned)",
        answer.status()
    );
    // `upgrade` writes the head only, and `tiny_http` keeps `Content-Length` out of it: the
    // body follows without a length (the client reads it until it gives up or closes).
    let (status, headers, body) = answer.into_parts();
    let mut head = tiny_http::Response::empty(status);
    for (name, value) in &headers {
        if let Ok(header) = tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes()) {
            head = head.with_header(header);
        }
    }
    let mut stream = request.upgrade("graphersal-refused", head);
    if let Err(err) = stream.write_all(&body).and_then(|()| stream.flush()) {
        tracing::warn!(target: "graphersal_server", "the 413 answer broke off: {err}");
    }
    std::mem::forget(stream);
}

/// An answer as `tiny_http` writes it (a header `tiny_http` cannot carry is left out).
fn tiny_answer(answer: graphersal_serve::Response) -> tiny_http::Response<io::Cursor<Vec<u8>>> {
    let (status, headers, body) = answer.into_parts();
    let mut response = tiny_http::Response::from_data(body).with_status_code(status);
    for (name, value) in &headers {
        if let Ok(header) = tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes()) {
            response = response.with_header(header);
        }
    }
    response
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
