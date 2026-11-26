# Configuration Reference

Every file of the server directory that holds settings, with every key, its default and its
meaning. The files are TOML. An unknown key is an error in every one of them, so a typo stops
the server (or fails the database) instead of being ignored.

| File | Written by | Read |
|---|---|---|
| `server.toml` | you (`init` writes a commented file with the defaults) | at start, some keys at every create, upload or load |
| `access.toml` | the server and the console (`user ...`, the Users tab, the admin API) | at start, then kept in memory |
| `aliases.toml` | the server (`db alias ...`, the Databases tab, the admin API) | at start, then kept in memory |
| `mcp.toml` | the server (`mcp ...`, the MCP tab, the admin API) | at start, then kept in memory |
| `databases/<name>/database.toml` | the server (flags, settings); its `[limits.query]` also by hand | at start, and again at every load of the database |

The other files of the directory (run files, sessions, logs) are described in
[The Server Directory](server_directory.md).

## `server.toml`

Every key is optional (its default applies) and an unknown key is an error, so a typo
stops the server at start instead of being ignored. `init` writes a commented file with the
defaults:

```toml
listen = "127.0.0.1:8484"   # IP:PORT or localhost:PORT; port 0 = a free port

[mcp]
enabled = true              # /mcp and /db/<name>/mcp exist (false: both answer 404); the
                            # runtime switch is mcp.toml's enabled (init writes it off)
max_sessions = 256          # MCP sessions remembered, the oldest forgotten first

[defaults]                  # creation parameters of NEW databases
chunk_bytes = 1048576       # uncompressed snapshot chunk size of a new Store (>= 1)
damage_policy = "maintenance"   # or "continue": what a Store does on damage

[limits]
upload_bytes = 4294967296   # the largest import upload (POST /api/admin/import), 4 GiB
max_connections = 256       # open connections served at once (one more: 503 + Retry-After)
header_timeout_secs = 30    # a request's head must arrive within this
idle_timeout_secs = 60      # a connection without a running request is closed after this
# memory_limit = "auto"     # the process memory ceiling: bytes, "auto" or "none"
# memory_headroom = 268435456   # what "auto" keeps free below a cgroup limit

[limits.query]              # query limits of every database; none when left out
# timeout_ms = 30000        # evaluationTimeout of every traversal, budget of a run
# max_operations = 10000000 # Rhai operations one script may run
# max_string_size = 16777216
# max_array_size = 100000
# max_map_size = 100000
# max_traversers = 10000000
# max_materialized_bytes = 524288000
# max_value_depth = 128

[sessions]                  # how long a browser session lasts; no limit when left out
# lifetime_secs = 2592000   # since its login
# idle_timeout_secs = 604800   # since its last request

[access]
enabled = true              # false: NO access control, every request is an admin

[http]
public_scheme = "http"      # "https" when a reverse proxy ends TLS (Secure login cookie)
# allowed_hosts = ["db.example.com"]   # public names behind a reverse proxy

[jobs]
# backup_root = "backups/jobs"   # the only directory backup jobs write into
```

