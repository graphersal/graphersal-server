# The server directory (EXPERIMENTAL)

Everything `graphersal-server` keeps lives in ONE directory: its configuration, its databases,
the shared embedding models, backups and deleted databases. `graphersal-server init DIR` creates
it; `graphersal-server serve --dir DIR` serves it. A directory is a server directory when it
holds `server.toml`.

The directory is private to the OS user that owns it, the operator: `init` creates it with mode
`0700` (unix), `sessions/` `0700` and the secret files (`access.toml`, `server.run`,
`sessions/*.json`) `0600`; `serve` and the console refuse a directory or secret owned by
another user or open to group or others ([`access.md`](access.md#the-security-model)).

```
<server-dir>/
  server.toml          configuration
  access.toml          users (token hashes), roles and grants (mode 0600)
  server.lock          one running server per directory (an OS file lock)
  server.run           the running server's address and operator token (mode 0600, only while it runs)
  server.open          the databases the running server has open (removed at a clean stop)
  recovery.json        the last unclean shutdown and its check of each database
  aliases.toml         alias -> database
  mcp.toml             the MCP switches (written by the server; missing = the defaults)
  databases/
    <name>/
      database.toml    the database's configuration
      store/           its graphersal Store (a directory Store)
  models/
    blobs/<2 hex>/<sha256>   one copy of every embedding model blob
    models.toml        derived index of the models (rebuilt, never read back)
  backups/<name>/      <UTC time>.zip + <UTC time>.toml per backup
  backups/jobs/        the backup root of backup jobs (server.toml [jobs] backup_root)
  trash/               deleted and replaced databases: <name>-<UTC time>/
  tmp/                 builds and uploads in progress (emptied at start)
  imports/             the import inbox: files and Store directories an admin imports by name
  sessions/            (mode 0700)
    browser.json       browser sessions (hashes only, mode 0600): they survive a restart
    mcp.json           MCP sessions (hashes only, mode 0600): they survive a restart
  logs/                (mode 0700, made by the first `start`)
    server.log         stdout and stderr of a server `start` runs in the background (mode 0600)
```

`serve` logs to stderr (redirect it to keep a log file). `graphersal-server start` runs
`serve` in the background and appends its stdout and stderr to `logs/server.log` (created on
the first `start`: `logs/` mode `0700`, the log `0600` on unix; each start adds a line `== <UTC
time> graphersal-server start: serve --dir ... ==`). Nothing rotates the log: truncate or
rotate it while the server is stopped (or use `copytruncate`). A `logs/` directory an earlier
version's `init` created is reused.

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
(`admin-api.md`); an unknown key is an error. The model (principals, levels, tokens, logins,
modes): [`access.md`](access.md).

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
| `[mcp] enabled` | `true` | at start | serve the MCP endpoints (`docs/mcp.md`) |
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

The login cookie (`gs_session`) carries `Max-Age` = `[sessions] lifetime_secs` when set, else
400 days (the longest a browser keeps a cookie), so a browser restart keeps the login; the
server still ends the session by its own limits. The logout clears it with `Max-Age=0`.

Query limits: `[limits.query]` here applies to every database; a database's `database.toml`
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

The `[defaults]` apply to databases created empty (`db create`) and to snapshot (`.gsnap`),
GraphSON and GraphML imports. A clone, a restore and a Store (`.gstore`, Store directory) import
carry the creation parameters of their source.

## `server.lock` and `server.run`

`server.lock` holds the OS's advisory file lock (`flock` on Unix, `LockFileEx` on Windows) while
a server or an offline console command works on the directory; the OS releases it when the
process ends, also on a crash. The file itself stays and means nothing when unlocked. A second
`serve` on a locked directory is refused.

`server.run` is written once the listener is bound (mode `0600` on unix) and removed at a
clean stop:

```toml
address = "127.0.0.1:51234"
url = "http://127.0.0.1:51234/"
pid = 12345
version = "0.3.0"
operator_token = "<64 hex digits>"
started_at_ms = 1764151975000
```

It holds the address actually bound (port `0` resolved) and the operator token of this run: a
new one at every start, an admin credential the console sends (`Authorization: Bearer`), so
whoever reads the file can act as an admin while the server runs ([`access.md`](access.md)). The console reads it to reach a
running server; it is trustworthy only while `server.lock` is held (a crash leaves it behind,
the next server overwrites it).

`graphersal-server status` reads both: a held lock is a running server (its facts come from
`GET /api/admin/server`, the uptime from `started_at_ms` when the server does not answer); a
`server.run` without the lock is reported as stale. `graphersal-server stop` removes a stale
one (holding the lock meanwhile) and `start` refuses while the lock is held.

## `server.open` and `recovery.json`

`server.open` (round 3) names the databases the running server has open: rewritten
atomically (with the directory's `fsync`) when a load starts, when it ends and after an unload,
and removed at a clean stop (after every Store closed cleanly). A database whose Store could not
be closed cleanly at an unload stays listed.

```toml
pid = 12345
started_at_ms = 1764151975000
databases = ["acme", "orders"]
```

A `server.open` (or a stale `server.run`) found by the next `serve` (with the lock held) means
the last server did not stop cleanly: see [After a crash](#after-a-crash). `stop` on a
stopped directory removes a stale `server.run`, never `server.open`, so the next start still
checks those databases.

`recovery.json` is the last unclean shutdown and its check, written by the server (JSON, the
shape of `uncleanShutdown` in [`admin-api.md`](admin-api.md#server)): when it was found, the
pid of the server that did not stop, whether its open set was known, and per database the
outcome (`pending`, `checking`, `ok`, `damaged`, `failed`, `needs-model`, `missing`), the state
afterwards, what the open of its Store did (clean close, journal commits replayed, the position,
a torn tail cut, ...), what the verify found and the damage report. It stays until the next
unclean shutdown replaces it; deleting it while the server is stopped only forgets the report.

## After a crash

A server killed by force (`kill -9`, an OOM kill, a power loss, a crash) leaves its databases'
Stores open. Nothing has to be repaired by hand first:

1. **The lock.** The OS releases `server.lock` with the process; `server.run` (and
   `server.open`) stay behind. `status` reports the stale `server.run` and, under "Last
   unclean shutdown", what the next start will check.
2. **The journal.** Every commit a client got an answer for is in its database's WAL; the next
   open of the Store replays it onto the last snapshot and cuts an interrupted last write (a
   commit in flight at the kill may or may not be there; it was never acknowledged).
3. **The check at the next start.** `serve` (or `start`) finds the stale files, says so in the
   start block (`UNCLEAN SHUTDOWN` and an "Unclean shutdown" section) and the log, records an
   audit event, and checks every database of `server.open` on a background thread: it is loaded
   again (also without `auto_load`: the server serves again what it served before), its Store's
   open report is kept, and the open Store is verified (`Store::verify`: every checksum,
   snapshot and WAL record). Each result is logged (`unclean shutdown: database 'x' is ok: ...`),
   audited (`database 'x' checked: ok (...)`) and written to `recovery.json`. Without
   `server.open` (a crash of a server older than 0.3.0) the open set is unknown and the
   `auto_load` databases are checked.
4. **Damage.** A Store the engine finds damaged at the open opens in maintenance mode, and one
   whose verify finds damage under the damage policy `maintenance` (the default) is frozen by
   the engine: the database is served **read-only** in the state `maintenance` with its damage
   report (`db list`, `db info`, `status`, the Databases tab, the Database settings). Queries read
   the intact data; writes are refused.
5. **Per database:** `db verify NAME` (or `--all`) scrubs a Store again at any time (exit 1 on
   damage). `db repair NAME [--name NEW]` builds a repaired copy as a NEW database (default
   `NAME-repaired`; the engine's `repair_to`: damaged snapshot chunks rebuilt from older
   snapshots and the WAL, what has no donor reported as lost, elements whose state differs from
   the journal reported as diverged; the copy is verified before it is registered; the damaged
   database is never written). Then either `db swap NAME NEW --yes` (the copy takes the name;
   the damaged one moves to `trash/`; grants, aliases, MCP settings and `database.toml` of the
   name stay) or keep both. A repaired copy starts a new history: older snapshots, marks and
   the store id (so the backup chain) are not carried. Layers and bases are not repaired.
6. **Or a backup.** `db backups NAME` lists the verified backups; `db restore NAME BACKUP`
   (unloaded) or `--name NEW` brings one back: everything after the backup is lost, so prefer a
   lossless repair when the repair report says so.

The same routes for scripts: `POST /api/admin/databases/<name>/verify`, `POST
/api/admin/verify`, `POST /api/admin/databases/<name>/repair`, `POST
/api/admin/databases/<name>/swap`, `GET /api/admin/recovery`
([`admin-api.md`](admin-api.md#verify-repair-and-swap)).

## Running as a service

`start`/`stop`/`status` are for a person at a terminal. A service manager runs `serve` in the
foreground instead: it keeps the process, its restarts and its log itself. `serve` stops
cleanly on SIGTERM (databases unloaded, `server.run` removed, the lock released) and writes the
log to stderr. Run it as the dedicated user that owns the directory (the directory's owner is
its operator: [`access.md`](access.md#the-security-model)).

systemd (`/etc/systemd/system/graphersal-server.service`):

```ini
[Unit]
Description=graphersal-server
After=network.target

[Service]
User=graphersal
Group=graphersal
ExecStart=/usr/local/bin/graphersal-server serve --dir /srv/graphersal --listen 127.0.0.1:8484
KillSignal=SIGTERM
TimeoutStopSec=120
Restart=on-failure
UMask=0077

[Install]
WantedBy=multi-user.target
```

`systemctl enable --now graphersal-server`; the log is `journalctl -u graphersal-server`. The
first admin's token of a directory without `access.toml` lands in the journal: run
`graphersal-server init /srv/graphersal` as that user first (it prints the token once on the
terminal).

launchd (`~/Library/LaunchAgents/org.graphersal.server.plist` for a user agent, or
`/Library/LaunchDaemons/` with a `UserName` key for a daemon):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>org.graphersal.server</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/graphersal-server</string>
    <string>serve</string>
    <string>--dir</string><string>/Users/me/graphersal</string>
    <string>--listen</string><string>127.0.0.1:8484</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ExitTimeOut</key><integer>120</integer>
  <key>StandardErrorPath</key><string>/Users/me/graphersal/logs/server.log</string>
  <key>StandardOutPath</key><string>/Users/me/graphersal/logs/server.log</string>
</dict>
</plist>
```

`launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/org.graphersal.server.plist`; stop it
with `launchctl bootout gui/$(id -u)/org.graphersal.server` (SIGTERM). Create `logs/` first
(`mkdir -m 700 /Users/me/graphersal/logs`).

`graphersal-server status --dir DIR` works on a server a service manager runs too; `stop` would
stop it as well, but the manager may restart it (systemd `Restart=on-failure` does not: the
stop is a clean exit).

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

The runtime switches of the MCP endpoints ([`mcp.md`](mcp.md#switches-at-runtime-the-mcp-tab-mcptoml)),
written by the server (the web UI's MCP tab, `PUT /api/admin/mcp...`, `graphersal-server mcp
...`), whole and atomically; a missing file means the defaults (MCP on, every database
offered, nothing forced read-only). A file that does not parse (an unknown key included) stops
`serve` and the console with the line it points at.

```toml
enabled = true          # false: /mcp and every /db/<name>/mcp answer 503 (sessions kept)

[databases.acme]        # a database or an alias, by name
exposed = false         # hidden from MCP (list_graphs, use_graph, /db/acme/mcp)
read_only = true        # MCP sessions only read it, whatever their grant
```

A name with the defaults is not written. A renamed alias keeps its settings; a deleted database
or removed alias loses them. `server.toml [mcp] enabled = false` overrides `enabled = true` here
(no endpoint at all).

## `databases/<name>/`

There is no registry file: the databases ARE the directories under `databases/`, read at start.
Names are unique case-insensitively. A NEW database or alias name (and a new user name) has
at most 32 characters, `[a-z0-9]` first, `[a-z0-9._-]` inside, a letter or digit last, no `..`
and no Windows device name (`con`, `nul.db`, `com1`, ...). A new database or alias cannot be
named `jobs` either (reserved: the default backup root `backups/jobs/` would also hold the
admin backups of a database of that name). A directory, alias or user named by 0.2.0 or
earlier under the older rule `[a-z0-9][a-z0-9._-]{0,62}` (not ending in `.`; so up to 63
characters, a trailing `-` or `_`, `..` inside, or `jobs`) is still loaded, served, granted
and restored under its own name; only making a new one is refused.

**Long paths on Windows.** A database's files live at `<server-dir>\databases\<name>\store\...`,
and the Store adds directories and file names of its own below that (and backups, `tmp\`,
`trash\` add a time stamp to the name). Windows refuses a path over 260 characters
(`MAX_PATH`) unless long paths are enabled (the `LongPathsEnabled` policy). The 32-character
name limit keeps the server's part short; keep the server directory itself short as well
(e.g. `C:\gs\srv`, not a deep folder under the user profile). A directory whose `database.toml` or Store is broken is listed as `failed`
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

### Backup jobs

Scheduled backup jobs (the playground's Jobs panel of a database, `POST
/db/<name>/api/jobs/define` with `"action": "backup"`; incremental directory backups run by
the database's job ticker while it is loaded, or at once with `jobs/run`) write only into the
**backup root**, `server.toml [jobs] backup_root` (default `backups/jobs/`; it may name a
mounted storage):

- a job's `path` is relative to the root: `"path": "nightly"` writes into
  `<backup root>/nightly/`; the job stores the resolved path;
- an absolute `path` is refused when the job is defined, also one inside the root (`200
  {"ok": false, "errors": [{"field": "path", "message", "help"}]}`, the form's field error);
- a path leaving the root (`..`, a symbolic link inside the root pointing out of it) is
  refused the same way by the database's file root;
- a catalog loaded through the playground (`catalog/load`) with a job writing outside the root
  is refused (`403`, nothing loaded);
- the Store panel's fork, attic fork and backup take a `dir` inside the root only (relative to
  it; outside: `403` "paths must lie inside the server's file root"); without a `dir` they
  write next to the database's store as before;
- at load, a stored job writing outside the root (an imported, restored or cloned store, or
  a root moved in `server.toml`) is **dropped** from the database's catalog before the
  database is served, with a warning on stderr naming the job and its path: define it again
  with a path inside the root.

Queries (the playground's `execute`, the MCP tools) never touch files: they cannot define a
backup job at all. The admin backups above (`backups/<name>/`) are the server's own and are not
affected. Keep the root's other writers trusted: a symbolic link created inside it after a
job was checked is not seen.

## `trash/`

A deleted database, and the database a restore replaced, move to `trash/<name>-<UTC time>/`
(recoverable by hand: stop the server and move it back into `databases/`). Nothing empties
`trash/` automatically.

## `tmp/`

New databases, backups, exports, upload bodies (imports and models) and unpacked model archives
are written in `tmp/` and renamed into place (or streamed or read, then removed). The server empties it at every start; leave it alone while a server
runs. (The temporary files of the atomic writes of configuration files live beside their file,
not here: [Concurrent changes](#concurrent-changes).)

## `imports/`

The import inbox: put a `.gsnap`, a `.gstore`, a Store directory, a GraphSON or a GraphML file
here by any means (scp, a mounted volume, a shared folder), and an admin imports it by its NAME:
the web UI's Import tab lists the inbox ("A file of the server's import inbox"), the admin API
lists it (`GET /api/admin/imports`) and imports one entry (`POST /api/admin/import {"inbox":
"<file>", "name", ...}`), the console as `graphersal-server import DIR --list` and
`graphersal-server import DIR --inbox FILE --name NAME`. The import copies what it needs: the
file stays in the inbox (remove it yourself when done); the import runs within the request, as
every import does.

The inbox is flat: an inbox name is ONE entry of `imports/` (no subdirectory paths; a name with
`/`, `\`, `:` or a leading `.`, `..` included, is refused). Entries starting with `.` (such as
`.DS_Store`, or a copy in progress named `.acme.gsnap.part`) are not listed. A symbolic link in
the inbox is followed only when it resolves inside `imports/`; one leading outside is neither
listed nor imported. `init` creates `imports/`; an older server directory gets it at its next
`init` or at the first listing.

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

## Concurrent changes

Many browsers, scripts and the console may change the server at the same time. The files stay
whole and correct:

- **Atomic writes.** Every configuration file is written to a temporary file beside it
  (`<file>.tmp-<pid>-<n>`, a new name for every write, so two writers never share one), flushed
  to the disk (`fsync`), renamed over the file, and on unix the directory is flushed too (not
  for `server.run`, which a power loss makes stale anyway): a reader, a crash or a power loss
  sees the old file or the whole new one, never a mix. A crash
  between the write and the rename leaves the temporary file; the next `serve` (and the console
  on a stopped server) removes it at start: only names this server makes
  (`<file>.tmp-<pid>-<n>`, `<file>.tmp` of the files below as written by 0.2, `access.toml.new-<pid>`),
  in the root, `sessions/`, `models/`, `databases/<name>/` and `backups/<name>/`.
- **One lock per file, held across read, change and write.** The change is made on a copy, the
  file is written, and only then does the server's memory take the new state: a failed write
  changes nothing.
- **Revisions against lost updates.** Users and grants, aliases, each database's settings and
  the MCP settings carry a `revision` that a change may name; a change based on an older state is refused
  (`409`, help `changed by someone else: reload`; [`admin-api.md`](admin-api.md#revisions-concurrent-changes)).

What writes which file at runtime, and under which lock:

| File | Writers | Lock |
|---|---|---|
| `server.toml` | `init` only (no admin request changes it yet) | the server lock (`init` of an existing directory: none) |
| `server.lock` | `serve`, the offline console, `start` | the OS file lock itself |
| `server.run` | `serve` at start (removed at stop); `stop` removes a stale one | the server lock |
| `server.open` | the loader: each load start, load end and unload (removed at a clean stop) | its writer's mutex (a leaf, taken before the loader's state is read, never while it is held) |
| `recovery.json` | the unclean-shutdown check at start (one write per database checked) | its report's mutex (a leaf) |
| `access.toml` | user and grant changes (admin API, console), alias rename/remove and database delete (grants follow), a new name taking leftover grants, the first admin (`init`, `serve`: created new, never replaced) | the access file's lock (`Access`), online; offline the server lock (one console command) |
| `aliases.toml` | alias set/flip, rename, remove | the registry's write lock |
| `mcp.toml` | the MCP switches (admin API, MCP tab, console `mcp`), alias rename (settings move) and remove, database delete and a new name (settings dropped) | the MCP settings' write lock (a leaf: it takes no other lock; a change of a name's settings holds the registry's read lock around it while the name must exist) |
| `databases/<name>/database.toml` | read-only, auto-load, promote, the `needs_model` marker, the settings change (description, read-only, auto-load and the `[limits.query]` overrides in ONE write: `PUT /api/admin/databases/<name>/settings`, `db set`, the Database settings tab); a new database's file is written in its build in `tmp/` before the rename into `databases/` | the registry's write lock, held across the revision check, the read of the file's current limits, the write and the memory update (read-only, also through the settings, also the loader's flip lock, taken before the registry) |
| `databases/<name>/store/` | the graphersal engine (the Store, its WAL, the database's job definitions) | the engine's own (one open Store per database) |
| `databases/` (add, remove), `trash/` | create, clone, import, restore (rename of a build), repair (a new database built in `tmp/`), delete, restore over a database, swap (the old directory to `trash/`, the copy renamed) | the registry's write lock; delete, promote and a restore over a database also hold the loader's lock (nothing loads meanwhile) |
| `sessions/browser.json` | login, logout, a session touched or ended | the browser session table's mutex |
| `sessions/mcp.json` | MCP sessions (new, `use_graph`, rename note, end), token changes | its writer's mutex (the MCP endpoint writes snapshots in order) |
| `models/blobs/`, `models/models.toml` | adopting blobs, the models listing (rebuilds the index), model add/remove | the models lock |
| `backups/<name>/<time>.zip`, `<time>.toml` | backups (built in `tmp/`, then renamed) | the backup naming lock (two backups in one second get `-2`) |
| `backups/jobs/` | backup jobs of the engine | the engine's job runner |
| `tmp/` | builds, uploads, exports, unpacked models | none needed: every name is unique (a counter) |
| `imports/` | nothing (created only; the admin fills it) | |
| `logs/server.log` | `start` (one line, appended), then the server's stdout and stderr | append mode |

Lock order (a thread holding one lock takes only those after it, so no two requests can
deadlock): the loader's state lock, then the loader's read-only flip lock, then the registry
(write or read), then the access file, then a session table (browser or MCP); the models lock,
the backup naming lock, the MCP settings' lock, the `server.open` writer's lock and the
recovery report's lock take no other lock (the `server.open` writer reads the loader's state
inside its own lock, never the other way round). The audit records go to the log, no file of
their own.

## External backups of the whole directory

Stop the server (or accept the state of a running one), then copy the directory with
`rsync -aH` (`-H` keeps the hard links between `models/blobs/` and the databases). `cp -r` or a
zip duplicates every linked model blob: bigger, still correct. For a consistent copy of one
database while it is served, use `graphersal-server db backup NAME`.
