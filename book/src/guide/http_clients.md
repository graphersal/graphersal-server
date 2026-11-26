# HTTP Clients and OpenAPI

How a program talks to `graphersal-server` over HTTP: the OpenAPI descriptions to generate a
client from, the token, the error format and the revisions. The routes themselves are
documented in [Admin API](../reference/admin_api.md) (the admin API) and in the descriptions below;
access control in [Users, Tokens and Grants](access.md); the MCP endpoints, which are not REST, in
[MCP](mcp.md).

## Two OpenAPI descriptions

| Description | URL | Offline | Describes |
|---|---|---|---|
| the server's | `GET /openapi.json` (public, no token) | `graphersal-server openapi --dir DIR` | the probes, `POST /api/login`/`logout`, `GET /api/info`, `GET /api/databases`, the whole admin API (`/api/admin/...`) |
| a database's | `GET /db/<name>/openapi.json` (also `/db/<name>/api/openapi.json`) | `graphersal-server openapi --dir DIR --db NAME` | the database's own HTTP API below `/db/<name>/`: queries (`POST /api/execute`, saved queries `POST /api/query`), schema, saved queries, indexes, compression, jobs, the Store (checkpoints, backups, history), elements, statistics, ... |

Both are OpenAPI 3.1 JSON. Use the database's description for data access, the server's for
administration. `GET /api/info` names both (`openapi`: `/openapi.json`, `databaseOpenapi`:
`/db/{name}/openapi.json`); `GET /api/databases` names each reachable database's as
`openapi`. The web UI links them: "My databases" has an "OpenAPI" link per database, the
admin's Databases tab one per row and "the server's API (OpenAPI)" above the table.

**`servers`** is the URL the request came to: `server.toml [http] public_scheme` (`https`
behind a TLS proxy) and the request's `Host` (the listen address's names, or a name of `[http]
allowed_hosts`), plus `/db/<name>` for a database. `<name>` may be an alias: the description
then names `/db/<alias>`, so a client built from it follows the alias (and its flips). The
console writes the same documents without a running server; their `servers` URL is `--server
URL` (`https://db.example.com`; a bare `host:port` gets the public scheme), else the first
`[http] allowed_hosts` name, else the listen address. Everything but `servers` is the same as
what the server answers.

**A database's description and the access layer.** It needs at least a read grant on the
name, like every route below `/db/<name>/` (anonymous without a grant: `401`; a user without
one: `404`), and is answered also while the database is not loaded. Every principal that
reaches the name gets the SAME document: it is not filtered by the caller's level. Instead
every operation carries `x-graphersal-access`:

| `x-graphersal-access` | With a read grant | With a write grant |
|---|---|---|
| `read` | answered | answered |
| `query` (`POST /api/execute`, `POST /api/query`) | runs under the reading authorizer: a script that writes fails (`execution.error`, status 200) and changes nothing | runs, may write |
| `write` (schema, elements, saved queries, indexes, jobs, save, Store operations, ...) | `409` (`the graph is served read-only: writes are refused`) | answered (a read-only database or a base still refuses: `409`) |