| Key | Default | Read | Meaning |
|---|---|---|---|
| `listen` (top level) | `127.0.0.1:8484` | at start | the listen address; `serve --listen ADDR` overrides it. Any address that is not a loopback address gets a warning in the `serve` start block: tokens travel in clear text over plain HTTP |
| `[mcp] enabled` | `true` | at start | serve the MCP endpoints (MCP) |
| `[mcp] max_sessions` | `256` | at start | MCP sessions remembered (at least 1) |
| `[defaults] chunk_bytes` | `1048576` | at every create | a new Store's chunk size (fixed for the database's life) |
| `[defaults] damage_policy` | `"maintenance"` | at every create | a new Store's damage policy (fixed for the database's life) |
| `[limits] upload_bytes` | `4294967296` | at every upload | the largest import upload body, announced as `uploadBytes` by `GET /api/info` |
| `[limits] max_connections` | `256` | at start | open connections served at once (at least 1); one more is accepted, answered `503` (`Retry-After: 1`, JSON `error` and `help`, in its own protocol) and closed; beyond 64 such refusals at once a connection is closed unanswered. `serve --max-connections N` overrides it |
| `[limits] header_timeout_secs` | `30` | at start | seconds a request's head may take to arrive, counted from the moment the connection waits for it (a new connection, or after the previous answer); a slower or silent client is disconnected. Never cuts a body. `serve --header-timeout SECS` overrides it |
| `[limits] idle_timeout_secs` | `60` | at start | seconds a connection without a running request stays open (HTTP/1.1: at most the shorter of this and `header_timeout_secs`). `serve --idle-timeout SECS` overrides it |
| `[limits] memory_limit` | none (the library's `auto`) | at start | the process memory ceiling, one setting for every database (`graphersal::exec::set_memory_limit`; the graphersal command line's `--memory-limit`): a number of bytes (`0` = none), `"auto"` (the cgroup limit minus the headroom, else 80 % of the physical memory) or `"none"`. A query, import or write that would take the process over it fails with a `memory.limit` diagnostic and rolls back. Only in `server.toml` (a `database.toml` refuses it). The start block shows the ceiling in effect when it is set |
| `[limits] memory_headroom` | none (a tenth of the cgroup limit, at least 64 MiB) | at start | what `memory_limit = "auto"` keeps free below a cgroup memory limit, in bytes (at least 1; `--memory-headroom`) |
| `[limits.query] timeout_ms` | none | at every load | the default `evaluationTimeout` of every traversal and the budget of a whole run (the Rhai code between traversals), in milliseconds (`--timeout`); `0` = no timeout |
| `[limits.query] max_operations` | none | at every load | Rhai operations one script may run (`--max-operations`); `0` = unlimited |
| `[limits.query] max_string_size` | none | at every load | the longest string a script or a step may build, in bytes: the script limit and the `traversal.max_string_bytes` default (`--max-string-size`); `0` = unlimited |
| `[limits.query] max_array_size` | none | at every load | the largest array a script may build, in items (`--max-array-size`); `0` = unlimited |
| `[limits.query] max_map_size` | none | at every load | the largest map a script may build, in entries (`--max-map-size`); `0` = unlimited |
| `[limits.query] max_traversers` | none (engine default 10M) | at every load | the `traversal.max_traversers` default: traversers one step may leave alive (`--max-traversers`); `0` = unlimited |
| `[limits.query] max_materialized_bytes` | none (engine default 1 GiB) | at every load | the `traversal.max_materialized_bytes` default: the memory one query holds at once (`--max-materialized-bytes`); `0` = unlimited |
| `[limits.query] max_value_depth` | none (128) | at every load | the deepest nesting of arrays and maps in one value, script and engine values (`--max-value-depth`); `0` = the default, never unlimited |
| `[sessions] lifetime_secs` | none (no limit) | at start | the longest a browser session lasts after its login, in seconds (at least 1); the browser logs in again afterwards |
| `[sessions] idle_timeout_secs` | none (no limit) | at start | the longest a browser session lasts after its last request, in seconds (at least 1); counted coarsely (the last request is recorded at most every minute, every quarter of a shorter timeout) |
| `[access] enabled` | `true` | at start | `false` turns access control off: nothing is refused, every request is an admin. Valid admin credentials keep their identity (the console's operator, an admin user); every other request (none, a user without the admin role, wrong credentials) runs as the admin principal `everyone`, which the audit names. Login still works but is not needed. The `serve` start block warns loudly, and louder (in the banner too) when the address is not a loopback address. `access.toml` is still read and its first admin created, so turning access back on needs no other change |
| `[http] public_scheme` | `"http"` | at start | how clients reach the server: `"http"`, or `"https"` when a reverse proxy ends TLS. `https` gives the `gs_session` login cookie `Secure` (a browser drops a `Secure` cookie over plain HTTP, except on localhost). On `http` with an address that is not a loopback address, the start block warns that tokens travel in clear text; on `https` it warns to let only the proxy reach the address |
| `[http] allowed_hosts` | `[]` | at start | further names a request's `Host` (and an `http://`/`https://` `Origin`) may name besides this machine's loopback names and the listen address: the public names behind a reverse proxy. Each a host name or IP address with an optional port (`"db.example.com"`, `"db.example.com:443"`); the port is not checked (a name is admitted on any port); lower case, no scheme or path. Other names are refused with `403` as before (DNS rebinding) |
| `[jobs] backup_root` | `"backups/jobs"` | at start | the backup root: the only directory backup jobs, and the Store panel's fork, attic fork and backup `dir`, may write into ("Backup jobs" under `backups/`). A relative path is relative to the server directory; an absolute one may name a mounted storage. Created (with its parents) by `serve` and `init`; must not be empty |

### The login cookie

The login cookie (`gs_session`) carries `Max-Age` = `[sessions] lifetime_secs` when set, else
400 days (the longest a browser keeps a cookie), so a browser restart keeps the login; the
server still ends the session by its own limits. The logout clears it with `Max-Age=0`.

### Query limits

`[limits.query]` here applies to every database; a database's `database.toml`
may carry its own `[limits.query]`, whose keys win one by one (`0` there lifts a global limit
for that database; set or cleared at runtime with `graphersal-server db set NAME limit KEY
VALUE|--clear`, `PUT /api/admin/databases/<name>/settings` or the web UI's Database settings
tab). Both files are read again whenever a database is loaded, so a change
applies at the database's next load (unload + load; a server restart loads the `auto_load`
ones again; `db info NAME` and the settings show the limits a loaded database runs under now
beside what its next load will use); a file that does not parse then fails the load (`failed` with the reason). The
key names are the graphersal command line's flags; the limits are the session presets of the
loaded database (`GET /db/<name>/api/statistics` shows them under `limits.presets`). Nothing
set: no limit at all, as in the graphersal playground. A configured limit is a ceiling: a
query may lower it for itself (`g.with("evaluationTimeout", 1000)`) but never raise or remove
it (above the ceiling, or `0`, the query fails with `OptionAboveCeiling`); a key left unset is
no ceiling.
The start block shows a `query limits` row (and a `memory ceiling` row) when `server.toml`
sets them.

