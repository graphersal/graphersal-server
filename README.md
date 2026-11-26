# graphersal-server

**EXPERIMENTAL. Private until its first public version.** Everything here (commands, files,
routes, answers) may change without notice.

A server for [Graphersal](https://github.com/graphersal/graphersal) graph databases: ONE process
serving MANY databases from one server directory, over HTTP. A database is either a plain Store
or a layer over a shared, frozen base database (many layers, one base in memory). Each loaded
database gets the Graphersal playground and its HTTP API under `/db/<name>/`; one MCP endpoint
(`/mcp`) lets an AI agent list the databases and pick one per session; the admin operations
(create, load, clone, back up, restore, export, import, aliases, shared embedding models, the
list of running queries with a stop per query) are
an HTTP API, a console (`graphersal-server db ...`) and the server's own web UI at `/`.

> **Access per user.** `access.toml` holds the users (token hashes) and their grants per
> database; `init` (or the first `serve` of a directory without the file) creates the first
> admin and prints its token once. The admin API and Runs are for admins; a database a user has
> no grant on does not exist for them; a `read` grant makes every request read-only; MCP
> follows the same grants. Tokens travel in clear text over plain HTTP: the server listens on
> the loopback (`127.0.0.1:8484`) by default and prints a warning when told to listen on any
> other address. Every request but the health probes is also checked by `Host` and `Origin`
> (against DNS rebinding and cross-site pages). Behind a reverse proxy that ends TLS, set
> `server.toml`'s `[http] public_scheme = "https"` (a `Secure` login cookie) and
> `allowed_hosts` (the public names); `[access] enabled = false` turns access control off
> (every request is an admin; the start block warns). See [`docs/access.md`](docs/access.md).

## Download / build

**Windows x86_64:** each release on GitHub carries a prebuilt binary,
`graphersal-server-v<version>-windows-x86_64.zip` (`graphersal-server.exe` with
`LICENSE-APACHE`, `NOTICE`, this README and `THIRD-PARTY-LICENSES.html`), and `SHA256SUMS` to
verify it (`certutil -hashfile <zip> SHA256`, or `Get-FileHash` in PowerShell). Unpack it and run
`graphersal-server.exe` from a console; Ctrl+C (or Ctrl+Break) stops it cleanly.

**Every other platform** (Linux, macOS, ...) builds from source, with Rust 1.88 or newer. The
Graphersal crates come from crates.io (exact pins in the root `Cargo.toml`, today `=0.8.0`,
versions in the committed `Cargo.lock`; `--locked` builds exactly those); the published `graphersal-serve` crate
also carries the playground built into the binary.

```bash
git clone https://github.com/graphersal/graphersal-server.git
cd graphersal-server
cargo build --release          # target/release/graphersal-server
cargo build --profile dist     # the release binary's profile: fat LTO, stripped symbols
```

## Run

```bash
graphersal-server init ./srv                          # create a server directory
graphersal-server serve --dir ./srv                   # serve it on 127.0.0.1:8484
graphersal-server serve --dir ./srv --listen 127.0.0.1:0   # on a free port
```

Open `http://127.0.0.1:8484/`: the server's web UI lists the databases with their states (see
[Web UI](#web-ui)). `serve` prints its start block on stderr (a banner, then the sections
Server, Databases, Embedding models and MCP, laid out like the graphersal CLI's dev server) and
only the address line `Graphersal server: http://...` on stdout. The log follows on stderr
(`RUST_LOG`, default `info`; colours only on a terminal): redirect stderr to keep a log file.
`serve` writes the address it bound into `./srv/server.run` while it runs;
Ctrl+C or SIGTERM stops it cleanly (databases are unloaded, the run file is removed). A second
`serve` on the same directory is refused (`server.lock`).

`serve` speaks HTTP/1.1 and h2c (HTTP/2 over plain TCP with prior knowledge, e.g. `curl
--http2-prior-knowledge`) on the same port. Connection limits (`server.toml`'s `[limits]`, each
overridden by a `serve` option; the start block's `connections` and `timeouts` rows show
them):

| Limit | Default | `serve` option | What |
|---|---|---|---|
| `max_connections` | 256 | `--max-connections N` | open connections served at once; one more is accepted, answered `503` with `Retry-After: 1` and closed (a client learns at once instead of hanging) |
| `header_timeout_secs` | 30 | `--header-timeout SECS` | a request's head must arrive within this, counted from the moment the connection waits for it; a slower (or silent) client is disconnected |
| `idle_timeout_secs` | 60 | `--idle-timeout SECS` | a connection without a running request is closed after this (HTTP/1.1 waits for the next request's head at most the shorter of the two) |

No timeout covers a whole request: an upload streams as long as it keeps sending (each read of
a body waits at most 120 s for the next piece).

In the background, without hunting for a pid:

```bash
graphersal-server start --dir ./srv --listen 127.0.0.1:8484   # detached; waits until ready
graphersal-server status --dir ./srv                          # running or stopped (exit 0 / 3)
graphersal-server stop --dir ./srv                            # graceful, waits until stopped
```

`start` takes the `serve` options (and `--timeout SECS`, default 60), spawns `serve` as a
detached process (its own session on Unix, no console window on Windows) with stdout and stderr
appended to `./srv/logs/server.log`, waits for `GET /readyz`, then prints the usual start block
and the address line (and, for a directory without `access.toml`, the first admin's token once:
never into the log). A second `start` is refused while a server runs; a server that fails to
start shows the last lines of its log. `stop` asks the server to stop through its admin API
(`POST /api/admin/shutdown` with the run's operator token: the same clean stop as Ctrl+C; on
Unix SIGTERM when the API cannot be reached) and waits until `server.lock` is free; without a
server it prints `not running` and exits 0 (removing a stale `server.run`). `status` shows the
pid, address, version, uptime, the databases (the loaded ones by name) and memory (the process's
resident memory, the loaded databases', the machine's total and available memory, the memory
limit with where it comes from and the `[limits]` settings, and what is free under it); a stale
`server.run` left by a crash is reported, and so is the last unclean shutdown ([After a
crash](#after-a-crash)). Under a service manager (systemd, launchd) run
`serve` in the foreground instead: [Running as a service](docs/server-dir.md#running-as-a-service).

### After a crash

A server killed by force (`kill -9`, an OOM kill, a power loss) needs no manual cleanup: the
OS releases `server.lock`, every acknowledged commit is in its database's journal (WAL), and
the next open of a Store replays it. The next `serve` or `start` notices that the last server
did not stop cleanly (it left `server.open`, the list of the databases it had open, or a stale
`server.run`) and says so in its start block and log (`UNCLEAN SHUTDOWN`, audited). Every
database that was open at that moment is loaded again (also without `auto_load`), its
journal replayed (what was replayed or cut off is kept) and its Store verified, in the
background; `status`, the Databases tab, `db info` and the Database settings show the result as
"Last unclean shutdown". A database whose Store turns out damaged is served **read-only** in
the state `maintenance` with its damage report (queries read the intact data, writes are
refused). Then, per database:

```bash
graphersal-server status --dir ./srv                 # the last unclean shutdown, per database
graphersal-server db verify --all --dir ./srv        # scrub every Store by hand (exit 1 on damage)
graphersal-server db repair acme --dir ./srv         # a repaired copy as the NEW database acme-repaired
graphersal-server db verify acme-repaired --dir ./srv
graphersal-server db swap acme acme-repaired --yes --dir ./srv   # the copy takes the name; the old one to trash/
# or keep both; or restore a backup instead:
graphersal-server db backups acme --dir ./srv
graphersal-server db restore acme BACKUP --dir ./srv  # (unload it first)
```

A repair never writes the damaged database: it rebuilds damaged parts from older snapshots and
the journal into a new database, reports what is lost or diverged, and the engine verifies the
copy. Layers and bases are not repaired (restore a backup). Details: [`docs/server-dir.md`,
"After a crash"](docs/server-dir.md#after-a-crash).

A first session from the console (in a second terminal while the server runs). The console
of the OS user owning `./srv` needs no token (it uses the run's operator token from
`./srv/server.run`); HTTP clients send a user token, here the first admin's, printed once by
`init` (`docs/access.md`):

```bash
graphersal-server db create acme --description "Acme" --dir ./srv
graphersal-server db load acme --dir ./srv
TOKEN=<the first admin's token>
curl -s -X POST http://127.0.0.1:8484/db/acme/api/execute -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"script": "g.add_v(\"person\").property(\"name\", \"ada\").iterate()"}'
curl -s -X POST http://127.0.0.1:8484/db/acme/api/execute -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' -d '{"script": "g.v().count().next()"}'
```

A user who should only read `acme` (its token printed once):

```bash
graphersal-server user add bob --dir ./srv
graphersal-server user grant bob acme read --dir ./srv
```

## Console

The console acts as the operator without credentials: the OS user that owns the server
directory is its operator. `init` makes the directory private (mode `0700`, the secret files
`0600`), and `serve` and every console command refuse a directory or secret owned by another
user or open to group or others, naming the `chmod`/`chown` that fixes it
([`docs/access.md`](docs/access.md#the-security-model)). Run a shared server under a dedicated
service user; other administrators run the console as that user, or send an admin token:
`--token TOKEN` (or `GRAPHERSAL_SERVER_TOKEN`) goes through the running server (address
`--server ADDRESS`, else from `DIR/server.run`) and touches no file of the directory. The
audit names the OS user of the operator's actions (`operator (os:<name>)`) and the token's
user otherwise.

Every command takes `--dir DIR`; `--json` prints the admin API's JSON answer instead of text.
With a server running on `DIR` the command goes to its admin API (address from
`DIR/server.run`, noted on stderr); otherwise it works on the directory itself (then `load` and
`unload` are refused: nothing would serve the database). Both paths call the same admin core.
The text output follows the graphersal CLI's layout: a result is a headline with its details in
one column and every command on its own line under a `# comment` (an import's `source:` is the
absolute path without `.`/`..` segments); listings (`db list`, `db
backups`, `models list`) are sections of aligned columns, with a Problems section for failed
databases and missing models. Paths and commands are never wrapped.

| Command | What |
|---|---|
| `start [serve options] [--timeout SECS]` | `serve` in the background (log `DIR/logs/server.log`); waits until ready, prints the start block and the address |
| `stop [--timeout SECS]` | stops the running server cleanly and waits; `not running` (exit 0) when none runs |
| `status [--json]` | running or stopped: pid, address, version, uptime, databases, memory (process, loaded databases, machine, limit, free), the log, the last unclean shutdown (and, stopped, one the next start will check); exit code 0 running, 3 stopped |
| `db list` | the databases and aliases with their states |
| `db create NAME [--base BASE] [--kind plain\|layer] [--role normal\|base] [--schema FILE.json] [--description TEXT] [--auto-load] [--read-only]` | a new empty database; `--base` makes a layer over a `role = base` database |
| `db delete NAME` | an unloaded database, moved to `DIR/trash/`; every grant on it is removed (a restore does not bring them back) |
| `db clone SOURCE NAME` | a fork of SOURCE's latest state (it may be loaded) |
| `db load NAME [--no-wait]`, `db unload NAME` | running server only; a loaded database's note shows its URL (`served at: http://.../db/NAME/`) |
| `db promote NAME` | an unloaded plain database becomes a base (read-only, frozen for good) |
| `db read-only NAME on\|off`, `db auto-load NAME on\|off` | flags, persisted; read-only applies at once |
| `db info NAME` | everything about a database (or the database an alias names): kind, role, base, aliases, created; state, flags, on disk, in memory; the query limits (its own overrides, the server's defaults, what the next load uses, what a loaded one runs under now); MCP, grants, backups; the Store's creation parameters (store id, damage policy, chunk size, created, backend, format version), read-only |
| `db set NAME description TEXT`, `db set NAME limit KEY VALUE`, `db set NAME limit KEY --clear` | the description, or a query limit override of `database.toml [limits.query]` (`timeout_ms`, `max_operations`, ...; `--clear`: the server's default applies again), checked like `server.toml`; a limit applies at the database's next load (unload + load) |
| `db alias set ALIAS DATABASE`, `db alias remove ALIAS`, `db alias rename OLD NEW` | aliases (a flip takes effect at once, also for the grants on the alias; a rename moves open MCP sessions and the alias's grants with it; a removal removes its grants) |
| `db backup NAME`, `db backups NAME` | verified ZIP backups in `DIR/backups/NAME/` |
| `db restore NAME BACKUP [--name NEW]` | over NAME (unloaded) or into the new database NEW |
| `db verify NAME`, `db verify --all` | the engine's scrub of NAME's Store (a loaded one: its open Store; damage found makes it read-only, state `maintenance`) or of every database's; exit 1 when damage is found |
| `db repair NAME [--name NEW]` | a repaired copy of the plain database NAME as the new database NEW (default `NAME-repaired`; never in place, NAME is not changed): what was rebuilt, lost or diverged; the copy is verified |
| `db swap NAME WITH --yes` | WITH (a repaired copy) takes the name NAME: NAME moves to `DIR/trash/`, WITH's name goes away; NAME's grants, aliases and settings stay; both unloaded first, NAME loaded again when it was |
| `db export NAME --out FILE [--no-models] [--materialize]` | a packed snapshot (`.gsnap`) |
| `import DIR SOURCE --name NAME [--base BASE] [--format F] [--description TEXT] [--role normal\|base]` | a new database from a `.gsnap`, a Store (directory or `.gstore`), GraphSON or GraphML; an embedding model it names without carrying comes from `models/` (else the database is `needs-model` until `models add`); a model blob a Store lacks is filled from `models/` (else the database is `needs-model` until `models add`, like a snapshot); `--role base` makes a base of a plain source |
| `import DIR --inbox FILE --name NAME [...]`, `import DIR --list` | the same from the import inbox `DIR/imports/` (FILE: one entry name there; a link leading outside is refused), and its listing |
| `models list`, `models add SOURCE [--repo R] [--license L]`, `models remove REPO` | the shared embedding models; SOURCE: a Model2Vec model directory, a `.tar` or `.tar.gz` of one, a `.gsnap` carrying models, or a repository `models/` holds |
| `user list` | the users of `access.toml`: roles, the last 4 characters of each token (`…a1b2`), what each reaches; then the grants of `anonymous` and roles; an alias is shown with its current database (`prod → prod-2026-11 (alias)`) |
| `user add NAME [--admin]` | a new user; its token is printed once on stdout (only a SHA-256 hash is kept) |
| `user token NAME [--revoke]` | a new token for NAME, printed once (the old one stops working), or with `--revoke` none; the user's browser and MCP sessions end at once |
| `user remove NAME` | the user and its grants (its sessions end) |
| `user admin NAME on\|off` | give or take the `admin` role |
| `user grant SUBJECT DATABASE read\|write`, `user revoke SUBJECT DATABASE` | grants; SUBJECT: a user name or `anonymous` (everyone, also without a token), DATABASE: a database, an alias (reached only through the alias, at its current database: `docs/access.md` "Grants on aliases") or `*`. The last admin (role and token) cannot be removed, demoted or revoked |
| `mcp status`, `mcp on\|off`, `mcp db NAME on\|off`, `mcp db NAME read-only on\|off` | the MCP switches of `DIR/mcp.toml` (a running server follows at once): MCP for the whole server (off: `/mcp` and `/db/NAME/mcp` answer 503, open sessions are kept), a database or alias offered to MCP or hidden, MCP read-only whatever the caller's grant; `status` lists them with the usage statistics of a running server ([`docs/mcp.md`](docs/mcp.md)) |

`graphersal-server --help` has the details. Exit codes: `0` ok, `1` failed (`Error: ...` and a
`Help: ...` line on stderr; where the admin API's help names a route, the console names the
console command instead: `graphersal-server db list --dir DIR lists ...`), `2` invalid invocation (`Error: ...` and the command that shows
the usage), `141` stdout closed before the result was written (`| head`).

## HTTP

| Path | What |
|---|---|
| `GET /healthz`, `GET /readyz` | liveness `200`; readiness `200` or `503` while starting or stopping |
| `GET /api/info` | the server's identity (`server: "graphersal-server"`, `protocol: "graphersal/1"`, `dev: false`), its UI profile `uiProfile` and the request's `principal`; for a logged-in principal also `uploadBytes`, the MCP endpoint and the version (anonymous gets only what the login page needs, plus `login: "/"`) |
| `GET /` | the server's web UI (the playground showing the `server` UI profile: the login page, "My databases" or the admin tabs, by who asks) |
| `POST /api/login`, `POST /api/logout` | the browser login with a user token (a `gs_session` cookie) and its logout |
| `GET /api/databases` | the databases and aliases the caller may reach, with its level and the playground path of each ("My databases") |
| `GET /server-ui/<file>` | the `server` UI profile's files (its modules and style sheet) |
| `/db/<name>/...` | the playground and its HTTP API (`POST /db/<name>/api/execute {"script"}`, ...) on the database or alias `<name>`, resolved per request, at the caller's level (`docs/access.md`: without a grant `401` for anonymous, `303` to the login page for its pages, `404` for a user); `409` with help when it is not loaded (never loaded implicitly), `404` when unknown |
| `/api/admin/...` | the admin API: [`docs/admin-api.md`](docs/admin-api.md) (also `GET /api/admin/server`, the server itself, and `POST /api/admin/shutdown`, which `stop` uses); also the run administration (`GET /api/admin/runs`, `GET /api/admin/databases/<name>/runs`, `POST /api/admin/databases/<name>/runs/cancel {"id"}`: every client's queued and running queries, stop one) |
| `/mcp`, `/db/<name>/mcp` | the MCP endpoints: [`docs/mcp.md`](docs/mcp.md) |

## Web UI

`/` serves Graphersal's playground showing graphersal-server's own UI profile, `server` (the
same mechanism graphersal-memory uses for its `memory` profile; the playground's book page
"UI profiles"). The profile and its modules live in `crates/graphersal-server/ui/` and are
built into the binary: `profile.json`, the tabs `databases-tab.js` (the list with load,
unload, clone, promote, delete, read-only and auto-load, create, aliases), `backups-tab.js`
(back up, list, restore), `import-tab.js` (upload with progress, a file picked from the server's import inbox
`imports/`, or a path on the server),
`export-tab.js` (`.gsnap` download), `models-tab.js` (the shared models; add one by
uploading a `.gsnap` or a `.tar` of a model directory with progress, or from a path on the
server) and `runs-tab.js` (the run administration: the queries that wait or run on the loaded
databases, whatever their client, refreshed every 2 s, with a Stop button per run; a loaded
database's row opens it filtered by "Runs…") and `users-tab.js` (users with the token
shown once and as `…last4` after, new token, revoke, admin role, remove; grants, anonymous
included, on databases and aliases, each alias marked with its current database) and
`mcp-tab.js` (MCP on or off for the whole server, each database or alias offered to MCP or
hidden and read-only for MCP, open sessions and tool calls per database, tool calls per tool and
per user with the recent errors since the start, the endpoint URL and the `claude mcp add`
command; model `mcp-api.js`) and `settings-tab.js` ("Database settings": one database, picked or
opened by a Databases row's "Settings…": its identity, state and sizes, query limits (its own
override, the server's default, the next load's value and, while loaded, the value in effect,
with "Unload and load now…" when they differ), MCP, grants and backups (with links to those
tabs) and the Store's creation parameters read-only; the description, read-only, auto-load and
each limit override are edited in place, every change based on the view's revision; model
`settings-api.js`; its "Damage and recovery" part shows a database in maintenance with its
damage report and its check after the last unclean shutdown), the views of non-admins `login-tab.js` (paste a token; then the page's `?next=`
path, only a path from the server root, or the root) and `mine-tab.js` ("My databases"), the
top bar's `header.js` (who is signed in, "Log out"), the pure models
`admin-api.js`, `databases.js`, `access-api.js`, `fresh.js` (fresh data per visit) and `sort.js` (sortable columns), the tabs' shared state `shared.js` and the style sheet
`server-ui.css` (on the playground's theme tokens; the profile names it in `styles`, so the
playground links it itself and `/` serves the playground's `index.html` unchanged). Every module imports only `vue` and the
profile's own files and calls the admin API itself, sending the host's request headers
(`window.GRAPHERSAL_ENGINE.headers`) when a page sets them. The root `GET /api/info` answers
each principal its own profile (`profile.json` for an admin; `profile-views.json` replaces the
tabs for a user, "My databases", and for anonymous, "Sign in"), so a page shows only what its
principal may use; after a login or logout the page loads the root again. A refused admin
request sends the page to the login page (`401`) or, when the principal lost the admin role
(`404`), to the root. The root has no graph: the profile
has no workspace tab, so the page only reads `GET /api/info` and the admin API (the modules get
the root info, e.g. `uploadBytes`, from the playground as `api.info.host`).

Every tab shows the current state on every visit, whatever changed it (another browser, a
script, a console command such as `graphersal-server user add`): the playground keeps its tabs
alive, so each tab reads its data again each time it is shown and when the window gets the
focus back or the page becomes visible while it is shown (`fresh.js`; at most one such read per
second). A refresh replaces only the shown data: an open dialog or form keeps what was typed.
The Runs tab polls only while it is shown. Every answer of the server carries
`Cache-Control: no-store`, so no browser or proxy shows a kept copy.

Every table (databases, aliases, users, grants, backups, the import inbox, models, databases
waiting for a model, runs, "My databases", MCP, the Database settings tab's limits, MCP names
and grants) sorts by a click on a column header (`sort.js`):
ascending, a second click descending; natural text order (`db2` before `db10`), sizes,
durations and ids by number, dates by time, missing values last; the sort is stable, and each
table keeps its choice in the browser's localStorage. Until the first click a table keeps the
server's order.

The Databases tab shows two size columns, "On disk" (the database's directory: Store
snapshots, journal, models) and "In memory" (a loaded database's graph; a layer's own changes,
its shared base in the tooltip), human-readable with the exact bytes in a tooltip, and above
the table a memory summary: the server process, the loaded databases (each graph once), the
machine's available and total memory, the memory limit (`server.toml [limits] memory_limit`,
by default 80 % of the machine or a container's limit minus `memory_headroom`) and what is free
under it (without a limit: on the machine). `db list` has the same two columns.

The Databases tab shows a database in the state `maintenance` (loaded read-only, its Store
damaged) with its damage report, "Verify" per row and "Verify all", and "Repair…" (a plain
database): a repaired copy as a new database, then "Let it take the name…" (confirmed: the
damaged one moves to `trash/`) or "Keep both", with the hint to restore a backup instead; above
the table the last unclean shutdown, each database it checked with its journal replay and
verify result.

The Databases tab explains bases and layers: the Kind column's tooltips, a "Bases and layers"
box under the table, and the confirmation of "Promote to base…" say what a base is (a frozen,
read-only database that layers are built over; each layer stores only its own changes), that
promotion is one way and forever, and when to use it. Each loaded
database keeps the ordinary playground under `/db/<name>/`; its `/api/info` carries `home: "/"`
(the link back to the server's UI), `database`, and the same `server: "graphersal-server"`,
`protocol: "graphersal/1"` and `dev: false` as the root.

Every `GET /api/info`, the root's and each database's, carries `"protocol": "graphersal/1"`,
the host-neutral marker of a Graphersal host: the playground takes an answer as its engine's
by this marker alone, not by `server`. `server: "graphersal-server"` only names the host (the
CLI's dev server answers `graphersal-dev`), and `dev: false` keeps the playground's DEV badge
off. The root answer adds `uiProfile`, the `server` profile the page at `/` shows.

Tests of the UI's modules (Node only, no browser):

```bash
node --test crates/graphersal-server/ui/tests/*.test.mjs
```

`profile.test.mjs` also checks the profile with the engine playground's own `validateProfile`,
and that its `isEngineInfo` accepts the root info (the fixed fields in `ui/tests/root-info.json`,
which a Rust test checks against the real answer) and refuses one without `protocol`, from `GRAPHERSAL_PLAYGROUND` (a playground
directory), else from the playground the binary embeds: the `ui/` directory of the
`graphersal-serve` crate in Cargo's registry (found with `cargo metadata`; run `cargo fetch` or
a build first, else these two tests are skipped).

## Server directory

```
<server-dir>/
  server.toml     listen address, MCP options, defaults for new databases, limits,
                  sessions, access control on/off, public scheme and host names, backup root
  access.toml     users (token hashes and last 4 characters), roles, grants
  server.lock     one running server per directory
  server.run      the running server's address and operator token (mode 0600, only while it runs)
  server.open     the databases the running server has open (removed at a clean stop)
  recovery.json   the last unclean shutdown and its check of each database
  aliases.toml    current = "acme-2026.3"
  mcp.toml        the MCP switches (on/off, per database offered and read-only), written by the server
  databases/<name>/database.toml + store/
  models/         one copy of every embedding model, hard-linked into the databases
  backups/<name>/ admin backups; backups/jobs/: the backup root of backup jobs
  imports/        the import inbox
  sessions/       browser and MCP sessions (hashes only), kept over restarts
  logs/server.log the log of a server `start` runs in the background (owner only)
  trash/  tmp/
```

The directory is mode `0700` and its secrets (`access.toml`, `server.run`, `sessions/`) are
owner only; `init DIR` on an older directory tightens them. There is no registry file: the
databases are the directories under `databases/`. Details of
every file and key: [`docs/server-dir.md`](docs/server-dir.md).

**Names.** A new database, alias or user name has up to 32 characters: lower-case letters,
digits, `.`, `_` and `-`; it starts and ends with a letter or digit, holds no `..` and is no
Windows device name (`con`, `nul`, `com1`, ...); `jobs` is reserved for databases and aliases.
Names are case-insensitive (`ACME` finds `acme`). Names made by 0.2.0 or earlier under the
older rule (up to 63 characters, a trailing `-` or `_`, `..` inside) keep working everywhere
(served, granted, aliased, restored under their own name); only new ones are refused. On
Windows keep the server directory itself short (e.g. `C:\gs\srv`): a Store adds its own
directories and file names below `databases/<name>/`, and a path over 260 characters
(`MAX_PATH`) fails unless long paths are enabled in Windows.

## Documentation

- [`docs/access.md`](docs/access.md): access control: principals, grants and levels, tokens,
  the first admin, browser login and sessions, MCP with a token, the modes, limits, audit.
- [`docs/server-dir.md`](docs/server-dir.md): the server directory, `server.toml`,
  `access.toml`, `database.toml`, models, backups, the import inbox, sessions, external
  backups.
- [`docs/admin-api.md`](docs/admin-api.md): every admin route, its body and answer.
- [`docs/mcp.md`](docs/mcp.md): the MCP endpoints, configuring an agent, the request headers.
- `scripts/smoke.sh`: an end-to-end smoke test (build, init, serve, the admin API, the
  database pages, MCP, users and a read-only user, the import inbox, a restart keeping the
  sessions, a clean stop); needs only bash, curl and python3 (Unix).
- `scripts/smoke.ps1`: the smoke test of a built binary in PowerShell 7 for Windows (CI and
  the release build): `pwsh scripts/smoke.ps1 -Bin <path to graphersal-server.exe>`.
- [`docs/release-notes-0.3.0.md`](docs/release-notes-0.3.0.md): what the next version changes
  (unreleased draft).
- [`docs/release-notes-0.2.0.md`](docs/release-notes-0.2.0.md): what 0.2.0 changes (access
  control and the rest); [`docs/release-notes-0.1.0.md`](docs/release-notes-0.1.0.md): what
  0.1.0 is and its known limits. CI (`.github/workflows/ci.yml`) and the release build (`release.yml`) run on Windows
  only for now.

## Known limitations

- A **base is frozen forever** (by design: layers depend on its exact state). A base with data
  comes from an import with role base (`import ... --role base`, `POST /api/admin/import?...
  &role=base`) or from promoting an unloaded plain database (`db promote NAME`, `POST
  /api/admin/databases/<name>/promote`). Promotion is one way: no command turns a base back
  into an ordinary database. To change a base's data, build a new base (clone it before
  promoting, or import again) and rebuild the layers over it. A base cannot get an embedding
  model later, so a source naming a model that neither it nor `models/` carries is refused as
  a base: add the model (`models add`) and import again, or import it as an ordinary database,
  add the model, then promote it.
- A **Store import lacking a model blob** that `models/` does not hold either becomes a
  `needs-model` database (since Graphersal 0.7.1, like a snapshot naming a model it does not
  carry): add the model to `models/` (`models add`, or the Models tab) and it is resolved. As a
  base (`--role base`) such a source is refused (`409`): a base cannot get a model later.
- The **run administration** (the Runs tab, `/api/admin/runs`) is under the admin surface and
  is for admins only: an admin sees every client's query texts and can stop them. The databases' own pages (`/db/<name>/`) do not offer it. A run id
  is unique within its database only.
- A model archive is a **ustar `.tar`**, uncompressed or gzip-compressed (`tar -cf model.tar
  -C <model dir> .`, `tar -czf model.tar.gz ...`); zstd, bzip2, xz and `.zip` are not taken.
- A **layer `.gsnap` export carries no models** (an engine rule); export the layer with
  `materialize=true` (`--materialize`) for a standalone snapshot with the models.
- **A query limit change applies at the database's next load** (unload + load, or a restart):
  the engine fixes a loaded database's session limits when it loads it. The settings
  (`db info`, the Database settings tab) show both the limits in effect now and those of the
  next load. The Store's creation parameters are shown read-only; the server creates every
  database with the engine's defaults, and a backend's container version is not reported by the
  engine (0.8.0).
- **Memory figures are the engine's estimates** of each graph's data (loaded embedding models
  are not in a database's figure, but in the process's resident memory). The process's
  resident memory is read on Linux, macOS and Windows only (`null` elsewhere).
- No admin tools over MCP. Browser and MCP sessions survive a restart (`sessions/`, hashes
  only; `docs/server-dir.md`). The MCP usage statistics live in memory and start again at every
  restart.
- **Windows: no permission check of the server directory** (unix only): its ACL is the
  boundary; create it where only the account running the server may write.
- **Plain HTTP** (HTTP/1.1 and h2c, no TLS, so no HTTP/2 negotiation by ALPN): tokens and
  session cookies travel in clear text; keep the server on the loopback or put a TLS reverse
  proxy in front (`[http] public_scheme = "https"`). On HTTP/2 an answer of a request whose body
  the route did not read resets that stream (HTTP/1.1 closes the connection with a lingering
  close instead).
- **Prebuilt binary for Windows x86_64 only**; other platforms build from source (see
  [Download / build](#download--build)). On Windows the clean stop is Ctrl+C or Ctrl+Break in
  the server's console (there is no SIGTERM); a killed server leaves `server.run` behind, and the
  next `serve` replaces it (the lock is released by the OS) and checks the databases it had
  open ([After a crash](#after-a-crash)).

## Licence

Apache-2.0, see `LICENSE-APACHE` and `NOTICE`.
