# CLAUDE.md

Agent guidance for **graphersal-server** (EXPERIMENTAL, public repository): one process serving
many Graphersal graph databases from one server directory over HTTP. Design notes and plans are
the maintainer's local working files outside this repository (git-ignored, never committed or
cited from tracked files). When this file disagrees with the code, the code wins: say so.

## Layout

- Workspace root `Cargo.toml` (all dependency versions in `[workspace.dependencies]`, each with
  a one-line justification); one crate `crates/graphersal-server` (lib + bin, published on crates.io: `include` lists what
  the package carries (code, `ui/`, `openapi/`, tests, licence files); `cargo publish --dry-run
  --locked` must stay clean; `[package.metadata.binstall]` names the release archives).
- graphersal with features `persist`, `persist-zip` (verified ZIP backups) and `io` (GraphSON
  and GraphML imports); graphersal-serve
  with features `ui-assets` (the embedded playground per mount and at `/`, where it shows the
  server's own `server` UI profile) and `mcp` (each
  context a host of the shared MCP endpoint); graphersal-mcp (the endpoint, directory mode).
- The four Graphersal crates (graphersal, graphersal-serve, graphersal-session, graphersal-mcp)
  from crates.io, pinned exactly (EXPERIMENTAL crates, exact pin during 0.x), all on the same
  version (today `=0.9.0`); `Cargo.lock` is committed and CI builds `--locked`. Never commit
  path dependencies or a `[patch]` section (`cargo publish` and `cargo deny` refuse them).
  graphersal also with `model-download` (S1: the server-side
  Hugging Face download; it brings `model2vec`, so the server runs the models it stores). The embedded playground
  is the `ui/` of the published graphersal-serve crate. Moving to a new Graphersal version:
  bump the four pins together, `cargo update -p graphersal ...`, commit the lock. Never edit the
  graphersal repository from a task here. This repository keeps its own `target/` (never share
  graphersal's).
- `src/lib.rs` modules: `access` (B2: `access.toml` = `AccessFile` (`[users.<name>] roles,
  token_sha256, token_last4`; `[[grant]] subject/database/level`, `deny_unknown_fields`,
  written atomically mode 0600, hashes only), `Principal` (`identity()`: `user:<name>`,
  `operator`, `everyone` (B6: kind `Everyone`, admin, only with `[access] enabled = false`),
  anonymous = `None`; `subjects()` incl. `anonymous` for everyone), `Level`
  (`None < Read < Write`; `granted_level` = admin/operator `Write`, else the highest grant of
  any subject; `effective_level` = min with the database mode (read-only flag or base);
  `Access::level_in` resolves aliases; round 3 B3: a grant may name an alias and counts only
  when the request names the alias: `AccessFile::granted_level_via(principal, requested, real)`,
  `Access::reach(registry, principal, name)` -> (entry, granted) is the one check of the router,
  `user_api` and `mcp::Directory`; `rename_grants`/`drop_grants` (audited `GrantsMoved` /
  `GrantRevoked`) called by `Admin::alias_rename` (under the registry's write lock) and
  `Admin::alias_remove` via `Admin::grants_state`; B3-FIX: grants never outlive their name: `Admin::delete` drops the database's grants (`Deleted::grants`), every new name (create, clone, import, restore into a new name: `Admin::commit_new`; a new alias: `Registry::alias_set_with`; a rename's new name: `Registry::alias_rename_with`, both claims under the registry's write lock) first drops the grants left on it (`Admin::drop_leftover_grants`, audited `GrantRevoked`, answer `leftoverGrantsRemoved` when > 0)), `Secret`/`TokenHash` (256-bit OS random via `ring`,
  SHA-256, constant-time compare, redacted `Debug`), the authenticator chain
  (`Authenticator` trait: `OperatorToken` (per run, `server.run`), `UserTokens` (Bearer),
  `SessionCookie` (`gs_session`, `SessionTable` trait, `MemorySessions`; a session ends when
  its user's token changes or it expires: `SessionLimits` (lifetime, idle timeout;
  `last_seen` touched at most every 60 s or a quarter of the idle timeout); SEC L3: `login`
  inserts with `SessionTable::insert_capped` (`MAX_BROWSER_SESSIONS_PER_USER` 32, the oldest
  dropped, one write));
  wrong credentials are `AuthError`, never anonymous), `Access` (`open_with_limits` prunes a
  restored table: dead or expired sessions)
  (open/authenticate/login/logout, user and grant changes audited), the first admin
  (`ensure_first_admin`: `init` (`InitReport::first_admin`) and `serve::start`
  (`Running::take_first_admin`) on a directory without `access.toml`; `console::first_admin_text`
  frames the token, printed once on stderr by `main.rs`); `Access::granted_level` (before the
  database mode) and `audit_as(principal)`; enforcement is the router's (B3)),
  `audit` (`AuditEvent` typed, `Audit` handle = sink + actor + OS user (round 3 B1:
  `with_os_user`; a record whose actor is `access::OPERATOR` carries `AuditRecord::os_user`,
  `actor_text()` = `operator (os:<name>)`; `serve::start` passes its own process user, the
  offline console and `init` theirs), `TracingAudit` on target
  `graphersal_server::audit`; `Admin::with_audit` records create/delete/clone/import/restore/
  promote/backup, read-only, auto-load, aliases), `config` (`server.toml`, `ServerConfig`, `parse_listen`, `[limits]
  upload_bytes` (default 4 GiB), `max_connections`, `header_timeout_secs`, `idle_timeout_secs`
  = `Limits`; `[sessions] lifetime_secs`, `idle_timeout_secs` = `SessionsConfig` (SEC L4: missing = 30 d
  / 7 d, `0` = no limit; `effective_*_secs`) -> `SessionLimits`, read at start; `[mcp]
  max_sessions_per_identity` (SEC M2, default 16, restart key); B6: `[access] enabled` = `AccessConfig`
  (default true), `[http] public_scheme` (`PublicScheme` http|https) and `allowed_hosts`
  (normalized by `parse`: lower case, port dropped, IPv6 in brackets) = `HttpConfig`, read at
  start; `Running::config()` = the config the server started with; B8: `[limits] memory_limit`
  (bytes, 0 = none, "auto", "none"; `limits::memory_limit` deserializer) and `memory_headroom`
  = the process memory ceiling, set once in `serve::start` after the lock
  (`limits::apply_memory_ceiling`, only what is set), server-wide only; `[limits.query]` =
  `limits::QueryLimits`; B12: `[jobs] backup_root` = `JobsConfig` (default `backups/jobs`, relative to the server dir; `ServerDir::backup_root`/`ensure_backup_root`, created by `init` and `serve::start`, resolved, passed as `Loader::with_backup_root` -> every context's `ContextBuilder::file_root`; at load `loader::drop_jobs_outside` drops stored jobs outside it (`jobs_outside_file_root`) in the launch closure, warning; the router refuses an absolute `path` of `POST /db/<n>/api/jobs/define` before forwarding (`router::jobs_define_body`, field-error shape, `mounts::forward_with_body`); MCP profiles keep NO policy: their queries run under the playground authorizers, which deny all file access); `toml_error_message` adds the line a TOML error points at), `limits`
  (B8: `QueryLimits` (keys = the graphersal CLI flags: `timeout_ms`, `max_operations`,
  `max_string_size`, `max_array_size`, `max_map_size`, `max_traversers`,
  `max_materialized_bytes`, `max_value_depth`; 0 = no limit, depth 0 = default), `over` (a
  database key wins), `presets()` -> `SessionPresets` (nothing set = `SessionPresets::new()`; else `with_ceilings(true)`: a query may lower a set limit with `g.with`, never raise it, A7),
  `summary` (start-block row `query limits`, plus `memory ceiling` when set);
  `DatabaseLimits` = `database.toml` `[limits]` (only `query`; memory keys refused with a
  hint); `Loader::query_presets` reads `server.toml` and `database.toml` again at EVERY load
  (a bad file fails the load), refreshes the registry's copy (`Registry::refresh_limits`);
  `update_config` keeps the file's limits when it rewrites `database.toml` (read BEFORE the
  change, so only a settings change alters them), so a hand edit of
  `[limits.query]` survives admin changes; tests `loader_tests.rs` `query_limits_*`;
  `QUERY_LIMIT_KEYS`, `MAX_LIMIT_VALUE` (2^63-1), `QueryLimits::get/set(key)`; the merged limits
  a load used are kept per loaded database: `Loader::limits_in_effect`), `settings` (round 3
  B7, private: a database's runtime-editable settings as ONE change, `SettingsChange
  {description, read_only, auto_load, limits: [(key, Option<u64>)]}` + `check` (description one
  line, <= `MAX_DESCRIPTION_CHARS` 1000; keys; values), `limit_value_of_json`/`_of_text`,
  `LIMITS_APPLY_TEXT` (S1: a limit applies AT ONCE to a loaded database's new runs:
  `Loader::apply_limits` = `Context::set_presets` of graphersal-serve 0.9.0, called by
  `Loader::change_settings` (a failure only warns: the file is written), after every load
  (a change made while loading) and, for `server.toml [limits.query]` through the admin API, by
  `Admin::change_server_settings` -> `Loader::apply_limits_everywhere`; a hand edit applies at
  the next load; `reloadNeeded` = a hand edit not applied yet); writer `Registry::change_settings` (one `update_config` under the
  registry's write lock, revision checked; a base with layers refuses read-only off), online
  `Loader::change_settings` (flip lock, then registry, then the context's read-only flip),
  `Admin::change_settings` in `ops_settings.rs` (offline base rule; audits `DescriptionSet`,
  `ReadOnlySet`, `AutoLoadSet`, `QueryLimitSet` per field; also `limits_in_effect`,
  `server_query_limits`, `store_facts` = `Store::inspect` -> `StoreFacts`); route
  `admin_api_settings.rs` (`GET|PUT databases/<n>/settings`: the view = listing row + aliases,
  layers, limits rows (override/server/effective/source/inEffect, `reloadNeeded`), the MCP
  state's rows of the name and its aliases, grants (`via` database|alias|all), admins, backups
  summary, store facts or `storeError`; a PUT takes the real name; `changed` fields); console
  `db info NAME` / `db set NAME description TEXT|limit KEY VALUE|limit KEY --clear`
  (`DbCommand::Info`/`Set`, `DbSetting::limit` validates for `main.rs`; `render_info`,
  `render_set`); UI `settings-tab.js` + model `settings-api.js`; tests `settings::tests`,
  `router_access_tests.rs` `database_settings_*`/`a_base_keeps_read_only_*`, `server_tests.rs`
  `db_info_and_db_set_offline_and_through_a_server`, `ui/tests/settings.test.mjs`), `sessions` (B9: sessions survive a
  restart, ALWAYS on, no setting: `sessions/browser.json` = `FileSessions` (a `SessionTable`,
  write-through on insert/touch/remove/retain, a failed insert rolled back) and
  `sessions/mcp.json` = `McpSessionFile` (graphersal-mcp `SessionStore`; `keeping(identity
  filter)` drops at load the MCP sessions of users gone or without a token, rewriting the file;
  `forget_identity` for the offline console / MCP off); JSON `{version: 1, sessions: [...]}`,
  hashes only, `write_atomic_private`, `sessions/` created on the first write; a missing file =
  empty, an unreadable/broken one (or another version) = a warning + empty, overwritten by the
  next change), `layout`
  (`ServerDir`, `init` (creates `sessions/` and the import inbox `imports/` too; the root `0700`,
  `security::tighten` at the end: `InitReport::secured` = old paths whose mode it tightened; an
  existing directory of another OS user refused by `security::check_owner`), file names,
  `write_atomic` (round 3 B-CONCUR: a temporary name of its own per write, `temp_path` =
  `<file>.tmp-<pid>-<n>`, `create_new`, fsync of the file, rename, fsync of the directory on
  unix (`sync_parent`: a plain `libc::fsync`, not macOS's `F_FULLFSYNC`; a failure only warns:
  the file is in place; `write_atomic_private_volatile` skips it for `server.run`, so the
  probes are not delayed by the disk); `is_temp_name` (ours only,
  plus the 0.2 `<file>.tmp` of `ATOMIC_FILES` and `access.toml.new-<pid>`) and
  `ServerDir::remove_stale_temps` (root, `sessions/`, `models/`, `databases/*/`, `backups/*/`;
  called by `Registry::scan`, i.e. with the server lock held)), `revision` (B-CONCUR:
  optimistic concurrency: `of(text)` = 16 hex of SHA-256 of the file text; `check(expected,
  current, what)` -> `ServerError::Conflict {what, revision}` = 409 with help `CONFLICT_HELP`
  "changed by someone else: reload" and `revision` in the answer (`admin_api::error_answer`);
  objects: users+grants (`AccessFile::revision`, checked in `Access::change` via
  `Access::edit(actor, expected)` = `AccessEdit`), aliases (`registry::aliases_revision`,
  `check_aliases` in `alias_set_with`/`alias_rename_with`/`alias_remove_with`), a database's
  settings (`DatabaseEntry::revision`, checked in `Registry::update_config`); `Admin::expecting`
  carries it; the admin API takes the body's `revision`, else `If-Match`
  (`from_if_match`; `*` = none); GET answers carry `revision`/`aliasesRevision`/entry `revision`
  and `ETag`; no revision = applied as before; inventory of files/writers/locks and the lock
  order in `book/src/reference/server_directory.md` "Concurrent changes" and `ops.rs` module docs; tests
  `ops_concurrency_tests.rs` (409s; the 8 x 50 stress over HTTP: models, files parse, no temps,
  memory = fresh scan)), `security` (round 3 B1: the OS user owning the server directory is the
  operator; `check(dir)` before `serve::start` and `console::run` (and `main.rs` before
  `ServerDir::open`): root + `server.toml` + `aliases.toml` owned by the euid and not g/o
  writable, `access.toml`, `server.run`, `sessions/` and its files owned and not g/o
  readable/writable; refusal `ServerError::InsecureDir {dir, problems, fixes}` (`error_text`
  prints `Help:` + `# what` / command lines: `chmod`, `sudo -u`, `sudo chown -R`, `init`);
  `check_as(dir, euid)` for tests; `os_user()` (`getpwuid_r` via `libc`, unix only dependency;
  Windows `USERNAME`); `create_private_dir` (`sessions/` on first write); Windows: no check,
  documented), `lock` (`ServerLock`, fd-lock on
  `server.lock`), `run_file` (`RunInfo`, `server.run`, mode 0600: it carries the per-run `operator_token`
  and (round 3 B2) `started_at_ms`), `daemon` (round 3 B2: `start(dir, program, serve_args,
  timeout)` = check, lock (held -> `ServerError::Service` naming pid/URL), first admin made HERE
  (token never in the log; removed again when the spawn fails), `logs/` 0700 + `logs/server.log`
  0600 appended (a `== <UTC> graphersal-server start: ... ==` line), lock dropped, `serve --dir
  <absolute> <args>` spawned detached (unix `pre_exec` `setsid`, Windows `DETACHED_PROCESS |
  CREATE_NEW_PROCESS_GROUP`), waits for its own `server.run` pid + `/readyz` 200 + the address
  line in the log; failure = `Service` with `log_tail`; `stop(dir, timeout)` = no lock -> remove a
  stale `server.run` under the lock, `NotRunning`; else `POST /api/admin/shutdown` with the
  operator token (connect error or 404 -> unix SIGTERM fallback via `libc::kill`), waits for the
  lock; `status(dir)` (stale run file reported, not removed; `GET /api/admin/server`);
  `stop_with_token`/`status_with_token` through an address; texts in `console/service.rs`;
  `main.rs` exit 3 = status stopped, stop of a stopped dir exit 0), `control` (round 3 B2,
  private: `GET /api/admin/server` facts (`ServerFacts` in the `Router`, with the
  `[limits]` memory settings `usage::MemorySettings` from `serve::start`; its `memory` object =
  `usage::memory_json`) and `POST
  /api/admin/shutdown`: operator or admin user, `everyone` 403, no `StopHook` 409, audited
  `AuditEvent::ShutdownRequested`; the hook (`ServeOptions::with_stop_hook`) feeds `serve`'s
  Ctrl+C channel, never stops the server on its own thread), `serve` (`start` -> `Running`, the
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
  through `graphersal_serve::Server`, then `HostCheck::check` on everything else, then login/
  logout, authentication and the routes, each refused per principal (see "Access is enforced"
  under the rules; every in-memory answer leaves `Router::answer` through `router::no_store`:
  `Cache-Control: no-store` exactly once, checked by `every_answer_says_no_store_exactly_once`): `/db/<name>/...` via `mounts`, `GET /api/info` (root identity: `server:
  router::SERVER_NAME` ("graphersal-server"; each mount's context sets the same via
  `ContextBuilder::server_name`, never `.dev(true)`), `protocol: graphersal_serve::PROTOCOL`
  ("graphersal/1", the only thing the playground's `isEngineInfo` checks; every mount answers it
  too), `dev: false`, no `product`, no `admin`; `ui/tests/root-info.json` = its fixed fields for
  the node test, checked by `router::tests::the_node_tests_root_info_is_the_answer`, `uiProfile` = `ui_assets::profile_for(View::of(principal))`, `uploadBytes` = the upload limit, `mcp` =
  `Mcp::status()` or null; `openapi`/`databaseOpenapi` for every principal), `/mcp` (every method) via `mcp::route` (404 when MCP is off),
  `GET /openapi.json` (public, before authentication) and `GET /db/<name>/openapi.json` (+
  `/db/<name>/api/openapi.json`; after the grant check, before the loaded check; anonymous `401`,
  not a page) via `openapi` (B-OPENAPI: the server's hand-written `openapi/server.json`; a
  database's = graphersal-serve's description asked once from a throwaway context
  (`engine_spec`; S1 dropped the old `openapi/engine-patch.json`: graphersal-serve 0.9.0
  describes all of it; to patch the engine's description again, reintroduce such a file with a
  test that fails once the pin carries the entry), then `database_spec`: servers = `<public_scheme>://<Host>/db/<name>`, security schemes
  `bearerAuth`/`sessionCookie` (+ `{}` when anonymous reaches the name or access is off), the
  access layer's 401/404/409 on every operation, `x-graphersal-access` read|query|write (a
  write = the engine's `E409ReadOnlyRequest`/`E409Write`), no filtering by level, the
  not-mounted paths (probes, `api/admin/runs*`, `api/ai/*`) removed; console `openapi
  --dir DIR [--db NAME] [--out FILE] [--server URL]` = `offline_spec` over `Registry::peek`
  (a scan that never cleans `tmp/`, safe beside a running server); a NEW route of the router or
  the admin API must be added to `openapi/server.json`:
  `router_access_tests.rs` `every_route_of_the_router_and_the_admin_api_is_described` scans the
  route tables (segment lists, one-word literals, root path literals) and asks the router),
  `GET /api/databases` via `user_api` (B4: "my databases" for every principal, anonymous
  included: only the databases/aliases with a grant (an alias also by its own grant), effective
  `level`, `playground` path, `openapi` path, `kind` database|alias;
  tests `router_access_tests.rs` `my_databases_*`), `/api/admin/...` via `admin_api`, other
  `/api/*` 404, `GET /server-ui/<file>` the profile's
  files (`ui_assets::file`), every other GET a file of the embedded playground at the root (`/` =
  its `index.html` unchanged: the profile names its style sheet in `styles`); `ready(loader)` installs
  the resolver), `ui_assets` (the `server` UI profile built into the binary: `include_bytes!`
  table of `crates/graphersal-server/ui/` (the tests excluded), `FILES_PREFIX` `/server-ui/`,
  `profile_for(View)` adds the binary's version and, for `View::User`/`View::Anonymous`,
  replaces the keys `ui/profile-views.json` names (tabs `mine` / `login`; `View::of(principal)`:
  admin/operator = `Admin`); a unit test checks every file in `ui/` and every
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
  rows carry `aliasOf`; listing rows (`Extras`) add `sizeBytes` (`usage::disk_bytes`: `layout::dir_bytes`
  cached 5 s per directory), `memoryBytes` (+ a layer's `sharedMemoryBytes`; round 3 B5:
  `Admin::loaded_memory` -> `Loader::memory`: handles cloned under the loader lock, then each
  graph's `memory_footprint_bytes` through `try_read`, a busy graph answers its last figure
  (`memory_seen`), a base row = the pooled base, the sum counts each pooled base once; `null`
  when not loaded), `pooled`/`pooledBy` (`Admin::pooled_bases`), `linkWarning`; `GET|POST models` (POST JSON `{path|repo}`, or the upload form
  `POST models?repo=&license=&format=gsnap|tar` with the file as the body: `receive_upload`
  (shared with the import upload) into `tmp/`, kind checked by `ModelFileFormat::of`, then
  `model_add` of that path), `DELETE models/<repo...>`; 409 helps: name taken,
  read-only base, base of layers (delete), base of loaded layers (unload); `GET /api/admin/databases/<name|alias>`; field names = the web UI's
  (`ui/databases.js` `databaseRows`); the actions: `POST databases` create, `DELETE databases/<name>`, `POST
  .../<name>/clone|load|unload|promote`, `PUT .../<name>/read-only|auto-load`, `GET|PUT .../<name>/settings` (B7, `admin_api_settings.rs`), `GET aliases`, `PUT|DELETE
  aliases/<alias>`, `POST aliases/<alias>/rename {"name"}` (`Admin::alias_rename`: `Registry::alias_rename`
  rewrites `aliases.toml` and, under the registry's write lock, calls `Mcp::rename_selection` so
  open `/mcp` sessions move to the new name with one note; answer `mcpSessionsMoved`); JSON bodies `deny_unknown_fields`; core errors -> `{"error","help"?}` via
  `status_of` (400 invalid name/input, 404, 409 rule refusals, 500); the route table is in its
  module docs and `book/src/reference/admin_api.md` (the web UI's contract, `ui/admin-api.js`: keep them in sync);
  the run administration (V25, graphersal-serve 0.7.1 #32): `GET runs` (every loaded
  database), `GET databases/<name>/runs` (`loaded`), `POST databases/<name>/runs/cancel {"id"}`
  over `Admin::runs` / `Admin::cancel_run` (`ops_runs.rs`: each context's OWN `Runs`
  (`Context::runs()`), ids unique per database; offline `Refused`); the mounts keep
  `ContextBuilder::admin_runs` OFF (query texts only through the admin surface, no token: the
  admin surface is admin-only since B3); web UI tab `runs-tab.js`;
  users and grants (B5): `admin_api_users.rs` (`GET|POST users`, `GET|DELETE users/<n>`,
  `POST|DELETE users/<n>/token`, `PUT users/<n>/admin|roles`, `GET|POST grants`,
  `DELETE grants/<subject>/<database>` (segments percent-decoded); user row `{name, roles,
  admin, hasToken, tokenLast4, grants}`; every grant object (also a user's) carries `kind`
  (`database`/`alias`/`all`/`missing`) and `target` (`add_kind`, the registry's aliases); the token only in the add/token answer) over
  `ops_users.rs` (`Admin::users`, `user_add`, `user_token`, `user_revoke_token`, `user_remove`,
  `user_set_roles`, `user_set_admin`, `grant_set` (database or alias must exist),
  `grant_revoke`): online through the router's `Access` (`Admin::with_access(access,
  principal)`; token change/removal also `Mcp::forget_sessions_of(user:<n>)`), offline through
  an `Access::open_with_limits` over the directory's `access.toml` and its `FileSessions`
  (missing file refused; actor = the operator, as the console holds the lock; a token change or
  removal also drops the user's rows of `sessions/mcp.json` via `McpSessionFile::forget_identity`,
  as does an online server with MCP off); the last usable admin (role + token) is refused in
  `Access` (`refuse_last_admin`, `ServerError::LastAdmin` 409); `anonymous`/`operator` are
  reserved user names (`RESERVED_USER_NAMES`);
  `POST .../<name>/backup`, `GET .../<name>/backups`, `POST .../<name>/restore {"backup",
  "name"?}`, `GET .../<name>/export?models=&materialize=`; `POST import?name=&base=&format=&description=&role=`
  with the raw file as the body (streamed by `copy_body_to` into `tmp/upload-*.bin` = `TmpFile`,
  quick 400/409 refusals before reading) or `POST import {"path" | "inbox", "name", "role"?, ...}` (JSON, absolute
  path read server-side, or one entry of the import inbox `imports/` (B11,
  `Admin::inbox_source`); the console's form), both answered by `import_answer` (`201` entry +
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
  `reset_layer` in `ops_layer.rs` (round 4 B-LAYER, S1: the engine's `reset_layer`; `db
  reset-layer NAME --yes`, `POST .../reset-layer {"confirm": true}`, UI "Reset layer…"): ONE
  transaction on the layer's own Store graph: the old layer's JOB definitions read, the engine's
  `reset_layer` (catalog back to the base's), the base's jobs removed and the layer's set again;
  journaled as one commit (history kept: a Store rollback / backup goes back; nothing to
  `trash/`); a loaded layer in place on its open Store (`Loader::open_store_of`, context held,
  `try_write_checked` polled up to `LOCK_WAIT` 30 s, else `Refused` naming the runs), an
  unloaded one (also offline) `Store::open_layer` under `Loader::while_idle` (the base taken
  BEFORE that lock: `base_of` reads the loader's pool under the same mutex); answer `inPlace`,
  `discarded`, `schemaReset`, `definitionsReset`, `jobsKept`; audited `LayerReset`; tests
  `ops_layer_tests.rs`, `router_access_tests.rs` `a_layer_reset_over_http_*`, `server_tests.rs`
  `db_reset_layer_offline_and_through_a_server`, `ui/tests/layer-reset.test.mjs`;
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
  layer snapshot never carries models)); the import inbox in `ops_inbox.rs` (B11: `Admin::inbox`
  lists `imports/` (`InboxFile`: name, kind guessed from the name or a Store dir, directory,
  bytes, modified; hidden and escaping-link entries left out; a missing `imports/` created),
  `Admin::inbox_source(name)` = ONE flat entry name (empty, leading `.`, `/` `\` `:` NUL ->
  `InvalidInput` 400), missing -> `SourceNotFound` 404, canonical path outside `imports/` ->
  `Refused` 409; returns the canonical path; admin API `GET imports` `{dir, imports}`, console
  `import DIR --list` / `import DIR --inbox FILE --name N` (`DbCommand::InboxList` /
  `ImportInbox`), UI Import tab source "inbox" (`admin-api.js` `inboxRows`,
  `inboxNameProblem`); the file stays in the inbox; tests beside it, HTTP in
  `router_access_tests.rs`, console in `server_tests.rs`); `import` in `ops_import.rs`
  (`ImportSource`/`ImportFormat`/`Imported`; detection: dir with a Store, `read_snapshot_info`
  head, single-file Store, extension, first character; full `.gsnap` =
  `Store::create_from_packed_dir` + `models_missing` + `models_dir` Missing -> `needs_model`
  marker in `database.toml` (state `needs-model`, kept by the scan via `DbState::at_rest`,
  cleared by `Registry::clear_needs_model` after a successful load); layer `.gsnap` =
  `matching_base` (shared with restore, refusal names the identity) + `read_layer_snapshot` +
  `create_layer_with`; Store dir / `.gstore` = `Store::convert_with(.., ConvertOptions::new()
  .with_fill_missing_blobs(BlobSource::store(models/)).with_skip_missing_blobs())` (V18: a blob
  the source lacks is filled from `models/` into the copy, then `adopt_build` hard-links it;
  `Built::models_filled`, merged into `models_resolved`, note "filled from the shared models";
  V25 (graphersal 0.7.1, H7): a blob `models/` lacks too is left out and `missing_blobs` of the
  copy makes the database `needs-model` like a snapshot (a base -> `Refused`, 409);
  `BlobUnavailable` -> `ServerError::ModelUnavailable` (409) is kept only as a defensive
  mapping, `BlobDamaged` ->
  `ServerError::ModelDamaged` (500, the `models/blobs/..` path parsed from the reason)) then
  `inspect` for a layer (a layer Store lacking a blob is no longer refused but is not marked
  `needs-model` either: `missing_blobs` runs for plain Stores only, as for layer snapshots);
  GraphSON/GraphML = `GraphSONImport::load` / `GraphSource::from_graphml_reader` +
  `Store::create_with`; `import_as(.., DatabaseRole::Base)` (`import` = role normal) makes a
  base of a PLAIN source: a layer source -> `InvalidInput` (400) before its base is matched, a
  source naming a model neither it nor `models/` carries -> `Refused` (409; a base cannot get a model later)); a missing model `models/` holds (`ModelsDir::holds`) is resolved in the build before commit (`ModelsDir::resolve`, which also adopts; `Imported::models_resolved`, `"modelsResolved"`), only the rest becomes `needs_model`; `#[cfg(test)] FailPoint` injects a failure/crash after
  the Store is written (create, restore, import); tests in `ops_tests.rs`,
  `ops_transfer_tests.rs`, `ops_promote_tests.rs` (bases by import and promotion, core + HTTP)
  and `ops_import_tests.rs` (uploads over a real listener: a 72 MiB
  generated GraphSON body, the 413 paths) (tiny synthetic model in `test_models.rs`, copied from graphersal's
  `model_fixture.rs`), also over a real listener), `console` (`DbCommand` -> one admin API request;
  `run`: `security::check` first; offline = the SAME `admin_api::route` over an offline `Admin`
  (audit actor `operator` + this OS user) with a built `Request`,
  locked = `client` to `RunInfo.address` (503 retried 10 s; `load` polls until not loading);
  `run_with_token(dir, command, token, address)` (B1 `--token`): through the running server
  with a user's token (`through_server`, shared with `run`), address given or from
  `server.run`, no check, no file written; no `server.run` -> `Connect` refusal;
  refusals = `ServerError::Remote {status, message, help}`; the text in `console/render.rs`
  (`render(command, answer, dir)`, `init_text`, `error_text` (`Error:` + `Help:`),
  `usage_error_text` (`Error:` + `# show the usage` + `graphersal-server --help`),
  `command_error_text` (`error_text` with the help's admin routes mapped to console commands by
  `console/help.rs` `console_help`; the HTTP API keeps its texts), `render_outcome` (a load
  through a server shows `served at: <server.run url>db/<name>/`, from `Outcome::server_url`);
  `console::source_path` = the import source absolute and lexically normalised (kept as given
  when a `..` after a symlink would name another file)) and
  `console/start.rs` (the `serve` start block: banner (`banner_for(exposure, access)`) + the
  warnings (`warnings`: access off (louder off the loopback), else off the loopback http =
  clear text, https = only the proxy should reach it), the Server rows access / scheme (with the
  cookie's `Max-Age`) / allowed hosts (`mode_rows`), sections
  Server / Databases / Problems / Embedding models / MCP; `address_line` = the one stdout line;
  stop lines)), `usage` (round 3 B5, private: `disk_bytes` (TTL cache), `process_resident_bytes`
  (Linux `/proc/self/statm`, macOS `proc_pidinfo` via `libc`, Windows `GetProcessMemoryInfo` via
  `windows-sys` (windows-only dependency, already in the tree), else `None`), `machine_memory`
  (`/proc/meminfo`; `hw.memsize` + `host_statistics64`; `GlobalMemoryStatusEx`), `memory_json`
  (the summary's fields, module docs; "free" = limit - RSS, else machine available); the
  console's `status` rows in `console/service.rs` `memory_rows`, `db list`'s ON DISK/MEMORY
  columns in `console/render.rs`; UI: `databases.js` `memoryRows`, `memoryText`/`memoryTitle`,
  `diskTitle`, `SIZE_COLUMNS`, `admin-api.js` `exactBytes`, `adminRequest.server`), `text` (crate-private: the graphersal CLI's layout, copied from its
  `sections.rs` + `Note`: `Section`/`render` (`── Title ───` headings at 78, `key: value` in
  one column, wrapped; `unwrapped` for paths/commands; `note`; `lines` + `table` for aligned
  columns), `Note` (headline, aligned details, commands under `# comment`, never wrapped),
  `bytes` = graphersal-session's `bytes_text`), `client`
  (minimal std `TcpStream` HTTP/1.1 client: `Host` = address, no `Origin`, `Connection: close`,
  Content-Length/chunked/close bodies, timeouts; `download` streams a `200` body into a writer:
  `db export` through a server), `error`
  (`ServerError`), `names` (two rules: NEW names (0.3.0) `validate_new_user_name` = at most
  `MAX_NAME_LEN` 32, `[a-z0-9]` first, `[a-z0-9._-]`, a letter/digit last, no `..`, no Windows
  device name, and `validate_new_name` = that plus the `RESERVED` names (`jobs`: the default
  backup root `backups/jobs/`); EXISTING names `validate_name` = the old rule
  `[a-z0-9][a-z0-9._-]{0,62}` not ending in '.' (`MAX_EXISTING_NAME_LEN` 63); `fold` = lower
  case for case-insensitive lookups. `validate_new_name` is used by `Registry::prepare_tmp`
  (create/clone/import/restore into another name), `alias_set` (a new alias only),
  `alias_rename` and the upload import's quick check; `validate_new_user_name` by
  `AccessFile::add_user`; `validate_name` alone for registry scan, lookups, grants, existing
  users and `Registry::prepare_tmp_former_name` (a restore under the deleted database's own
  name), so old names (longer, trailing '-'/'_', `..`, `jobs`) stay served; the admin API's
  help is `RESERVED_HELP`. The web UI mirrors the rule in `ui/admin-api.js` (`newNameReason`,
  `existingNameReason`, same order and texts; `ui/tests/names.test.mjs` checks the texts
  against `names.rs`; `NAME_HINT` under every name field)), `database` (`database.toml`:
  `DatabaseConfig`, `DatabaseKind`, `DatabaseRole`, `deny_unknown_fields`, `utc_now`),
  `registry` (`Registry`: NO registry file, the registry IS `databases/*/database.toml`; `scan`
  at start with the lock held: cleans `tmp/`, broken entries -> `DbState::Failed`, broken aliases
  dropped into `problems()`, never fatal; entries behind an `RwLock`; `DbState`), `admin` (the
  admin core as `impl Registry`: `list`, `get`, `resolve`, aliases (target must be a database:
  no chains; `alias_rename(old, new, moved)`: the new name unused, the callback runs under the write lock), `set_auto_load`, `set_read_only` (persists the flag only; a base with layers stays
  read-only), `prepare_tmp` -> `TmpBuild` -> `commit` (build in `tmp/`, atomic rename into
  `databases/`; a dropped build is removed), `delete` (the registry half of a delete: move into
  `trash/`, refusals), `prepare_replace` / `commit_replace` (a restore over an existing
  database)). Every admin change writes its file first
  (atomically), then memory, under the registry's write lock. `serve::start` scans after the
  bind and before `ready()` (`/readyz` 503 meanwhile); `Running::registry()`.
- `recovery` (round 3 B-CRASH, public: after a forced kill): `server.open` = `OpenFile`
  (`pid`, `started_at_ms`, `databases`; `write_atomic` WITH the directory fsync) written by
  `OpenRecord` (a leaf mutex taken BEFORE the loader's state is read: `Loader::record_open`,
  called after a load start, a load end, a thread-spawn failure and a CLEAN unload; set by
  `serve` after `router.ready()` and before `auto_load` (its fsync must not delay the probes),
  `Loader::set_open_record` writes the whole open set; `records_open()`), removed by
  `Running::shutdown` only when `Loader::shutdown` was Ok and a record was set (a start that
  fails before the scan keeps the evidence); `daemon::stop` removes a stale `server.run`, never
  `server.open`. `detect(dir)` (lock held, before this server writes `server.run`) =
  `Previous {pid, started_at_ms, url, databases: Option (None = no server.open: the auto_load
  databases are checked, `names_to_check`), run_file_left}`. `start_check` (in `serve::start`
  after `auto_load`): `Recovery::begin` (`recovery.json` = `UncleanShutdown {detectedAtMs,
  previousPid, ..., databasesKnown, databases: [DatabaseCheck {name, outcome: CheckOutcome
  (pending|checking|ok|damaged|failed|needs-model|missing), state, recovery: RecoveryFacts (the
  engine's `OpenReport`), verify: VerifyFacts, damage, message, checkedAtMs}], done}`, camelCase
  JSON), log warn, audit `UncleanShutdown`, `loader.load` of every name (whether or not
  auto-load), then ONE thread `graphersal-server-recovery`: per name `loader.wait` (1 h),
  `Loader::open_report_of`, `Admin::verify`, log + audit `DatabaseChecked` + `Recovery::update`;
  `finish`. `Recovery` (leaf mutex) lives on the loader (`set_recovery`/`recovery()`;
  `Admin::recovery()`), offline `read_report(dir)`. Shown by: start block (`console/start.rs`
  warning row `UNCLEAN SHUTDOWN` + section "Unclean shutdown", `Running::unclean_shutdown()`),
  `status` ("Last unclean shutdown": `console/service.rs` `unclean_section`; stopped = `detect`
  as "pending" + `recovery.json`; `status_json_of` adds `uncleanShutdown`/`uncleanShutdownPending`
  for a stopped server), `GET /api/admin/server` (`uncleanShutdown`, `databases.maintenance`),
  `GET /api/admin/recovery`, the settings view's `recovery`, `db info` ("Damage and
  recovery"), the Databases tab's box. `DbState::Maintenance {damage}` (`is_open()` = loaded or
  maintenance, `is_busy()` = + loading; every former `Loaded` check uses them): set by
  `Loader::run_load` when the engine opened the Store in maintenance mode (`LoadedDb::damage`
  = `Store::maintenance()` text; layer stores too) and by `Admin::verify` when the open Store's
  verify froze it (`damage_found` non-empty + policy `maintenance`); entry JSON `damage` +
  `reason`. Admin side `ops_damage.rs`: `Admin::verify` (open store: context held,
  `store.verify()`; else `Store::verify_dir`), `verify_all`, `repair(name, new)` (plain normal
  only; `prepare_tmp` build, `store.repair_to` / `Store::repair_dir`, `adopt_build`,
  `commit_new`, audit `DatabaseRepaired`; default name `free_repair_name` = `<n>-repaired`,
  `-2`..), `swap(name, with)` (unload both, `Loader::while_all_idle`, `Registry::swap_in` in
  `admin.rs`: old dir -> trash, `with` dir renamed, old `database.toml` bytes put back, `with`
  entry removed, `drop_grants_on(with)` under the write lock; reload when it was loaded; audit
  `DatabaseSwapped`), `repair_json`. Routes `admin_api_damage.rs` (`POST databases/<n>/verify`,
  `POST verify`, `POST databases/<n>/repair {"name"?}` (empty body allowed), `POST
  databases/<n>/swap {"with","confirm":true}` (400 without confirm), `GET recovery`;
  `damage_help`, `unclean_json`); console `db verify NAME|--all` (exit 1 on damage:
  `console::failed`), `db repair NAME [--name NEW]`, `db swap NAME WITH --yes` (usage error
  without `--yes`); help mapping in `console/help.rs`. UI: `admin-api.js` `isOpenState`,
  `damageActions`/`repairAction` (NOT in `rowActions`), `repairedName`, `verifyText`,
  `verifyAllText`, `problemLines`, `repairSummary`, `uncleanRows`, `adminRequest.verify|
  verifyAll|repair|swap|recovery`, `confirmText('swap')`; `databases.js` state `maintenance`
  (tone danger, row `damage`); `databases-tab.js` (Verify, Verify all, Repair… dialog with
  the report then "Let it take the name…"/"Keep both", the unclean box); `settings-api.js`
  `recoveryView` + "Damage and recovery" in `settings-tab.js`. Tests: `recovery::tests`,
  `ops_damage_tests.rs` (a snapshot segment damaged by hand: maintenance, repair, swap; open
  set), `server_tests.rs` `a_server_killed_mid_writes_is_checked_at_the_next_start` (unix,
  SIGKILL of a child mid-writes) and `a_damaged_database_is_in_maintenance_and_is_repaired_and_swapped`,
  `ui/tests/recovery.test.mjs`. Docs: `book/src/reference/server_directory.md` "`server.open` and
  `recovery.json`", `book/src/troubleshooting.md` "A server was killed by force" and "A database
  is in maintenance", `book/src/reference/admin_api.md` "Verify, repair and swap".
  Round 4 B-VERIFY: `ProblemFacts` gained `kind` (`damage` | `model`), `model`, `candidates`,
  `blob`, `shared`, `remedy` (serde defaults: old `recovery.json` reads); `VerifyFacts` gained
  `verdict` (`ok` | `needs-model` | `damaged`; `verdict()` derives it for old reports) and
  `models_missing`; `VerifyFacts::name_models(known, missing, shared)` names a missing blob
  (engine text "the blob is missing", path `blobs/../<hash>`) by `known(hash)` (S1: the verify's
  own `VerifyReport::models` by `ModelInfo::blob_hash`, exact for every model of the catalog,
  plain or layer, open or files; else `ModelsDir::repo_of_blob` = the manifest of
  `models/blobs/<hash>`), else (a fallback only) by elimination among `VerifyReport::models` in
  `ModelState::Missing`; only model problems = verdict `needs-model`. `Admin::verify` then
  `follow_models` (plain, not base, state unloaded/needs-model: `Registry::set_needs_model` +
  state `needs-model`, so `models add` resolves it; a `needs-model` db verifying ok: marker
  cleared, `unloaded`; loaded/layer/base untouched) and `keep_last_verify`: `LastVerify`
  (`recovery.rs`, `databases/<name>/verify.json` = `LAST_VERIFY_FILE`, `write_atomic`, version 1,
  first 100 problems; `LastVerify::failed` for a verify that failed; read on demand, no cache:
  it lives and dies with the database directory). JSON: verify result `verdict`, `modelsMissing`,
  `help` = `model_help` for needs-model; list rows `lastVerify` (`last_verify_brief`); settings
  `recovery.needsModel` + `recovery.lastVerify` (+ `headline`). Console: `verify_note` /
  `problem_text` / `render_verify_all` ("needs model", `models add` commands), `db info` rows
  "needs model" + "last verify". UI: `admin-api.js` `verifyVerdict`, `problemText`,
  `modelRemedy`, `verifyHelp`, `verifyAllText(results, {total, cancelled})`, `verifyAllLabel`,
  `runVerifyAll` (Verify all = per-database calls from the UI: progress `x/y`, Cancel before the
  next), `verifyBadge`; `databases.js` `lastVerifyOf` (row `lastVerify`); `settings-api.js`
  `verifyView` (one wording: last verify / needs-model / never verified), block
  `data-settings-verify` in `settings-tab.js`. Tests: `models_tests.rs`
  `a_verify_names_a_missing_model_blob_and_its_remedy_and_keeps_the_last_result`,
  `name_models_by_catalog_elimination_and_leaves_other_blobs_as_damage`,
  `a_loaded_database_names_a_vanished_model_blob_from_its_catalog_and_stays_loaded`, console
  `db_verify_names_a_missing_model_and_the_command_that_adds_it`, `ui/tests/verify.test.mjs`.
- `server_settings` (round 4 B-SETTINGS, public): `server.toml` edited at runtime. `KEYS` (every
  key by dotted name, file order, `Kind` bool/count/text/memory/hosts, `Applies` now|create| (S1: the query limits are `now`, `load` is gone)
  restart, help), `value_of(config, key)` (JSON), `toml_of_json`/`toml_of_text` (API / console
  values), `edit(text, path, changes)` -> `Edited {text, config, changed, rewritten}`: the change on
  the TOML table, checked by `ServerConfig::parse`, then a LINE PATCH of the file's own text
  (key line replaced, a `# key = ...` example taken over, a reset key commented out, a missing
  section appended) kept only when it parses to the same config, else the whole file rewritten
  from the table with a header (`rewritten`); no `toml_edit` (not in the tree). `LiveConfig`
  (on the loader: `Loader::set_live_config` by `serve`; `None` in unit tests): `applied` config
  (the start config + the `now` keys applied since), the writer mutex (a LEAF lock), `apply(file)`
  (upload limit -> `Router::upload_limit`; memory -> `limits::apply_memory_live` (engine setters,
  runtime-safe); sessions -> `Access::set_session_limits` (shared `Arc<RwLock<SessionLimits>>`
  with `SessionCookie`, prunes); public scheme -> `Router::modes()` cookie `Secure`/`Max-Age`,
  openapi URLs), `pending(file)` = now/restart keys whose file value differs from `applied`
  (derived, never stored: hand edits show too). Admin: `ops_settings.rs` `server_settings` /
  `change_server_settings` (revision of the file text, `write_atomic`, apply, audit
  `ConfigSet {key, value}` per changed key). Route `admin_api_config.rs` (`GET|PUT config`,
  `{"set": {key: value|null}, "revision"?}`, answer + `changed`/`applied`/`rewritten`,
  `mcpExposeNew` from `mcp.toml`); `GET /api/admin/server` `restartPending`; console `config
  get [KEY]|set KEY VALUE|unset KEY` (`DbCommand::ConfigGet/ConfigSet`, text in
  `console/settings.rs`) and `config check [FILE]` (local parse), `status` row "restart
  pending"; `init --config FILE --set KEY=VALUE` (`ServerDir::init_configured`, refused on an
  existing directory). `[defaults] auto_load`/`read_only` (new): copied at create and import
  (`CreateSpec::auto_load/read_only` are `Option`: `None` = the default; console
  `--no-auto-load`/`--no-read-only`; listing `newDatabases`; settings view `serverDefaults`).
  UI: the tab is now "Settings" (`settings-tab.js`): picker entries `:defaults` / `:server` first
  (no database name starts with `:`) mount `server-settings-view.js` (model
  `server-settings-api.js`); limits rows `inherited` (badge inherited/override, button
  "Inherit"); `shared.js` `serverState` + `readServerState` feed the header's "restart pending"
  badge; create form starts from `list.newDatabases` (`createBody(form, defaults)`). Tests:
  `server_settings::tests`, `router_access_tests.rs`
  `server_settings_are_for_admins_audited_and_based_on_the_files_revision`, `server_tests.rs`
  `server_settings_apply_at_once_or_after_a_restart_over_http`,
  `server_defaults_are_inherited_by_databases_key_by_key_and_new_databases_start_with_them`,
  `config_commands_offline_and_through_a_server`, `init_takes_server_toml_from_a_file_and_set_values`,
  `ui/tests/server-settings.test.mjs`. Docs: `book/src/reference/configuration.md` (Read column
  live/load/create/restart, "Changing server.toml at runtime", "Server defaults and inheritance").
- `server_backup` (round 3 B-CONF, public): the backup of the whole server in ONE ZIP and its
  restore into a NEW directory. Container `archive` (private: stored entries, zip64 always,
  CRC-32 + SHA-256 (`ring`) per entry; `ZipWriter::add_bytes/add_file/finish`, one-pass
  `ZipReader::next_entry` -> `Entry` (Read) + `finish()` = CRC check + SHA-256; graphersal's own
  zip.rs is private, so a copy of its subset). Entries: `ROOT_FILES` (`server.toml`,
  `aliases.toml`, `access.toml`, `mcp.toml`), `databases/<n>/database.toml`, `models.json`,
  with databases `databases/<n>/backup.zip` + `backup.toml` (the per-database backup's first
  half `Admin::backup_to_tmp` in `ops_transfer.rs`, shared with `Admin::backup`; `Sidecar` is
  `pub(crate)`) and `models/blobs/<2>/<hash>`, `manifest.json` LAST (`Manifest`:
  format/`FORMAT_VERSION` 1/serverVersion/createdAt/sourceDir (name only)/withDatabases/files
  (path, bytes, sha256)/databases (included, reason)/excluded (`EXCLUDED` + per-db stores)).
  Writer `ops_server_backup.rs` (`Admin::server_backup(with_databases, out)`,
  `server_backup_to_file` (`.part` 0600 + rename, audited `AuditEvent::ServerBackedUp`),
  `server_backup_to_tmp` (the API download)); `config_snapshot` copies the files under the
  registry read lock, then `Access::hold()`, then `McpControl::hold()` (both new read-lock
  helpers); a database whose backup fails is left out with a reason (never fails the whole
  backup). Route `GET /api/admin/server-backup[?withDatabases=1]` answered by the router as an
  `Answer::Download` (`admin_api::is_server_backup`/`server_backup`; `Download` is now generic:
  `Download::of(TmpFile, headers, what)`, `serve::pump` takes a `TmpFile`; headers
  `X-Graphersal-Server-Backup`, `-Databases`, `-Skipped`); `everyone` `403` in the router.
  `restore(archive, dir, RestoreOptions)`: `check_target` (missing or empty, owner), pass 1
  `read_entries` (small entries kept, `EntryKind::of` whitelist), `check_manifest` (format,
  version, server version newer -> `Refused` unless `with_force_version`; every listed file's
  bytes+sha; no unlisted entry; unknown names refused unless forced), parse checks, then
  `Cleanup` guard (removes everything on error), `server.toml` 0600, `ServerDir::init_with(root,
  false)` (no first admin), pass 2 blobs -> `models/blobs/`, db zips -> `backups/<n>/from-server-
  backup.zip`, `Admin::open_offline` + `Admin::restore` per db (non-layers first; leftover-grant
  drop is a no-op: `access.toml` is written AFTER), archive `database.toml` written back when
  kind/role/base match, then `aliases.toml`/`mcp.toml`/`access.toml` (0600), every top dir 0700,
  `tighten`; databases without data skipped + notes. `ServerError::BackupArchive {path,
  message}`. Console `console/backup.rs` (`server_backup` offline under the lock or download
  through the server into `.part` 0600, `server_backup_with_token`, `default_backup_file`,
  `server_backup_text`, `restore_json`, `restore_text`); `main.rs` `backup` / `restore`
  (`parse_simple`). UI: Backups tab section "Server backup" (`admin-api.js`
  `serverBackupPath`, `fileNameOf`, `fetchServerBackup`, `SERVER_BACKUP_WARNING`). Tests:
  `archive::tests`, `server_backup::tests`, `ops_server_backup_tests.rs`, `router_access_tests.rs`
  `the_server_backup_download_is_for_admins_only_and_audited`, `server_tests.rs`
  `a_server_backup_restores_on_a_new_directory_and_serves_the_same_users_and_data`,
  `ui/tests/server-backup.test.mjs`, smoke step. Docs: `book/src/guide/backups.md` "Back up the whole server" +
  "Move a server to a new machine", `book/src/guide/security.md` "Server backups are secrets",
  `book/src/reference/admin_api.md` "Server backup". No scheduled server backup (the job mechanism is per
  database in the engine).
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
  -> its blob unpacked into `tmp/` (its manifest as `graphersal-embed-model.json`, so revision
  and commit survive) + `Store::import_model_dir`; a definition still `Missing` after that and a
  `models/` blob with the SAME identity (repo + the three model files' hashes; another revision
  string, card or licence = other blob bytes) -> the same import, which the engine plans as
  `ImportPlan::Restore` (definition rewritten under its name); another identity never resolves
  (S2, tests `a_download_*` in `models_tests.rs`); then adopt. `copied_blobs` feeds
  the listing's `linkWarning`. S1 model download (`model_download.rs`, private: `Downloads` on the
  loader = ONE download slot, a leaf mutex; `DownloadStatus` running|done|failed|cancelled,
  progress from the engine's `DownloadEvent`s, `ReportFacts` of `ModelDownloadReport`;
  `ENDPOINT_ENV` `GRAPHERSAL_HF_ENDPOINT` / `TOKEN_ENV` `HF_TOKEN` read from the server's
  environment at each download start, never from a request; `ModelDownloadSpec::endpoint` is the
  tests' only): `Admin::model_download_start` (repo/revision checked first, `Refused` while one
  runs, online a thread `graphersal-server-model-download`, offline synchronous),
  `run_download` = `ModelCache::at(tmp scratch).download_staged` -> `StagedModel::new_model` ->
  `Store::import_model_dir` into a scratch Store in `tmp/` -> `ModelsDir::adopt` ->
  `finish_add` (shared with `model_add`: scan, `resolve_waiting`); audited
  `ModelDownloadStarted`/`ModelDownloaded`/`ModelDownloadFailed`; `Loader::shutdown` cancels a
  running one (waits up to 10 s); routes `GET|POST models/download`, `POST
  models/download/cancel` (body `{"repo", "modelRevision"?}`: not `revision`, the API's
  optimistic-concurrency word; `admin_api::download_json`; `DELETE models/download...` is 405,
  not a model removal), console `models download REPO [--revision R]` / `models download
  --cancel` (`DbCommand::ModelsDownload*`; through a server `console::wait_for_download` polls
  until it ends), UI Models tab section "Download from Hugging Face" (`admin-api.js`
  `downloadRequest`/`downloadView`/`downloadNotice`, poll `DOWNLOAD_POLL_MS` while shown); tests
  `model_download::tests`, `models_tests.rs` `an_offline_download_*`/`an_online_download_*`
  (fake hub `test_models::FakeHub`), `router_access_tests.rs`
  `the_model_download_routes_are_for_admins_only`, `server_tests.rs`
  `models_download_offline_and_through_a_server`, `ui/tests/model-download.test.mjs`. Admin side in `ops_models.rs` (`models_index`, `model_add`
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
- `mcp` (private): ONE `graphersal_mcp::Mcp` per process, built at start by
  `mcp::persistent_endpoint` (`with_session_store(McpSessionFile)` after `with_max_sessions`;
  a store load error = a warning and an in-memory endpoint; `mcp::endpoint` = in memory, tests
  only) from `server.toml` `[mcp]` (`enabled`, `max_sessions` default 256 =
  `DEFAULT_MCP_SESSIONS`; read at start only), `with_endpoint("/mcp")`, never signed sessions
  (directory mode keeps the selection in the session table); held by the loader
  (`Loader::with_mcp`, `Loader::mcp()`; `Loader::new` = no MCP, the unit tests); no token (the
  old `[mcp] token` is gone: an unknown key now). `Directory { loader, access, principal }` is
  built PER REQUEST by the router and implements `GraphDirectory` over the registry + loader
  for that principal, no session job: only databases with a granted level (`list` filters,
  `resolve`/`host` of a hidden name = the unknown-name error word for word; `host` for a `read`
  grant = the context's profile `.with_read_only(true)`; listing `readOnly` also for a read
  grant); `list` (databases +
  aliases; a session keeps the NAME it selected (alias or database) and graphersal-mcp resolves it per call: a flip moves it, a removed name errors, a rename moves it via `Admin::alias_rename`; kind plain/layer/base from `database.toml`, state, readOnly = `read_only` or base,
  description; an alias row carries its target's fields + `aliasOf`), `resolve` (alias -> real
  name NOW, or the alias's own name when the caller reaches it only through an alias grant, so
  `host` resolves it again; unknown -> `no database or alias 'x'`), `host` (loaded -> the context + its
  `mcp_profile()`; else `McpError::Unavailable` with the state's text, `...; ask an operator to
  load it`). `route` adapts the request (as graphersal-serve's `mcp.rs`) to
  `Mcp::handle_directory` (POST/DELETE; GET 405). `/db/<name>/mcp` is graphersal-serve's own
  route behind the mount, on the same `Mcp` (MCPPROF: answered by the router itself under the
  name's profile, see `mcp_profiles`). The endpoint never loads/unloads. Contract:
  `book/src/guide/mcp.md`. E2E in `tests/server_tests.rs` (`mcp_*`).
- `mcp_profiles` (MCPPROF, private; module docs = the contract): MCP profiles = the graphersal
  CLI's `--mcp-config` JSON (graphersal-mcp `McpConfig`), instructions INLINE (`check`: nulls
  dropped, <= `MAX_PROFILE_BYTES`, `McpConfig::parse` with a read callback that refuses a
  `file`; `inline_from_disk` for the console's `mcp profile set FILE`, via the public
  `console::read_mcp_profile`), `warnings` (writing tools a reader never gets,
  `saved_query_tools` on `/mcp`), `builtin()` (`GET /api/admin/mcp/builtin`: graphersal-mcp's
  `instructions()` + every tool's write/read description from a throwaway endpoint's
  `directory_tool_listings`, the opt-in `usage_statistics` asked by name), `preview_database`
  / `preview_directory` (a throwaway `Mcp` answers `initialize`/`tools/list`/`resources/list`
  over the loaded context, else `empty_host()` = a OnceLock context over `Sample("empty")`),
  `saved_queries(context)` (`Session::list_queries`), `rewrite_directory_answer` (`/mcp`: the
  `result` of `initialize`/`tools/list`/`resources/list` replaced by a throwaway directory
  endpoint's for the server default profile; the real endpoint is built with the built-in
  profile and keeps the sessions), `ProfileSource` (own/database/default/builtin). Storage:
  `mcp.toml` `[default_profile]` and `NameSettings::profile` (`[databases.<n>.profile]`, so
  rename/forget/claim_new/server backup carry or drop them; `NameSettings` is no longer `Copy`);
  `McpSettings::profiles()` parses them (a bad one = `Config` at open, `InvalidInput` on
  change), cached in `McpControl::profiles` (replaced under the settings' write lock);
  `McpControl::profile_for(requested, real)` = own -> alias's database -> default -> built-in
  (whole profiles). Applied: `/db/<name>/mcp` is answered by the ROUTER (`mcp::route_database`
  = `Mcp::handle_with_profile` on the database's context, `mcp::database_profile` per request:
  the context's profile (read-only for a base) `.with_config`, read-only for a read grant or MCP
  read-only), no longer forwarded to graphersal-serve (whose `ContextBuilder::mcp_profile` is
  fixed at load); `/mcp`: `Directory::host` gives the default config, `mcp::route` rewrites the
  answers. Admin: `Admin::mcp_profile_set(name|None, Some|None)` in `ops_mcp.rs` (audited
  `McpProfileSet`), `Admin::loaded_context`; routes in `admin_api_mcp.rs` (`mcp/builtin`,
  `mcp/profile`, `mcp/databases/<n>/profile`, `mcp/preview`; the MCP state's `defaultProfile`
  and rows' `profile`); console `mcp profile show|set FILE|clear|preview [NAME] [--read]`
  (`DbCommand::McpProfile*`, `render_profile*`, `render_preview`); UI `mcp-profile-editor.js`
  over `mcp-profile-api.js` (form <-> profile writes only what differs from the built-in texts;
  `importConfigFiles` inlines the instructions file chosen with the config). Tests:
  `mcp_profiles::tests`, `mcp_control::tests::profiles_round_trip_*`, `router_access_tests.rs`
  `mcp_profiles_apply_*`, `saved_query_tools_of_a_profile_*`,
  `a_cli_mcp_config_imports_and_answers_as_graphersal_mcp_does` (the KB-like config gives the
  same `initialize`/`tools/list`/`resources/list` as graphersal-mcp with it), `server_tests.rs`
  `mcp_profiles_by_console_offline_through_a_server_and_in_a_server_backup`,
  `ui/tests/mcp-profile.test.mjs`. Docs: `book/src/guide/mcp.md` "Profiles". By hand:
  `scripts/mcp-profile-demo.sh DIR [PORT]` (a demo directory, started in the background).
- `mcp_control` (round 3 B6, private): the runtime MCP switches in `mcp.toml`
  (`McpSettings {enabled, expose_new, databases: {name: NameSettings {exposed, read_only}}}`, names folded,
  defaults not written, canonical text + `revision`; round 4 B-MCPDEF: `ServerDir::init` (new
  directory only, not a server-backup restore) writes `McpSettings::initial()` = `enabled =
  false`, `expose_new = false`; a missing file/key = `enabled = true`, `expose_new = false`; a
  name WITHOUT an entry is exposed (the old rule: an upgraded directory offers what it did); every
  NEW name gets its entry at birth via `McpControl::claim_new(name, expose)` called by
  `Admin::claim_mcp_settings` from `Admin::commit_new` (create/import/clone/restore-new/repair;
  after `drop_leftover_grants`, before `Registry::commit`, forgotten again if the commit fails; a
  write failure refuses the creation) and from `Admin::alias_set`'s claim closure (new alias only);
  the choice: `Admin::exposing(Option<bool>)` (API body `"mcp"`, upload `?mcp=`, console
  `--mcp`/`--no-mcp`), `CreateSpec::mcp` for create, else `expose_new`; answers carry
  `mcpExposed` (`admin_api::with_mcp`); `Admin::mcp_set_expose_new` (audited `McpExposeNewSet`),
  `PUT mcp {"enabled"?, "exposeNew"?}`, console `mcp expose-new on|off`, UI MCP-tab checkbox +
  "Expose through MCP" in create/clone/new alias/import forms; tests
  `server_tests.rs` `a_new_server_has_mcp_off_and_new_names_are_hidden_from_mcp`,
  `an_older_server_directory_keeps_its_mcp_exposure`, `ui/tests/mcp-defaults.test.mjs`; `McpControl` = `RwLock` held across
  check/write/memory (a LEAF lock), `open(dir)` (missing = defaults, a bad file = `Config`,
  stops `serve`), `change(expected, f)`, `forget_name`/`rename` (called from
  `Admin::drop_grants_on`/`move_alias_grants`: delete, alias remove, a new name drop; rename
  moves)); set once on the loader by `serve` (`Loader::set_mcp_control`/`mcp_control()`,
  `None` in unit tests = all on). Semantics: `requested` resolving to `real` is exposed when
  both are, read-only when either is (an alias narrows, never widens); `server.toml [mcp]
  enabled = false` stays the hard switch (no `Mcp` built). Enforcement: router `/mcp` and
  `/db/<n>/mcp` -> `mcp::switched_off()` 503 (sessions kept); `/db/<n>/mcp` hidden ->
  `mcp::not_exposed` 404, read-only -> `mark_read_only`; `mcp::Directory` filters `list`,
  refuses hidden names with the unknown-name text, `host` read-only profile, `resolve` routes by
  the alias name when the alias has own settings. `McpUsage` (in memory since start): open
  sessions from the endpoint's saved table (`ObservedSessions` wraps `McpSessionFile` as the
  `SessionStore`) + per-database endpoint sessions (`mount_session`, sha256 of the
  `Mcp-Session-Id` of an `initialize` answer), tool calls observed by the router
  (`mcp::buffer_body` reads the POST body, at most 16 MiB + 1, `observe` parses the JSON-RPC
  request/answer): per tool/user/database (bounded `MAX_NAMES`, `(other)`), errors, last
  `RECENT_ERRORS` = 50, `refused`. Admin: `ops_mcp.rs` (`Admin::mcp_control`, online the
  loader's, offline the file; `mcp_set_enabled`, `mcp_set_name` under `Registry::while_named`,
  audited `McpSwitched`/`McpDatabaseSet`), routes `admin_api_mcp.rs` (`GET`/`PUT mcp`, `PUT
  mcp/databases/<n>`, state JSON with `databases` rows and `usage`), console `mcp
  status|on|off|db NAME [read-only] on|off` (`DbCommand::Mcp*`, `render_mcp`), UI `mcp-tab.js`
  + model `mcp-api.js`. Tests: `mcp_control::tests`, `router_access_tests.rs` `mcp_switches_*`,
  `mcp_usage_*`, `server_tests.rs` `mcp_commands_offline_and_through_a_server`,
  `ui/tests/mcp.test.mjs`.
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
  `serve`, `db list|create|delete|clone|load|unload|promote|read-only|auto-load|info|set|alias|backup|
  backups|restore|verify|repair|swap|export ... --dir DIR [--json]` (`db swap` needs `--yes`) (`db set NAME limit KEY --clear`: the only
  `--clear`), `import DIR SOURCE --name N [--base B] [--format F]
  [--description D] [--role normal|base] [--json]` (or `--inbox FILE` instead of SOURCE; `import DIR
  --list`), `models list|add SOURCE [--repo R] [--license L]|remove REPO
  --dir DIR [--json]` (`DbCommand::Models*`; SOURCE absolute when it exists, else a repo),
  `user list|add NAME [--admin]|token NAME [--revoke]|remove NAME|admin NAME on|off|grant SUBJECT
  DATABASE read|write|revoke SUBJECT DATABASE --dir DIR [--json]`, `backup --dir DIR [--out FILE]
  [--with-databases]` and `restore FILE --dir NEWDIR [--force-version]` (B-CONF) (`DbCommand::User*`; tests in
  `console/user_tests.rs`, both paths; `db`/`import`/`models`/`user` take `--token TOKEN`
  (env `GRAPHERSAL_SERVER_TOKEN`) and `--server ADDRESS` (`Remote::of`; `--server` without a
  token = usage error; `print_outcome` -> `console::run_with_token`); a console command
  installs the log subscriber only when `RUST_LOG` is set (offline audit lines))
  (`DbCommand::Import`: the JSON path form, SOURCE made absolute;
  offline the same route over the offline admin) (`db export` offline writes the file through
  `Admin::export_to_file`, through a server streams the download into `FILE.part`) (prints `note: ... sent to its admin API at ADDR` on stderr when it went through a
  server), the tracing subscriber (ANSI colours only when stderr is a terminal), the ctrlc
  handler; it only WRITES the console's text (`out`/`err`, never `println!`: a closed pipe
  must not panic). stdout = results (`serve`: the address line only), stderr =
  diagnostics/log; exit codes 0 ok, 1 failed, 2 usage, 3 `status` stopped, 141 stdout closed.
  `serve` logs to stderr; `start` appends the detached server's output to `logs/server.log`.
- `crates/graphersal-server/ui/`: the server's web UI, the playground UI profile `server`
  (the graphersal-memory `memory` profile's pattern; the engine's playground mechanism: its
  `playground/CLAUDE.md` "UI profiles"): `profile.json` (the admin's: module tabs Databases, Backups, Import,
  Export, Models, Runs, Users, MCP (`mcp-tab.js` over `mcp-api.js`, B6), Settings (round 4: was
  "Database settings"; `settings-tab.js` over `settings-api.js`, B7; id `settings`, address parameter `db`, opened by
  a Databases row's "Settings…"), `header` = `header.js`; no workspace: the root has no graph; `files` = `server-ui/` relative to `/`),
  `profile-views.json` (the keys a user's view (`mine-tab.js`, "My databases" over `GET
  api/databases`) and anonymous's (`login-tab.js`, "Sign in": paste a token, `POST api/login`,
  then the address's `?next=` if `access-api.js` `safeNext` takes it, else the root) replace;
  the server picks the view per principal, the page never decides it),
  the tab modules `*-tab.js` (Vue options components with `template`, props `api` and `params`;
  cross-tab links through `api.navigate(tab, {db})`), the pure models `admin-api.js` (requests,
  rules, texts of every admin action, the Users tab's `userRows` (`tokenText` `…last4`),
  `grantRows`, `userActions` (last usable admin), forms and texts), `databases.js` (`databaseRows`, `fetchDatabases`,
  `hostHeaders`, `downloadBlob`) and `access-api.js` (login/logout, `safeNext`, `principalOf`,
  `headerModel`, `mineRows`, `refusalAction`: 401 with `login` -> the login page, 404 -> read the
  root info again and load the root when no longer admin), `shared.js` (one list, admin client
  with `onRefusal` = `handleRefusal`, and root info for every
  tab; polls while a database is loading; `useFreshData(refresh)`, `useSort(table, columns)` +
  the header component `SortTh`), `fresh.js` (the playground keeps profile tabs alive in
  `<keep-alive>`, so `onMounted` runs once: EVERY tab reads its data through `useFreshData`, on
  set-up, on each later visit and on window focus/visibility while shown; a refresh replaces
  only shown data, never form or dialog input; a poll runs only while its tab is active),
  `sort.js` (typed stable comparator, header toggling, the sort kept per table in localStorage;
  every table header is a `<sort-th>`, checked by `ui/tests/sort.test.mjs`; `useSort(table,
  columns, defaultSort)`: Databases, Aliases, MCP default to name), `toasts.js` (round 4 B-UI:
  the ONE notice system of every tab: `shared.js` `notify(notice)` pushes a toast into a stack
  mounted once on `document.body` (`ToastStack`, its own `createApp`), fixed bottom right (B-UIFIX:
  never over the top bar or the tab bar; width capped at 340 px; a long help clamped to 3 lines
  with a "More" expander, `isLongHelp`; looks like the playground's system journal notices), ok 6 s, warn 12 s, error until
  closed, paused on hover/focus, at most 5, repeats counted; NO tab renders a notice banner any
  more, checked by `ui/tests/toasts.test.mjs`; persistent states (a read error, the unclean
  shutdown, the MCP status, a token shown once) stay inline), `menu.js` (the row actions menu
  `shared.js` `ActionMenu` (`items` `{key, text, title, enabled, danger, href | run}` or
  `{separator}`): WAI-ARIA menu button, fixed to the viewport (`menuPlacement`: upward near the
  bottom), outside click/scroll/resize/Escape close it; the Databases table has one per row
  holding every row action and the OpenAPI link: no horizontal scroll down to ~1024 px) and
  `shared.js` `FormError` (a form's error and problems right UNDER its submit button: every admin
  form and the sign-in form; checked by `ui/tests/toasts.test.mjs`); uploads (Import tab, Models tab's "A file from this
  computer": `modelUploadRequest` + `uploadImport({kind: 'model'})`) go through XHR with
  progress, sized against `api.info.host.uploadBytes` first; `server-ui.css` (`srv-*` classes on the playground's
  theme tokens and generic classes). Modules import ONLY `vue` and `./<own file>.js` (checked by
  `ui/tests/profile.test.mjs`) and fetch `/api/admin/*` themselves with the host headers. A new
  file in `ui/` goes into `ui_assets::FILES`. Tests: `ui/tests/*.test.mjs` (node:http mocks of
  the admin API; the profile checked with the engine's `validateProfile` from
  `GRAPHERSAL_PLAYGROUND`, else the registry copy of graphersal-serve's `ui/` found through
  `cargo metadata`; skipped when absent, failed on CI).
- Docs: the mdBook in `book/` is the ONE place of the documentation (the contract for readers
  who did not follow the code; keep it in sync with it; its structure and style rules:
  `book/CLAUDE.md`; not published, no CI: `mdbook build book`). `README.md` is only an overview,
  the quick start and a link to the book. Pages code and tests cite: `book/src/reference/
  admin_api.md` (every admin route, checked against `admin_api.rs`; the web UI's contract),
  `book/src/reference/server_directory.md` (every file of the directory, the "Concurrent
  changes" inventory), `book/src/reference/configuration.md` (every key of `server.toml`,
  `access.toml`, `aliases.toml`, `mcp.toml`, `database.toml`), `book/src/guide/mcp.md`,
  `book/src/guide/access.md`, `book/src/guide/security.md`, `book/src/guide/http_clients.md`,
  `book/src/troubleshooting.md` (task recipes, incl. "A server was killed by force"),
  `book/src/cheat_sheet.md` (every console command: a new command or option goes there and into
  `reference/console.md`), and the release notes `book/src/release_notes/<version>.md`
  (release.yml uses the page of the tagged version as the release text; 0.2.0 = the access
  series, on Graphersal 0.8.0; 0.3.0 = the first public release, on Graphersal 0.9.0; a new
  version's page starts as a draft extended by every change).
- `scripts/smoke.sh`: the end-to-end smoke test (bash + curl + python3 only): builds, inits a
  temp server dir, serves on `127.0.0.1:0` (URL from `server.run`), walks create/load/execute/
  alias/read-only/backup/restore/export/upload import/promote + layer/models/MCP/unload/delete,
  access (anonymous refused, login/logout), users and a read-only user (reads, cannot write, does
  not see another database, "my databases"), the reserved name `jobs`, the inbox, SIGTERM stop,
  a restart (a browser cookie and an MCP session with its selection survive);
  `ok <step>` / `FAIL <step>: why` lines, exit 1 on failure, the server killed in a trap
  (`SMOKE_BIN` skips the build, `SMOKE_VERSION` checks `--version` and `/api/info`,
  `SMOKE_KEEP=1` keeps the temp dir). Extend it with a new user-visible feature, AND port the step
  to `scripts/smoke.ps1` (its PowerShell port for Windows, the same steps in the same order; its
  header lists the deliberate differences: clean stop through `stop --dir`, no owner check, no
  h2c request).
- Release and CI (`.github/workflows/`): `release.yml` (tag `v<version>` = workspace version,
  or `gh workflow run Release -f ref=<ref> [-f dry_run=false]`): 4 platforms (linux-x86_64 and
  linux-aarch64 static musl, macos-arm64, windows-x86_64; no macOS x86_64, user decision
  2026-11-26), `dist` profile, smoke test per platform, archives
  `graphersal-server-v<version>-<platform>.tar.gz|.zip`, `SHA256SUMS`, installers test,
  `cargo publish --dry-run`, attestations (public repo), DRAFT release only. `install.sh` /
  `install.ps1` at the root (env prefix `GRAPHERSAL_SERVER_`, `--base-url` for tests): keep the
  platform list in step with the release matrix and the binstall overrides. `ci.yml` runs BY HAND
  only (user decision 2026-11-26: `gh workflow run CI -f ref=<branch>`): fmt, Linux + Windows
  clippy/test/node/smoke, `cargo deny check` (`deny.toml`), MSRV 1.88.
- Docker (round R3): `Dockerfile` (multi-stage: `rust:<RUST_VERSION>-alpine` build, `--locked
  --profile dist`, thin LTO unless `--build-arg LTO=fat`, BuildKit cache mounts; runtime
  `alpine` for the entry point's shell and busybox `wget` of the `HEALTHCHECK` on `/readyz`;
  user `graphersal` uid/gid 65532; `/data` 0700 = the server directory, `VOLUME`; `EXPOSE 8484`;
  OCI labels), `docker/entrypoint.sh` (no arguments or only options: `init /data --set
  listen=0.0.0.0:$PORT` when `/data` is missing or empty, then `exec serve --dir /data --listen
  0.0.0.0:$PORT` with the options; other arguments: `exec graphersal-server "$@"`; env
  `GRAPHERSAL_SERVER_DIR`, `GRAPHERSAL_SERVER_PORT`), `.dockerignore` (an allowlist: manifest,
  lock, `crates/`, licence files, the entry point). Book: `book/src/guide/docker.md`. A change to
  the CLI that the entry point relies on (`init --set`, `serve --listen`, the owner check) checks
  that page and the entry point too.
- Public-repository files (round R2): `CONTRIBUTING.md` (the human version of these rules: keep
  its commands in step with the Commands section), `SECURITY.md`, `CLA.md` (Graphersal's CLA,
  its "Project" also names this repository; never edit it without the user),
  `.github/workflows/cla.yml` (the CLA check, as graphersal's; signatures on the branch
  `cla-signatures`), `TRADEMARKS.md` (follows Graphersal's policy), `.github/ISSUE_TEMPLATE/`
  and `.github/pull_request_template.md`. Nothing tracked names a private path, a private file or
  a private repository: the history is public.
- Tests: unit tests beside the code (`test_support::TempDir`), end-to-end tests in
  `crates/graphersal-server/tests/server_tests.rs` (spawns the binary, port 0, SIGTERM; the
  `db`/`import`/`models` console offline and through a running server).

## Commands

On the maintainer's machine every build/test/server runs under the local watchdog script
`$WD` (RSS ceilings; not part of this repository: without it, run the commands directly).

```bash
$WD cargo test
cargo fmt --all --check                      # rustfmt.toml: max_width = 100
node --test crates/graphersal-server/ui/tests/*.test.mjs   # the web UI's modules
$WD cargo clippy --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p graphersal-server
WD_TIMEOUT=600 $WD scripts/smoke.sh   # end to end, ok/FAIL lines
mdbook build book                            # the book (book/CLAUDE.md); must build without warnings
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
- Concurrent changes (B-CONCUR): a writer of a server file takes that file's lock and holds it
  across read, change and write; memory changes only after the write succeeded; never take a
  lock earlier in the documented order while holding a later one (loader state, loader flips,
  registry, access, session tables; models/naming/MCP settings/`server.open` writer/recovery
  report locks are leaves; the `server.open` writer reads the loader's state inside its own
  lock, so it is never taken while the loader's state lock is held). A new runtime writer goes
  into the inventory of `book/src/reference/server_directory.md`; a new editable shared object gets a revision.
- Tokens and session ids: only hashes at rest, never logged (`Secret`'s `Debug` is redacted;
  audit events show `token_last4` only).
- Access is enforced by the router (B3, module docs of `router.rs`): authentication right after
  the Host check (wrong credentials `401 {"error","login":"/"}` + `WWW-Authenticate: Bearer`,
  a refused cookie cleared; never downgraded to anonymous), identity set on the request;
  `/api/admin/...` (Runs included) admin only (anonymous 401, other users 404 "unknown
  endpoint"), the admin core audits with `Access::audit_as(principal)`; `/db/<name>/...` by
  granted level (none: anonymous 401 for every name, a page GET 303 `/?next=`, logged in = the
  unknown-name 404; read = `Request::mark_read_only` on the forwarded request; the database
  mode applies in its context); `/mcp` per principal (`mcp::Directory`); `POST /api/login
  {"token"}` (`gs_session` cookie `Max-Age=<the effective session lifetime, else 400 days>;
  HttpOnly; SameSite=Strict; Path=/`, `; Secure` with `public_scheme = "https"`;
  `router::Modes`) and `POST /api/logout`
  run before authentication; root `/api/info` minimal for anonymous (`server`, `protocol`,
  `dev`, `experimental`, `uiProfile`, `principal`, `login`), `principal` for everyone; the UI
  files at `/` and `/server-ui/` are public. The console sends the run's operator token
  (`client::request(address, token, ..)`). Tests: `router_access_tests.rs`; the HTTP tests go
  as the admin through `test_support::operator_token(address)` (unit) and `operator_token` /
  `as_admin` in `tests/server_tests.rs` (the run file of a live test directory). Default listen `127.0.0.1:8484`, a warning for any
  non-loopback address (once, in the console's start block; `serve::start` itself only logs a
  debug line), `graphersal_serve::HostCheck` on every request but the probes (with
  `[http] allowed_hosts`). `[access] enabled = false` (B6): `router::open_principal` replaces
  the refusals: valid admin credentials keep their identity, everything else is `everyone`
  (tests `router_access_tests.rs` `access_off_*`, `the_login_cookie_*`, `allowed_hosts_*`).
- Security hardening (SEC-FIX; book `guide/security.md`): `everyone` is refused shutdown,
  server backup, `POST users` and `POST users/<n>/token` (`admin_api::issues_credentials`);
  `/mcp` refuses anonymous without an anonymous grant (`Access::anonymous_has_grant`), every
  `initialize` first runs `mcp::make_room` (per-identity quota; plain ids of this run's sessions
  kept in `McpUsage`, redacted `Debug`), anonymous MCP bodies <= 1 MiB (`mcp::body_limit`);
  `X-Graphersal-Client` is forwarded as sent: graphersal-serve 0.9.0 keys runs and change-feed
  entries by the identity the router sets on the forwarded request (S1 removed the server's own
  HMAC rewrite and feed masking); the login body <= 4 KiB (`router::MAX_LOGIN_BODY`); `serve`
  gives bodies of requests that may run without credentials
  (`Router::may_run_without_credentials`) `UNAUTHENTICATED_BODY_DEADLINE` (10 s) and every
  drain 5 s, answers the probes on the async task (also over the cap), bounds the blocking pool
  (`serve::blocking_threads`); every answer gets `X-Frame-Options: DENY` and
  `router::CONTENT_SECURITY_POLICY` (`framing_refused`); a console token without `--server`
  uses `server.run` only via `RunInfo::read_live` (lock held, owner's file); `--token-file`
  (`main.rs` `token_args`) and a warning for `--token`. Tests: `router_access_tests.rs` (end),
  `serve_tests.rs` (end), `server_tests.rs` `a_token_*`.
