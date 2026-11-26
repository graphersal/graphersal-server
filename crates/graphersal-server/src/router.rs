//! Which answer a request gets: the probes and the lifecycle 503s (`graphersal_serve::Server`),
//! the `Host`/`Origin` checks (every request but the probes), the browser login and logout,
//! then authentication ([`Access::authenticate`]: wrong credentials are `401`, never anonymous;
//! the principal's identity goes onto the request), then the routes, each refused per
//! principal:
//!
//! - `POST /api/login {"token"}`: a browser session for a user token, its id in the
//!   `gs_session` cookie (`Max-Age=<the session lifetime (default 30 days), else 400 days>; HttpOnly;
//!   SameSite=Strict; Path=/`, plus `Secure` when `[http] public_scheme = "https"`: a browser
//!   restart keeps the login); `POST /api/logout` ends it and clears the cookie (`Max-Age=0`).
//!   Both run before authentication (a stale cookie must not lock a browser out of logging in
//!   again).
//! - `/db/<name>/...`: the database `<name>` (or the database an alias names, resolved per
//!   request), its playground, API and MCP endpoint, with the prefix stripped
//!   ([`crate::mounts`]), for a principal granted at least `read` on it: a grant on the
//!   database's name (or `*`), or, when the request names an alias, a grant on the alias (it
//!   reaches the alias's current target only through the alias name). Without a grant the
//!   database does not exist for the principal: anonymous gets `401` (for EVERY name, so
//!   nothing about existence leaks; a `GET` of a page or static file `303` to the login page
//!   `/?next=<path>`), a logged-in principal the unknown-name `404`. A `read` grant marks the
//!   forwarded request read-only (`Request::mark_read_only`: the reading authorizer, write
//!   routes `409`, the read-only MCP profile); the database's own mode still applies in its
//!   context. `/db/<name>` redirects to `/db/<name>/`; `409` for a database that is not
//!   loaded.
//! - `GET /api/info`: the server's identity, its UI profile (`uiProfile`) and the principal
//!   (`principal {id, kind, admin}`); for anonymous only what the login page needs (`server`,
//!   `protocol`, `dev`, `experimental`, `uiProfile`, `principal`, `login: "/"`); everyone gets
//!   `openapi` (`/openapi.json`) and `databaseOpenapi` (`/db/{name}/openapi.json`).
//! - `GET /openapi.json`: the server's own OpenAPI description, public (before
//!   authentication, like the UI files); `GET /db/<name>/openapi.json` (and
//!   `/db/<name>/api/openapi.json`): the database's, behind the grant check of the mount but
//!   also while it is not loaded ([`crate::openapi`]).
//! - `GET /api/databases`: the databases and aliases the principal may reach, with its level
//!   and the playground path of each ([`crate::user_api`]; anonymous: the ones its anonymous
//!   grants open, often none), the web UI's "my databases" page.
//! - `/api/admin/...` (the admin API, Runs included; [`crate::admin_api`]; the server itself,
//!   `server` and `shutdown`: [`crate::control`]): admins only;
//!   anonymous gets `401`, a logged-in principal without the admin role `404` as if the API did
//!   not exist. The admin core records its events with the principal as the actor.
//! - `/mcp`: the server-wide MCP endpoint over the databases the principal may see
//!   ([`crate::mcp`]; `404` when `server.toml` turns MCP off, `503` while `mcp.toml` switches
//!   it off: [`crate::mcp_control`]). `/db/<name>/mcp` follows the same switch, answers `404`
//!   for a name MCP does not offer and is marked read-only for a name MCP serves read-only;
//!   the tool calls of both are counted ([`crate::mcp_control::McpUsage`]).
//! - any other `/api/...` path: `404`;
//! - `GET /server-ui/<file>`: a file of the `server` UI profile ([`crate::ui_assets`]);
//! - every other `GET`: a file of the embedded playground at the root (`/` = its `index.html`,
//!   unchanged; the page shows the `server` profile of the principal: the admin tabs, a user's
//!   "My databases" or anonymous's "Sign in"; no graph), `404` when there is no such file. These files are public (the login page is one).
//!
//! A `401` is `{"error", "login": "/"}` with `WWW-Authenticate: Bearer`; a refused session
//! cookie is cleared with it.
//!
//! Every answer the server sends carries `Cache-Control: no-store` exactly once ([`no_store`]):
//! the API answers (root, admin and each database's), the UI files, the errors and redirects.
//! Every in-memory answer also carries `X-Frame-Options: DENY` and a `Content-Security-Policy`
//! ([`framing_refused`], SEC L6).
//! A browser never shows an answer it kept from an earlier visit (the web UI reads every tab's
//! data again on each visit).
//!
//! Access control off (`server.toml [access] enabled = false`): nothing is refused for its
//! principal. Valid admin credentials keep their identity (the operator, an admin user); every
//! other request (none, a user without the admin role, wrong credentials) runs as the admin
//! principal `everyone` ([`Principal::everyone`]); the audit names the actor either way. Login
//! and logout still work (a session is written to `sessions/`), but nobody needs them.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use graphersal_serve::{
    HostCheck, HttpError, Lifecycle, Method, Request, Response, ServeError, Ui,
};
use serde_json::json;

