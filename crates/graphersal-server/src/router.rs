//! Which answer a request gets: the probes and the lifecycle 503s (`graphersal_serve::Server`),
//! the `Host`/`Origin` checks (every request but the probes), then the routes:
//!
//! - `/db/<name>/...`: the database `<name>` (or the database an alias names, resolved per
//!   request), its playground, API and MCP endpoint, with the prefix stripped
//!   ([`crate::mounts`]); `/db/<name>` redirects to `/db/<name>/`; `404` for an unknown name,
//!   `409` for a database that is not loaded;
//! - `GET /api/info`: the server's identity and its UI profile (`uiProfile`, the `server`
//!   profile of [`crate::ui_assets`]);
//! - `/api/admin/...`: the admin API ([`crate::admin_api`]);
//! - `/mcp`: the server-wide MCP endpoint over every database ([`crate::mcp`]; `404` when
//!   `server.toml` turns MCP off);
//! - any other `/api/...` path: `404`;
//! - `GET /server-ui/<file>`: a file of the `server` UI profile ([`crate::ui_assets`]);
//! - every other `GET`: a file of the embedded playground at the root (`/` = its `index.html`,
//!   unchanged; the page shows the `server` profile: the database list and the admin tabs, no
//!   graph), `404` when there is no such file.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use graphersal_serve::{
    HostCheck, HttpError, Lifecycle, Method, Request, Response, ServeError, Ui,
};
use serde_json::json;

use crate::loader::Loader;
use crate::mounts::{self, Databases};

/// The answer to a request ([`Router::answer`]).
pub(crate) enum Answer {
    /// An answer in memory.
    Response(Response),
    /// A file streamed to the client (an export download), removed once it is sent.
    Download(crate::admin_api::Download),
}

/// The state the router answers from.
pub(crate) struct Router {
    /// The lifecycle, the probes and the graph resolver.
    pub(crate) lifecycle: graphersal_serve::Server,
    /// The `Host`/`Origin` checks of the listener.
    pub(crate) host_check: HostCheck,
    /// The loader (and through it the registry), set by [`Self::ready`].
    loader: OnceLock<Arc<Loader>>,
    /// The playground served at the root.
    ui: Ui,
}

impl Router {
    /// A router that is loading (no answer but the probes until [`Self::ready`]).
    pub(crate) fn new(host_check: HostCheck) -> Self {
        Self {
            lifecycle: graphersal_serve::Server::new(),
            host_check,
            loader: OnceLock::new(),
            ui: Ui::embedded(),
        }
    }

    /// The server serves its routes and the databases of `loader`.
    pub(crate) fn ready(&self, loader: Arc<Loader>) -> Result<(), ServeError> {
        self.loader
            .set(loader.clone())
            .map_err(|_| ServeError::Lifecycle("the router was made ready before".to_string()))?;
        self.lifecycle.ready_with(Arc::new(Databases { loader }))
    }

    /// The largest import upload: `server.toml`'s `[limits] upload_bytes` (read per request;
    /// the default while the server is not ready or the file does not read).
    pub(crate) fn upload_limit(&self) -> u64 {
        self.loader
            .get()
            .and_then(|loader| loader.registry().dir().load_config().ok())
            .map_or(crate::config::DEFAULT_UPLOAD_BYTES, |config| {
                config.limits.upload_bytes
            })
    }

    /// Answers one request, a download buffered into the answer (tests).
    #[cfg(test)]
    pub(crate) fn route(&self, request: Request<'_>) -> Response {
        match self.answer(request) {
            Answer::Response(response) => response,
            Answer::Download(download) => download.into_response(),
        }
    }