### Creation defaults

The `[defaults]` apply to databases created empty (`db create`) and to snapshot (`.gsnap`),
GraphSON and GraphML imports. A clone, a restore and a Store (`.gstore`, Store directory) import
carry the creation parameters of their source.

## `access.toml`

The users, their roles and their grants per database; written by the server and the console
(atomically, mode `0600` on unix), never holding a token, only its SHA-256 and its last four
characters (for listings). `init` and the first `serve` of a directory without it make the first
admin (`admin`) and print its token once.

```toml
[users.alice]
roles = ["admin"]          # admin: every database (write) and the admin API
token_sha256 = "<64 hex digits>"
token_last4 = "a1b2"

[[grant]]
subject = "user:bob"       # or "anonymous" (everyone, also without a token), "role:<r>"
database = "acme"          # a database name or "*"
level = "read"             # or "write"
```

Change it with `graphersal-server user ... --dir DIR` (offline on the file, or through the
running server so its sessions follow at once) or the admin API's `users` and `grants` routes
([Admin API](admin_api.md#users-and-grants)); an unknown key is an error. The model (principals, levels, tokens, logins,
modes): [Users, Tokens and Grants](../guide/access.md).

## `aliases.toml`

One line per alias; an alias names a database (never another alias):

```toml
current = "acme-2026.3"
```

Aliases and database names share one namespace. `/db/<alias>/...` and the MCP endpoint resolve
an alias on every request, so a flip (`PUT /api/admin/aliases/<alias>`) takes effect at once. A
rename (`POST /api/admin/aliases/<alias>/rename`) rewrites the file in one atomic step. An
alias whose target is missing is dropped at start and reported in the listing's `problems`.

## `mcp.toml`

The runtime switches of the MCP endpoints ([MCP](../guide/mcp.md#switches-at-runtime-the-mcp-tab-mcptoml)),
written by the server (the web UI's MCP tab, `PUT /api/admin/mcp...`, `graphersal-server mcp
...`), whole and atomically. `init` writes it with `enabled = false` and `expose_new = false`:
a new server offers nothing to MCP. A missing file (a server directory made by an older
version) means MCP on, every database offered, new names hidden, nothing forced read-only. A
file that does not parse (an unknown key included) stops `serve` and the console with the line
it points at.

```toml
enabled = true          # false: /mcp and every /db/<name>/mcp answer 503 (sessions kept)
expose_new = false      # whether a NEW database or alias is offered to MCP (default false)

[databases.acme]        # a database or an alias, by name
exposed = false         # hidden from MCP (list_graphs, use_graph, /db/acme/mcp)
read_only = true        # MCP sessions only read it, whatever their grant
```

A name without an entry is offered (the rule of older versions, so an upgraded directory offers
what it offered before); a name offered and not read-only is not written. Every database or
alias the server creates gets its entry at birth: `exposed = false` unless `expose_new` is true
or it was created with `--mcp` / `"mcp": true` ([MCP](../guide/mcp.md#new-servers-and-new-databases)).
A renamed alias keeps its settings; a deleted database or removed alias loses them. `server.toml [mcp] enabled = false` overrides `enabled = true` here
(no endpoint at all).

## `database.toml`

```toml
kind = "layer"                   # "plain" (default) | "layer"
role = "normal"                  # "normal" (default) | "base"
base = "product-2026.3"          # layers only: the role = "base" database below it
auto_load = false                # load at server start
read_only = false                # serve read-only
created = "2026-11-22T10:00:00Z" # UTC, set at creation
description = "Acme's graph"
needs_model = "org/model"        # set by an import naming a model neither it nor models/ carries

[limits.query]                   # this database's query limits, key by key over server.toml's
timeout_ms = 5000
max_traversers = 0               # 0: no limit here, whatever server.toml says
```

| Key | Default | Meaning |
|---|---|---|
| `kind` | `"plain"` | `plain`: an ordinary Store. `layer`: a layer Store over a base database (it stores only its own changes) |
| `role` | `"normal"` | `base`: a frozen database layers may be built over. It is always served read-only; loaded for reading it is an empty layer over the shared base in memory. Set by `db create --role base` (empty), `import --role base` (with the source's data) or `db promote` (an unloaded plain database); never turned back (a base is frozen forever) |
| `base` | none | a layer's base database (a `role = "base"` database) |
| `auto_load` | `false` | loaded in the background at server start |
| `read_only` | `false` | writes through the database's API and MCP endpoint are refused; a base is always read-only |
| `created` | none | the creation time (UTC, RFC 3339) |
| `description` | `""` | free text, one line (the settings change refuses line breaks and control characters, at most 1000 characters) |
| `[limits.query] ...` | none (`server.toml`'s) | the database's own query limits, the same keys as `server.toml`'s `[limits.query]`; each key set here wins over the global one. Read at every load, so a change applies at the next load. Set or cleared at runtime by the settings change (`db set NAME limit KEY VALUE\|--clear`, `PUT /api/admin/databases/<name>/settings`, the Database settings tab; validated like this file); may also be edited by hand while the server runs (every other rewrite of the file keeps the file's limits). `[limits]` holds only `query`: the memory ceiling is server-wide |
| `needs_model` | none | the repository of an embedding model the database's Store names but does not hold, set by an import whose source names a model that `models/` lacks too (one `models/` holds is taken from there at the import): the database is listed as `needs-model` and cannot load until `models add <repo>` (the marker is removed then, or by the first successful load) |

Unknown keys are an error (the database is listed `failed`). The server writes the file
atomically (temporary file + rename); edit it by hand only while no server runs, except its
`[limits.query]` (read again at every load). While the server runs, change the description,
the flags and the limits through the settings change instead (`graphersal-server db set`,
the admin API, the web UI's Database settings tab): it is based on the file's revision, so a
change someone else made meanwhile is refused (`409`) instead of being overwritten.

The base pool: loading a layer loads its base once into memory, shared by every loaded layer
over it (and by the base itself when it is loaded for reading); the base leaves memory with its
last user. A base that layers name cannot be deleted, replaced by a restore or made writable.

## Limits at a glance

- **Query limits** (`[limits.query]` in `server.toml`, and per database in its
  `database.toml`, read at every load) are **ceilings**. A query may lower a limit for itself
  (`g.with("evaluationTimeout", 1000)`), but it can never raise or remove one: that query
  fails with `OptionAboveCeiling`. A key left unset is no ceiling: the engine's default then
  applies, and a query may raise it. To cap a limit at the engine's default, set the key to
  that value explicitly (for example `max_traversers = 10000000`). `0` means no limit (for
  `max_value_depth`, the default).
- **The memory ceiling** (`[limits] memory_limit`, server-wide) is the process memory limit.
  A query, import or write that would cross it fails and rolls back.
- **The backup root** (`[jobs] backup_root`, default `backups/jobs/`) is the only directory
  backup jobs, and the Store panel's fork, attic fork and backup `dir`, write into. Paths are
  relative to it; absolute or escaping paths are refused. Jobs found outside it at load are
  dropped with a warning. Queries never touch files. The name `jobs` is reserved for databases
  and aliases, because admin backups of a database named `jobs` would land in the default
  root.
- **The import inbox** (`imports/`): files put there by any means (scp, a mounted volume) are
  imported by an admin by name (`import DIR --inbox FILE`, the Import tab,
  `POST /api/admin/import {"inbox"}`). The inbox is flat, and a link leading outside it is
  refused. The source path form (`{"path"}`) is admin-only as well.