use crate::access::{Access, AuthError, Level, Principal, SESSION_COOKIE};
use crate::loader::Loader;
use crate::mounts::{self, Databases};

/// The login page of the server (its UI at the root), named in every `401`.
pub(crate) const LOGIN_PAGE: &str = "/";
/// The browser login route.
pub(crate) const LOGIN: &str = "/api/login";
/// The browser logout route.
pub(crate) const LOGOUT: &str = "/api/logout";

/// The host name every `GET /api/info` answers as `server`: the root's and each database
/// mount's (`ContextBuilder::server_name`), so a client tells this server from the CLI's dev
/// server (`graphersal-dev`). The playground recognises a Graphersal host by `protocol`
/// ([`graphersal_serve::PROTOCOL`]), not by this name.
pub(crate) const SERVER_NAME: &str = "graphersal-server";

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
    /// The access state, set by [`Self::ready`].
    access: OnceLock<Arc<Access>>,
    /// The playground served at the root.
    ui: Ui,
    /// The modes of `server.toml` the router answers in (access control, cookies).
    modes: Modes,
    /// The server's own facts for `GET /api/admin/server` ([`crate::control`]).
    facts: Option<crate::control::ServerFacts>,
    /// What `POST /api/admin/shutdown` calls ([`crate::control`]).
    stop_hook: Option<crate::serve::StopHook>,
}

/// The `Max-Age` of the login cookie without a session lifetime (`[sessions] lifetime_secs =
/// 0`): 400 days, the longest a browser keeps a cookie.
pub(crate) const DEFAULT_COOKIE_MAX_AGE_SECS: u64 = 400 * 24 * 60 * 60;

/// The modes of `server.toml` the router answers in, read at start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Modes {
    /// `[access] enabled`: `false` = every request runs as an admin ([`open_principal`]).
    pub(crate) access_enabled: bool,
    /// `[http] public_scheme = "https"`: the login cookie gets `Secure`, and the API
    /// descriptions name `https://` URLs ([`crate::openapi`]).
    pub(crate) secure_cookies: bool,
    /// The login cookie's `Max-Age` in seconds: the session lifetime (`[sessions]
    /// lifetime_secs`, default 30 days), else [`DEFAULT_COOKIE_MAX_AGE_SECS`] (a browser
    /// restart keeps the login).
    pub(crate) cookie_max_age_secs: u64,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            access_enabled: true,
            secure_cookies: false,
            cookie_max_age_secs: crate::config::DEFAULT_SESSION_LIFETIME_SECS,
        }
    }
}

impl Modes {
    /// The modes of `config`.
    pub(crate) fn of(config: &crate::config::ServerConfig) -> Self {
        Self {
            access_enabled: config.access.enabled,
            secure_cookies: config.http.public_scheme == crate::config::PublicScheme::Https,
            cookie_max_age_secs: config
                .sessions
                .effective_lifetime_secs()
                .unwrap_or(DEFAULT_COOKIE_MAX_AGE_SECS),
        }
    }

    /// The `Set-Cookie` value of a new browser session `id`.
    fn session_cookie(&self, id: &str) -> String {
        self.cookie(id, self.cookie_max_age_secs)
    }

    /// The `Set-Cookie` value that ends the browser session cookie.
    fn cleared_session_cookie(&self) -> String {
        self.cookie("", 0)
    }

    fn cookie(&self, value: &str, max_age: u64) -> String {
        let secure = if self.secure_cookies { "; Secure" } else { "" };
        format!(
            "{SESSION_COOKIE}={value}; Max-Age={max_age}; HttpOnly; SameSite=Strict; Path=/{secure}"
        )
    }
}

/// The principal of a request while access control is off: valid admin credentials keep their
/// identity (the console's operator, an admin user: the audit names them); anything else
/// (no credentials, a user without the admin role, refused credentials) is `everyone`, an
/// admin too.
fn open_principal(authenticated: Result<Principal, AuthError>) -> Principal {
    match authenticated {
        Ok(principal) if principal.is_admin() => principal,
        _ => Principal::everyone(),
    }
}

/// The database route that defines a scheduled job (below `/db/<name>/`).
const JOBS_DEFINE: &str = "api/jobs/define";

/// The help of a refused absolute backup job path.
pub(crate) const BACKUP_PATH_HELP: &str = "give a path relative to the server's backup root \
     (server.toml [jobs] backup_root), for example nightly: the backups go to \
     <backup root>/nightly";

