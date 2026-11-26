# HTTP Routes

Every path the server answers, by its first segments. Programs: see [HTTP Clients and
OpenAPI](../guide/http_clients.md); every admin route is in the [Admin API](admin_api.md).


| Path | What |
|---|---|
| `GET /healthz`, `GET /readyz` | liveness `200`; readiness `200` or `503` while starting or stopping; answered at once, also on a connection over the cap |
| `GET /api/info` | the server's identity (`server: "graphersal-server"`, `protocol: "graphersal/1"`, `dev: false`), its UI profile `uiProfile` and the request's `principal`; for a logged-in principal also `uploadBytes`, the MCP endpoint and the version (anonymous gets only what the login page needs, plus `login: "/"`); everyone gets `openapi` and `databaseOpenapi`, the paths of the API descriptions |
| `GET /` | the server's web UI (the playground showing the `server` UI profile: the login page, "My databases" or the admin tabs, by who asks) |
| `POST /api/login`, `POST /api/logout` | the browser login with a user token (a `gs_session` cookie) and its logout; the login body is at most 4 KiB (`413`) and must arrive within 10 s |
| `GET /api/databases` | the databases and aliases the caller may reach, with its level, the playground path and the OpenAPI path of each ("My databases") |
| `GET /openapi.json`, `GET /db/<name>/openapi.json` | the OpenAPI 3.1 descriptions to generate HTTP clients from: the server's own routes (public), and a database's HTTP API (behind its grant; Bearer token or session cookie as security schemes): [HTTP Clients and OpenAPI](../guide/http_clients.md) |
| `GET /server-ui/<file>` | the `server` UI profile's files (its modules and style sheet) |
| `/db/<name>/...` | the playground and its HTTP API (`POST /db/<name>/api/execute {"script"}`, ...) on the database or alias `<name>`, resolved per request, at the caller's level ([Users, Tokens and Grants](../guide/access.md#401-404-and-303): without a grant `401` for anonymous, `303` to the login page for its pages, `404` for a user); `409` with help when it is not loaded (never loaded implicitly), `404` when unknown |
| `/api/admin/...` | the admin API: [Admin API](admin_api.md) (also `GET /api/admin/server`, the server itself, and `POST /api/admin/shutdown`, which `stop` uses); also the run administration (`GET /api/admin/runs`, `GET /api/admin/databases/<name>/runs`, `POST /api/admin/databases/<name>/runs/cancel {"id"}`: every client's queued and running queries, stop one) |
| `/mcp`, `/db/<name>/mcp` | the MCP endpoints: [MCP](../guide/mcp.md); `/mcp` answers anonymous `401` unless anonymous holds a grant; an anonymous body is at most 1 MiB |


`serve` speaks HTTP/1.1 and h2c (HTTP/2 over plain TCP with prior knowledge, e.g. `curl
--http2-prior-knowledge`) on the same port. No timeout covers a whole authenticated request: an
upload streams as long as it keeps sending (each read of a body waits at most 120 s for the next
piece). A request that may run without credentials (the login; with access control on, one
naming no `Authorization` and no session cookie) has 10 s for its whole body ([Security
Model](../guide/security.md#limits-for-clients-without-credentials)). Every answer carries
`Cache-Control: no-store`, `X-Frame-Options: DENY` and a `Content-Security-Policy` refusing
framing. The
connection limits are `server.toml`'s `[limits]` ([Configuration
Reference](configuration.md#servertoml)).

Every `GET /api/info`, the root's and each database's, carries `"protocol": "graphersal/1"`,
the host-neutral marker of a Graphersal host: the playground takes an answer as its engine's
by this marker alone. `server: "graphersal-server"` names the host (the Graphersal CLI's dev
server answers `graphersal-dev`), and `dev: false` keeps the playground's DEV badge off. The
root answer adds `uiProfile`, the profile the page at `/` shows. A database's `/api/info` adds
`database` (its real name) and `home: "/"` (the link back to the server's UI).
