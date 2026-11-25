# The server directory (EXPERIMENTAL)

Everything `graphersal-server` keeps lives in ONE directory: its configuration, its databases,
the shared embedding models, backups and deleted databases. `graphersal-server init DIR` creates
it; `graphersal-server serve --dir DIR` serves it. A directory is a server directory when it
holds `server.toml`.

```
<server-dir>/
  server.toml          configuration
  access.toml          users (token hashes), roles and grants (mode 0600)
  server.lock          one running server per directory (an OS file lock)
  server.run           the running server's address (only while it runs)
  aliases.toml         alias -> database
  databases/
    <name>/
      database.toml    the database's configuration
      store/           its graphersal Store (a directory Store)
  models/
    blobs/<2 hex>/<sha256>   one copy of every embedding model blob
    models.toml        derived index of the models (rebuilt, never read back)
  backups/<name>/      <UTC time>.zip + <UTC time>.toml per backup
  trash/               deleted and replaced databases: <name>-<UTC time>/
  tmp/                 builds and uploads in progress (emptied at start)
  sessions/
    browser.json       browser sessions (hashes only, mode 0600): they survive a restart
    mcp.json           MCP sessions (hashes only, mode 0600): they survive a restart
```

There is no log directory: the server logs to stderr (redirect it to keep a log file). A
`logs/` directory an earlier version's `init` created is left alone.

`init DIR` accepts a missing or empty directory, or an existing server directory (then it only
creates the missing parts and keeps `server.toml` as it is).

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
(`admin-api.md`); an unknown key is an error.

## `server.toml`

TOML; every key is optional (its default applies) and an unknown key is an error, so a typo
stops the server at start instead of being ignored. `init` writes a commented file with the
defaults:

```toml
listen = "127.0.0.1:8484"   # IP:PORT or localhost:PORT; port 0 = a free port

[mcp]
enabled = true              # /mcp and /db/<name>/mcp (false: both answer 404)
max_sessions = 256          # MCP sessions remembered, the oldest forgotten first

[defaults]                  # creation parameters of NEW databases
chunk_bytes = 1048576       # uncompressed snapshot chunk size of a new Store (>= 1)
damage_policy = "maintenance"   # or "continue": what a Store does on damage

[limits]
upload_bytes = 4294967296   # the largest import upload (POST /api/admin/import), 4 GiB
max_connections = 256       # open connections served at once (one more: 503 + Retry-After)
header_timeout_secs = 30    # a request's head must arrive within this
idle_timeout_secs = 60      # a connection without a running request is closed after this

[sessions]                  # how long a browser session lasts; no limit when left out
# lifetime_secs = 2592000   # since its login
# idle_timeout_secs = 604800   # since its last request
```

| Key | Default | Read | Meaning |
|---|---|---|---|
| `listen` (top level) | `127.0.0.1:8484` | at start | the listen address; `serve --listen ADDR` overrides it. Any address that is not a loopback address gets a warning in the `serve` start block: tokens travel in clear text over plain HTTP |
| `[mcp] enabled` | `true` | at start | serve the MCP endpoints (`docs/mcp.md`) |
| `[mcp] max_sessions` | `256` | at start | MCP sessions remembered (at least 1) |
| `[defaults] chunk_bytes` | `1048576` | at every create | a new Store's chunk size (fixed for the database's life) |
| `[defaults] damage_policy` | `"maintenance"` | at every create | a new Store's damage policy (fixed for the database's life) |
| `[limits] upload_bytes` | `4294967296` | at every upload | the largest import upload body, announced as `uploadBytes` by `GET /api/info` |
| `[limits] max_connections` | `256` | at start | open connections served at once (at least 1); one more is accepted, answered `503` (`Retry-After: 1`, JSON `error` and `help`, in its own protocol) and closed; beyond 64 such refusals at once a connection is closed unanswered. `serve --max-connections N` overrides it |
| `[limits] header_timeout_secs` | `30` | at start | seconds a request's head may take to arrive, counted from the moment the connection waits for it (a new connection, or after the previous answer); a slower or silent client is disconnected. Never cuts a body. `serve --header-timeout SECS` overrides it |
| `[limits] idle_timeout_secs` | `60` | at start | seconds a connection without a running request stays open (HTTP/1.1: at most the shorter of this and `header_timeout_secs`). `serve --idle-timeout SECS` overrides it |
| `[sessions] lifetime_secs` | none (no limit) | at start | the longest a browser session lasts after its login, in seconds (at least 1); the browser logs in again afterwards |
| `[sessions] idle_timeout_secs` | none (no limit) | at start | the longest a browser session lasts after its last request, in seconds (at least 1); counted coarsely (the last request is recorded at most every minute, every quarter of a shorter timeout) |