Every operation also carries the access layer's answers: `401` (no credentials and no
anonymous grant, or a token nobody holds), `404` (unknown or not granted), `409` (not loaded;
on a writing operation also a read grant); where the engine itself answers the same code the
description names both. The routes the server does not serve below a mount are left out (the
probes, the engine's own run administration, the AI Chat of the dev server).

The engine's part comes from graphersal-serve (the version this server is built with), with
what its later versions added: the `409` of a read-only request on every writing route and
the `403` of a backup or fork path outside the server's backup root.

## Security

Both descriptions declare two security schemes and accept either:

| Scheme | OpenAPI | Use |
|---|---|---|
| `bearerAuth` | `{type: http, scheme: bearer}` | `Authorization: Bearer <token>`: a user token, 64 hex digits (256 random bits), shown once by `graphersal-server user add NAME` / `user token NAME` or the admin API (`POST /api/admin/users`). The console's operator token (`server.run`) works the same. Programs use this. |
| `sessionCookie` | `{type: apiKey, in: cookie, name: gs_session}` | the browser session cookie `POST /api/login {"token"}` sets (`HttpOnly`, `SameSite=Strict`). The web UI uses this. |

The global `security` of a database's description is `[{bearerAuth: []}, {sessionCookie: []}]`;
when anonymous reaches that name too (an anonymous grant, or `server.toml [access] enabled =
false`) it adds `{}` (no credentials). In the server's description the public operations
(`/openapi.json`, login, logout, the probes) say `security: [{}]`, `/api/info`,
`/api/databases` and `/db/{name}/openapi.json` accept anonymous too, and every admin operation
needs a token of the admin role (`x-graphersal-access: admin`).

Wrong credentials are always `401`, never treated as anonymous. Tokens travel in clear text
over plain HTTP: use `https` behind a TLS proxy (`public_scheme = "https"`) for anything but
the loopback address.

Every request but the probes must name the server in `Host` (curl does), and a browser page of
another origin is refused (`403`).

## curl

```bash
TOKEN=...   # graphersal-server user add alice  (printed once)
BASE=http://127.0.0.1:8484

# The descriptions
curl -s "$BASE/openapi.json" -o server.openapi.json
curl -s -H "Authorization: Bearer $TOKEN" "$BASE/db/acme/openapi.json" -o acme.openapi.json

# The databases this token reaches, with its level
curl -s -H "Authorization: Bearer $TOKEN" "$BASE/api/databases"

# A query (Gremlin in Rhai; one unit: it commits when ok)
curl -s -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"script": "g.v().has_label(\"person\").values(\"name\").to_list()"}' \
  "$BASE/db/acme/api/execute"

# A saved query with named arguments: {ok, results, mutated, changes}
curl -s -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"name": "people_older_than", "params": {"age": 30}}' \
  "$BASE/db/acme/api/query"

# Admin: the databases, then a revision-checked change
curl -s -H "Authorization: Bearer $ADMIN_TOKEN" "$BASE/api/admin/databases"
curl -s -i -H "Authorization: Bearer $ADMIN_TOKEN" "$BASE/api/admin/databases/acme/settings" | grep -i etag
curl -s -X PUT -H "Authorization: Bearer $ADMIN_TOKEN" -H "Content-Type: application/json" \
  -H 'If-Match: "3f9c0a17d2b84e61"' -d '{"description": "the ACME graph"}' \
  "$BASE/api/admin/databases/acme/settings"
```

The console writes the descriptions offline (no running server needed, nothing written in
the directory):

```bash
graphersal-server openapi --dir /srv/graphs --out server.openapi.json
graphersal-server openapi --dir /srv/graphs --db acme --server https://db.example.com --out acme.openapi.json
```

## Generating a client

Any OpenAPI 3.1 generator works on the downloaded file; the schemas are deliberately loose
(objects with the documented keys; [Admin API](../reference/admin_api.md) and the engine's book pages
are the authoritative descriptions of every answer). Examples:

```bash
# TypeScript types (openapi-typescript), then fetch with openapi-fetch
npx openapi-typescript acme.openapi.json -o acme.d.ts

# A Python client (openapi-generator)
npx @openapitools/openapi-generator-cli generate -i acme.openapi.json -g python -o acme-client

# A Rust client
npx @openapitools/openapi-generator-cli generate -i server.openapi.json -g rust -o graphersal-admin
```

Configure the generated client's bearer token (`bearerAuth`) with the user token; its base URL
comes from `servers` (override it when the client runs elsewhere). A description is
EXPERIMENTAL like the server: regenerate the client when you upgrade the server, and pin the
server version you generated against (`info.version`; a database's description names the
engine's version as `info.x-graphersal-engine-version`).

## Errors

An error answer is JSON `{"error": "<sentence>", "help": "<what to do>"?}` with a 4xx or 5xx
status:

- `400` an invalid body, name, query or schema; `401` no or wrong credentials (`{"error",
  "login": "/"}`, `WWW-Authenticate: Bearer`); `403` a refused `Host`/`Origin`, a path outside
  the backup root, or the principal `everyone` asking for a server backup or a shutdown; `404`
  unknown (or not granted); `405` method not allowed; `409` a rule refuses it (not loaded,
  read-only, name taken, a stale revision, ...); `413` too large; `415` an unknown upload
  format; `422` the engine refuses (a database route); `500` a Store or file operation failed;
  `503` starting or stopping (`Retry-After: 1`) or MCP switched off.
- A database's form-like operations (`*/define`, `*/create`, `element/*`) answer `200 {"ok":
  false, "errors": [{"field", "message", "help"?}]}` for refused input, and a failing script
  is `200` with `execution.error`: check `ok` / `execution.error`, not only the status.

## Revisions

Shared objects carry a `revision` (users and grants, aliases, each database's settings, the MCP
settings): `GET` answers it in the body and as `ETag`. A change may name the revision it is
based on, in the body's `revision` field or as `If-Match: "<revision>"` (the body wins). When
someone else changed the object since, the change is refused and nothing changes:

```json
409 {"error": "... changed by someone else since you read them (now revision 3f9c0a17d2b84e61); nothing was changed",
     "help": "changed by someone else: reload", "revision": "3f9c0a17d2b84e61"}
```

Read the object again, then repeat the change with the new revision. Without a revision a
change is applied to the current state. The server's description marks these operations with
the `If-Match` parameter and a `409` (`RevisionConflict`). Details:
[Admin API](../reference/admin_api.md#revisions-concurrent-changes).
