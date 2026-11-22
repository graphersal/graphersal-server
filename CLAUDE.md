# CLAUDE.md

Agent guidance for **graphersal-server** (EXPERIMENTAL, private): one process serving many
Graphersal graph databases from one server directory over HTTP. Design and plan live in the
graphersal repository (`../graphersal/ai/roadmap/graphersal-server.md`,
`../graphersal/ai/graphersal-server/PLAN.md`; git-ignored session files there, never cite them
from published files). When this file disagrees with the code, the code wins: say so.

## Layout

- Workspace root `Cargo.toml` (all dependency versions in `[workspace.dependencies]`, each with
  a one-line justification); one crate `crates/graphersal-server` (lib + bin, `publish = false`).
- Path dependencies on the graphersal repository's MAIN checkout beside this one:
  `../graphersal/crates/graphersal`, `../graphersal/crates/graphersal-serve` and
  `../graphersal/crates/graphersal-session` (later also `graphersal-mcp`). Never edit that
  repository from a task here. This repository keeps its own `target/` (never share
  graphersal's).
- `src/lib.rs` modules: `config` (`server.toml`, `ServerConfig`, `parse_listen`), `layout`
  (`ServerDir`, `init`, file names, `write_atomic`), `lock` (`ServerLock`, fd-lock on
  `server.lock`), `run_file` (`RunInfo`, `server.run`), `serve` (`start` -> `Running`, the
  `tiny_http` listener adapted from graphersal-cli's dev server), `router` (probes and 503s
  through `graphersal_serve::Server`, `HostCheck`, own routes `/`, `/api/info`; the graph
  resolver `NoDatabases` answers 404 until the HTTP routes of the registry exist, V4), `error`
  (`ServerError`), `names` (name rule `[a-z0-9][a-z0-9._-]{0,62}`, Windows device names refused,
  `fold` = lower case for case-insensitive lookups), `database` (`database.toml`:
  `DatabaseConfig`, `DatabaseKind`, `DatabaseRole`, `deny_unknown_fields`, `utc_now`),
  `registry` (`Registry`: NO registry file, the registry IS `databases/*/database.toml`; `scan`
  at start with the lock held: cleans `tmp/`, broken entries -> `DbState::Failed`, broken aliases
  dropped into `problems()`, never fatal; entries behind an `RwLock`; `DbState`), `admin` (the
  admin core as `impl Registry`: `list`, `get`, `resolve`, aliases (target must be a database:
  no chains), `set_auto_load`, `set_read_only` (persists the flag only; a base with layers stays
  read-only), `prepare_tmp` -> `TmpBuild` -> `commit` (build in `tmp/`, atomic rename into
  `databases/`; a dropped build is removed)). Every admin change writes its file first
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
  for V4's resolver. Contexts: `launch` + `context_builder(listener HostCheck clone)`,
  `mount_path("/db/<name>")`, `read_only(config)` + `runtime_read_only(help)`, a job ticker
  (plain/layer). Plain = `open_store` + `ServedSource::Store`; layer = pooled base +
  `open_layer_store` + `ServedSource::layer_store`; base (`role = "base"`) = an EMPTY layer
  over the pooled base (`ServedSource::graph(base.new_layer())`) with the read-only gate on for
  good (C3), no ticker, no Store opened. Base pool: one `LayerBase` per base name
  (`layer_base_of_store`, no lock), users = its loaded layers + the base itself; it leaves the
  pool with its last user; unloading a base with loaded layers is refused. Lock order: loader
  state, then registry. Tests in `loader_tests.rs` (synthetic stores from the `modern` and
  `large` samples; `test_support::skip_on_ci`).
- `src/main.rs`: the hand-parsed command line (no parser crate, like graphersal's CLI), the
  tracing subscriber, the ctrlc handler. stdout = results, stderr = diagnostics/log; exit codes
  0 ok, 1 failed, 2 usage.
- Tests: unit tests beside the code (`test_support::TempDir`), end-to-end tests in
  `crates/graphersal-server/tests/server_tests.rs` (spawns the binary, port 0, SIGTERM).

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