The `[defaults]` apply to databases created empty (`db create`) and to snapshot (`.gsnap`),
GraphSON and GraphML imports. A clone, a restore and a Store (`.gstore`, Store directory) import
carry the creation parameters of their source.

## `server.lock` and `server.run`

`server.lock` holds the OS's advisory file lock (`flock` on Unix, `LockFileEx` on Windows) while
a server or an offline console command works on the directory; the OS releases it when the
process ends, also on a crash. The file itself stays and means nothing when unlocked. A second
`serve` on a locked directory is refused.

`server.run` is written once the listener is bound and removed at a clean stop:

```toml
address = "127.0.0.1:51234"
url = "http://127.0.0.1:51234/"
pid = 12345
version = "0.1.0"
```

It holds the address actually bound (port `0` resolved). The console reads it to reach a
running server; it is trustworthy only while `server.lock` is held (a crash leaves it behind,
the next server overwrites it).

## `aliases.toml`

One line per alias; an alias names a database (never another alias):

```toml
current = "acme-2026.3"
```

Aliases and database names share one namespace. `/db/<alias>/...` and the MCP endpoint resolve
an alias on every request, so a flip (`PUT /api/admin/aliases/<alias>`) takes effect at once. A
rename (`POST /api/admin/aliases/<alias>/rename`) rewrites the file in one atomic step. An
alias whose target is missing is dropped at start and reported in the listing's `problems`.

## `databases/<name>/`

There is no registry file: the databases ARE the directories under `databases/`, read at start.
Names follow `[a-z0-9][a-z0-9._-]{0,62}` (no Windows device names) and are unique
case-insensitively. A directory whose `database.toml` or Store is broken is listed as `failed`
with the reason; it never stops the server.

Every create, clone, import and restore builds the new database in `tmp/` and renames it into
`databases/` in one step: a failure leaves no database, and the debris of a crash in `tmp/` is
removed at the next start.

### `database.toml`

```toml
kind = "layer"                   # "plain" (default) | "layer"
role = "normal"                  # "normal" (default) | "base"
base = "product-2026.3"          # layers only: the role = "base" database below it
auto_load = false                # load at server start
read_only = false                # serve read-only
created = "2026-11-22T10:00:00Z" # UTC, set at creation
description = "Acme's graph"
needs_model = "org/model"        # set by an import naming a model neither it nor models/ carries
```