    /// Answers one request: an in-memory answer, or a file to stream (an export download).
    pub(crate) fn answer(&self, mut request: Request<'_>) -> Answer {
        // Probes first: no Host check (a Kubernetes probe names the pod IP), no lock.
        if let Some(answer) = self.lifecycle.probe(request.method(), request.url()) {
            return Answer::Response(answer);
        }
        // Loading or stopping: the lifecycle's 503.
        let Some(loader) = self.loader.get() else {
            return Answer::Response(self.lifecycle.handle(request));
        };
        if self.lifecycle.state() != Lifecycle::Ready {
            return Answer::Response(self.lifecycle.handle(request));
        }
        if let Err(refused) = self.host_check.check(&request) {
            return Answer::Response(error_response(refused));
        }
        let url = request.url().to_string();
        let (path, query) = match url.split_once('?') {
            Some((path, query)) => (path, Some(query)),
            None => (url.as_str(), None),
        };
        let method = request.method().clone();
        if let Some((name, rest)) = mounts::split_db_path(path) {
            let Ok(entry) = loader.registry().resolve(name) else {
                return Answer::Response(mounts::unknown_database(name));
            };
            let Some(rest) = rest else {
                return Answer::Response(mounts::redirect_to_slash(path, query));
            };
            // Only a check: the resolver takes the context again and holds it for the request.
            if loader.context(&entry.name).is_none() {
                return Answer::Response(mounts::not_loaded(&entry));
            }
            let target = match query {
                Some(query) => format!("/{rest}?{query}"),
                None => format!("/{rest}"),
            };
            return Answer::Response(self.lifecycle.handle(mounts::forward(
                &mut request,
                &target,
                &entry.name,
            )));
        }
        let read = matches!(method, Method::Get | Method::Head);
        let answer = if path == crate::mcp::ENDPOINT {
            match loader.mcp() {
                Some(mcp) => return Answer::Response(crate::mcp::route(mcp, loader, &mut request)),
                None => Err(HttpError::new(
                    404,
                    "the MCP endpoint is off: server.toml [mcp] enabled = false",
                )),
            }
        } else if path == "/api/info" {
            if read {
                info(self.upload_limit(), loader.mcp().map(|mcp| mcp.status()))
            } else {
                Err(not_allowed(&method, path))
            }
        } else if let Some(endpoint) = path.strip_prefix("/api/admin/") {
            let admin = crate::ops::Admin::online(loader.clone());
            if let Some(name) = crate::admin_api::export_target(&method, endpoint) {
                return match crate::admin_api::export(&admin, name, query) {
                    Ok(download) => Answer::Download(download),
                    Err(response) => Answer::Response(response),
                };
            }
            crate::admin_api::route(&admin, &method, endpoint, &mut request)
        } else if path == "/api" || path.starts_with("/api/") {
            Err(HttpError::new(404, format!("unknown endpoint {path}")))
        } else if let Some(name) = path.strip_prefix(crate::ui_assets::FILES_PREFIX) {
            if read {
                profile_file(path, name)
            } else {
                Err(not_allowed(&method, path))
            }
        } else if read {
            self.serve_ui(path)
        } else {
            Err(not_allowed(&method, path))
        };
        Answer::Response(answer.unwrap_or_else(error_response))
    }

    /// A file of the playground at the root (`/` and a directory: its `index.html`).
    fn serve_ui(&self, path: &str) -> Result<Response, HttpError> {
        let not_found = || HttpError::new(404, format!("not found: {path}"));
        let decoded = percent_decode(path).ok_or_else(not_found)?;
        let (bytes, name) = self
            .ui
            .read(decoded.trim_start_matches('/'))
            .ok_or_else(not_found)?;
        Ok(Response::new(200, bytes)
            .with_header("Content-Type", content_type(Path::new(&name)))
            .with_header("Cache-Control", "no-store")
            .with_header("X-Content-Type-Options", "nosniff"))
    }
}

/// A file of the `server` UI profile (`name` = the path after `/server-ui/`), `404` when the
/// profile has no such file.
fn profile_file(path: &str, name: &str) -> Result<Response, HttpError> {
    let (bytes, kind) = crate::ui_assets::file(name)
        .ok_or_else(|| HttpError::new(404, format!("not found: {path}")))?;
    Ok(Response::new(200, bytes.to_vec())
        .with_header("Content-Type", kind)
        .with_header("Cache-Control", "no-store")
        .with_header("X-Content-Type-Options", "nosniff"))
}

/// `405` for `method` on `path`.
fn not_allowed(method: &Method, path: &str) -> HttpError {
    HttpError::new(405, format!("method {method} is not allowed on {path}"))
}

/// The `{"error": message}` answer of a refused request (as `graphersal_serve` answers it).
fn error_response(error: HttpError) -> Response {
    let response = Response::json(error.status(), &json!({ "error": error.message() }));
    if error.status() == 503 {
        response.with_header("Retry-After", "1")
    } else {
        response
    }
}

/// `GET /api/info` at the root: the server's identity (`server: "graphersal-server"`), the
/// host-neutral protocol marker of a Graphersal host (`protocol: "graphersal/1"`) and its UI. The playground served at `/` takes the answer as its engine's because it names its host
/// and carries `uiProfile`, the `server` profile ([`crate::ui_assets::profile`]: the database
/// list and the admin tabs, no workspace, so the page never asks the root for a graph).
/// `uploadBytes` is the largest import upload (a client checks a file's size before it sends
/// it; the profile's modules read it from `api.info.host`); `mcp` is the MCP endpoint's status
/// (`{endpoint: "/mcp", readOnly, client}`), null when MCP is off.
fn info(upload_bytes: u64, mcp: Option<serde_json::Value>) -> Result<Response, HttpError> {
    let profile = crate::ui_assets::profile().map_err(|err| {
        tracing::error!("{err}");
        HttpError::new(500, err)
    })?;
    Ok(Response::json(
        200,
        &json!({
            "server": "graphersal-server",
            "protocol": "graphersal/1",
            "version": env!("CARGO_PKG_VERSION"),
            "experimental": true,
            "uploadBytes": upload_bytes,
            "mcp": mcp,
            "uiProfile": profile,
        }),
    ))
}

