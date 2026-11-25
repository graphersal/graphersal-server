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
> (against DNS rebinding and cross-site pages).

## Download / build

**Windows x86_64:** each release on GitHub carries a prebuilt binary,
`graphersal-server-v<version>-windows-x86_64.zip` (`graphersal-server.exe` with
`LICENSE-APACHE`, `NOTICE`, this README and `THIRD-PARTY-LICENSES.html`), and `SHA256SUMS` to
verify it (`certutil -hashfile <zip> SHA256`, or `Get-FileHash` in PowerShell). Unpack it and run
`graphersal-server.exe` from a console; Ctrl+C (or Ctrl+Break) stops it cleanly.

**Every other platform** (Linux, macOS, ...) builds from source, with Rust 1.88 or newer. The
Graphersal crates come from crates.io (exact pins in the root `Cargo.toml`, versions in the
committed `Cargo.lock`; `--locked` builds exactly those); the published `graphersal-serve` crate
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

A first session from the console (in a second terminal while the server runs):

```bash
graphersal-server db create acme --description "Acme" --dir ./srv
graphersal-server db load acme --dir ./srv
curl -s -X POST http://127.0.0.1:8484/db/acme/api/execute \
  -H 'Content-Type: application/json' \
  -d '{"script": "g.add_v(\"person\").property(\"name\", \"ada\").iterate()"}'
curl -s -X POST http://127.0.0.1:8484/db/acme/api/execute \
  -H 'Content-Type: application/json' -d '{"script": "g.v().count().next()"}'
```

## Console

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
| `db list` | the databases and aliases with their states |
| `db create NAME [--base BASE] [--kind plain\|layer] [--role normal\|base] [--schema FILE.json] [--description TEXT] [--auto-load] [--read-only]` | a new empty database; `--base` makes a layer over a `role = base` database |
| `db delete NAME` | an unloaded database, moved to `DIR/trash/` |
| `db clone SOURCE NAME` | a fork of SOURCE's latest state (it may be loaded) |
| `db load NAME [--no-wait]`, `db unload NAME` | running server only; a loaded database's note shows its URL (`served at: http://.../db/NAME/`) |
| `db promote NAME` | an unloaded plain database becomes a base (read-only, frozen for good) |
| `db read-only NAME on\|off`, `db auto-load NAME on\|off` | flags, persisted; read-only applies at once |
| `db alias set ALIAS DATABASE`, `db alias remove ALIAS`, `db alias rename OLD NEW` | aliases (a flip takes effect at once; a rename moves open MCP sessions with it) |
| `db backup NAME`, `db backups NAME` | verified ZIP backups in `DIR/backups/NAME/` |
| `db restore NAME BACKUP [--name NEW]` | over NAME (unloaded) or into the new database NEW |
| `db export NAME --out FILE [--no-models] [--materialize]` | a packed snapshot (`.gsnap`) |
| `import DIR SOURCE --name NAME [--base BASE] [--format F] [--description TEXT] [--role normal\|base]` | a new database from a `.gsnap`, a Store (directory or `.gstore`), GraphSON or GraphML; an embedding model it names without carrying comes from `models/` (else the database is `needs-model` until `models add`); a model blob a Store lacks is filled from `models/` (else the database is `needs-model` until `models add`, like a snapshot); `--role base` makes a base of a plain source |
| `models list`, `models add SOURCE [--repo R] [--license L]`, `models remove REPO` | the shared embedding models; SOURCE: a Model2Vec model directory, a `.tar` or `.tar.gz` of one, a `.gsnap` carrying models, or a repository `models/` holds |
| `user list` | the users of `access.toml`: roles, the last 4 characters of each token (`…a1b2`), what each reaches; then the grants of `anonymous` and roles |
| `user add NAME [--admin]` | a new user; its token is printed once on stdout (only a SHA-256 hash is kept) |
| `user token NAME [--revoke]` | a new token for NAME, printed once (the old one stops working), or with `--revoke` none; the user's browser and MCP sessions end at once |
| `user remove NAME` | the user and its grants (its sessions end) |
| `user admin NAME on\|off` | give or take the `admin` role |
| `user grant SUBJECT DATABASE read\|write`, `user revoke SUBJECT DATABASE` | grants; SUBJECT: a user name or `anonymous` (everyone, also without a token), DATABASE: a name or `*`. The last admin (role and token) cannot be removed, demoted or revoked |

`graphersal-server --help` has the details. Exit codes: `0` ok, `1` failed (`Error: ...` and a
`Help: ...` line on stderr; where the admin API's help names a route, the console names the
console command instead: `graphersal-server db list --dir DIR lists ...`), `2` invalid invocation (`Error: ...` and the command that shows
the usage), `141` stdout closed before the result was written (`| head`).

## HTTP

