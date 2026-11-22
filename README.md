# graphersal-server

**EXPERIMENTAL. Private until its first public version.** Everything here (commands, files,
routes, answers) may change without notice.

A server for [Graphersal](https://github.com/graphersal/graphersal) graph databases: ONE process
serving MANY databases from one server directory, over HTTP. A database is either a plain Store
or a layer over a shared, frozen base database (many layers, one base in memory). Each loaded
database gets the Graphersal playground and its HTTP API under `/db/<name>/`; one MCP endpoint
(`/mcp`) lets an AI agent list the databases and pick one per session; the admin operations
(create, load, clone, back up, restore, export, import, aliases, shared embedding models) are
an HTTP API, a console (`graphersal-server db ...`) and an admin panel in the playground's
landing view at `/`.

> **No access control.** Anyone who reaches the port can read, change and delete every
> database. The server listens on the loopback (`127.0.0.1:8484`) by default and logs a warning
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

Open `http://127.0.0.1:8484/`: the landing view lists the databases with their states and holds
the admin panel. `serve` writes the address it bound into `./srv/server.run` while it runs;
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

| Command | What |
|---|---|
| `db list` | the databases and aliases with their states |
| `db create NAME [--base BASE] [--kind plain\|layer] [--role normal\|base] [--schema FILE.json] [--description TEXT] [--auto-load] [--read-only]` | a new empty database; `--base` makes a layer over a `role = base` database |
| `db delete NAME` | an unloaded database, moved to `DIR/trash/` |
| `db clone SOURCE NAME` | a fork of SOURCE's latest state (it may be loaded) |
| `db load NAME [--no-wait]`, `db unload NAME` | running server only |
| `db read-only NAME on\|off`, `db auto-load NAME on\|off` | flags, persisted; read-only applies at once |
| `db alias set ALIAS DATABASE`, `db alias remove ALIAS` | aliases (a flip takes effect at once) |
| `db backup NAME`, `db backups NAME` | verified ZIP backups in `DIR/backups/NAME/` |
| `db restore NAME BACKUP [--name NEW]` | over NAME (unloaded) or into the new database NEW |
| `db export NAME --out FILE [--no-models] [--materialize]` | a packed snapshot (`.gsnap`) |
| `import DIR SOURCE --name NAME [--base BASE] [--format F] [--description TEXT]` | a new database from a `.gsnap`, a Store (directory or `.gstore`), GraphSON or GraphML |
| `models list`, `models add SOURCE [--repo R] [--license L]`, `models remove REPO` | the shared embedding models |

`graphersal-server --help` has the details. Exit codes: `0` ok, `1` failed (the error and a
`help:` line on stderr), `2` invalid invocation.

## HTTP

| Path | What |
|---|---|
| `GET /healthz`, `GET /readyz` | liveness `200`; readiness `200` or `503` while starting or stopping |
| `GET /api/info` | the server's identity: `admin: true`, `uploadBytes`, the MCP endpoint |
| `GET /` | the playground's landing view and admin panel |
| `/db/<name>/...` | the playground and its HTTP API (`POST /db/<name>/api/execute {"script"}`, ...) on the database or alias `<name>`, resolved per request; `409` with help when it is not loaded (never loaded implicitly), `404` when unknown |
| `/api/admin/...` | the admin API: [`docs/admin-api.md`](docs/admin-api.md) |
| `/mcp`, `/db/<name>/mcp` | the MCP endpoints: [`docs/mcp.md`](docs/mcp.md) |

## Server directory

```
<server-dir>/
  server.toml     listen address, MCP options, defaults for new databases, limits
  server.lock     one running server per directory
  server.run      the running server's address (only while it runs)
  aliases.toml    current = "acme-2026.3"
  databases/<name>/database.toml + store/
  models/         one copy of every embedding model, hard-linked into the databases
  backups/  trash/  tmp/  logs/
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
- A **base cannot be edited** (by design: layers depend on its exact state). The API and the
  console create a base EMPTY (`db create NAME --role base`), and an import always makes a plain
  database or a layer. A base with data is made by hand: build a plain database (create and
  write, or import), stop the server, set `role = "base"` and `read_only = true` in its
  `databases/<name>/database.toml`, start the server again.
- A **layer `.gsnap` export carries no models** (an engine rule); export the layer with
  `materialize=true` (`--materialize`) for a standalone snapshot with the models.
- An **upload declaring more than the upload limit** gets its `413`, but the server abandons
  the connection without reading the body (tiny_http 0.12); clients should check the size
  against `uploadBytes` of `GET /api/info` before sending.
- An **import of a snapshot naming an embedding model it does not carry** stays `needs-model`
  even when `models/` holds the model, until `models add <repo>` (or `POST /api/admin/models
  {"repo"}`) resolves it.
- **No memory figures** in the database listing (only `sizeBytes` on disk): a loaded database's
  memory is known only inside its session thread.
- No admin tools over MCP; MCP sessions live in memory (a restart forgets them).
- `logs/` is created but not written: the server logs to stderr.

## Licence

Apache-2.0, see `LICENSE-APACHE` and `NOTICE`.