/// `%XX` decoded (as `graphersal_serve` decodes static paths); `None` for a broken escape or
/// a result that is not UTF-8.
fn percent_decode(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = path.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The content type of a playground file (the types `graphersal_serve` serves).
fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("wasm") => "application/wasm",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("md") | Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Registry;
    use crate::registry::tests::server;
    use crate::test_support::TempDir;
    use graphersal_serve::Exposure;
    use serde_json::Value;

    const PORT: u16 = 8484;

    fn router() -> Router {
        Router::new(HostCheck::new(
            Exposure::Loopback,
            PORT,
            "http://127.0.0.1:8484/",
        ))
    }

    /// Makes `router` ready over an empty server directory (kept alive by the answer).
    fn make_ready(router: &Router) -> TempDir {
        let temp = TempDir::new("router");
        let dir = server(&temp);
        let loader = Loader::new(Registry::scan(&dir).unwrap(), router.host_check.clone());
        router.ready(loader).unwrap();
        temp
    }

    fn get(router: &Router, url: &str, host: Option<&str>) -> Response {
        let mut request = Request::new(Method::Get, url, std::io::empty());
        if let Some(host) = host {
            request = request.with_header("Host", host);
        }
        router.route(request)
    }

    fn json_of(response: &Response) -> Value {
        serde_json::from_slice(response.body()).unwrap()
    }

    #[test]
    fn probes_answer_while_loading_and_the_rest_waits() {
        let router = router();
        assert_eq!(get(&router, "/healthz", None).status(), 200);
        let readyz = get(&router, "/readyz", None);
        assert_eq!(readyz.status(), 503);
        assert_eq!(json_of(&readyz)["status"], "loading");
        assert_eq!(
            get(&router, "/api/info", Some("127.0.0.1:8484")).status(),
            503
        );
    }

    #[test]
    fn ready_routes() {
        let router = router();
        let _temp = make_ready(&router);
        assert_eq!(json_of(&get(&router, "/readyz", None))["status"], "ready");
        let info = get(&router, "/api/info?x=1", Some("localhost:8484"));
        assert_eq!(info.status(), 200);
        let info = json_of(&info);
        assert!(
            info.get("admin").is_none(),
            "the root shows the profile, not a landing"
        );
        assert_eq!(info["server"], "graphersal-server");
        assert_eq!(info["protocol"], "graphersal/1");
        assert!(info.get("product").is_none());
        assert_eq!(info["experimental"], true);
        assert_eq!(info["uiProfile"]["name"], "server");
        assert_eq!(info["uiProfile"]["version"], env!("CARGO_PKG_VERSION"));
        let landing = get(&router, "/", Some("127.0.0.1:8484"));
        assert_eq!(landing.status(), 200);
        let page = String::from_utf8_lossy(landing.body()).into_owned();
        assert!(page.contains("<html"));
        assert!(
            !page.contains("server-ui/"),
            "index.html is served unchanged: {page}"
        );
        let module = get(
            &router,
            "/server-ui/databases-tab.js",
            Some("127.0.0.1:8484"),
        );
        assert_eq!(module.status(), 200);
        assert!(module.headers().iter().any(
            |(name, value)| name == "Content-Type" && value == "text/javascript; charset=utf-8"
        ));
        assert_eq!(
            get(&router, "/server-ui/nope.js", Some("127.0.0.1:8484")).status(),
            404
        );
        assert_eq!(
            get(
                &router,
                "/server-ui/tests/profile.test.mjs",
                Some("127.0.0.1:8484")
            )
            .status(),
            404
        );
        let other = get(&router, "/js/main.js", Some("127.0.0.1:8484"));
        assert_eq!(other.status(), 200);
        assert!(!String::from_utf8_lossy(other.body()).contains("server-ui.css"));
        let missing = get(&router, "/db/x/api/info", Some("127.0.0.1:8484"));
        assert_eq!(missing.status(), 404);
        assert!(json_of(&missing)["error"].as_str().unwrap().contains("'x'"));
        let post = router.route(
            Request::new(Method::Post, "/api/info", std::io::empty())
                .with_header("Host", "127.0.0.1:8484"),
        );
        assert_eq!(post.status(), 405);
    }

    #[test]
    fn foreign_hosts_and_origins_are_refused_but_probes_are_not() {
        let router = router();
        let _temp = make_ready(&router);
        assert_eq!(get(&router, "/api/info", None).status(), 403);
        assert_eq!(
            get(&router, "/api/info", Some("evil.example:8484")).status(),
            403
        );
        let cross = router.route(
            Request::new(Method::Get, "/api/info", std::io::empty())
                .with_header("Host", "127.0.0.1:8484")
                .with_header("Origin", "http://evil.example"),
        );
        assert_eq!(cross.status(), 403);
        assert_eq!(
            get(&router, "/healthz", Some("10.0.0.5:8484")).status(),
            200
        );
    }

    #[test]
    fn stopping_answers_503() {
        let router = router();
        let _temp = make_ready(&router);
        router.lifecycle.stop();
        assert_eq!(
            json_of(&get(&router, "/readyz", None))["status"],
            "stopping"
        );
        assert_eq!(get(&router, "/healthz", None).status(), 200);
        assert_eq!(
            get(&router, "/api/info", Some("127.0.0.1:8484")).status(),
            503
        );
    }
}