| Path | What |
|---|---|
| `GET /healthz`, `GET /readyz` | liveness `200`; readiness `200` or `503` while starting or stopping |
| `GET /api/info` | the server's identity (`server: "graphersal-server"`, `protocol: "graphersal/1"`, `dev: false`), its UI profile `uiProfile`, `uploadBytes`, the MCP endpoint |
| `GET /` | the server's web UI (the playground showing the `server` UI profile: the login page, "My databases" or the admin tabs, by who asks) |
| `POST /api/login`, `POST /api/logout` | the browser login with a user token (a `gs_session` cookie) and its logout |
| `GET /api/databases` | the databases and aliases the caller may reach, with its level and the playground path of each ("My databases") |
| `GET /server-ui/<file>` | the `server` UI profile's files (its modules and style sheet) |
| `/db/<name>/...` | the playground and its HTTP API (`POST /db/<name>/api/execute {"script"}`, ...) on the database or alias `<name>`, resolved per request; `409` with help when it is not loaded (never loaded implicitly), `404` when unknown |
| `/api/admin/...` | the admin API: [`docs/admin-api.md`](docs/admin-api.md); also the run administration (`GET /api/admin/runs`, `GET /api/admin/databases/<name>/runs`, `POST /api/admin/databases/<name>/runs/cancel {"id"}`: every client's queued and running queries, stop one) |
| `/mcp`, `/db/<name>/mcp` | the MCP endpoints: [`docs/mcp.md`](docs/mcp.md) |

## Web UI

`/` serves Graphersal's playground showing graphersal-server's own UI profile, `server` (the
same mechanism graphersal-memory uses for its `memory` profile; the playground's book page
"UI profiles"). The profile and its modules live in `crates/graphersal-server/ui/` and are
built into the binary: `profile.json`, the tabs `databases-tab.js` (the list with load,
unload, clone, promote, delete, read-only and auto-load, create, aliases), `backups-tab.js`
(back up, list, restore), `import-tab.js` (upload with progress, or a path on the server),
`export-tab.js` (`.gsnap` download), `models-tab.js` (the shared models; add one by
uploading a `.gsnap` or a `.tar` of a model directory with progress, or from a path on the
server) and `runs-tab.js` (the run administration: the queries that wait or run on the loaded
databases, whatever their client, refreshed every 2 s, with a Stop button per run; a loaded
database's row opens it filtered by "Runs…") and `users-tab.js` (users with the token
shown once and as `…last4` after, new token, revoke, admin role, remove; grants, anonymous
included), the views of non-admins `login-tab.js` (paste a token; then the page's `?next=`
path, only a path from the server root, or the root) and `mine-tab.js` ("My databases"), the
top bar's `header.js` (who is signed in, "Log out"), the pure models
`admin-api.js`, `databases.js` and `access-api.js`, the tabs' shared state `shared.js` and the style sheet
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
the root info, e.g. `uploadBytes`, from the playground as `api.info.host`). Each loaded
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
  server.toml     listen address, MCP options, defaults for new databases, limits
  server.lock     one running server per directory
  server.run      the running server's address (only while it runs)
  aliases.toml    current = "acme-2026.3"
  databases/<name>/database.toml + store/
  models/         one copy of every embedding model, hard-linked into the databases
  backups/  trash/  tmp/
```

There is no registry file: the databases are the directories under `databases/`. Details of
every file and key: [`docs/server-dir.md`](docs/server-dir.md).

## Documentation

- [`docs/server-dir.md`](docs/server-dir.md): the server directory, `server.toml`,
  `database.toml`, models, backups, external backups.
- [`docs/admin-api.md`](docs/admin-api.md): every admin route, its body and answer.
- [`docs/mcp.md`](docs/mcp.md): the MCP endpoints, configuring an agent, the request headers.
- `scripts/smoke.sh`: an end-to-end smoke test (build, init, serve, the admin API, the
  database pages, MCP, a clean stop); needs only bash, curl and python3 (Unix).
- `scripts/smoke.ps1`: the smoke test of a built binary in PowerShell 7 for Windows (CI and
  the release build): `pwsh scripts/smoke.ps1 -Bin <path to graphersal-server.exe>`.
- [`docs/release-notes-0.1.0.md`](docs/release-notes-0.1.0.md): what 0.1.0 is and its known
  limits. CI (`.github/workflows/ci.yml`) and the release build (`release.yml`) run on Windows
  only for now.

## Known limitations

- **Plain HTTP** (see above): tokens and session cookies travel in clear text; keep the server
  on the loopback or behind a TLS proxy.
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
- **No memory figures** in the database listing (only `sizeBytes` on disk): a loaded database's
  memory is known only inside its session thread.
- No admin tools over MCP; MCP sessions live in memory (a restart forgets them).
- **Plain HTTP** (HTTP/1.1 and h2c, no TLS, so no HTTP/2 negotiation by ALPN): put a reverse
  proxy in front for anything beyond the loopback. On HTTP/2 an answer of a request whose body
  the route did not read resets that stream (HTTP/1.1 closes the connection with a lingering
  close instead).
- **Prebuilt binary for Windows x86_64 only**; other platforms build from source (see
  [Download / build](#download--build)). On Windows the clean stop is Ctrl+C or Ctrl+Break in
  the server's console (there is no SIGTERM); a killed server leaves `server.run` behind, and the
  next `serve` replaces it (the lock is released by the OS).

## Licence

Apache-2.0, see `LICENSE-APACHE` and `NOTICE`.