| Key | Default | Meaning |
|---|---|---|
| `kind` | `"plain"` | `plain`: an ordinary Store. `layer`: a layer Store over a base database (it stores only its own changes) |
| `role` | `"normal"` | `base`: a frozen database layers may be built over. It is always served read-only; loaded for reading it is an empty layer over the shared base in memory. Set by `db create --role base` (empty), `import --role base` (with the source's data) or `db promote` (an unloaded plain database); never turned back (a base is frozen forever) |
| `base` | none | a layer's base database (a `role = "base"` database) |
| `auto_load` | `false` | loaded in the background at server start |
| `read_only` | `false` | writes through the database's API and MCP endpoint are refused; a base is always read-only |
| `created` | none | the creation time (UTC, RFC 3339) |
| `description` | `""` | free text |
| `needs_model` | none | the repository of an embedding model the database's Store names but does not hold, set by an import whose source names a model that `models/` lacks too (one `models/` holds is taken from there at the import): the database is listed as `needs-model` and cannot load until `models add <repo>` (the marker is removed then, or by the first successful load) |

Unknown keys are an error (the database is listed `failed`). The server writes the file
atomically (temporary file + rename); edit it by hand only while no server runs.

The base pool: loading a layer loads its base once into memory, shared by every loaded layer
over it (and by the base itself when it is loaded for reading); the base leaves memory with its
last user. A base that layers name cannot be deleted, replaced by a restore or made writable.

### `store/`

An ordinary graphersal directory Store (snapshot chunks, journal, `blobs/`). The graphersal
tools can open it while no server serves it.

## `models/`

The server keeps ONE copy of every embedding model blob in `models/blobs/<2 hex>/<sha256>` (the
Store blob layout) and puts a HARD LINK of it into each database's `store/blobs/`. Store blobs
are content-addressed and immutable, so for the engine the link is an ordinary blob file: a
database's delete or prune removes only its link, an export embeds the bytes. Every create,
clone, import and restore links the new database's model blobs before the database appears; at
start the server does it for every idle database. An import whose source names a model without
carrying it gets the model from `models/` (linked the same way) when `models/` holds it. A
Store import whose source lacks a model blob gets the blob filled into its copy from
`models/blobs/` (checked by its hash), and the filled file is then replaced by a hard link like
any other; a blob `models/` lacks too refuses the import (add the model first), one `models/`
holds damaged refuses it naming the file.

Where the file system refuses hard links (another file system, FAT/exFAT, most SMB shares, FUSE
mounts) the blob is COPIED instead: correct, but no disk saving. The server logs a warning and
shows it in an import's `notes`, the model's row and the database's row (`linkWarning`).

`models/models.toml` is a derived index, one row per blob (repository, identity, licence and
size read from the model's own manifest inside the blob; the databases holding it; the
`needs-model` databases waiting for it; the users holding a copy). It is rebuilt at every
listing and model change and never read back: edits are lost.

`graphersal-server models list|add|remove` and `/api/admin/models` manage it
(`docs/admin-api.md`): a model is added from a Model2Vec model directory, a `.tar` or `.tar.gz`
of one (unpacked safely into `tmp/` first) or a `.gsnap` carrying models, on the server's machine
or uploaded through the web UI's Models tab (the upload goes through `tmp/` too).

## `backups/`

`backups/<name>/<UTC time>.zip` is a verified ZIP backup of the database `<name>` (the time with
dashes instead of colons, such as `2026-11-22T10-00-00Z`; `-2`, `-3`, ... when that name exists),
with a sidecar `<UTC time>.toml` recording the database's kind, role, base, description and, for
a layer, its base's identity. A layer's backup holds only the layer and restores only over a
base with the same identity. Backups stay when their database is deleted; nothing removes them
automatically.

## `trash/`

A deleted database, and the database a restore replaced, move to `trash/<name>-<UTC time>/`
(recoverable by hand: stop the server and move it back into `databases/`). Nothing empties
`trash/` automatically.

## `tmp/`

New databases, backups, exports, upload bodies (imports and models) and unpacked model archives
are written in `tmp/` and renamed into place (or streamed or read, then removed). The server empties it at every start; leave it alone while a server
runs.

## `sessions/`

The sessions that survive a restart (always on: there is no setting to turn it off), so a
release or a restart does not log browsers out or make MCP clients initialize again.

- `browser.json`: one row per browser session (`gs_session` cookie): the SHA-256 of the session
  id, the user, the SHA-256 of the token it logged in with, created and last seen (seconds since
  the Unix epoch; last seen is updated coarsely, at most every minute).
- `mcp.json`: one row per MCP session: the SHA-256 of its `Mcp-Session-Id`, the identity of its
  `initialize` (`user:<name>`, `operator`, or none for anonymous), the selected database or
  alias, a pending alias rename note, the protocol revision, the client's name and version, and
  when it was created.

```json
{
  "version": 1,
  "sessions": [
    { "id_sha256": "<64 hex>", "user": "alice", "token_sha256": "<64 hex>",
      "created_unix_s": 1795000000, "last_seen_unix_s": 1795003600 }
  ]
}
```

Neither file holds a usable id or token, only their hashes. Every change is written at once
(login, logout, a new MCP session, `use_graph`, a rename note taken, an end), the whole file
atomically, mode `0600` on unix. At start both are read back: a browser session whose user is
gone, whose user holds another token now, or that expired (`[sessions]` above) is dropped, and
so is an MCP session of a user that is gone or has no token. A missing file is no sessions; a
file that cannot be read or parsed is a warning on stderr and no sessions (the server starts;
browsers log in again, MCP clients initialize again) and is overwritten by the next change.

Revoking or reissuing a user's token, or removing the user, ends its browser and MCP sessions at
once, in memory and in these files, through the running server or with the console on the
directory (`graphersal-server user ...`). Deleting the files (while no server runs) logs every
browser out and ends every MCP session. The MCP endpoint keeps at most `[mcp] max_sessions`
sessions, the oldest forgotten first, restarts included.

## External backups of the whole directory

Stop the server (or accept the state of a running one), then copy the directory with
`rsync -aH` (`-H` keeps the hard links between `models/blobs/` and the databases). `cp -r` or a
zip duplicates every linked model blob: bigger, still correct. For a consistent copy of one
database while it is served, use `graphersal-server db backup NAME`.