/// The body of a `jobs/define` request, read (at most `graphersal_serve`'s JSON body limit) to
/// check it before it is forwarded: `Err` is the answer instead. A body that is not a JSON
/// object, or has no string `path`, is forwarded unchanged (the context reports it). A
/// backup job's absolute `path` is refused like the engine's own field errors (`200`
/// `{"ok": false, "errors": [{"field": "path", "message", "help"}]}`), so the playground's job
/// form shows it on the path field.
fn jobs_define_body(request: &mut Request<'_>) -> Result<Vec<u8>, Response> {
    let text = request.text_body().map_err(error_response)?;
    let path = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|body| body.get("path")?.as_str().map(|p| p.trim().to_string()))
        .filter(|path| !path.is_empty());
    match path {
        Some(path) if is_absolute_path(&path) => Err(Response::json(
            200,
            &json!({
                "ok": false,
                "errors": [{
                    "field": "path",
                    "message": format!(
                        "'{path}' is an absolute path: backup jobs write only below the \
                         server's backup root"
                    ),
                    "help": BACKUP_PATH_HELP,
                }],
            }),
        )),
        _ => Ok(text.into_bytes()),
    }
}

/// Whether `path` is absolute on any platform this server runs on: rooted (`/x`, `\x`) or
/// with a Windows prefix (`C:`, `\\server\share`), also a Windows path on Unix (a name such
/// as `c:x` is refused there too).
fn is_absolute_path(path: &str) -> bool {
    let native = Path::new(path);
    if native.is_absolute()
        || native.has_root()
        || matches!(
            native.components().next(),
            Some(std::path::Component::Prefix(_))
        )
    {
        return true;
    }
    let bytes = path.as_bytes();
    path.starts_with('\\')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

impl Router {
    /// A router that is loading (no answer but the probes until [`Self::ready`]).
    pub(crate) fn new(host_check: HostCheck) -> Self {
        Self {
            lifecycle: graphersal_serve::Server::new(),
            host_check,
            loader: OnceLock::new(),
            access: OnceLock::new(),
            ui: Ui::embedded(),
            modes: Modes::default(),
            facts: None,
            stop_hook: None,
        }
    }

    /// The router with the server's facts and its stop hook ([`crate::control`]).
    pub(crate) fn with_control(
        mut self,
        facts: crate::control::ServerFacts,
        stop_hook: Option<crate::serve::StopHook>,
    ) -> Self {
        self.facts = Some(facts);
        self.stop_hook = stop_hook;
        self
    }

    /// The router answering in `modes` (`server.toml`'s, read at start).
    pub(crate) fn with_modes(mut self, modes: Modes) -> Self {
        self.modes = modes;
        self
    }

    /// The modes the router answers in now: access control as at start, the login cookie's
    /// `Secure` and `Max-Age` as the running server applies them (changed at runtime by the
    /// admin API, round 4 B-SETTINGS).
    fn modes(&self) -> Modes {
        match self.loader.get().and_then(|loader| loader.live_config()) {
            Some(live) => Modes {
                access_enabled: self.modes.access_enabled,
                secure_cookies: live.secure_cookies(),
                cookie_max_age_secs: live.cookie_max_age_secs(),
            },
            None => self.modes,
        }
    }

    /// The server serves its routes and the databases of `loader`, each request refused or
    /// let through by `access`.
    pub(crate) fn ready(&self, loader: Arc<Loader>, access: Arc<Access>) -> Result<(), ServeError> {
        self.access
            .set(access)
            .map_err(|_| ServeError::Lifecycle("the router was made ready before".to_string()))?;
        self.loader
            .set(loader.clone())
            .map_err(|_| ServeError::Lifecycle("the router was made ready before".to_string()))?;
        self.lifecycle.ready_with(Arc::new(Databases { loader }))
    }

    /// The largest import upload: `server.toml`'s `[limits] upload_bytes` as the running server
    /// applies it (round 4 B-SETTINGS: changed at runtime by the admin API), else read from
    /// the file per request (the default while the server is not ready or the file does not
    /// read).
    pub(crate) fn upload_limit(&self) -> u64 {
        if let Some(live) = self.loader.get().and_then(|loader| loader.live_config()) {
            return live.upload_bytes();
        }
        self.loader
            .get()
            .and_then(|loader| loader.registry().dir().load_config().ok())
            .map_or(crate::config::DEFAULT_UPLOAD_BYTES, |config| {
                config.limits.upload_bytes
            })
    }

    /// Whether a request to `path` with `headers` may run without credentials, so its body
    /// gets a deadline (SEC M1, [`crate::serve`]): the login always; with access control on,
    /// a request that names no credentials (no `Authorization`, no session cookie). A request
    /// with wrong credentials is refused before its body is read.
    pub(crate) fn may_run_without_credentials(
        &self,
        path: &str,
        headers: &[(String, String)],
    ) -> bool {
        if path == LOGIN {
            return true;
        }
        if !self.modes().access_enabled {
            return false;
        }
        let named = |name: &str| headers.iter().any(|(n, _)| n.eq_ignore_ascii_case(name));
        let cookie = headers.iter().any(|(n, v)| {
            n.eq_ignore_ascii_case("cookie")
                && v.split(';')
                    .any(|pair| pair.trim().starts_with(&format!("{SESSION_COOKIE}=")))
        });
        !named("Authorization") && !cookie
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
    /// Every in-memory answer carries `Cache-Control: no-store` ([`no_store`]; a download sets
    /// it itself).
    pub(crate) fn answer(&self, request: Request<'_>) -> Answer {
        match self.answer_uncached(request) {
            Answer::Response(response) => Answer::Response(framing_refused(no_store(response))),
            download @ Answer::Download(_) => download,
        }
    }

    /// [`Self::answer`] before [`no_store`].
    fn answer_uncached(&self, mut request: Request<'_>) -> Answer {
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
        let Some(access) = self.access.get() else {
            return Answer::Response(self.lifecycle.handle(request));
        };
        let url = request.url().to_string();
        let (path, query) = match url.split_once('?') {
            Some((path, query)) => (path, Some(query)),
            None => (url.as_str(), None),
        };
        let method = request.method().clone();
        // The server's own API description: public (it carries no data), like the UI files.
        if path == crate::openapi::SERVER_SPEC_PATH {
            let answer = if matches!(method, Method::Get | Method::Head) {
                self.server_spec(&request)
            } else {
                Err(not_allowed(&method, path))
            };
            return Answer::Response(answer.unwrap_or_else(error_response));
        }
        if path == LOGIN || path == LOGOUT {
            let answer = if method != Method::Post {
                Err(not_allowed(&method, path))
            } else if path == LOGIN {
                login(access, &self.modes(), &mut request)
            } else {
                logout(access, &self.modes(), &request)
            };
            return Answer::Response(answer.unwrap_or_else(error_response));
        }
        let principal = match access.authenticate(&request) {
            authenticated if !self.modes().access_enabled => open_principal(authenticated),
            Ok(principal) => principal,
            Err(err) => {
                return Answer::Response(refused_credentials(
                    &self.modes(),
                    &err,
                    &method,
                    path,
                    &url,
                ));
            }
        };
        if let Some(identity) = principal.identity() {
            request.set_identity(identity);
        }
        if let Some((name, rest)) = mounts::split_db_path(path) {
            return Answer::Response(self.database(
                loader,
                access,
                &principal,
                &mut request,
                (name, rest),
                (path, query, &url),
            ));
        }
        let read = matches!(method, Method::Get | Method::Head);
        let answer = if path == crate::mcp::ENDPOINT {
            match loader.mcp() {
                Some(_) if loader.mcp_control().is_some_and(|c| !c.enabled()) => {
                    if let Some(control) = loader.mcp_control() {
                        control.usage().refused();
                    }
                    return Answer::Response(crate::mcp::switched_off());
                }
                // Anonymous may open MCP sessions only where anonymous is granted something
                // (SEC M2): otherwise its `initialize` flood could evict users' sessions.
                Some(_) if principal.is_anonymous() && !access.anonymous_has_grant() => {
                    return Answer::Response(unauthorized(
                        "log in to use MCP: send a user token (Authorization: Bearer)",
                        None,
                    ));
                }
                Some(mcp) => {
                    let directory = crate::mcp::Directory {
                        loader,
                        access,
                        principal: &principal,
                    };
                    return Answer::Response(crate::mcp::route(mcp, &directory, &mut request));
                }
                None => Err(HttpError::new(
                    404,
                    "the MCP endpoint is off: server.toml [mcp] enabled = false",
                )),
            }
        } else if path == "/api/info" {
            if read {
                self.info_for(loader, &principal)
            } else {
                Err(not_allowed(&method, path))
            }
        } else if path == crate::user_api::DATABASES {
            if read {
                Ok(crate::user_api::databases(loader, access, &principal))
            } else {
                Err(not_allowed(&method, path))
            }
        } else if let Some(endpoint) = path.strip_prefix("/api/admin/") {
            if !principal.is_admin() {
                return Answer::Response(admin_refused(&principal, path));
            }
            if let Some(answer) = crate::control::route(
                self.facts.as_ref(),
                self.stop_hook.as_ref(),
                (loader, access),
                &principal,
                &method,
                endpoint,
            ) {
                return Answer::Response(answer);
            }
            let admin = crate::ops::Admin::online(loader.clone())
                .with_audit(access.audit_as(&principal))
                .with_access(access.clone(), principal.clone());
            if principal.kind == crate::access::PrincipalKind::Everyone
                && crate::admin_api::issues_credentials(&method, endpoint)
            {
                return Answer::Response(Response::json(
                    403,
                    &serde_json::json!({
                        "error": "making a user or a token needs the operator token or an admin \
                                  user's token, also with access control off",
                        "help": "run graphersal-server user add NAME (or user token NAME) --dir \
                                 DIR as the directory's owner, or send an admin user's token",
                    }),
                ));
            }
            if crate::admin_api::is_server_backup(&method, endpoint) {
                if principal.kind == crate::access::PrincipalKind::Everyone {
                    return Answer::Response(Response::json(
                        403,
                        &serde_json::json!({
                            "error": "a server backup holds the users' token hashes: it needs the \
                                      operator token or an admin user's token, also with access \
                                      control off",
                            "help": "run graphersal-server backup --dir DIR as the directory's \
                                     owner, or send an admin user's token",
                        }),
                    ));
                }
                return match crate::admin_api::server_backup(&admin, query) {
                    Ok(download) => Answer::Download(download),
                    Err(response) => Answer::Response(response),
                };
            }
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

    /// A request to `/db/<name>/<rest>` (`rest` `None`: no slash after the name) by
    /// `principal`: refused without a grant (module docs), else forwarded to the database's
    /// context with the principal's identity, read-only for a `read` grant.
    fn database(
        &self,
        loader: &Loader,
        access: &Access,
        principal: &Principal,
        request: &mut Request<'_>,
        (name, rest): (&str, Option<&str>),
        (path, query, url): (&str, Option<&str>, &str),
    ) -> Response {
        // An alias's own grants count only through the alias (`Access::reach`).
        let Some((entry, granted)) = access.reach(loader.registry(), principal, name) else {
            if !principal.is_anonymous() {
                return mounts::unknown_database(name);
            }
            if is_page_request(request.method(), path) {
                return redirect_to_login(url);
            }
            return unauthorized("log in to reach this database", None);
        };
        let Some(rest) = rest else {
            return mounts::redirect_to_slash(path, query);
        };
        // The database's API description: the server's patch of the engine's, answered also
        // while the database is not loaded (crate::openapi).
        if crate::openapi::DATABASE_SPEC_PATHS.contains(&rest) {
            if !matches!(request.method(), Method::Get | Method::Head) {
                return error_response(not_allowed(request.method(), path));
            }
            return self
                .database_spec(loader, access, request, name)
                .unwrap_or_else(error_response);
        }
        // Only a check: the resolver takes the context again and holds it for the request.
        if loader.context(&entry.name).is_none() {
            return mounts::not_loaded(&entry);
        }
        let target = match query {
            Some(query) => format!("/{rest}?{query}"),
            None => format!("/{rest}"),
        };
        // The per-database MCP endpoint: `mcp.toml`'s switches (off: 503; the name or its
        // database not exposed: 404; read-only: the request marked read-only), and its body
        // kept to count its tool calls.
        let mcp_control = (rest == "mcp" && loader.mcp().is_some())
            .then(|| loader.mcp_control())
            .flatten();
        let mut mcp_read_only = false;
        if let Some(control) = mcp_control {
            if !control.enabled() {
                control.usage().refused();
                return crate::mcp::switched_off();
            }
            if !control.exposed(name, &entry.name) {
                control.usage().refused();
                return crate::mcp::not_exposed(&crate::names::fold(name));
            }
            mcp_read_only = control.read_only(name, &entry.name);
        }
        // A backup job's path is relative to the backup root: an absolute one is refused here
        // (the context's file root resolves a relative one and refuses one leaving the root).
        let buffered = if request.method() == &Method::Post && rest == JOBS_DEFINE {
            match jobs_define_body(request) {
                Ok(body) => Some(body),
                Err(answer) => return answer,
            }
        } else if mcp_control.is_some() {
            let (max, strict) = crate::mcp::body_limit(principal);
            match crate::mcp::buffer_body(request, max, strict) {
                Ok(body) => body,
                Err(answer) => return answer,
            }
        } else {
            None
        };
        // A new MCP session: the principal's own oldest ones make room first (SEC M2).
        if let (Some(control), Some(mcp), Some(body)) = (mcp_control, loader.mcp(), &buffered)
            && crate::mcp::is_initialize(body)
        {
            let directory = crate::mcp::Directory {
                loader,
                access,
                principal,
            };
            crate::mcp::make_room(
                mcp,
                &directory,
                control.usage(),
                principal.identity().as_deref(),
                crate::mcp::session_quota(loader),
            );
        }
        let session = crate::mcp::header_of(request.headers(), crate::mcp::SESSION_HEADER);
        let mut forwarded = match &buffered {
            Some(body) => mounts::forward_with_body(request, &target, &entry.name, body),
            None => mounts::forward(request, &target, &entry.name),
        };
        if let Some(identity) = principal.identity() {
            forwarded.set_identity(identity);
        }
        if granted == Level::Read || mcp_read_only {
            forwarded.mark_read_only();
        }
        let identity = forwarded.identity().map(str::to_string);
        // The change feed hides other identities' client ids itself (SEC M3, graphersal-serve
        // keys runs and feed entries by the identity set above).
        let answer = self.lifecycle.handle(forwarded);
        if let (Some(control), Some(sent)) = (mcp_control, &buffered) {
            let new_session = crate::mcp::header_of(answer.headers(), crate::mcp::SESSION_HEADER);
            if let Some(opened) = new_session.as_deref()
                && session.as_deref() != Some(opened)
            {
                control.usage().session_opened(opened);
            }
            control.usage().observe(
                &crate::mcp_control::ObservedRequest {
                    identity: identity.as_deref(),
                    mount: Some(name),
                    session: session.as_deref(),
                    new_session: new_session.as_deref(),
                    body: sent,
                },
                answer.status(),
                answer.body(),
            );
        }
        answer
    }

    /// The base URL of `request` for the API descriptions: `[http] public_scheme` and its
    /// `Host` (checked before); `400` without one.
    fn base_url(&self, request: &Request<'_>) -> Result<String, HttpError> {
        crate::openapi::request_base_url(self.modes().secure_cookies, request.header("Host"))
            .ok_or_else(|| HttpError::bad_request("the request names no Host"))
    }

    /// `GET /openapi.json`: the server's own API description ([`crate::openapi`]).
    fn server_spec(&self, request: &Request<'_>) -> Result<Response, HttpError> {
        let spec = crate::openapi::server_spec(&self.base_url(request)?).map_err(|err| {
            tracing::error!(target: "graphersal_server", "{err}");
            HttpError::new(500, err.to_string())
        })?;
        Ok(Response::json(200, &spec))
    }

    /// `GET /db/<name>/openapi.json` (the principal reaches `name`, checked before): the
    /// database's API description ([`crate::openapi::database_spec`]); it names the
    /// anonymous alternative when anonymous reaches `name` too (or access control is off).
    fn database_spec(
        &self,
        loader: &Loader,
        access: &Access,
        request: &Request<'_>,
        name: &str,
    ) -> Result<Response, HttpError> {
        let base = self.base_url(request)?;
        let engine = crate::openapi::engine_spec().map_err(|err| {
            tracing::error!(target: "graphersal_server", "{err}");
            HttpError::new(500, err.to_string())
        })?;
        let anonymous = !self.modes().access_enabled
            || access
                .reach(loader.registry(), &Principal::anonymous(), name)
                .is_some();
        let name = crate::names::fold(name);
        Ok(Response::json(
            200,
            &crate::openapi::database_spec(&engine, &base, &name, anonymous),
        ))
    }

    /// `GET /api/info` for `principal` (module docs).
    fn info_for(&self, loader: &Loader, principal: &Principal) -> Result<Response, HttpError> {
        let view = crate::ui_assets::View::of(principal);
        let mut answer = if principal.is_anonymous() {
            anonymous_info()?
        } else {
            info_value(
                self.upload_limit(),
                loader.mcp().map(|mcp| mcp.status()),
                view,
            )?
        };
        answer["principal"] = json!({
            "id": principal.id,
            "kind": principal.kind.as_str(),
            "admin": principal.is_admin(),
        });
        Ok(Response::json(200, &answer))
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

/// Whether `method path` asks for a page or a static file a browser navigates to (a `GET`
/// outside `/api`, the MCP endpoints and the API descriptions (`openapi.json`: a client's
/// document, answered `401`), at the root or under `/db/<name>/`).
fn is_page_request(method: &Method, path: &str) -> bool {
    if !matches!(method, Method::Get | Method::Head) {
        return false;
    }
    let rest = match mounts::split_db_path(path) {
        Some((_, Some(rest))) => rest,
        Some((_, None)) => return true,
        None => path.trim_start_matches('/'),
    };
    !(rest == "api"
        || rest.starts_with("api/")
        || rest == "mcp"
        || crate::openapi::DATABASE_SPEC_PATHS.contains(&rest))
}

/// `401 {"error": message, "login": "/"}` with `WWW-Authenticate: Bearer`; `clear`: the
/// `Set-Cookie` value that clears the session cookie too.
fn unauthorized(message: &str, clear: Option<String>) -> Response {
    let response = Response::json(401, &json!({ "error": message, "login": LOGIN_PAGE }))
        .with_header("WWW-Authenticate", "Bearer");
    match clear {
        Some(cookie) => response.with_header("Set-Cookie", &cookie),
        None => response,
    }
}

/// `303` to the login page with the return path `url` (`/?next=<url>`; `/` itself: `/`).
fn redirect_to_login(url: &str) -> Response {
    let location = if url == LOGIN_PAGE {
        LOGIN_PAGE.to_string()
    } else {
        format!("{LOGIN_PAGE}?next={}", percent_encode(url))
    };
    Response::new(303, Vec::new())
        .with_header("Location", &location)
        .with_header("Cache-Control", "no-store")
}

/// The answer to credentials the chain refused: `401`; a refused session cookie is cleared, and
/// a browser navigating to a page goes to the login page instead.
fn refused_credentials(
    modes: &Modes,
    err: &AuthError,
    method: &Method,
    path: &str,
    url: &str,
) -> Response {
    let session = *err == AuthError::InvalidSession;
    if session && is_page_request(method, path) {
        return redirect_to_login(url).with_header("Set-Cookie", &modes.cleared_session_cookie());
    }
    unauthorized(
        &err.to_string(),
        session.then(|| modes.cleared_session_cookie()),
    )
}

/// The admin API refused to `principal`: `401` for anonymous, `404` for a principal without the
/// admin role (as if the API did not exist).
fn admin_refused(principal: &Principal, path: &str) -> Response {
    if principal.is_anonymous() {
        unauthorized("log in as an admin to use the admin API", None)
    } else {
        error_response(HttpError::new(404, format!("unknown endpoint {path}")))
    }
}

/// The largest body of `POST /api/login` (SEC M1): it carries one token; larger is `413`, read
/// no further.
pub(crate) const MAX_LOGIN_BODY: usize = 4 * 1024;

/// The JSON object of a body of at most `max` bytes (`413` beyond it, declared or read; `415`
/// without `Content-Type: application/json`).
fn small_json_body(request: &mut Request<'_>, max: usize) -> Result<serde_json::Value, HttpError> {
    use std::io::Read;
    let json_type = request.header("Content-Type").is_some_and(|t| {
        t.trim_start()
            .to_ascii_lowercase()
            .starts_with("application/json")
    });
    if !json_type {
        return Err(HttpError::new(
            415,
            "send the body as Content-Type: application/json",
        ));
    }
    let too_large = || HttpError::new(413, format!("the body is larger than {max} bytes"));
    if request.body_length().is_some_and(|len| len > max) {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    request
        .body()
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| HttpError::bad_request(format!("cannot read the body: {err}")))?;
    if bytes.len() > max {
        return Err(too_large());
    }
    let body: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|err| HttpError::bad_request(format!("malformed JSON body: {err}")))?;
    if !body.is_object() {
        return Err(HttpError::bad_request("the body must be a JSON object"));
    }
    Ok(body)
}

/// The body of `POST /api/login`.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginBody {
    token: String,
}

/// `POST /api/login {"token"}`: a browser session for the user token, its id in the
/// `gs_session` cookie; `401` for a token no user holds.
fn login(access: &Access, modes: &Modes, request: &mut Request<'_>) -> Result<Response, HttpError> {
    let body = small_json_body(request, MAX_LOGIN_BODY)?;
    let body: LoginBody = serde_json::from_value(body)
        .map_err(|err| HttpError::bad_request(format!("the body must be {{\"token\"}}: {err}")))?;
    match access.login(body.token.trim()) {
        Ok((id, principal)) => {
            let cookie = modes.session_cookie(id.expose());
            Ok(Response::json(
                200,
                &json!({ "principal": {
                    "id": principal.id,
                    "kind": principal.kind.as_str(),
                    "admin": principal.is_admin(),
                }}),
            )
            .with_header("Set-Cookie", &cookie))
        }
        Err(AuthError::Unavailable(message)) => {
            tracing::error!(target: "graphersal_server", "login: {message}");
            Err(HttpError::new(
                500,
                format!("cannot start a session: {message}"),
            ))
        }
        Err(err) => Ok(unauthorized(&err.to_string(), None)),
    }
}

/// `POST /api/logout`: the browser session of the request's cookie ends (if any) and the cookie
/// is cleared; `{"loggedOut": bool}` (`false`: there was no live session).
fn logout(access: &Access, modes: &Modes, request: &Request<'_>) -> Result<Response, HttpError> {
    let ended = match crate::access::cookie(request, SESSION_COOKIE) {
        Some(id) => access.logout(id).map_err(|err| {
            tracing::error!(target: "graphersal_server", "logout: {err}");
            HttpError::new(500, err.to_string())
        })?,
        None => false,
    };
    Ok(Response::json(200, &json!({ "loggedOut": ended }))
        .with_header("Set-Cookie", &modes.cleared_session_cookie()))
}

/// The `Content-Security-Policy` of every answer (SEC L6): no page of this server may be framed
/// (`frame-ancestors 'none'`), load plugins (`object-src 'none'`) or change its base URL
/// (`base-uri 'none'`). Scripts are not restricted further: the embedded playground runs inline
/// scripts, an import map and modules from its CDNs.
pub(crate) const CONTENT_SECURITY_POLICY: &str =
    "frame-ancestors 'none'; object-src 'none'; base-uri 'none'";

/// `response` with `X-Frame-Options: DENY` and [`CONTENT_SECURITY_POLICY`] (SEC L6: the admin
/// UI and every database's playground share one origin; no other site may frame them), each
/// unless it names one already.
pub(crate) fn framing_refused(response: Response) -> Response {
    let named = |response: &Response, name: &str| {
        response
            .headers()
            .iter()
            .any(|(field, _)| field.eq_ignore_ascii_case(name))
    };
    let response = if named(&response, "X-Frame-Options") {
        response
    } else {
        response.with_header("X-Frame-Options", "DENY")
    };
    if named(&response, "Content-Security-Policy") {
        response
    } else {
        response.with_header("Content-Security-Policy", CONTENT_SECURITY_POLICY)
    }
}

/// `response` with `Cache-Control: no-store`, unless it names a `Cache-Control` already (the
/// JSON answers of `Response::json` and the files do): no browser or proxy keeps an answer of
/// this server, so every visit of a tab shows the current state.
pub(crate) fn no_store(response: Response) -> Response {
    let named = response
        .headers()
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("cache-control"));
    if named {
        response
    } else {
        response.with_header("Cache-Control", "no-store")
    }
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
/// host-neutral protocol marker of a Graphersal host (`protocol: "graphersal/1"`,
/// [`graphersal_serve::PROTOCOL`]), `dev: false` (not a dev server) and its UI. The playground
/// served at `/` takes the answer as its engine's because it carries the protocol marker; it
/// shows `uiProfile`, the `server` profile of the principal's view
/// ([`crate::ui_assets::profile_for`]: the admin tabs, a user's "My databases", anonymous's
/// "Sign in"; no workspace, so the page never asks the root for a graph).
/// `uploadBytes` is the largest import upload (a client checks a file's size before it sends
/// it; the profile's modules read it from `api.info.host`); `mcp` is the MCP endpoint's status
/// (`{endpoint: "/mcp", readOnly, client}`), null when MCP is off.
#[cfg(test)]
fn info(upload_bytes: u64, mcp: Option<serde_json::Value>) -> Result<Response, HttpError> {
    Ok(Response::json(
        200,
        &info_value(upload_bytes, mcp, crate::ui_assets::View::Admin)?,
    ))
}

/// The server's UI profile for `view` ([`crate::ui_assets::profile_for`]: an admin's tabs, a
/// user's "My databases", anonymous's "Sign in"); `500` when it cannot be built.
fn ui_profile(view: crate::ui_assets::View) -> Result<serde_json::Value, HttpError> {
    crate::ui_assets::profile_for(view).map_err(|err| {
        tracing::error!("{err}");
        HttpError::new(500, err)
    })
}

/// The answer of [`info`] as JSON (the router adds the principal).
fn info_value(
    upload_bytes: u64,
    mcp: Option<serde_json::Value>,
    view: crate::ui_assets::View,
) -> Result<serde_json::Value, HttpError> {
    let profile = ui_profile(view)?;
    Ok(json!({
            "server": SERVER_NAME,
            "protocol": graphersal_serve::PROTOCOL,
            "dev": false,
            "version": env!("CARGO_PKG_VERSION"),
            "experimental": true,
            "uploadBytes": upload_bytes,
            "mcp": mcp,
            "uiProfile": profile,
            "openapi": crate::openapi::SERVER_SPEC_PATH,
            "databaseOpenapi": crate::openapi::DATABASE_SPEC_TEMPLATE,
    }))
}

/// `GET /api/info` for anonymous: only what the login page needs (the playground at the root
/// takes the answer as its engine's by `protocol` and shows `uiProfile`; `login` names the
/// login page); the router adds the principal.
fn anonymous_info() -> Result<serde_json::Value, HttpError> {
    Ok(json!({
        "server": SERVER_NAME,
        "protocol": graphersal_serve::PROTOCOL,
        "dev": false,
        "experimental": true,
        "uiProfile": ui_profile(crate::ui_assets::View::Anonymous)?,
        "login": LOGIN_PAGE,
        "openapi": crate::openapi::SERVER_SPEC_PATH,
        "databaseOpenapi": crate::openapi::DATABASE_SPEC_TEMPLATE,
    }))
}

/// `text` with every byte outside the URL's unreserved characters and `/` as `%XX` (a return
/// path in a query string).
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `%XX` decoded (as `graphersal_serve` decodes static paths); `None` for a broken escape or
/// a result that is not UTF-8.
pub(crate) fn percent_decode(path: &str) -> Option<String> {
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
    #[test]
    fn absolute_backup_paths_are_recognised_on_every_platform() {
        for path in ["/tmp/x", "\\x", "C:\\x", "c:x", "\\\\server\\share"] {
            assert!(super::is_absolute_path(path), "{path}");
        }
        for path in ["nightly", "a/b", "../escape", "./x"] {
            assert!(!super::is_absolute_path(path), "{path}");
        }
    }

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

    /// The operator token of the routers [`make_ready`] makes (the admin of these tests).
    const OPERATOR: &str = "router-tests-operator-token";

    /// Makes `router` ready over an empty server directory (kept alive by the answer), with
    /// [`OPERATOR`] as the operator token.
    fn make_ready(router: &Router) -> TempDir {
        let temp = TempDir::new("router");
        let dir = server(&temp);
        let loader = Loader::new(Registry::scan(&dir).unwrap(), router.host_check.clone());
        let (access, _) = Access::open(
            &dir,
            Some(crate::access::TokenHash::of(OPERATOR)),
            crate::audit::Audit::default(),
        )
        .unwrap();
        router.ready(loader, Arc::new(access)).unwrap();
        temp
    }

    /// `GET url` as the admin (the operator token).
    fn get(router: &Router, url: &str, host: Option<&str>) -> Response {
        let mut request = Request::new(Method::Get, url, std::io::empty())
            .with_header("Authorization", &format!("Bearer {OPERATOR}"));
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

    /// `ui/tests/root-info.json` is the root info the node test `profile.test.mjs` hands the
    /// playground's `isEngineInfo`: every field it names is what [`info`] answers, so the node
    /// test checks the real answer, not a copy that can drift.
    #[test]
    fn the_node_tests_root_info_is_the_answer() {
        let fixture: Value =
            serde_json::from_str(include_str!("../ui/tests/root-info.json")).unwrap();
        let answer = json_of(&info(1024, None).unwrap());
        let fixture = fixture.as_object().unwrap();
        for (key, value) in fixture {
            assert_eq!(&answer[key], value, "{key}");
        }
        let mut rest: Vec<&str> = answer
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .filter(|key| !fixture.contains_key(*key))
            .collect();
        rest.sort_unstable();
        // The fields the fixture leaves out vary per server (the node test adds the profile).
        assert_eq!(rest, ["mcp", "uiProfile", "uploadBytes", "version"]);
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
        assert_eq!(info["dev"], false);
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

#[cfg(test)]
#[path = "router_access_tests.rs"]
mod access_tests;
