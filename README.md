# graphersal-server

**EXPERIMENTAL. Private until its first public version.** Everything here (commands, files,
routes, answers) may change without notice.

A server for [Graphersal](https://github.com/graphersal/graphersal) graph databases: ONE process
serving MANY databases from one server directory, over HTTP. A database is either a plain Store
or a layer over a shared, frozen base database (many layers, one base in memory). Each loaded
database gets the Graphersal playground and its HTTP API under `/db/<name>/`; one MCP endpoint
(`/mcp`) lets an AI agent list the databases and pick one per session; the admin operations
(create, load, clone, back up, restore, export, import, aliases, shared embedding models) are
an HTTP API, a console (`graphersal-server db ...`) and the server's own web UI at `/`.

> **No access control.** Anyone who reaches the port can read, change and delete every
> database. The server listens on the loopback (`127.0.0.1:8484`) by default and prints a warning
> when told to listen on any other address. Keep it on the loopback. Every request but the
> health probes is checked by `Host` and `Origin` (against DNS rebinding and cross-site pages);
> that is not authentication. The optional MCP token (`docs/mcp.md`) guards only the MCP
> endpoints.

## Build

The crate depends on Graphersal's crates by path: a checkout of the `graphersal` repository
must sit beside this one (`../graphersal`).

```bash
cargo build --release          # target/release/graphersal-server
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
| `db alias set ALIAS DATABASE`, `db alias remove ALIAS` | aliases (a flip takes effect at once) |
| `db backup NAME`, `db backups NAME` | verified ZIP backups in `DIR/backups/NAME/` |
| `db restore NAME BACKUP [--name NEW]` | over NAME (unloaded) or into the new database NEW |
| `db export NAME --out FILE [--no-models] [--materialize]` | a packed snapshot (`.gsnap`) |
| `import DIR SOURCE --name NAME [--base BASE] [--format F] [--description TEXT] [--role normal\|base]` | a new database from a `.gsnap`, a Store (directory or `.gstore`), GraphSON or GraphML; an embedding model it names without carrying comes from `models/` (else the database is `needs-model` until `models add`); a model blob a Store lacks is filled from `models/` (else the import is refused: add the model, import again); `--role base` makes a base of a plain source |
| `models list`, `models add SOURCE [--repo R] [--license L]`, `models remove REPO` | the shared embedding models; SOURCE: a Model2Vec model directory, an uncompressed `.tar` of one, a `.gsnap` carrying models, or a repository `models/` holds |

`graphersal-server --help` has the details. Exit codes: `0` ok, `1` failed (`Error: ...` and a
`Help: ...` line on stderr; where the admin API's help names a route, the console names the
console command instead: `graphersal-server db list --dir DIR lists ...`), `2` invalid invocation (`Error: ...` and the command that shows
the usage), `141` stdout closed before the result was written (`| head`).

## HTTP

| Path | What |
|---|---|
| `GET /healthz`, `GET /readyz` | liveness `200`; readiness `200` or `503` while starting or stopping |
| `GET /api/info` | the server's identity (`server: "graphersal-server"`, `protocol: "graphersal/1"`, `dev: false`), its UI profile `uiProfile`, `uploadBytes`, the MCP endpoint |
| `GET /` | the server's web UI (the playground showing the `server` UI profile) |
| `GET /server-ui/<file>` | the `server` UI profile's files (its modules and style sheet) |
| `/db/<name>/...` | the playground and its HTTP API (`POST /db/<name>/api/execute {"script"}`, ...) on the database or alias `<name>`, resolved per request; `409` with help when it is not loaded (never loaded implicitly), `404` when unknown |
| `/api/admin/...` | the admin API: [`docs/admin-api.md`](docs/admin-api.md) |
| `/mcp`, `/db/<name>/mcp` | the MCP endpoints: [`docs/mcp.md`](docs/mcp.md) |

## Web UI

`/` serves Graphersal's playground showing graphersal-server's own UI profile, `server` (the
same mechanism graphersal-memory uses for its `memory` profile; the playground's book page
"UI profiles"). The profile and its modules live in `crates/graphersal-server/ui/` and are
built into the binary: `profile.json`, the tabs `databases-tab.js` (the list with load,
unload, clone, promote, delete, read-only and auto-load, create, aliases), `backups-tab.js`
(back up, list, restore), `import-tab.js` (upload with progress, or a path on the server),
`export-tab.js` (`.gsnap` download) and `models-tab.js` (the shared models; add one by
uploading a `.gsnap` or a `.tar` of a model directory with progress, or from a path on the
server), the pure model
`admin-api.js` and `databases.js`, the tabs' shared state `shared.js` and the style sheet
`server-ui.css` (on the playground's theme tokens; the profile names it in `styles`, so the
playground links it itself and `/` serves the playground's `index.html` unchanged). Every module imports only `vue` and the
profile's own files and calls the admin API itself, sending the host's request headers
(`window.GRAPHERSAL_ENGINE.headers`) when a page sets them. The root has no graph: the profile
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
directory), else `../graphersal/playground`.

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
  database pages, MCP, a clean stop); needs only bash, curl and python3.

## Known limitations

- **No access control** (see above): loopback only. The MCP token is the only secret, and it
  guards only the MCP endpoints.
- A **base is frozen forever** (by design: layers depend on its exact state). A base with data
  comes from an import with role base (`import ... --role base`, `POST /api/admin/import?...
  &role=base`) or from promoting an unloaded plain database (`db promote NAME`, `POST
  /api/admin/databases/<name>/promote`). Promotion is one way: no command turns a base back
  into an ordinary database. To change a base's data, build a new base (clone it before
  promoting, or import again) and rebuild the layers over it. A base cannot get an embedding
  model later, so a source naming a model that neither it nor `models/` carries is refused as
  a base: add the model (`models add`) and import again, or import it as an ordinary database,
  add the model, then promote it.
- A **Store import lacking a model blob** that `models/` does not hold either is refused
  (`409`), not imported as `needs-model` like a snapshot: a Store copy is verified and cannot
  leave a referenced blob out. Add the model to `models/`, then import again (the blob is
  filled in and hard-linked).
- A model archive is an **uncompressed ustar `.tar`** (`tar -cf model.tar -C <model dir> .`);
  compressed archives and `.zip` are not taken.
- A **layer `.gsnap` export carries no models** (an engine rule); export the layer with
  `materialize=true` (`--materialize`) for a standalone snapshot with the models.
- **No memory figures** in the database listing (only `sizeBytes` on disk): a loaded database's
  memory is known only inside its session thread.
- No admin tools over MCP; MCP sessions live in memory (a restart forgets them).

## Licence

Apache-2.0, see `LICENSE-APACHE` and `NOTICE`.
