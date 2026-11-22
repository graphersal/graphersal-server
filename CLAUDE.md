# CLAUDE.md

Agent guidance for **graphersal-server** (EXPERIMENTAL, private): one process serving many
Graphersal graph databases from one server directory over HTTP. Design and plan live in the
graphersal repository (`../graphersal/ai/roadmap/graphersal-server.md`,
`../graphersal/ai/graphersal-server/PLAN.md`; git-ignored session files there, never cite them
from published files). When this file disagrees with the code, the code wins: say so.

## Layout

- Workspace root `Cargo.toml` (all dependency versions in `[workspace.dependencies]`, each with
  a one-line justification); one crate `crates/graphersal-server` (lib + bin, `publish = false`).
- graphersal-serve with feature `ui-assets` (the embedded playground per mount and at `/`).
- Path dependencies on the graphersal repository's MAIN checkout beside this one:
  `../graphersal/crates/graphersal`, `../graphersal/crates/graphersal-serve` and
  `../graphersal/crates/graphersal-session` (later also `graphersal-mcp`). Never edit that
  repository from a task here. This repository keeps its own `target/` (never share
  graphersal's).
- `src/lib.rs` modules: `config` (`server.toml`, `ServerConfig`, `parse_listen`), `layout`
  (`ServerDir`, `init`, file names, `write_atomic`), `lock` (`ServerLock`, fd-lock on
  `server.lock`), `run_file` (`RunInfo`, `server.run`), `serve` (`start` -> `Running`, the
  `tiny_http` listener adapted from graphersal-cli's dev server), `router` (probes and 503s
  through `graphersal_serve::Server`, then `HostCheck::check` on everything else, then the
  routes: `/db/<name>/...` via `mounts`, `GET /api/info` (root identity, `admin: true`, no
  `mountPath` = the playground's landing view), `/api/admin/...` via `admin_api`, other `/api/*`
  and `/mcp` 404, every other GET a file of the embedded playground at the root (`/` =
  `index.html`); `ready(loader)` installs the resolver), `mounts` (`/db/<name>/<rest>`:
  `registry.resolve` on EVERY request (alias flips at once), unknown -> 404 JSON, `/db/x` ->
  308 `/db/x/` (query kept), no context -> 409 `{"error": "database 'x' is not loaded", "help":
  "load it: POST /api/admin/databases/x/load", "state"}` (the text names loading / failed
  (reason) / needs-model; loading adds `Retry-After: 1`), else `forward`: prefix stripped,
  client `X-Graphersal-Graph` dropped and set to the REAL name, `Server::handle`; the resolver
  `Databases` takes `loader.context(header)` and the request holds that `Arc` only for its
  duration; `DatabaseInfo` hooks add `database` (real name) and `admin: true` to the per-DB
  `/api/info`), `admin_api` (`GET /api/admin/databases` `{databases, aliases, problems}`, alias
  rows carry `aliasOf`; `GET /api/admin/databases/<name|alias>`; field names = the playground's
  landing view; the actions: `POST databases` create, `DELETE databases/<name>`, `POST
  .../<name>/clone|load|unload`, `PUT .../<name>/read-only|auto-load`, `GET aliases`, `PUT|DELETE
  aliases/<alias>`; JSON bodies `deny_unknown_fields`; core errors -> `{"error","help"?}` via
  `status_of` (400 invalid name/input, 404, 409 rule refusals, 500); the route table is in its
  module docs and `docs/admin-api.md` (the playground panel's contract: keep them in sync)),
  `ops` (`Admin`: THE admin core of the API and the console; online = with the `Loader`,
  offline = `Admin::open_offline` takes the server lock and scans; `create` (empty, optional
  schema, plain/layer, built in `tmp/` via `prepare_tmp`/`commit`, Store options from
  `server.toml` `[defaults]` read per create; a layer's base from the loader's pool or
  `layer_base_of_store`), `delete` (unloaded only, under `Loader::while_idle`; refused while an
  alias names it or layers name it as base; to `trash/<name>-<UTC time with - for :>`),
  `clone_database` (loaded: the open Store's `fork` with the context held so an unload waits;
  unloaded: `Store::fork_dir` / `fork_layer_dir`; config copied, `auto_load` off), load/unload
  (offline: refused naming `serve`), read-only (offline: base refuses `false`), auto-load,
  aliases; `#[cfg(test)] FailPoint` injects a failure/crash after the Store is written; tests in
  `ops_tests.rs`, also over a real listener), `console` (`DbCommand` -> one admin API request;
  `run`: offline = the SAME `admin_api::route` over an offline `Admin` with a built `Request`,
  locked = `client` to `RunInfo.address` (503 retried 10 s; `load` polls until not loading);
  refusals = `ServerError::Remote {status, message, help}`; `render` = human text), `client`
  (minimal std `TcpStream` HTTP/1.1 client: `Host` = address, no `Origin`, `Connection: close`,
  Content-Length/chunked/close bodies, timeouts), `error`
  (`ServerError`), `names` (name rule `[a-z0-9][a-z0-9._-]{0,62}`, Windows device names refused,
  `fold` = lower case for case-insensitive lookups), `database` (`database.toml`:
  `DatabaseConfig`, `DatabaseKind`, `DatabaseRole`, `deny_unknown_fields`, `utc_now`),
  `registry` (`Registry`: NO registry file, the registry IS `databases/*/database.toml`; `scan`
  at start with the lock held: cleans `tmp/`, broken entries -> `DbState::Failed`, broken aliases
  dropped into `problems()`, never fatal; entries behind an `RwLock`; `DbState`), `admin` (the
  admin core as `impl Registry`: `list`, `get`, `resolve`, aliases (target must be a database:
  no chains), `set_auto_load`, `set_read_only` (persists the flag only; a base with layers stays
  read-only), `prepare_tmp` -> `TmpBuild` -> `commit` (build in `tmp/`, atomic rename into
  `databases/`; a dropped build is removed), `delete` (the registry half of a delete: move into
  `trash/`, refusals)). Every admin change writes its file first
  (atomically), then memory, under the registry's write lock. `serve::start` scans after the
  bind and before `ready()` (`/readyz` 503 meanwhile); `Running::registry()`.
- `loader` (`Loader`, beside the registry, `Running::loader()`): `load` (background thread,
  state `loading` -> `loaded` / `failed (reason)` / `needs-model (repo)` when a model blob is
  missing; a load of a loaded/loading DB answers `LoadStart::Already*`, no error), `wait`,
  `unload` (map entry removed first, ticker stopped, bounded wait `UNLOAD_WAIT` for
  `background_runs() == 0` and for the requests holding the `Arc<Context>` (strong count),
  context dropped, `Store::shutdown`), `set_read_only` (persist via the admin core, then
  `Context::set_read_only`; a base refuses `false`), `auto_load` (after `router.ready()`),
  `shutdown` (at server stop: joins the loads, unloads layers before bases), `context(name)`
  for the resolver (`mounts`). Contexts: `launch` + `context_builder(listener HostCheck clone)`,
  `mount_path("/db/<name>")`, `read_only(config)` + `runtime_read_only(help)`, a job ticker
  (plain/layer). Plain = `open_store` + `ServedSource::Store`; layer = pooled base +
  `open_layer_store` + `ServedSource::layer_store`; base (`role = "base"`) = an EMPTY layer
  over the pooled base (`ServedSource::graph(base.new_layer())`) with the read-only gate on for
  good (C3), no ticker, no Store opened. Base pool: one `LayerBase` per base name
  (`layer_base_of_store`, no lock), users = its loaded layers + the base itself; it leaves the
  pool with its last user; unloading a base with loaded layers is refused. Lock order: loader
  state, then registry. Tests in `loader_tests.rs` (synthetic stores from the `modern` and
  `large` samples; `test_support::skip_on_ci`).
- `src/main.rs`: the hand-parsed command line (no parser crate, like graphersal's CLI): `init`,
  `serve`, `db list|create|delete|clone|load|unload|read-only|auto-load|alias ... --dir DIR
  [--json]` (prints `note: ... sent to its admin API at ADDR` on stderr when it went through a
  server, `help:` lines for refusals), the tracing subscriber, the ctrlc handler. stdout =
  results, stderr = diagnostics/log; exit codes 0 ok, 1 failed, 2 usage.
- Tests: unit tests beside the code (`test_support::TempDir`), end-to-end tests in
  `crates/graphersal-server/tests/server_tests.rs` (spawns the binary, port 0, SIGTERM; the
  `db` console offline and through a running server).

## Commands

Run every build/test/server under `../graphersal/ai/bench/watchdog.sh` (RSS ceilings).

```bash
../graphersal/ai/bench/watchdog.sh cargo test
cargo fmt --all --check                      # rustfmt.toml: max_width = 100
../graphersal/ai/bench/watchdog.sh cargo clippy --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p graphersal-server
```

## Rules (graphersal's rules in spirit)

- No panics on request paths (no `unwrap`/`expect` outside tests); `Result` with `ServerError`
  (`#[non_exhaustive]`, named-field variants too) for anything touching files, locks, sockets.
- Library code logs through `tracing`, never `println!`/`eprintln!` (the binary may print).
- Build every path with `Path::join` one component at a time (Windows).
- Public enums and result/config structs `#[non_exhaustive]` (give them constructors/builders);
  every public item documented (`#![warn(missing_docs)]`).
- A new dependency only with a justification line in `[workspace.dependencies]`.
- English in every file. Synthetic test data only; servers on port 0; kill what you start.
- Do not change or delete an existing test without listing it (file, name, reason) in the
  task report.
- No access control in this version: default listen `127.0.0.1:8484`, a warning for any
  non-loopback address, `graphersal_serve::HostCheck` on every request but the probes.
