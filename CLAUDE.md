# CLAUDE.md

Agent guidance for **graphersal-server** (EXPERIMENTAL, private): one process serving many
Graphersal graph databases from one server directory over HTTP. Design and plan live in the
graphersal repository (`../graphersal/ai/roadmap/graphersal-server.md`,
`../graphersal/ai/graphersal-server/PLAN.md`; git-ignored session files there, never cite them
from published files). When this file disagrees with the code, the code wins: say so.

## Layout

- Workspace root `Cargo.toml` (all dependency versions in `[workspace.dependencies]`, each with
  a one-line justification); one crate `crates/graphersal-server` (lib + bin, `publish = false`).
- graphersal with features `persist`, `persist-zip` (verified ZIP backups) and `io` (GraphSON
  and GraphML imports); graphersal-serve
  with features `ui-assets` (the embedded playground per mount and at `/`, where it shows the
  server's own `server` UI profile) and `mcp` (each
  context a host of the shared MCP endpoint); graphersal-mcp (the endpoint, directory mode).
- The four Graphersal crates (graphersal, graphersal-serve, graphersal-session, graphersal-mcp)
  from crates.io, pinned exactly (`=0.7.0`: EXPERIMENTAL crates, exact pin during 0.x), all on
  the same version; `Cargo.lock` is committed and CI builds `--locked`. The embedded playground
  is the `ui/` of the published graphersal-serve crate. Moving to a new Graphersal version:
  bump the four pins together, `cargo update -p graphersal ...`, commit the lock. Never edit the
  graphersal repository from a task here. This repository keeps its own `target/` (never share
  graphersal's).
- `src/lib.rs` modules: `config` (`server.toml`, `ServerConfig`, `parse_listen`, `[limits]
  upload_bytes` (default 4 GiB), `max_connections`, `header_timeout_secs`, `idle_timeout_secs`
  = `Limits`), `layout`
  (`ServerDir`, `init`, file names, `write_atomic`), `lock` (`ServerLock`, fd-lock on
  `server.lock`), `run_file` (`RunInfo`, `server.run`), `serve` (`start` -> `Running`, the
  listener: hyper 1.x `http1` server connections, and `http2` ones for h2c with prior
  knowledge (`sniff` peeks the preface; the HTTP/1 path is unchanged otherwise), on a tokio
  multi-thread runtime owned by `Running`; connection cap `[limits] max_connections`
  (`ConnectionLimits`, a semaphore; over it `refuse_connection` answers 503 + `Retry-After: 1`
  in the connection's protocol, at most `OVERFLOW_CONNECTIONS` at once, more are closed),
  `header_timeout_secs` (hyper's HTTP/1 header read timeout + the sniff), `idle_timeout_secs`
  (`Idle`: per-connection `Activity`, graceful shutdown when no request runs), `serve`
  flags `--max-connections`/`--header-timeout`/`--idle-timeout` (coordinator decision C7: no axum, the router is ours; graphersal-serve stays
  HTTP-library-free); every request answered on the blocking pool (`spawn_blocking`: router,
  session jobs, file I/O never on an async worker); the body is `BodyReader` (an `io::Read`
  pulling hyper's body one frame per read via `Handle::block_on`, idle timeout 120 s), a download
  is `pump`ed from its file through a bounded channel (`Outgoing::Stream`, `Content-Length`); a
  declared body over `MAX_DECLARED_BODY_BYTES` (64 MiB), or over the upload limit for an import
  upload (`is_upload`: `POST /api/admin/import` or `POST /api/admin/models` not JSON; `Router::upload_limit`, read per
  request), is answered 413 JSON at once without reading (`refuse_oversize`, Host check first;
  `Expect: 100-continue` never gets its 100) with `Connection: close`; a body the route left
  unread closes the connection after the answer (a declared rest up to 256 KiB is drained);
  every connection ends with `linger` (shutdown write, discard input for up to 2 s, close: no
  reset eats the answer); stop: `watch` signal -> accept task ends, `graceful_shutdown` on each
  connection, wait for `in_flight` up to 5 s, unload, `shutdown_timeout`), `router` (probes and 503s
  through `graphersal_serve::Server`, then `HostCheck::check` on everything else, then the
  routes: `/db/<name>/...` via `mounts`, `GET /api/info` (root identity: `server:
  router::SERVER_NAME` ("graphersal-server"; each mount's context sets the same via
  `ContextBuilder::server_name`, never `.dev(true)`), `protocol: graphersal_serve::PROTOCOL`
  ("graphersal/1", the only thing the playground's `isEngineInfo` checks; every mount answers it
  too), `dev: false`, no `product`, no `admin`; `ui/tests/root-info.json` = its fixed fields for
  the node test, checked by `router::tests::the_node_tests_root_info_is_the_answer`, `uiProfile` = `ui_assets::profile()`, `uploadBytes` = the upload limit, `mcp` =
  `Mcp::status()` or null), `/mcp` (every method) via `mcp::route` (404 when MCP is off),
  `/api/admin/...` via `admin_api`, other `/api/*` 404, `GET /server-ui/<file>` the profile's
  files (`ui_assets::file`), every other GET a file of the embedded playground at the root (`/` =
  its `index.html` unchanged: the profile names its style sheet in `styles`); `ready(loader)` installs
  the resolver), `ui_assets` (the `server` UI profile built into the binary: `include_bytes!`
  table of `crates/graphersal-server/ui/` (the tests excluded), `FILES_PREFIX` `/server-ui/`,
  `profile()` adds the binary's version; a unit test checks every file in `ui/` and every
  `styles` entry is built in; the modules read the root info (`uploadBytes`) from
  `api.info.host`, `databases.js` `hostInfo`, never fetch it themselves),
  `mounts` (`/db/<name>/<rest>`:
  `registry.resolve` on EVERY request (alias flips at once), unknown -> 404 JSON, `/db/x` ->
  308 `/db/x/` (query kept), no context -> 409 `{"error": "database 'x' is not loaded", "help":
  "load it: POST /api/admin/databases/x/load", "state"}` (the text names loading / failed
  (reason) / needs-model; loading adds `Retry-After: 1`), else `forward`: prefix stripped,
  client `X-Graphersal-Graph` dropped and set to the REAL name, `Server::handle`; the resolver
  `Databases` takes `loader.context(header)` and the request holds that `Arc` only for its
  duration; `DatabaseInfo` hooks add `database` (real name), `home: "/"` (the playground's link back to the server UI;
  no `admin`) to the per-DB `/api/info`), `admin_api` (`GET /api/admin/databases` `{databases, aliases, problems}`, alias
  rows carry `aliasOf`; listing rows (`Extras`) add `sizeBytes` (`layout::dir_bytes`),
  `pooled`/`pooledBy` (`Admin::pooled_bases`), `linkWarning`; no `memoryBytes` (only the session
  thread knows it); `GET|POST models` (POST JSON `{path|repo}`, or the upload form
  `POST models?repo=&license=&format=gsnap|tar` with the file as the body: `receive_upload`
  (shared with the import upload) into `tmp/`, kind checked by `ModelFileFormat::of`, then
  `model_add` of that path), `DELETE models/<repo...>`; 409 helps: name taken,
  read-only base, base of layers (delete), base of loaded layers (unload); `GET /api/admin/databases/<name|alias>`; field names = the web UI's
  (`ui/databases.js` `databaseRows`); the actions: `POST databases` create, `DELETE databases/<name>`, `POST
  .../<name>/clone|load|unload|promote`, `PUT .../<name>/read-only|auto-load`, `GET aliases`, `PUT|DELETE
  aliases/<alias>`; JSON bodies `deny_unknown_fields`; core errors -> `{"error","help"?}` via
  `status_of` (400 invalid name/input, 404, 409 rule refusals, 500); the route table is in its
  module docs and `docs/admin-api.md` (the web UI's contract, `ui/admin-api.js`: keep them in sync);
  `POST .../<name>/backup`, `GET .../<name>/backups`, `POST .../<name>/restore {"backup",
  "name"?}`, `GET .../<name>/export?models=&materialize=`; `POST import?name=&base=&format=&description=&role=`
  with the raw file as the body (streamed by `copy_body_to` into `tmp/upload-*.bin` = `TmpFile`,
  quick 400/409 refusals before reading) or `POST import {"path", "name", "role"?, ...}` (JSON, absolute
  path read server-side; the console's form), both answered by `import_answer` (`201` entry +
  `"import": {format, layerBase?, modelsMissing, modelsResolved, skipped, notes}`, 415 for an unknown format);
  the export is NOT answered by
  `route`: the router asks `export_target` first and answers `Answer::Download` (the `.gsnap`
  written into `tmp/` by `Admin::export_to_tmp`, streamed by `serve.rs` from the file with
  `Content-Length`, then removed); `Router::route` (tests) buffers it),
  `ops` (`Admin`: THE admin core of the API and the console; online = with the `Loader`,
  offline = `Admin::open_offline` takes the server lock and scans; `create` (empty, optional
  schema, plain/layer, built in `tmp/` via `prepare_tmp`/`commit`, Store options from
  `server.toml` `[defaults]` read per create; a layer's base from the loader's pool or
  `layer_base_of_store`), `delete` (unloaded only, under `Loader::while_idle`; refused while an
  alias names it or layers name it as base; to `trash/<name>-<UTC time with - for :>`),
  `clone_database` (loaded: the open Store's `fork` with the context held so an unload waits;
  unloaded: `Store::fork_dir` / `fork_layer_dir`; config copied, `auto_load` off), load/unload
  (offline: refused naming `serve`), read-only (offline: base refuses `false`), auto-load,
  aliases; `promote` (an UNLOADED plain normal database -> `role = "base"` + `read_only`, one
  way (U11: a base is frozen forever, no demotion), under `Loader::while_idle`, `Store::inspect`
  refuses a layer store, `Registry::promote_to_base` / `admin::promote_refusal` hold the rules;
  a base's Store is never opened for writing afterwards, so its `BaseRef` stays fixed);
  backup/backups/restore/export in `ops_transfer.rs` (a child module of `ops`:
  backups = `Store::backup_zip` of the open Store (context held) or `Store::backup_zip_dir`,
  built in `tmp/`, renamed to `backups/<name>/<UTC time with ->.zip` + sidecar `<stamp>.toml`
  (kind, role, base, description, layer base identity); restore = `Store::restore_zip` into a
  `prepare_tmp` build (new name; an existing OTHER database is `NameInUse`) or a
  `Registry::prepare_replace` build over the database's own name (unloaded only, under
  `while_idle`; `commit_replace` moves the old dir to `trash/` then renames; a base that layers
  name is refused); the kind/base come from `Store::inspect` of the restored Store: a layer's
  `layer_base` must equal the named (or the only matching) base database's identity
  (`base_identity`: pooled `LayerBase::base_ref`, else `BaseRef::new(inspect.graph_id,
  commit_seq, last_commit_at)`); export = `write_graph_snapshot` of the open graph, or of
  `Store::open_read_only` / `open_layer_read_only(...).into_graph()` for an unloaded one,
  `WriteOptions::with_materialize` for a materialized layer, `ModelCopy` from the spec (a
  layer snapshot never carries models)); `import` in `ops_import.rs`
  (`ImportSource`/`ImportFormat`/`Imported`; detection: dir with a Store, `read_snapshot_info`
  head, single-file Store, extension, first character; full `.gsnap` =
  `Store::create_from_packed_dir` + `models_missing` + `models_dir` Missing -> `needs_model`
  marker in `database.toml` (state `needs-model`, kept by the scan via `DbState::at_rest`,
  cleared by `Registry::clear_needs_model` after a successful load); layer `.gsnap` =
  `matching_base` (shared with restore, refusal names the identity) + `read_layer_snapshot` +
  `create_layer_with`; Store dir / `.gstore` = `Store::convert_with(.., ConvertOptions::new()
  .with_fill_missing_blobs(BlobSource::store(models/)))` (V18: a blob the source lacks is filled
  from `models/` into the copy, then `adopt_build` hard-links it; `Built::models_filled`, merged
  into `models_resolved`, note "filled from the shared models"; `BlobUnavailable` ->
  `ServerError::ModelUnavailable` (409, repos from the source's `Store::models_dir`, help names
  `POST /api/admin/models`: a Store copy cannot become `needs-model`), `BlobDamaged` ->
  `ServerError::ModelDamaged` (500, the `models/blobs/..` path parsed from the reason)) then
  `inspect` for a layer;
  GraphSON/GraphML = `GraphSONImport::load` / `GraphSource::from_graphml_reader` +
  `Store::create_with`; `import_as(.., DatabaseRole::Base)` (`import` = role normal) makes a
  base of a PLAIN source: a layer source -> `InvalidInput` (400) before its base is matched, a
  source naming a model neither it nor `models/` carries -> `Refused` (409; a base cannot get a model later)); a missing model `models/` holds (`ModelsDir::holds`) is resolved in the build before commit (`ModelsDir::resolve`, which also adopts; `Imported::models_resolved`, `"modelsResolved"`), only the rest becomes `needs_model`; `#[cfg(test)] FailPoint` injects a failure/crash after
  the Store is written (create, restore, import); tests in `ops_tests.rs`,
  `ops_transfer_tests.rs`, `ops_promote_tests.rs` (bases by import and promotion, core + HTTP)
  and `ops_import_tests.rs` (uploads over a real listener: a 72 MiB
  generated GraphSON body, the 413 paths) (tiny synthetic model in `test_models.rs`, copied from graphersal's
  `model_fixture.rs`), also over a real listener), `console` (`DbCommand` -> one admin API request;
  `run`: offline = the SAME `admin_api::route` over an offline `Admin` with a built `Request`,
  locked = `client` to `RunInfo.address` (503 retried 10 s; `load` polls until not loading);
  refusals = `ServerError::Remote {status, message, help}`; the text in `console/render.rs`
  (`render(command, answer, dir)`, `init_text`, `error_text` (`Error:` + `Help:`),
  `usage_error_text` (`Error:` + `# show the usage` + `graphersal-server --help`),
  `command_error_text` (`error_text` with the help's admin routes mapped to console commands by
  `console/help.rs` `console_help`; the HTTP API keeps its texts), `render_outcome` (a load
  through a server shows `served at: <server.run url>db/<name>/`, from `Outcome::server_url`);
  `console::source_path` = the import source absolute and lexically normalised (kept as given
  when a `..` after a symlink would name another file)) and
  `console/start.rs` (the `serve` start block: banner (+ the network warning), sections
  Server / Databases / Problems / Embedding models / MCP; `address_line` = the one stdout line;
  stop lines)), `text` (crate-private: the graphersal CLI's layout, copied from its
  `sections.rs` + `Note`: `Section`/`render` (`── Title ───` headings at 78, `key: value` in
  one column, wrapped; `unwrapped` for paths/commands; `note`; `lines` + `table` for aligned
  columns), `Note` (headline, aligned details, commands under `# comment`, never wrapped),
  `bytes` = graphersal-session's `bytes_text`), `client`
  (minimal std `TcpStream` HTTP/1.1 client: `Host` = address, no `Origin`, `Connection: close`,
  Content-Length/chunked/close bodies, timeouts; `download` streams a `200` body into a writer:
  `db export` through a server), `error`
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
  `trash/`, refusals), `prepare_replace` / `commit_replace` (a restore over an existing
  database)). Every admin change writes its file first
  (atomically), then memory, under the registry's write lock. `serve::start` scans after the
  bind and before `ready()` (`/readyz` 503 meanwhile); `Running::registry()`.
- `model_archive` (private, V18): `is_tar` (ustar magic + checksum) and `unpack(archive, dir)`,
  the safe unpack of a model directory's tar (rules in its module docs; tests beside it, their
  `entry`/`archive`/`model_tar`/`gzip` builders reused by `models_tests.rs`); `is_gzip` (magic
  `1f 8b 08`) and `gunzip(archive, out, max_bytes)` (V24: flate2 `MultiGzDecoder` into a `.tar`
  file, refused past `max_bytes` = `[limits] upload_bytes`: the decompression-bomb guard; the
  result must be a tar). Format by content, never by file name; no zstd.
- `models` (`ModelsDir` over `models/`, U9): one physical copy of every model blob in
  `models/blobs/<2 hex>/<sha256>` (the Store blob layout), HARD-LINKED into each database's
  `store/blobs/`; no engine change. `adopt(store_dir)` = the dedupe pass (same name + length in
  `models/blobs` -> link to `blobs/tmp-srv-*` then rename over the file; a new blob is linked
  into `models/` first; link failure -> COPY into `models/`, the database keeps its own copy,
  warning logged and returned; `#[cfg(test)] fail_links`), under a process-wide mutex with
  `remove_blob`. Adoption touches only Stores nothing serves: builds in `tmp/` before
  `commit` (create, clone, import, restore: `Admin::adopt_build`), `needs-model` databases
  during `models add`, and at server start a background thread (`Admin::adopt_all`, per
  database under `Loader::while_idle`; loading/loaded ones skipped). `scan` = the derived index
  `models/models.toml` (one row per blob: repo/identity/licence read from the blob's own ustar
  manifest `graphersal-model.json`, users = databases holding the blob, `copied` = users whose
  file is another inode (unix), `waiting` = `needs_model` markers), rebuilt at every listing
  and model change, never read back. `resolve(store_dir, repos)`: `Store::restore_blobs_from(..,
  BlobSource::store(models/))` when a definition's blob is missing; a repo without a definition
  -> its blob unpacked into `tmp/` + `Store::import_model_dir`; then adopt. `copied_blobs` feeds
  the listing's `linkWarning`. Admin side in `ops_models.rs` (`models_index`, `model_add`
  (`ModelAddSpec`: a model dir via a scratch Store + `import_model_dir`, a ustar `.tar` of one
  (a `.tar.gz` gunzipped into a third scratch first) unpacked first by `model_archive::unpack` into a second scratch in `tmp/` (V18; refuses
  absolute paths, `..`, `\`/`:`, links, devices, GNU long names, > 1000 entries, depth > 16,
  duplicates; skips pax headers and `._*`; the model dir = the root or its only top dir), a
  `.gsnap` via `create_from_packed_dir`, harvested by adoption; or a repo `models/` holds; then
  `resolve_waiting`: plain `needs-model` databases (bases skipped), checked with `while_idle`, resolved, marker
  cleared + state `unloaded` under `while_idle`), `model_remove` (refused while a database
  holds a blob, waits for it or names it in `Store::models_dir`)). An import resolves
  a missing model from `models/` when it holds it (V12); `models add <repo>` resolves the rest. Tests in
  `models_tests.rs`.
- `mcp` (private): ONE `graphersal_mcp::Mcp` per process, built at start by `mcp::endpoint`
  from `server.toml` `[mcp]` (`enabled`, `token`, `max_sessions` default 256 =
  `DEFAULT_MCP_SESSIONS`; read at start only), `with_endpoint("/mcp")`, never signed sessions
  (directory mode keeps the selection in the session table); held by the loader
  (`Loader::with_mcp`, `Loader::mcp()`; `Loader::new` = no MCP, the unit tests). `Directory`
  implements `GraphDirectory` over the registry + loader, no session job: `list` (databases +
  aliases; kind plain/layer/base from `database.toml`, state, readOnly = `read_only` or base,
  description; an alias row carries its target's fields + `aliasOf`), `resolve` (alias -> real
  name NOW; unknown -> `no database or alias 'x'`), `host` (loaded -> the context + its
  `mcp_profile()`; else `McpError::Unavailable` with the state's text, `...; ask an operator to
  load it`). `route` adapts the request (as graphersal-serve's `mcp.rs`) to
  `Mcp::handle_directory` (POST/DELETE; GET 405). `/db/<name>/mcp` is graphersal-serve's own
  route behind the mount, on the same `Mcp`. The endpoint never loads/unloads. Contract:
  `docs/mcp.md`. E2E in `tests/server_tests.rs` (`mcp_*`).
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
  (plain/layer); with MCP on `shared_mcp(mcp)`, a base also `mcp_profile(Profile::new(true))`
  (plain/layer keep the endpoint's profile: the runtime toggle refuses agents' writes). Plain = `open_store` + `ServedSource::Store`; layer = pooled base +
  `open_layer_store` + `ServedSource::layer_store`; base (`role = "base"`) = an EMPTY layer
  over the pooled base (`ServedSource::graph(base.new_layer())`) with the read-only gate on for
  good (C3), no ticker, no Store opened. Base pool: one `LayerBase` per base name
  (`layer_base_of_store`, no lock), users = its loaded layers + the base itself; it leaves the
  pool with its last user; unloading a base with loaded layers is refused. Lock order: loader
  state, then registry. Tests in `loader_tests.rs` (synthetic stores from the `modern` and
  `large` samples; `test_support::skip_on_ci`).
- `src/main.rs`: the hand-parsed command line (no parser crate, like graphersal's CLI): `init`,
  `serve`, `db list|create|delete|clone|load|unload|promote|read-only|auto-load|alias|backup|
  backups|restore|export ... --dir DIR [--json]`, `import DIR SOURCE --name N [--base B] [--format F]
  [--description D] [--role normal|base] [--json]`, `models list|add SOURCE [--repo R] [--license L]|remove REPO
  --dir DIR [--json]` (`DbCommand::Models*`; SOURCE absolute when it exists, else a repo)
  (`DbCommand::Import`: the JSON path form, SOURCE made absolute;
  offline the same route over the offline admin) (`db export` offline writes the file through
  `Admin::export_to_file`, through a server streams the download into `FILE.part`) (prints `note: ... sent to its admin API at ADDR` on stderr when it went through a
  server), the tracing subscriber (ANSI colours only when stderr is a terminal), the ctrlc
  handler; it only WRITES the console's text (`out`/`err`, never `println!`: a closed pipe
  must not panic). stdout = results (`serve`: the address line only), stderr =
  diagnostics/log; exit codes 0 ok, 1 failed, 2 usage, 141 stdout closed. No `logs/`: the
  server logs to stderr.
- `crates/graphersal-server/ui/`: the server's web UI, the playground UI profile `server`
  (the graphersal-memory `memory` profile's pattern; the engine's playground mechanism: its
  `playground/CLAUDE.md` "UI profiles"): `profile.json` (module tabs Databases, Backups, Import,
  Export, Models; no workspace: the root has no graph; `files` = `server-ui/` relative to `/`),
  the tab modules `*-tab.js` (Vue options components with `template`, props `api` and `params`;
  cross-tab links through `api.navigate(tab, {db})`), the pure models `admin-api.js` (requests,
  rules, texts of every admin action) and `databases.js` (`databaseRows`, `fetchDatabases`,
  `hostHeaders`, `downloadBlob`), `shared.js` (one list, admin client and root info for every
  tab; polls while a database is loading); uploads (Import tab, Models tab's "A file from this
  computer": `modelUploadRequest` + `uploadImport({kind: 'model'})`) go through XHR with
  progress, sized against `api.info.host.uploadBytes` first; `server-ui.css` (`srv-*` classes on the playground's
  theme tokens and generic classes). Modules import ONLY `vue` and `./<own file>.js` (checked by
  `ui/tests/profile.test.mjs`) and fetch `/api/admin/*` themselves with the host headers. A new
  file in `ui/` goes into `ui_assets::FILES`. Tests: `ui/tests/*.test.mjs` (node:http mocks of
  the admin API; the profile checked with the engine's `validateProfile` from
  `GRAPHERSAL_PLAYGROUND`, else the registry copy of graphersal-serve's `ui/` found through
  `cargo metadata`; skipped when absent, failed on CI).
- Docs (the contract for readers who did not follow the code; keep them in sync with it):
  `README.md` (what it is, run, console, HTTP, known limitations), `docs/server-dir.md` (every
  file and key of the server directory), `docs/admin-api.md` (every admin route, checked
  against `admin_api.rs`), `docs/mcp.md` (endpoints, request headers, flow).
- `scripts/smoke.sh`: the end-to-end smoke test (bash + curl + python3 only): builds, inits a
  temp server dir, serves on `127.0.0.1:0` (URL from `server.run`), walks create/load/execute/
  alias/read-only/backup/restore/export/upload import/promote + layer/models/MCP/unload/delete, SIGTERM stop;
  `ok <step>` / `FAIL <step>: why` lines, exit 1 on failure, the server killed in a trap
  (`SMOKE_BIN` skips the build, `SMOKE_KEEP=1` keeps the temp dir). Extend it with a new
  user-visible feature.
- Tests: unit tests beside the code (`test_support::TempDir`), end-to-end tests in
  `crates/graphersal-server/tests/server_tests.rs` (spawns the binary, port 0, SIGTERM; the
  `db`/`import`/`models` console offline and through a running server).

## Commands

Run every build/test/server under `../graphersal/ai/bench/watchdog.sh` (RSS ceilings).

```bash
../graphersal/ai/bench/watchdog.sh cargo test
cargo fmt --all --check                      # rustfmt.toml: max_width = 100
node --test crates/graphersal-server/ui/tests/*.test.mjs   # the web UI's modules
../graphersal/ai/bench/watchdog.sh cargo clippy --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p graphersal-server
WD_TIMEOUT=600 ../graphersal/ai/bench/watchdog.sh scripts/smoke.sh   # end to end, ok/FAIL lines
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
  non-loopback address (once, in the console's start block; `serve::start` itself only logs a
  debug line), `graphersal_serve::HostCheck` on every request but the probes.
