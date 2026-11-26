# MCP

**EXPERIMENTAL.** graphersal-server serves its databases to AI agents over the Model Context
Protocol (Streamable HTTP transport, JSON answers). There are two kinds of endpoint, served by
ONE endpoint object (the same MCP sessions and statistics for both), each answering per
principal ([Users, Tokens and Grants](access.md)):

| Endpoint | Serves | Tools |
|---|---|---|
| `/mcp` | every database of the server directory | `list_graphs`, `use_graph`, and the data tools, each with an optional `graph` argument |
| `/db/<name>/mcp` | the loaded database `<name>` (or the database an alias names, resolved per request) | the data tools of one graph (as the graphersal dev server's `/mcp` offers them) |

Configure an agent once with `/mcp`; use a per-database endpoint when an agent should only ever
see one database.

**Off by default.** A new server (`graphersal-server init`) has MCP switched off, and every new
database or alias is hidden from MCP until an admin exposes it (see
[New servers and new databases](#new-servers-and-new-databases)). To let agents in:

```bash
graphersal-server mcp on --dir DIR                     # switch MCP on (the server directory)
graphersal-server mcp db acme on --dir DIR             # offer acme to MCP
graphersal-server db create notes --mcp --dir DIR      # a new database offered at once
```

## Configuration

`server.toml`, read when the server starts (restart it after a change):

```toml
[mcp]
enabled = true          # default; false: no MCP endpoint, /mcp and every /db/<name>/mcp answer 404
max_sessions = 256      # MCP sessions remembered, the oldest forgotten first (at least 1)
max_sessions_per_identity = 16  # MCP sessions one principal holds; a new one ends its own oldest
```

### Switches at runtime: the MCP tab, `mcp.toml`

`server.toml [mcp] enabled` decides at start whether the endpoints exist at all (`init` writes
`true`). While they do, `mcp.toml`'s `enabled` is the switch operators use: MCP answers only when
both are on (`server.toml` off wins: no endpoint is built). An admin switches it at runtime, with
no restart, in the web UI's **MCP** tab, with the admin
API (`GET`/`PUT /api/admin/mcp`, `PUT /api/admin/mcp/databases/<name>`;
[Admin API](../reference/admin_api.md#mcp)) or the console:

```bash
graphersal-server mcp status --dir DIR                 # switches, databases, usage
graphersal-server mcp off --dir DIR                    # | mcp on
graphersal-server mcp db acme off --dir DIR            # hide acme from MCP | ... on
graphersal-server mcp db acme read-only on --dir DIR   # MCP only reads acme | ... off
graphersal-server mcp expose-new on --dir DIR          # offer NEW databases and aliases | ... off
```

The server keeps them in `mcp.toml` of the server directory ([Configuration Reference](../reference/configuration.md#mcptoml)):

- **MCP off** (whole server): `/mcp` and every `/db/<name>/mcp` answer `503 {"error": "MCP is
  switched off on this server", "help"}` at once. Open MCP sessions are kept, not ended: their
  requests are refused while it is off and go on (same session id, same selection) when it is on
  again. `server.toml [mcp] enabled = false` stays the hard switch: then no endpoint exists and
  the runtime switch cannot turn it on (restart after changing `server.toml`).
- **A database or alias hidden** (`exposed = false`): `list_graphs` does not list it, `use_graph`
  and `"graph"` answer as for an unknown name, `/db/<name>/mcp` answers `404 {"error": "MCP is
  switched off for '<name>'", "help"}`. The HTTP API and the playground are not affected.
- **MCP read-only** (`read_only = true`): MCP sessions only read the database, whatever their
  grant: the reading profile, `list_graphs` shows `readOnly: true`, a write tool is refused. The
  HTTP API still follows the grant. The effective level of an MCP request is the least of the
  caller's grant, the database's mode (its read-only flag, a base) and this switch.
- **Aliases**: a setting belongs to a name. A request through an alias is offered when the alias
  AND its database are exposed, read-only when the alias OR its database is read-only: an alias
  narrows its database, never widens it. A session that selected an alias with settings of its own
  answers `use_graph` with the alias's name as `graph`, so the settings apply on every call. A
  renamed alias keeps its settings; a deleted database or removed alias loses them (a new one of
  that name starts hidden, as every new name).

### New servers and new databases

- **A new server has MCP off.** `init` writes `mcp.toml` with `enabled = false` and `expose_new =
  false`. Switch it on in the MCP tab, with `PUT /api/admin/mcp {"enabled": true}` or with
  `graphersal-server mcp on`.
- **A new name is hidden from MCP.** Every database the server creates (create, import, clone, a
  restore into a new name, repair) and every new alias gets an entry `exposed = false` in
  `mcp.toml` at birth, unless it is created with "expose through MCP": the console's `--mcp`
  (`db create`, `db clone`, `db alias set`, `db restore`, `import`), the admin API's `"mcp":
  true` (an upload import: `?mcp=true`), the checkbox "Expose through MCP" in the web UI's forms.
  `--no-mcp` (`"mcp": false`) hides it whatever the default. The answer carries `"mcpExposed"`
  and the console prints the command that exposes it.
- **The server-wide default** is `mcp.toml`'s `expose_new` (default `false`): the MCP tab's
  checkbox "Expose new databases and aliases", `PUT /api/admin/mcp {"exposeNew": true}`,
  `graphersal-server mcp expose-new on`. It applies to names created from now on; the names there
  keep their settings. `mcp status` shows it.
- **What keeps its setting**: a restore over an existing database, a promotion to a base and a
  swap keep the name and so its setting; a renamed alias carries its setting; flipping an alias to
  another database keeps the alias's setting.
- **A name without an entry is offered.** That is the rule of every earlier version, kept so that
  an upgraded server directory offers exactly what it offered before: its databases and aliases
  have no entries, so they stay offered, and a directory without `mcp.toml` keeps MCP on. Only
  the names created after the upgrade are hidden (unless `expose_new` is on). A database copied
  into `databases/` by hand also has no entry: hide it with `mcp db NAME off`.

### Usage statistics

The MCP tab, `GET /api/admin/mcp` (`usage`) and `graphersal-server mcp status` (through a
running server) show what agents did since the server started (in memory, reset by a restart):
the open MCP sessions (per user, per database: a session's selection on `/mcp` or the database
of its `/db/<name>/mcp`), the tool calls in total, per tool, per user and per database with their
errors, the last 50 tool errors (time, tool, user, database, message) and the requests refused
while MCP was off or a database hidden. A tool call is counted on the database it names
(`graph`), the one `use_graph` selects, or the session's selection.

`GET /api/info` at the root announces the endpoint: `"mcp": {"endpoint": "/mcp", "readOnly":
false, "client": {...} | null}` (`client`: the last client that initialized), or `"mcp": null`
when MCP is off. A database's `GET /db/<name>/api/info` announces its own endpoint
(`"endpoint": "/db/<name>/mcp"`, `readOnly` of that database).

### Adding the endpoint to an agent

Claude Code (the server on its default address), with a user token (`graphersal-server user
add NAME`, then grants; [Users, Tokens and Grants](access.md)):

```bash
claude mcp add --transport http --header "Authorization: Bearer <token>" \
  graphersal http://127.0.0.1:8484/mcp
# one database only:
claude mcp add --transport http --header "Authorization: Bearer <token>" \
  acme http://127.0.0.1:8484/db/acme/mcp
```

Without the header the agent is anonymous: `/mcp` serves it the databases granted to
`anonymous` and answers `401` while `access.toml` grants anonymous nothing (so a client
without credentials cannot open sessions at all); `/db/<name>/mcp` answers `401` without an
anonymous grant on that name. An anonymous request body is at most 1 MiB.

Other clients: any MCP client with the Streamable HTTP transport; the URL is the endpoint's.
Requests go through the same `Host`/`Origin` checks as the rest of the server: a browser page
of another site is refused (`403`), and so is a `Host` header that does not name this server.

### Requests

Every JSON-RPC message is one `POST` to the endpoint, answered with one JSON object (no
server-sent event stream):

| Header | When | Value |
|---|---|---|
| `Content-Type` | always | `application/json` |
| `Accept` | optional | must allow `application/json` when sent (`application/json, text/event-stream`; else `406`) |
| `Mcp-Session-Id` | after `initialize` | the id from the `initialize` answer's `Mcp-Session-Id` header; an unknown id is `404` (initialize again). A request without one is served, but on `/mcp` it has no selection: `use_graph` is refused and every data tool needs `graph` |
| `MCP-Protocol-Version` | optional | a revision the server speaks: `2025-11-25`, `2025-06-18`, `2025-03-26` (else `400`) |
| `Authorization` | for a user | `Bearer <user token>` (`access.toml`); without it the request is anonymous (the anonymous grants only); a token nobody holds is `401` |

A notification (such as `notifications/initialized`) is answered `202` without a body. With
curl (`scripts/smoke.sh` walks through the whole flow):

```bash
curl -si http://127.0.0.1:8484/mcp -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion":
       "2025-06-18", "capabilities": {}, "clientInfo": {"name": "curl", "version": "1"}}}'
# then, with the Mcp-Session-Id of that answer:
curl -s http://127.0.0.1:8484/mcp -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' -H 'Mcp-Session-Id: <id>' \
  -d '{"jsonrpc": "2.0", "id": 2, "method": "tools/call",
       "params": {"name": "use_graph", "arguments": {"name": "acme"}}}'
```

A tool's answer is the JSON-RPC `result` with `content[0].text` (the tool's answer as text;
`list_graphs` and `use_graph` answer JSON text) and `isError` (`true` for a tool error, such as
a database that is not loaded).

## The flow on `/mcp`

1. `initialize` starts an MCP session (`Mcp-Session-Id` in the answer); its instructions explain
   the flow below.
2. `list_graphs` lists the databases and aliases:

   ```json
   {"graphs": [
     {"name": "acme", "kind": "layer", "state": "loaded", "readOnly": false, "description": "..."},
     {"name": "product-2026.3", "kind": "base", "state": "loaded", "readOnly": true},
     {"name": "archive", "kind": "plain", "state": "unloaded", "readOnly": false},
     {"name": "prod", "aliasOf": "acme", "kind": "layer", "state": "loaded", "readOnly": false}
   ], "selected": null}
   ```

   `kind` is `plain`, `layer` or `base`; `state` is `unloaded`, `loading`, `loaded`,
   `needs-model` or `failed` (with a `reason`); `readOnly` is the database's `read_only` flag
   (always `true` for a base). An alias row shows its database's kind, state and flag.
3. `use_graph {"name": "acme"}` selects the database for this MCP session; every other tool
   then works on it. The session keeps the NAME it selected, a database or an alias, and
   resolves it on every call, as `/db/<name>/` does (see "Sessions follow their alias" below):
   `use_graph {"name": "prod"}` answers `"selected": "prod", "graph": "acme"` (the name, then
   the database it names now), and `list_graphs` shows `"selected": "prod", "selectedGraph":
   "acme"`.
4. Every data tool (`gql`, `query`, `get_schema`, `statistics`, `run_saved_query`, ...) takes an
   optional `"graph": "<name or alias>"`: it runs that one call on another database without
   changing the selection (the alias resolved for that call).
5. Without a selection and without `graph`, a tool call fails with an error naming
   `list_graphs` and `use_graph`.

Saved queries are not tools of their own (on neither kind of endpoint in this version); run them
with `run_saved_query` (and `graph` on `/mcp` when needed).

### Sessions follow their alias

A session that selected an alias keeps the alias's name:

- **Re-pointed** (`PUT /api/admin/aliases/<alias> {"target"}`): the next call works on the new
  database, silently. Errors of the new database (read-only, not loaded) are ordinary tool
  errors.
- **Removed** (`DELETE /api/admin/aliases/<alias>`): a call fails with `... (this MCP session
  selected "<alias>", which names no graph now): pick another graph: list_graphs, then
  use_graph`, as long as no database or alias of that name exists. An alias created again under
  the name simply works on.
- **Renamed** (`POST /api/admin/aliases/<alias>/rename {"name"}`, console `db alias rename OLD
  NEW`, web UI "Rename…"): the session moves to the new name. Its next tool answer starts with
  one line `note: alias "<old>" was renamed to "<new>"`; several renames before that call give
  one note from the first old name to the current one (none when they lead back to the first
  name). A `use_graph` call before it drops the note.
- A session that selected a database by its own name stays on it (databases have no rename).

**Grants on aliases** ([Grants on aliases](access.md#grants-on-aliases)): a principal whose only grant names
the alias reaches the alias's current database through the alias alone. `list_graphs` lists the
alias (with `aliasOf`) but not the database as a graph of its own; `use_graph {"name": "prod"}`
answers `"graph": "prod"` (the alias's own name instead of the database's), and `use_graph` or
`"graph"` with the database's real name answer like an unknown name. A flip moves such a
session too; a rename carries the grant with the alias; a removal ends the access (the grant goes
with the alias).

The per-database endpoint `/db/<name>/mcp` resolves `<name>` per request: after a rename of an
alias, `/db/<old name>/mcp` answers `404`.

### Reconnecting

The selection belongs to the MCP session. A client that reconnects (a new `initialize`) starts
without a selection and calls `use_graph` again; the error of its first call says so. The
server remembers at most `max_sessions` sessions: the oldest is forgotten first, its client gets
`404` for its unknown session id, initializes again and selects its database again. One
principal (a user, the operator, anonymous as one) holds at most `max_sessions_per_identity`
sessions (default 16) on `/mcp` and every `/db/<name>/mcp` together: its next `initialize`
first ends its own oldest session, never another principal's. A server
restart keeps the sessions (and their selections). `DELETE
/mcp` with the session id ends a session. `GET /mcp` (a server-sent event stream) is not
offered: `405`.

### Databases that are not loaded

A database that is not loaded is listed with its state. Selecting it (`use_graph`) or naming it
(`graph`) is a tool error that names the state, and the selection stays as it was:

- `database 'x' is not loaded; ask an operator to load it`
- `database 'x' is not loaded: it is loading; retry in a moment`
- `database 'x' is not loaded: it failed (<reason>); ask an operator to fix it and load it`
- `database 'x' is not loaded: it needs the embedding model '<repo>'; ask an operator to add the model and load it`

The MCP endpoint never loads or unloads a database: only an operator does, through the admin
API, the console or the server's web UI. A database unloaded while a session has it
selected answers the same error until it is loaded again (the session keeps its selection).

A per-database endpoint of a database that is not loaded answers the mount's `409` (see the
[Admin API](../reference/admin_api.md#load-and-unload)), an unknown name `404`.

### Read-only

- A base (`role = "base"`) is read-only for good: its agents see only the reading tools.
- A database whose read-only flag an operator turned on (`PUT
  /api/admin/databases/<name>/read-only`) keeps its tool list, but every write of an agent is
  refused with a read-only error at once; turning it off allows writes again (no reload).

## Limits of this version

- No admin tools over MCP: an agent cannot create, delete, load, unload or change a database.
- Access follows `access.toml` (the old `[mcp] token` is gone; `server.toml` refuses it):
  `/mcp` lists and serves only the databases the caller's user (or anonymous) is granted, a
  hidden database answers exactly like an unknown name, a `read` grant gets the reading tools
  and profile; `/db/<name>/mcp` answers `401` (anonymous) or `404` (a user) without a grant. An
  MCP session belongs to the principal that initialized it: another principal using its id
  gets `404`. Tokens travel in clear text over plain HTTP.
- MCP sessions survive a restart: they are kept in the server directory's `sessions/mcp.json`
  (hashes of the ids only; [sessions/](../reference/server_directory.md#sessions)), so a client goes on with its session id, its
  selection and a pending rename note. Revoking or reissuing a user's token, or removing the
  user, ends that user's MCP sessions at once (their clients get `404` and initialize again).
