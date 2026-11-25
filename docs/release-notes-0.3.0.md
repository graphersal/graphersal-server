# graphersal-server 0.3.0 (unreleased)

**EXPERIMENTAL.** Everything here (commands, files, routes, answers) may change without notice
in later versions. This is a draft: it grows with every change until the release.

**Upgrading from 0.2.0:** `serve` and the console now refuse a server directory that is not
private to its owner. A directory made by 0.2.0 usually has `sessions/` (and maybe the
directory itself) open to group or others: run `graphersal-server init DIR` once as the
directory's owner; it tightens the modes and lists them under `owner only`. The error of a
refused command names the exact `chmod`/`chown` too.

## Safe concurrent changes

Many browsers (and scripts, and the console) may change the server at the same time; the files
stay whole and every acknowledged change stays
([`server-dir.md`](server-dir.md#concurrent-changes)).

- **Atomic writes with their own temporary file.** Every configuration file (`access.toml`,
  `aliases.toml`, `database.toml`, `server.run`, the session files, `models/models.toml`,
  backup sidecars) is written to a temporary file of its own (`<file>.tmp-<pid>-<n>`; 0.2 used
  one fixed `<file>.tmp` per file), flushed to the disk, renamed into place, and on unix the
  directory is flushed too (the new file survives a power loss). Temporary files a crash left
  behind are removed at the next start (only names the server makes).
- **Locks held across read, change and write; memory only after the write.** Fixed on the way:
  the grants of a deleted database or a removed alias are now dropped in the same step as the
  name (before, a new database or alias of that name made in between could lose the grants it
  had just been given); a grant is written while its database or alias is known to exist (it
  could land on a name deleted at that moment); `user admin NAME on|off` reads and changes the
  roles in one step (a concurrent role change is no longer undone); the `grantsRemoved` count of
  a user removal is taken in the same step. The lock order is documented.
- **Revisions against lost updates.** Users and grants (`access.toml`), aliases
  (`aliases.toml`) and each database's settings (`database.toml`) carry a `revision`: `GET
  users`, `GET grants`, `GET aliases` (also as `ETag`), `aliasesRevision` of `GET databases`,
  and `revision` in every database entry. A change may send the revision it is based on (body
  field `revision` or `If-Match`); when someone else changed the object meanwhile it is refused
  with `409 {"error", "help": "changed by someone else: reload", "revision": <current>}` and
  nothing changes. Without a revision a change is applied as before (the console, scripts,
  older clients). Checked: every users and grants change, alias set/flip/rename/remove, read-only
  and auto-load ([`admin-api.md`](admin-api.md#revisions-concurrent-changes)).
- **Web UI:** the Users tab, the alias dialogs and remove, and the read-only and auto-load
  toggles send the revision of what they show; on a `409` the tab shows a warning (the server's
  text and help, and that the tab now shows the current state) and reads its data again.
- The library: `revision` (`of`, `check`, `from_if_match`, `etag`, `CONFLICT_HELP`),
  `ServerError::Conflict`, `Admin::expecting`, `Access::edit` (`AccessEdit`, with `set_admin`),
  `Access::revision`, `AccessFile::revision`, `DatabaseEntry::revision`,
  `Registry::aliases_with_revision`, `aliases_revision`, `alias_remove_with`, `delete_with`,
  `set_read_only_expecting`, `set_auto_load_expecting`, `Loader::set_read_only_expecting`,
  `ServerDir::remove_stale_temps`; `Registry::alias_set_with` and `alias_rename_with` take the
  expected revision.

## The server directory is the boundary

The OS user owning the server directory is the operator (PostgreSQL's peer-authentication
model): whoever can write the directory could edit `access.toml` or read the operator token
anyway, so the console needs no credentials there. 0.3.0 enforces that the directory really is
private ([`access.md`](access.md#the-security-model)):

- **`init`** creates the directory with mode `0700`, `sessions/` with `0700` and the secret
  files (`access.toml`, `server.run`, `sessions/*.json`) with `0600`; every later write keeps
  these modes. On an existing directory owned by the current user it tightens the modes it
  finds too open (`owner only` in its output).
- **`serve` and every console command** (`db`, `import`, `models`, `user`; offline and through
  a running server with the operator token) refuse, with exit code 1, a directory or file
  owned by another OS user (root on another user's directory included), a directory,
  `server.toml` or `aliases.toml` writable by group or others, and a secret (`access.toml`,
  `server.run`, `sessions/` and its files) readable or writable by group or others. The error
  lists every problem and the command fixing it (`chmod 600 ...`, `sudo -u OWNER ...`,
  `sudo chown -R ...`).
- **Windows:** no check (it would need the Win32 security API); the directory's ACL is the
  boundary.
- **Audit:** the operator's records name the OS user, `actor="operator (os:<name>)"`: the
  console's own user offline, and the user the server runs as for the operator token through
  the server. An offline console command logs its audit lines on stderr when `RUST_LOG` is set
  (`RUST_LOG=graphersal_server::audit=info`).
- **`--token TOKEN`** (or `GRAPHERSAL_SERVER_TOKEN`) for `db`, `import`, `models` and `user`:
  the command goes to the running server with that user's admin token (address `--server
  ADDRESS`, else the directory's `server.run`) and touches no file of the directory, so the
  permission checks do not apply; the audit names the token's user. Without a running server
  it is refused. `--server` without a token is a usage error.

## start, stop and status

A server in the background without hunting for its pid:

- **`graphersal-server start --dir DIR [serve options] [--timeout SECS]`** spawns `serve` as a
  detached process (Unix: a new session, no controlling terminal; Windows: `DETACHED_PROCESS |
  CREATE_NEW_PROCESS_GROUP`, no fork), stdin closed, stdout and stderr appended to
  `DIR/logs/server.log` (new: `logs/` mode `0700`, the log `0600`). It waits until the server
  answers `GET /readyz` (at most SECS, default 60), then prints the usual start block on
  stderr and the address line on stdout, and exits 0. A directory without `access.toml` gets
  its first admin from `start` itself, so the token is printed once on the terminal and never
  written into the log. Refused while a server runs on DIR (naming its pid and URL); a server
  that exits before it is ready, or is not ready in time, is exit code 1 with the last lines of
  its log (one still starting after the timeout keeps running, and the error says so).
- **`graphersal-server stop --dir DIR [--timeout SECS]`** asks the running server to stop
  through the new operator-only route `POST /api/admin/shutdown` (with the operator token of
  `server.run`); the server answers `202` and stops exactly as on Ctrl+C. On Unix, when the
  API cannot be reached, it falls back to SIGTERM (the same clean stop). It waits until
  `server.lock` is released (default 60 s; after that the error names `kill`/`taskkill`) and
  prints `stopped`. Without a server it prints `not running` and exits **0** (as LSB init
  scripts do for stopping a stopped service); a stale `server.run` a crashed server left is
  removed and named.
- **`graphersal-server status --dir DIR [--json]`**: running or stopped, the pid, address,
  version, uptime, databases (count, loaded ones by name), memory (the ceiling; the live heap
  is not measured by this build) and the log; a stale `server.run` is reported, not removed.
  Exit code **0** running, **3** stopped (the LSB convention).
- `stop` and `status` also take `--token TOKEN [--server ADDRESS]` like the other console
  commands.
- New admin routes: `GET /api/admin/server` (the server's facts, any admin) and `POST
  /api/admin/shutdown` (the operator or an admin user; `403` for `everyone` with access
  control off; audited). `server.run` gains `started_at_ms`.
- The library: `ServeOptions::with_stop_hook(StopHook)` (what the shutdown route calls; `serve`
  routes it into its Ctrl+C channel), `graphersal_server::daemon` (`start`, `stop`, `status`
  and their `--token` forms), `ServerError::Service`, `AuditEvent::ShutdownRequested`.
- The `serve` start block's `stop` row names `graphersal-server stop --dir DIR`.
- Under systemd or launchd run `serve` in the foreground: examples in
  [`server-dir.md`](server-dir.md#running-as-a-service).

## Shorter names for new databases, aliases and users

- **A NEW name** (create, clone, import by upload, path or inbox, restore into a new name, a
  new alias, alias rename, user add; console, admin API and web UI alike) has **at most 32
  characters** (was 63), **ends with a letter or digit** (no trailing `-`, `_` or `.`) and
  holds **no `..`**; the rest is unchanged (`[a-z0-9]` first, `[a-z0-9._-]` inside, no Windows
  device names, `jobs` reserved for databases and aliases). A refusal is `400` (console: exit
  code 1) with `invalid name "<name>": <reason>`, one reason per check.
- **Existing names keep working:** a database, alias, user or grant named by 0.2.0 under the
  older rule (up to 63 characters, a trailing `-` or `_`, `..` inside) is still loaded,
  served under `/db/<name>/`, looked up, granted, flipped (an alias) and backed up; its backup
  restores over it and, after the database was deleted, under its own name again (which a
  `jobs` database made before 0.2.0 now may too). Only a restore into ANOTHER name takes the
  new rule.
- **Web UI:** every field where a new database, alias or user name is typed (create, clone,
  import, restore into a new database, alias, alias rename, add user) shows the rule under it
  (*Up to 32 characters: lower-case letters, digits, '.', '_' and '-'; starts and ends with a
  letter or digit.*) and checks it before sending, with the server's own error text. The name
  suggested from an import file is always a valid new name (cut to 32 characters, `..` made
  one dot, no trailing `-`/`_`/`.`), and the default name of a restore into a new database
  (`<name>-restored`) is shortened to fit.
- **Windows:** the short names keep paths under the server directory short; keep the server
  directory itself short too (`MAX_PATH` = 260 characters unless long paths are enabled;
  [`server-dir.md`](server-dir.md#databasesname)).
- The library: `names::MAX_NAME_LEN` is 32; new `names::MAX_EXISTING_NAME_LEN` (63) and
  `names::validate_new_user_name`; `validate_new_name` checks the new rule; `validate_name`
  stays the rule of existing names; `Registry::prepare_tmp_former_name` (a restore under the
  deleted database's own name).

## Grants on aliases

0.2.0 refused a grant on an alias (`409`: grant the database). 0.3.0 takes it
([`access.md`](access.md#grants-on-aliases)):

- **Through the alias only:** a grant on an alias reaches the database the alias names NOW, but
  only when the request names the alias: `/db/<alias>/...`, MCP `use_graph <alias>` (or
  `"graph": "<alias>"`), the alias's row in the server-wide `/mcp` `list_graphs` and in "My
  databases". The real name stays hidden without a grant of its own (`404`, `401` for
  anonymous; not listed as a database). The level through the alias is the higher of the grants
  on the alias and on its database, capped by the database's mode. `*` grants are unchanged.
- **Flip:** pointing the alias at another database moves the access at once, also for open MCP
  sessions that selected the alias.
- **Rename:** `db alias rename OLD NEW` (and `POST /api/admin/aliases/<alias>/rename`) moves the
  grants on OLD to NEW in `access.toml` (answer `grantsMoved`; the console prints `grants moved`
  when there were any; audited as `GrantsMoved`).
- **Remove:** `db alias remove ALIAS` (and `DELETE /api/admin/aliases/<alias>`) removes the
  grants on the alias too (answer `grantsRemoved`; each audited as a revoked grant), so a later
  alias or database of that name inherits nothing.
- **Every grant listing marks the name:** the admin API's grant objects (in `GET users`, `GET
  grants`, `POST grants` and each user's `grants`) gain `kind` (`database`, `alias`, `all` for
  `*`, `missing` for a name that names nothing now) and `target` (the alias's current
  database, else `null`); `GET /api/databases` rows gain `kind` (`database` or `alias`).
  `graphersal-server user list` shows `prod → prod-2026-11 (alias)` and `gone (missing)`, and
  `user grant` on an alias says which database it reaches now. Web UI: the Users tab's grant
  tables show `prod → prod-2026-11 (alias)`, its database picker offers the aliases too (marked,
  after the databases), and "My databases" shows `alias → <database>` or `database` beside each
  name.
- **Grants never outlive their name** ([`access.md`](access.md#grants-never-outlive-their-name)).
  0.2.0 (and the first 0.3.0 builds) kept the grants of a deleted database in `access.toml`, so
  they applied again to whatever took the name later: a new database, an alias, a rename onto
  it. Now:
  - `db delete` (and `DELETE /api/admin/databases/<name>`, the Databases tab) removes every
    grant on the database, each audited as a revoked grant; the answer gains `grantsRemoved`,
    the console prints `grants removed: N (a restore does not bring them back)` when there were
    any, and the web UI's notice names them (its delete confirmation says so too).
  - A restore does not bring grants back (the Backups tab's restore form and the console's
    restore note say so); a restore over an existing database keeps its grants.
  - Grants still left on a name (from 0.2.0 or a failed rewrite of `access.toml`) are removed
    first when a new database or alias takes the name (create, clone, import, a restore into a
    new or a deleted database's name, a new alias, the new name of an alias rename), audited
    each; the answer gains `leftoverGrantsRemoved` (only when > 0), the console prints a
    `warning:` line and the web UI a warning notice. A failure to rewrite `access.toml` there
    refuses the operation. Until then such grants stay listed as `missing` and revocable.
  - The library: `Deleted::grants`, `Imported::leftover_grants`, `Restored::leftover_grants`,
    `Renamed::leftover`; `Admin::create_reported`, `Admin::clone_reported` (the entry and the
    leftover count); `Admin::alias_set` answers the leftover count (was `()`);
    `Registry::alias_set_with`, `Registry::alias_rename_with` (a `claim` run under the
    registry's write lock).
- The library: `AccessFile::granted_level_via`, `grants_on`, `rename_grants`, `drop_grants`;
  `Access::reach`, `granted_level_via`, `rename_grants`, `drop_grants` (`level_in` counts an
  alias's own grants); `Admin::alias_remove` answers the number of grants removed;
  `Renamed::grants`; `AuditEvent::GrantsMoved`.

## The web UI shows the current state, sorts every table, explains bases

- **Fresh data on every visit.** Every tab of every UI profile (the admin tabs, "My databases",
  the sign-in page's open databases) reads its data again each time it is shown, and when the
  window gets the focus back or the page becomes visible while it is shown. Before, the
  playground kept a tab alive with the data of its first visit: a user added with
  `graphersal-server user add` (or in another browser) did not appear in Users until the page
  was reloaded. A refresh replaces only the shown data; an open dialog or form keeps its input.
  The Runs tab's 2-second poll stops while another tab is shown.
- **`Cache-Control: no-store` on every answer**, exactly once: the root, user and admin API
  (also refusals, unknown routes and redirects), each database's API and the UI files. Answers
  that named it twice (`GET /api/databases`, a `401`, login and logout) name it once now.
- **Sortable columns** on every table (databases, aliases, users, grants, backups, the import
  inbox, models, databases waiting for a model, runs, "My databases"): click a header for
  ascending, again for descending (indicator and `aria-sort`); text in natural order, sizes,
  durations and ids by number, dates by time, missing values last, stable; the choice is kept
  per table in the browser (localStorage). The import inbox rows carry `bytes` for it.
- **Bases and layers explained.** The confirmation of "Promote to base…" and its tooltip say
  what a base is (a frozen, read-only database that layers are built over; each layer stores
  only its own changes), that promotion is one way and forever, and when to use it; the
  Databases tab's Kind column and the Backups tab's Kind column explain each kind in a tooltip,
  and a "Bases and layers" box under the database table sums it up.
