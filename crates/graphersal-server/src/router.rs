//! Which answer a request gets: the probes and the lifecycle 503s (`graphersal_serve::Server`),
//! the `Host`/`Origin` checks, then the server's own routes.
//!
//! Routes of this version: `GET /healthz`, `GET /readyz`, `GET /` (a placeholder text),
//! `GET /api/info` (the server's identity, announcing `admin`). Everything else is a `404`
//! answered by the graph resolver [`NoDatabases`] (databases come in a later version).

use std::sync::Arc;

use graphersal_serve::{
    Context, GraphResolver, HostCheck, HttpError, Lifecycle, Method, Request, Response,
};
use serde_json::json;

/// The state the router answers from.
pub(crate) struct Router {
    /// The lifecycle, the probes and the graph resolver.
    pub(crate) lifecycle: graphersal_serve::Server,
    /// The `Host`/`Origin` checks of the listener.
    pub(crate) host_check: HostCheck,
}

/// The graph resolver of a server without databases: every request it is asked about is a
/// `404`.
pub(crate) struct NoDatabases;

impl GraphResolver for NoDatabases {
    fn resolve(&self, request: &Request<'_>) -> Result<Arc<Context>, HttpError> {
        Err(HttpError::new(
            404,
            format!(
                "not found: {} {} (this graphersal-server version serves no databases yet)",
                request.method(),
                path_of(request.url())
            ),
        ))
    }
}

/// The path of a request target (the query string cut off).
fn path_of(url: &str) -> &str {
    url.split('?').next().unwrap_or_default()
}

impl Router {
    /// A router that is loading (no answer but the probes until [`Self::ready`]).
    pub(crate) fn new(host_check: HostCheck) -> Self {
        Self {
            lifecycle: graphersal_serve::Server::new(),
            host_check,
        }
    }

    /// The server serves its routes.
    pub(crate) fn ready(&self) -> Result<(), graphersal_serve::ServeError> {
        self.lifecycle.ready_with(Arc::new(NoDatabases))
    }

    /// Answers one request.
    pub(crate) fn route(&self, request: Request<'_>) -> Response {
        // Probes first: no Host check (a Kubernetes probe names the pod IP), no lock.
        if let Some(answer) = self.lifecycle.probe(request.method(), request.url()) {
            return answer;
        }
        // Loading or stopping: the lifecycle's 503.
        if self.lifecycle.state() != Lifecycle::Ready {
            return self.lifecycle.handle(request);
        }
        if let Err(refused) = self.host_check.check(&request) {
            return error_response(refused);
        }
        let path = path_of(request.url());
        match path {
            "/" | "/api/info" => {
                if !matches!(request.method(), Method::Get | Method::Head) {
                    return error_response(HttpError::new(
                        405,
                        format!("method {} is not allowed on {path}", request.method()),
                    ));
                }
                if path == "/" { landing() } else { info() }
            }
            _ => self.lifecycle.handle(request),
        }
    }
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

/// `GET /`: a placeholder until the playground's server mode is served here.
fn landing() -> Response {
    let text = format!(
        "graphersal-server {} (EXPERIMENTAL)\n\nThis server serves no databases yet; the \
         playground with the database list and the admin panel comes in a later version.\n\
         Probes: /healthz, /readyz. Server information: /api/info.\n",
        env!("CARGO_PKG_VERSION")
    );
    Response::new(200, text.into_bytes())
        .with_header("Content-Type", "text/plain; charset=utf-8")
        .with_header("Cache-Control", "no-store")
        .with_header("X-Content-Type-Options", "nosniff")
}

/// `GET /api/info`: the server's identity; `admin: true` tells the playground that this server
/// has the admin API (the playground's server mode checks it).
fn info() -> Response {
    Response::json(
        200,
        &json!({
            "server": "graphersal-server",
            "version": env!("CARGO_PKG_VERSION"),
            "experimental": true,
            "admin": true,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
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
        router.ready().unwrap();
        assert_eq!(json_of(&get(&router, "/readyz", None))["status"], "ready");
        let info = get(&router, "/api/info?x=1", Some("localhost:8484"));
        assert_eq!(info.status(), 200);
        let info = json_of(&info);
        assert_eq!(info["admin"], true);
        assert_eq!(info["server"], "graphersal-server");
        assert_eq!(info["experimental"], true);
        let landing = get(&router, "/", Some("127.0.0.1:8484"));
        assert_eq!(landing.status(), 200);
        assert!(String::from_utf8_lossy(landing.body()).contains("EXPERIMENTAL"));
        let missing = get(&router, "/db/x/api/info", Some("127.0.0.1:8484"));
        assert_eq!(missing.status(), 404);
        assert!(
            json_of(&missing)["error"]
                .as_str()
                .unwrap()
                .contains("/db/x/api/info")
        );
        let post = router.route(
            Request::new(Method::Post, "/api/info", std::io::empty())
                .with_header("Host", "127.0.0.1:8484"),
        );
        assert_eq!(post.status(), 405);
    }

    #[test]
    fn foreign_hosts_and_origins_are_refused_but_probes_are_not() {
        let router = router();
        router.ready().unwrap();
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
        router.ready().unwrap();
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
