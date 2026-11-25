# MCP endpoints

**EXPERIMENTAL.** graphersal-server serves its databases to AI agents over the Model Context
Protocol (Streamable HTTP transport, JSON answers). There are two kinds of endpoint, served by
ONE endpoint object (the same MCP sessions, token and statistics for both):

| Endpoint | Serves | Tools |
|---|---|---|
| `/mcp` | every database of the server directory | `list_graphs`, `use_graph`, and the data tools, each with an optional `graph` argument |
| `/db/<name>/mcp` | the loaded database `<name>` (or the database an alias names, resolved per request) | the data tools of one graph (as the graphersal dev server's `/mcp` offers them) |

Configure an agent once with `/mcp`; use a per-database endpoint when an agent should only ever
see one database.

## Configuration

`server.toml`, read when the server starts (restart it after a change):

```toml
[mcp]
enabled = true          # default; false: /mcp and every /db/<name>/mcp answer 404
# token = "<secret>"    # optional: every MCP request needs Authorization: Bearer <secret>
max_sessions = 256      # MCP sessions remembered, the oldest forgotten first (at least 1)
```

`GET /api/info` at the root announces the endpoint: `"mcp": {"endpoint": "/mcp", "readOnly":
false, "client": {...} | null}` (`client`: the last client that initialized), or `"mcp": null`
when MCP is off. A database's `GET /db/<name>/api/info` announces its own endpoint
(`"endpoint": "/db/<name>/mcp"`, `readOnly` of that database).

### Adding the endpoint to an agent

Claude Code (the server on its default address):

```bash
claude mcp add --transport http graphersal http://127.0.0.1:8484/mcp
# with a token:
claude mcp add --transport http graphersal http://127.0.0.1:8484/mcp \
  --header "Authorization: Bearer <secret>"
# one database only:
claude mcp add --transport http acme http://127.0.0.1:8484/db/acme/mcp
```

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
| `Authorization` | with a `token` | `Bearer <token>` (else `401`) |

A notification (such as `notifications/initialized`) is answered `202` without a body. With
curl (`scripts/smoke.sh` walks through the whole flow):

```bash
curl -si http://127.0.0.1:8484/mcp -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion":
       "2025-06-18", "capabilities": {}, "clientInfo": {"name": "curl", "version": "1"}}}'
# then, with the Mcp-Session-Id of that answer:
curl -s http://127.0.0.1:8484/mcp -H 'Content-Type: application/json' \
  -H 'Mcp-Session-Id: <id>' \
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

The per-database endpoint `/db/<name>/mcp` resolves `<name>` per request: after a rename of an
alias, `/db/<old name>/mcp` answers `404`.

### Reconnecting

The selection belongs to the MCP session. A client that reconnects (a new `initialize`) starts
without a selection and calls `use_graph` again; the error of its first call says so. The
server remembers at most `max_sessions` sessions: the oldest is forgotten first, its client gets
`404` for its unknown session id, initializes again and selects its database again. `DELETE
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

A per-database endpoint of a database that is not loaded answers the mount's `409` (see
`docs/admin-api.md`), an unknown name `404`.

### Read-only

- A base (`role = "base"`) is read-only for good: its agents see only the reading tools.
- A database whose read-only flag an operator turned on (`PUT
  /api/admin/databases/<name>/read-only`) keeps its tool list, but every write of an agent is
  refused with a read-only error at once; turning it off allows writes again (no reload).

## Limits of this version

- No admin tools over MCP: an agent cannot create, delete, load, unload or change a database.
- No access control beyond the optional token: whoever reaches the port (and knows the token,
  if one is set) can read and change every loaded database that is not read-only. Keep the
  server on a loopback address (the default `127.0.0.1:8484`).
- One token for every endpoint; no per-database or per-agent policy.
- MCP sessions live in memory: a server restart forgets them (clients initialize again).
