# Admin API (EXPERIMENTAL)

The admin HTTP API of `graphersal-server`, below `/api/admin/`. The server's web UI at `/` (the
`server` UI profile, `crates/graphersal-server/ui/`, model `ui/admin-api.js`) and the console (`graphersal-server db ...`, when a server runs on the directory) are its
clients. Both the API and the console's offline mode call the same admin core, so the answers
below are also what `graphersal-server db ... --json` (and `user ... --json`) prints.

The MCP endpoints (`/mcp`, `/db/<name>/mcp`) are documented in [`mcp.md`](mcp.md), the server
directory and its files (`server.toml`, `database.toml`, `models/`, `backups/`) in
[`server-dir.md`](server-dir.md), the access model (principals, grants, levels, tokens, the
401/404/303 rules) in [`access.md`](access.md).

The admin API is for admins only (a user with the `admin` role in `access.toml`, or the
console with the run's operator token from `server.run`): send `Authorization: Bearer
<token>` or the browser session cookie. Without credentials it answers `401 {"error",
"login": "/"}`; a logged-in user without the admin role gets `404` as if the API did not
exist; a token nobody holds is `401`. Every request is also checked by `Host` and `Origin` (DNS
rebinding, cross-site pages): send `Host: <the server's address>`; a browser page of another
origin is refused with `403`.

Browser login (beside the admin API): `POST /api/login {"token": "<user token>"}` answers
`200 {"principal": {"id", "kind", "admin"}}` with the cookie `gs_session` (`Max-Age=<[sessions]
lifetime_secs, else 400 days>; HttpOnly; SameSite=Strict; Path=/`, plus `Secure` when
`server.toml [http] public_scheme = "https"`), `401` for a token nobody holds; `POST /api/logout` ends the session
of the request's cookie and clears it (`200 {"loggedOut": bool}`). Browser sessions survive a
server restart (kept in `sessions/browser.json`, hashes only) and last until the logout, a
change of the user's token, the user's removal, or the `[sessions]` limits of `server.toml`
(none by default; `server-dir.md`); an ended session's cookie answers `401` and is cleared. `GET /api/info` names the
request's `principal` (`{id, kind, admin}`); for anonymous it answers only `server`,
`protocol`, `dev`, `experimental`, `uiProfile`, `principal` and `login: "/"`. Its `uiProfile`
follows the principal: an admin gets the management tabs and Users, a user without the admin
role only "My databases" (`mine`), anonymous only "Sign in" (`login`). The login page follows
the `next` of its address after a login only when it is a path from the server root (starts
with `/`, not `//`, no backslash, space or control character); otherwise it loads `/`.

### My databases (any principal)

`GET /api/databases` (beside the admin API, for every principal, anonymous included) answers
the databases and aliases the caller may reach, sorted by name:

```json
{"databases": [{"name": "acme", "state": "loaded", "level": "write", "readOnly": false,
  "description": "", "playground": "/db/acme/", "kind": "database"},
  {"name": "cur", "state": "loaded", "level": "read", "readOnly": true, "description": "",
  "playground": "/db/cur/", "kind": "alias", "aliasOf": "acme-2026"}]}
```

`level` is the caller's effective level (its highest grant, capped by the database's mode: a
read-only database or a base is `read`; an admin: `write`), `readOnly` is `level == "read"`,
`state` is the entry's state without the admin's `reason`, `playground` the path of its
playground from the server root, `kind` `database` or `alias`, `aliasOf` (alias rows) the
database the alias names now. A database the caller has no grant on is never listed; an alias is
listed when the caller has a grant on its database or on the alias itself (a grant on an alias
reaches the alias's current database only through the alias, see `docs/access.md` "Grants on
aliases"); anonymous without anonymous grants
gets an empty list (`200`). Wrong credentials are `401`; any method but `GET`/`HEAD` is `405`.

## Conventions

- Request bodies are JSON objects with `Content-Type: application/json` (else `415`), at most
  16 MiB (else `413`); the one exception is an import upload (the file itself as the body, see
  Import). Unknown fields are refused (`400`), so a typo is never ignored.
- Answers are JSON. An error is `{"error": "<sentence>", "help": "<what to do>"?}`.
- Every answer carries `Cache-Control: no-store` (exactly once; the export download too): no
  browser or proxy keeps an answer, so a client always reads the current state.
- Shared objects carry a `revision` (users and grants, aliases, each database's settings): a
  change may name the revision it was based on and is then refused with `409` when someone else
  changed the object since ([Revisions](#revisions-concurrent-changes)). Without a revision a
  change is applied as before.
- Every `GET` route but the export also answers `HEAD`.
- The actions take **real database names**; an alias is not followed (`404`). Only
  `GET /api/admin/databases/<name>` also answers an alias. Names are case-insensitive and
  answered in lower case; databases and aliases share one namespace. A NEW name (create,
  clone, import, restore under a new name, alias set of a new alias, alias rename, user add)
  has at most 32 characters, `[a-z0-9]` first, `[a-z0-9._-]` inside, a letter or digit last,
  no `..` and no Windows device name; otherwise `400` `invalid name "<name>": <reason>` (the
  web UI checks the same rule with the same texts before sending). A new database or alias
  cannot take the reserved name `jobs` either: `400` with help (the default backup root
  `backups/jobs/` would collide with that database's admin backups). A name made before 0.3.0
  under the older rule `[a-z0-9][a-z0-9._-]{0,62}` (not ending in `.`) keeps working in every
  route: lookups, `/db/<name>/`, grants, flipping its alias, restoring a backup under its own
  name.
- Status codes: `200`/`201`/`202` success; `400` invalid name, body or schema; `404` no such
  database, alias, model or endpoint; `405` method not allowed on an existing endpoint; `409` a rule
  refuses it (name taken, database loaded, named by an alias, base of layers, a base's
  read-only, a failed database, a layer without its base); `413`/`415` body (`415` also: an
  import source of no known format); `500` a Store or file operation failed;
  `503` the server is starting or stopping (retry, `Retry-After: 1`).

## Entry

The object describing one database (listing rows, and the answer of most actions):

| Field | Type | When | Meaning |
|---|---|---|---|
| `name` | string | always | the database's name (for an alias row: the alias) |
| `state` | string | always | `unloaded`, `loading`, `loaded`, `needs-model`, `failed` |
| `kind` | string | config readable | `plain` or `layer` |
| `role` | string | config readable | `normal` or `base` |
| `readOnly` | bool | config readable | served read-only (a base: always `true`) |
| `autoLoad` | bool | config readable | loaded at server start |
| `description` | string | config readable | free text |
| `base` | string | layers | the base database |
| `created` | string | when known | UTC, RFC 3339 |
| `reason` | string | `failed`, `needs-model` | why |
| `model` | string | `needs-model` | the missing model's repository |
| `aliasOf` | string | alias rows | the database the alias names (other fields are that database's) |
| `import` | object | import answers | see Import |
| `sizeBytes` | number | listing rows | the database's directory on disk (every file once: Store snapshots, journal, models, settings; a model blob linked from `models/` counts in full in each database holding it, so the sum over databases exceeds the disk use); measured at most once per 5 s per database |
| `memoryBytes` | number \| null | listing rows | a loaded database's graph in memory: the engine's footprint (`memory_footprint_bytes`: the graph's own data, cached by the engine per commit; loaded embedding models and workload statistics not included). A plain database: its graph; a layer: its own changes; a base loaded for reading: the frozen base graph. `null` when it is not loaded (or a writer held it since its load, so it was never measured: a busy graph answers its last figure) |
| `sharedMemoryBytes` | number | listing rows of a loaded layer | its base's memory, shared with the base and every other layer over it (not part of `memoryBytes`) |
| `pooled` | bool | listing rows of a pooled base | `true`: the base is held in memory in the shared pool (by its loaded layers or itself) |
| `pooledBy` | [string] | with `pooled` | the loaded layers holding it (empty: only the base itself, loaded for reading) |
| `linkWarning` | string | listing rows | some of its model blobs are copies, not hard links to `models/` (the file system has no links there: correct, no disk saving; unix only) |
| `revision` | string | config readable | the revision of its settings (`database.toml`): send it with a read-only or auto-load change ([Revisions](#revisions-concurrent-changes)) |

Listing rows are the answers of `GET /api/admin/databases` and `GET /api/admin/databases/<name>`.
A listing never waits for a graph: a database a writer holds answers its last measured memory.

A `failed` database whose `database.toml` cannot be read has only `name`, `state`, `reason`.

## Routes

| Method | Path | Body | Answer |
|---|---|---|---|
| GET | `/api/admin/databases` | | `200 {"databases": [entry...], "aliases": {alias: database}, "aliasesRevision", "problems": [string...]}` (rows sorted by name, alias rows included) |
| POST | `/api/admin/databases` | `{"name", "kind"?, "role"?, "base"?, "schema"?, "description"?, "autoLoad"?, "readOnly"?}` | `201` entry (state `unloaded`) |
| GET | `/api/admin/databases/<name>` | | `200` entry (`<name>` may be an alias: an alias row); `ETag`: its `revision` |
| DELETE | `/api/admin/databases/<name>` | | `200 {"name", "trash": "trash/<name>-<UTC time>", "grantsRemoved"}` (every grant on the database goes with it; a restore does not bring them back) |
| POST | `/api/admin/databases/<name>/clone` | `{"name"}` | `201` entry of the new database |
| POST | `/api/admin/databases/<name>/load` | | `202` entry + `"load": "started"` or `"already-loading"`; `200` entry + `"load": "already-loaded"` |
| POST | `/api/admin/databases/<name>/unload` | | `200` entry + `"unloaded": true` (was loaded) or `false` (was not) |
| POST | `/api/admin/databases/<name>/promote` | | `200` entry (`role: base`, `readOnly: true`; one way) |
| PUT | `/api/admin/databases/<name>/read-only` | `{"readOnly": bool, "revision"?}` | `200` entry (with its new `revision`) |
| PUT | `/api/admin/databases/<name>/auto-load` | `{"autoLoad": bool, "revision"?}` | `200` entry (with its new `revision`) |
| POST | `/api/admin/databases/<name>/backup` | | `201` backup |
| GET | `/api/admin/databases/<name>/backups` | | `200 {"database", "backups": [backup...]}` (oldest first; also for the name of a deleted database) |
| POST | `/api/admin/databases/<name>/restore` | `{"backup", "name"?}` | `201` entry + `"restoredFrom": {"database", "backup"}`, `"replaced": bool`, `"trash"` (when replaced) |
| GET | `/api/admin/databases/<name>/export?models=true\|false&materialize=true\|false` | | `200` the `.gsnap` file as a download |
| POST | `/api/admin/import?name=<n>&base=<b>&format=<f>&description=<d>&role=<normal\|base>` | the file itself (`Content-Type: application/octet-stream`, or anything but JSON) | `201` entry + `"import"` |
| POST | `/api/admin/import` | JSON `{"path" \| "inbox", "name", "base"?, "format"?, "description"?, "role"?}` | `201` entry + `"import"` |
| GET | `/api/admin/imports` | | `200 {"dir", "imports": [{"name", "kind", "directory", "bytes", "modified"}...]}`: the import inbox |
| GET | `/api/admin/models` | | `200 {"models": [model...], "missing": [{"database", "model"}...]}` |
| POST | `/api/admin/models` | `{"path"?, "repo"?, "license"?}` | `201` (from a path) or `200` (a repository) `{"added": [model...], "resolved": [database...], "stillMissing": [{"database", "model"}...], "notes": [string...]}` |
| POST | `/api/admin/models?repo=<r>&license=<l>&format=<gsnap\|tar>` | the file itself (anything but JSON): a `.gsnap` carrying model blobs or a `.tar` (or gzip-compressed `.tar.gz`/`.tgz`) of a Model2Vec model directory | `201`, the same answer |
| DELETE | `/api/admin/models/<repo or blob hash>` | | `200 {"model", "removed": [blob...], "bytes"}` |
| GET | `/api/admin/aliases` | | `200 {"aliases": {alias: database}, "revision"}` (`ETag`: the revision) |
| PUT | `/api/admin/aliases/<alias>` | `{"target", "revision"?}` | `200 {"alias", "target", "leftoverGrantsRemoved"?}` (new alias or flip) |
| DELETE | `/api/admin/aliases/<alias>` | | `200 {"alias", "removed": true, "grantsRemoved"}` (the grants on the alias go with it) |
| POST | `/api/admin/aliases/<alias>/rename` | `{"name", "revision"?}` | `200 {"alias", "renamedFrom", "target", "mcpSessionsMoved", "grantsMoved", "leftoverGrantsRemoved"?}` (see Aliases) |
| GET | `/api/admin/runs` | | `200 {"runs": [run...], "textHeadChars": 200}` (every loaded database) |
| GET | `/api/admin/databases/<name>/runs` | | `200 {"database", "loaded": bool, "runs": [run...], "textHeadChars": 200}` |
| POST | `/api/admin/databases/<name>/runs/cancel` | `{"id"}` | `200 {"database", "id", "cancelled": bool}` |
| GET | `/api/admin/users` | | `200 {"users": [user...], "grants": [grant...], "revision"}` (users by name; every grant, also of `anonymous`; `ETag`: the revision) |
| POST | `/api/admin/users` | `{"name", "admin"?, "revision"?}` | `201` user + `"token"` (shown once) |
| GET | `/api/admin/users/<name>` | | `200` user |
| DELETE | `/api/admin/users/<name>` | | `200 {"name", "removed": true, "grantsRemoved", "mcpSessionsEnded"}` |
| POST | `/api/admin/users/<name>/token` | | `200` user + `"token"` (shown once) + `"mcpSessionsEnded"` |
| DELETE | `/api/admin/users/<name>/token` | | `200` user (`hasToken: false`) + `"mcpSessionsEnded"` |
| PUT | `/api/admin/users/<name>/admin` | `{"admin": bool, "revision"?}` | `200` user |
| PUT | `/api/admin/users/<name>/roles` | `{"roles": [string...], "revision"?}` | `200` user |
| GET | `/api/admin/grants` | | `200 {"grants": [grant...], "revision"}` (`ETag`: the revision) |
| POST | `/api/admin/grants` | `{"subject", "database", "level", "revision"?}` | `200` grant (new, or its level replaced) |
| DELETE | `/api/admin/grants/<subject>/<database>` | | `200 {"subject", "database", "revoked": bool}` |
| GET | `/api/admin/mcp` | | `200` the MCP state (see MCP; `ETag`: its revision) |
| PUT | `/api/admin/mcp` | `{"enabled": bool, "revision"?}` | `200` the MCP state |
| PUT | `/api/admin/mcp/databases/<name>` | `{"exposed"?: bool, "readOnly"?: bool, "revision"?}` | `200` the MCP state + `"changed": {"name", "exposed", "readOnly"}` |
| GET | `/api/admin/server` | | `200 {"server", "version", "pid", "address", "url", "directory", "startedAtMs", "uptimeSecs", "databases": {"total", "loaded": [name...], "loading": [name...]}, "aliases", "memory": {"processBytes", "liveBytes", "databasesBytes", "machineTotalBytes", "machineAvailableBytes", "limitBytes", "limitSource", "configuredLimit", "headroomBytes", "availableBytes", "freeBytes", "freeBasis"}}` (see Server) |
| POST | `/api/admin/shutdown` | | `202 {"stopping": true, "pid"}`, then the server stops (see Server) |

### Revisions (concurrent changes)

Several admins (browsers, scripts, the console) may change the server at once. Every change is
serialized on the server and written atomically, so no change is ever half applied or torn.
What a revision adds: a change made from a page that shows an OLD state (someone else changed
the object meanwhile) is refused instead of silently overwriting the other change.

| Object | File | Its revision | Changes that check it |
|---|---|---|---|
| users and grants | `access.toml` | `revision` of `GET users` and `GET grants` (and their `ETag`) | `POST users`, `DELETE users/<n>`, `POST`/`DELETE users/<n>/token`, `PUT users/<n>/admin`, `PUT users/<n>/roles`, `POST grants`, `DELETE grants/<s>/<d>` |
| aliases | `aliases.toml` | `revision` of `GET aliases` (and its `ETag`), `aliasesRevision` of `GET databases` | `PUT aliases/<a>`, `DELETE aliases/<a>`, `POST aliases/<a>/rename` |
| a database's settings | `databases/<name>/database.toml` | `revision` of its entry (every listing row; `GET databases/<name>` also as `ETag`) | `PUT databases/<name>/read-only`, `PUT databases/<name>/auto-load` |

A revision is an opaque string (16 hex digits derived from the file's content: the same state
has the same revision, also after a restart or a console change). Send it as the body's
`revision` field, or as the `If-Match` header (`"<revision>"`; `If-Match: *` checks nothing);
the body wins when both are given. When the object has another revision now, the change is
refused and nothing changes:

```json
409 {"error": "the users and grants changed by someone else since you read them (now revision 3f9c0a17d2b84e61); nothing was changed",
     "help": "changed by someone else: reload",
     "revision": "3f9c0a17d2b84e61"}
```

Read the object again (it may already be what you wanted) and repeat the change with the new
revision. A change WITHOUT a revision is applied to the current state, as before 0.3.0: the
console's commands, scripts and older clients send none. The web UI sends the revision of what
it shows with every change of an existing object (the Users tab, alias set/flip/rename/remove,
the read-only and auto-load toggles) and on a `409` shows the notice and reads the tab's data
again. Creating a database, deleting one, load/unload, promote, backups, restores, imports and
models take no revision (they create or remove whole objects and refuse what is no longer
possible on their own).

### Create

`POST /api/admin/databases` makes a new EMPTY database:

- `kind`: `"plain"` (default) or `"layer"`; a `base` without `kind` makes a layer.
- `role`: `"normal"` (default) or `"base"`. A base is always read-only and may carry layers;
  a layer cannot be a base.
- `base`: a layer's base, which must be a `role = "base"` database (`409` otherwise).
- `schema`: a graphersal schema (JSON object) for the new graph; `400` when it does not parse,
  or when a layer's base data does not fit it.
- `description` (default `""`), `autoLoad` (default `false`), `readOnly` (default `false`).

The database is built in `tmp/` and renamed into `databases/` in one step: a failure leaves no
database (and a crash leaves only debris in `tmp/`, removed at the next start). The new Store
takes `server.toml`'s `[defaults]` (`chunk_bytes`, `damage_policy`). The answer comes when the
Store is written; the database is `unloaded`.

### Delete

Only an UNLOADED database (`409` with help `POST .../unload` otherwise). The directory moves to
`trash/<name>-<UTC time>/` (recoverable by hand). Refused (`409`) while an alias names the
database (help: remove or flip the alias) and for a base database that layers name.

Every grant on the database goes with it: `grantsRemoved` in the answer, each audited as a
revoked grant. A later database or alias of that name inherits nothing, and a restore of its
backup does not bring the grants back. If `access.toml` cannot be rewritten after the database
is gone, the answer is an error (`500`) and the grants stay, listed as `missing`, until revoked
or until the name is taken again (below).

### Grants left on a name

A grant on a name nothing holds (`kind: missing` in the grant listings: written by 0.2.0 or an
earlier 0.3.0 build, which kept a deleted database's grants, or left by a failed rewrite of
`access.toml`) never reaches what takes the name later. Create, clone, import, a restore into a
new or a deleted database's name, a new alias (`PUT aliases/<alias>`) and the new name of an
alias rename remove those grants first, before the name becomes reachable, each audited as a
revoked grant; the answer then carries `"leftoverGrantsRemoved": N` (absent when there were
none), the console prints a warning and the web UI shows a warning notice. When `access.toml`
cannot be rewritten then, the operation is refused and nothing changes. Until then such a grant
is listed and can be revoked (`DELETE grants/<subject>/<name>`).

### Clone

A fork of the database's latest state into a new database: same `kind`, `role`, `base`,
`description` and `readOnly`, `autoLoad` off, a new Store identity, independent afterwards.
Works on a loaded database (no unload needed) and on an unloaded one. Built in `tmp/` like a
create.

### Load and unload

`load` answers at once; poll `GET /api/admin/databases/<name>` until `state` is no longer
`loading` (then `loaded`, `failed` with `reason`, or `needs-model` with `model`). Loading a
layer loads its base into a shared pool first. Requests to `/db/<name>/...` of a database that
is not loaded answer `409 {"error", "help": "load it: POST /api/admin/databases/<name>/load",
"state"}`.

`unload` answers once the database is unloaded: running requests and scheduled jobs get up to
30 s, then its Store is closed. Unloading a base that loaded layers use is refused (`409`).

### Promote

`POST /api/admin/databases/<name>/promote` (no body) turns an UNLOADED plain database into a
base: `role` becomes `base` and `readOnly` `true` in its `database.toml`; its data stays as it
is, and layers can be created over it at once (`POST /api/admin/databases {"name", "base":
"<name>"}`). A layer binds to the base's identity (its graph id and last commit), and nothing
writes a base's Store after this (a base is never opened for writing: it is served as an
empty, read-only layer over the shared base), so that identity stays fixed.

**A base is frozen forever** (a design decision): promotion is one way, and no route turns a
base back into an ordinary database. To change a base's data, build a new base (clone the
database before promoting it, or import again) and rebuild the layers over the new one.

Refused with `409`: a database that is loaded, being loaded or unloaded (help: `POST
.../unload`, then promote again); a layer (help: export it with `materialize=true` and import
the file with `role=base`); a base; a `needs-model` database (help: add the model first; a
base cannot get a model later); a failed one. An unknown name is `404`.

### Read-only and auto-load

`read-only` is persisted in the database's `database.toml` and, when the database is loaded,
applied at once: writes through its API and MCP endpoint are refused with a read-only error;
a running request finishes. A base refuses `false` (`409`). `auto-load` is persisted only.

### Aliases

`PUT aliases/<alias> {"target"}` creates an alias or points it at another database (a flip):
`/db/<alias>/...` and MCP sessions that selected the alias follow at once, and so do the grants
on the alias. `DELETE` removes it and the grants on it (`{"alias", "removed": true,
"grantsRemoved"}`). An alias names a database, never another alias.

A NEW alias never inherits grants left on its name ([Grants left on a name](#grants-left-on-a-name));
a flip keeps the alias's grants.

`POST aliases/<alias>/rename {"name"}` gives the alias a new name and keeps its database, in one
atomic rewrite of `aliases.toml`. The answer: `alias` (the new name), `renamedFrom` (the old
name), `target`, `mcpSessionsMoved` (the open MCP sessions on `/mcp` that had selected the old
name: they move to the new one, and the next answer of each carries one note, see
`docs/mcp.md`), `grantsMoved` (the grants on the old name, now on the new one in `access.toml`).
Refused: an old name that is no alias (`404`; a database name is `409` with
the help: databases have no rename, point an alias at it), an invalid new name (`400`), a new
name a database, another alias or the alias itself holds (`409`, help naming the listing), a
broken `aliases.toml` (`409`). After a rename `/db/<old name>/...` (also `/db/<old>/mcp`) is
`404`; open playground pages under the old name stop working.

### Backup

`POST .../<name>/backup` writes a verified ZIP backup of the database's latest state into
`backups/<name>/<UTC time>.zip` (the time with dashes instead of colons, such as
`2026-11-22T10-00-00Z`; `-2`, `-3`, ... when that exists). Every entry is read back and checked;
damage fails the backup and nothing is kept. It works on a loaded database (its open Store; an
unload waits for it) and on an unloaded one. The archive is written in `tmp/` and renamed into
place; a sidecar `<backup>.toml` beside it records the database's `kind`, `role`, `base`,
`description` and, for a layer, its base's identity. A layer's backup holds only the layer: it
restores only over the same base.

Scheduled backup jobs of a database (its playground's Jobs panel, `/db/<name>/api/jobs/...`)
are not part of the admin API: they write only below `server.toml [jobs] backup_root`, a
job's path relative to it; an absolute path or one leaving the root is refused when the job is
defined (`server-dir.md`, "Backup jobs").

A backup object:

| Field | Type | When | Meaning |
|---|---|---|---|
| `database` | string | always | the database backed up |
| `backup` | string | always | its name (the UTC time with dashes): what `restore` takes |
| `file` | string | always | `backups/<database>/<backup>.zip`, relative to the server directory |
| `bytes` | number | always | the archive's size |
| `created` | string | sidecar | UTC, RFC 3339 |
| `kind`, `role` | string | sidecar | the database's kind and role when it was taken |
| `base` | string | layers | the base database's name when it was taken |
| `layerBase` | string | layers | the base's identity (`graph <lineage id> at commit <n> (commit time <micros>)`) |

### Restore

`POST .../<name>/restore {"backup": "<backup>", "name"?: "<new>"}` restores the backup
`<backup>` (or `<backup>.zip`) of `<name>`:

- without `name` (or `name` = `<name>`): over the database `<name>` itself, which must be
  UNLOADED (`409`, help `POST .../unload`). The new Store is built in `tmp/`; the old directory
  moves to `trash/<name>-<UTC time>/` (`"trash"` in the answer), then the build is renamed into
  its place. The database keeps its `autoLoad`, `readOnly` and `description`. When `<name>`
  no longer exists (deleted), the backup becomes a new database of that name. A base that
  layers name is not replaced (`409`: restore it into a new name).
- with another `name`: a new database (`409` when the name is taken), `autoLoad` off, built in
  `tmp/` like a create.

A restore never brings grants back: a deleted database's grants were removed with it, and a
restore into a new name starts without any (grant access again). A restore over the existing
database keeps its grants.

A layer's backup is checked against its base: the base database the sidecar names (without a
sidecar: any base database) must be a `role = "base"` database whose identity is the one the
layer was built over; otherwise `409` naming that identity (and the base databases that have
it, if any). Unknown backup: `404` with help `GET .../<name>/backups`. The restored database is
`unloaded`.

### Export

`GET .../<name>/export` answers a packed snapshot (`.gsnap`) of the database's latest state as a
download: `Content-Type: application/octet-stream`, `Content-Length`,
`Content-Disposition: attachment; filename="<name>.gsnap"` (`<name>.layer.gsnap` for a layer
snapshot), `X-Graphersal-Snapshot: full | layer`, `X-Graphersal-Models: included | excluded`.
The query:

- `models` (default `true`): carry the embedding models' blobs. `false`: the model definitions
  stay in the snapshot's catalog, the blobs are left out (an importer resolves them by name).
- `materialize` (default `false`, layers only): `false` writes a LAYER snapshot (only the
  layer's changes; it loads only over the same base and never carries models); `true` writes a
  standalone full snapshot of everything the layer shows (portable; with the base's models
  unless `models=false`).

Other parameters or values: `400`. It works on a loaded database (its open graph at its last
commit; writes wait while it is encoded) and on an unloaded one (read from its files). The file
is written into `tmp/` first and streamed from there (removed after the answer), so an error
is still a JSON answer.

### Import

A new database from another one, in three forms:

- **Upload** (the web UI's Import tab): `POST /api/admin/import?name=<name>` with the file as the raw
  request body (`Content-Type: application/octet-stream`; not multipart) and a
  `Content-Length`. Query parameters (URL-encoded): `name` (required), `base` (a layer's base
  database), `format` (`snapshot`, `store`, `graphson`, `graphml`; default: detected),
  `description`, `role` (`normal`, the default, or `base`; anything else `400` before the body
  is read); any other parameter `400`. The body is streamed into `tmp/upload-*.bin`, never
  held in memory, imported, then removed. Its size limit is `server.toml`'s
  `[limits] upload_bytes` (default 4 GiB), announced as `uploadBytes` by the root
  `GET /api/info`: check the file's size against it before sending. A body over the limit is
  `413` with `Connection: close`: a declared `Content-Length` over the limit is answered at once,
  WITHOUT reading the body (`{"error", "help"}`; a client sending `Expect: 100-continue` gets the
  `413` instead of its `100`); a body without a declared length (chunked) is answered `413` as
  soon as it grows over the limit (`{"error"}`, naming `upload_bytes`). Either way the server
  then closes the connection cleanly: the client reads the whole answer, then EOF. An invalid or
  taken name is answered before the body is read (the connection is then closed too). Upload with
  `XMLHttpRequest` for a progress bar (`xhr.upload.onprogress`; `fetch` has none); the import
  itself runs within the request (the answer comes when the database is written).
- **Path** (the console while a server runs): `POST /api/admin/import` with
  `Content-Type: application/json` and `{"path": "/absolute/path", "name", "base"?, "format"?,
  "description"?, "role"?}`: the server reads the file or directory on its own machine. A relative path
  is `400`, a missing one `404`.
- **Inbox** (the web UI's Import tab, the console's `import DIR --inbox FILE`): the same JSON
  form with `{"inbox": "<entry name>"}` instead of `"path"` (exactly one of the two, else
  `400`): one entry of the server directory's import inbox `imports/`. A name that is not one
  plain entry (a separator `/`, `\` or `:`, a leading `.`, `..` included) is `400`, a missing
  entry `404` (help: `GET /api/admin/imports lists the import inbox`), a symbolic link leading
  outside `imports/` `409`. The file stays in the inbox.

`GET /api/admin/imports` lists the inbox (an older server directory gets `imports/` here):

| Field | Type | Meaning |
|---|---|---|
| `dir` | string | the inbox's absolute path on the server's machine |
| `imports[].name` | string | the entry's name (what `"inbox"` takes), sorted |
| `imports[].kind` | string or null | `snapshot`, `store`, `graphson`, `graphml` guessed from the name (`.gsnap`; `.gstore` or a directory holding a Store; `.json`/`.jsonl`/`.graphson`; `.xml`/`.graphml`), else `null`; the import still detects the content |
| `imports[].directory` | bool | a directory (a Store directory) |
| `imports[].bytes` | number | its size (a directory: every file below it) |
| `imports[].modified` | string or null | last modification, RFC 3339 UTC |

Entries starting with `.` and symbolic links leading outside `imports/` are not listed.

Sources, detected from the content (a file name never decides a snapshot):

| Source | Result |
|---|---|
| packed snapshot `.gsnap`, full | a plain database; its embedding models come along when it carries them; when it names a model without carrying it, the model is taken from the server's shared `models/` when that holds it (hard-linked like any shared blob, listed in `modelsResolved`), else the database is created in the state `needs-model` (`model` names it; kept across restarts, cleared by `POST /api/admin/models` or the first successful load) |
| packed snapshot, layer (`.layer.gsnap`) | a layer database over the `role = "base"` database with the snapshot's base identity: `base` names it, else the only base database with it; none (or the named one does not match): `409` naming the identity |
| Store directory or single-file `.gstore` | copied file by file into a directory Store (verified; its creation parameters come along); a layer Store is matched with its base like a layer snapshot. A model blob the Store lacks is filled into the copy from the server's shared `models/` (checked by its SHA-256, then hard-linked to `models/`; its model listed in `modelsResolved`, a note says it was filled). A blob `models/` lacks too is left out of the copy, and the database is created in the state `needs-model` like a snapshot naming a model it does not carry (`model` and `modelsMissing` name it; `POST /api/admin/models` with the model resolves it; a load before that answers `needs-model` again). A blob `models/` holds damaged (its bytes do not match its name) is `500`, the error naming the file `models/blobs/<2 hex>/<hash>`, with help; a damaged blob in the source Store itself fails the copy's verification (`500`) |
| GraphSON (`.json`, `.jsonl`, `.graphson`) or GraphML (`.xml`, `.graphml`) | a plain database holding the file's graph (the Store gets `server.toml`'s `[defaults]`); data GraphSON cannot hold is skipped and noted |

Without an extension (an upload) a file that is neither a snapshot nor a Store is GraphSON when
its first character is `{` or `[`, GraphML when it is `<`; anything else is `415`. `base` for a
source that is not a layer is `400`; a file that does not read as its format `400`.

`role=base` makes the new database a base (`role: base`, `readOnly: true`, frozen forever, see
Promote) holding the source's data. Only a plain source becomes a base: a full snapshot, a
plain Store, GraphSON or GraphML. A layer source (layer snapshot or layer Store) is `400`; a
source naming an embedding model that neither it nor `models/` carries is `409` (a base cannot
get a model later; help: add the model to `models/` and import again, or import it as an
ordinary database, add the model, then promote it). A model `models/` holds is taken from
there before the base is frozen.

The answer is the new entry (state `unloaded`, or `needs-model`) with `"import"`:

| Field | Type | When | Meaning |
|---|---|---|---|
| `format` | string | always | `snapshot`, `store`, `graphson`, `graphml` |
| `layerBase` | string | layers | the base identity the layer was built over |
| `modelsMissing` | [string] | always | the models (repositories) named but not carried, and `models/` lacks them (the database is `needs-model`) |
| `modelsResolved` | [string] | always | the models taken from `models/` at the import: named but not carried (a snapshot), or whose blob a Store lacked (filled into the copy) |
| `skipped` | number | always | values, entries or elements the import skipped (GraphSON) |
| `notes` | [string] | always | what was skipped or converted, missing models |

The database is built in `tmp/` and renamed into `databases/` in one step: a failed import
leaves nothing behind.

### Models

The server keeps ONE copy of every embedding model blob in `models/blobs/<2 hex>/<sha256>`
and puts a hard link of it into each database's `store/blobs/` (Store blobs are
content-addressed and immutable: for the engine it is an ordinary blob file; a database's
prune or delete removes only its link; an export embeds the bytes). Every create, clone,
import and restore shares the new database's model blobs this way before the database
appears, and the server does it at start for every idle database. Where the file system
refuses hard links (another file system, FAT/exFAT, most SMB shares, FUSE mounts) the blob is
copied instead, with a warning in the log, in an import's `notes`, in the model row and the
database row (`linkWarning`). `models/models.toml` is a derived index (rebuilt at every
listing and model change; edits are lost). Back up a server directory with `rsync -H` to keep
the links (`cp -r` or a zip duplicates them: bigger, still correct).

A model (one blob):

| Field | Type | When | Meaning |
|---|---|---|---|
| `repo` | string or null | always | the repository (`owner/name`) from the model's own manifest; `null` for a blob that is not a model archive |
| `identity` | string or null | always | `model2vec/<repo>@<12 hex>` |
| `license` | string or null | always | the licence (SPDX id) |
| `blob` | string | always | the blob's SHA-256 (hex) |
| `bytes` | number | always | its size |
| `users` | [string] | always | the databases whose Store holds it |
| `waiting` | [string] | always | the `needs-model` databases waiting for its repository |
| `copied` | bool | always | some user holds a copy, not a link |
| `copiedIn` | [string] | always | those users |
| `linkWarning` | string | when copied | the warning text |
| `links` | number | unix | the hard links of the file in `models/blobs/` (`1`: no database links it) |

`POST /api/admin/models` adds models: `{"path": "/absolute/path"}` names a Model2Vec model
directory (`repo` and `license` for a directory that does not name them), a tar archive of
one (uncompressed or gzip-compressed), or a `.gsnap` carrying model blobs, read on the server's machine (a relative path `400`, a missing one `404`, one
that holds no model `400`); `{"repo"}` alone names a model `models/` already holds (`404` with
help when it does not). Then every plain `needs-model` database whose repository `models/` now
holds gets it (the blob its catalog references restored from `models/`, or, when its catalog
has no definition of the model, the model imported from the blob's files) and becomes
`unloaded` (`resolved`); the others stay (`stillMissing`). A base is never resolved (it is
frozen; only a hand-edited `database.toml` could mark one). An import of a snapshot that names
a model it does not carry takes it from `models/` right away when `models/` holds it (see
Import), so only a model `models/` lacks leaves a database `needs-model`.

The **upload form** sends the file itself as the body (`Content-Type:
application/octet-stream`, anything but JSON) with the query parameters `repo`, `license` (as
in the JSON form) and `format` (`gsnap` or `tar`, which also names a gzip-compressed tar;
default: detected from the content; a file of another format than the one named is `400`); any other parameter `400`. The body is
streamed into `tmp/` exactly like an import upload (same `[limits] upload_bytes`, same `413`
behaviour, an empty body `400`), added as a path, and removed. A file that is neither a packed
snapshot nor a tar archive is `400`. A tar archive is POSIX ustar (`tar -cf
model.tar -C potion-base-8M .`, or `tar -cf model.tar potion-base-8M`), uncompressed or
gzip-compressed (`tar -czf model.tar.gz ...`; told apart by the gzip magic bytes, not the file
name; zstd and other compressions are not taken): the model directory is the archive's root or
its only top-level directory. A gzip file is decompressed into `tmp/` first, at most
`[limits] upload_bytes` bytes (a decompression bomb is refused with `400` once it passes them;
a corrupt or truncated gzip file, or one that does not hold a tar archive, is `400` too, all
as `not a usable model archive: ...`). It is unpacked into `tmp/` first and
refused as a whole (`400`, `not a usable model archive: ...`) for an absolute path, a `..`
component, a backslash or a colon in a name, a symbolic or hard link, a device or FIFO, a GNU
long name, more than 1000 entries, nesting deeper than 16 directories, a file in it twice, an
entry past the end or a broken header checksum; pax headers and macOS resource forks (`._*`)
are skipped. The bytes written are bounded by the (uncompressed) archive itself. The console's
`models add FILE.tar` (or `FILE.tar.gz`) takes the same archives.

`DELETE /api/admin/models/<repo>` (the repository's slash is part of the path) removes every
blob of the repository from `models/` (a blob hash removes that blob); refused (`409`, the
message names them, with help) while any database holds one of its blobs, waits for it, or
names it in its Store's catalog.

### Runs

The run administration: every query that waits or runs on a loaded database, whatever its
client (a browser page under `/db/<name>/`, an MCP agent on `/mcp` or `/db/<name>/mcp`, a
script calling `/db/<name>/api/execute`), and stopping one of them. It is part of the admin
surface and has its access rules (none in this version: the loopback listen and the
`Host`/`Origin` checks); the query texts are other clients' data, so they are offered only here:
the databases' own mounts keep the playground engine's `/db/<name>/api/admin/runs` off (`404`),
and a page under `/db/<name>/` shows no Runs button. The server's web UI has a **Runs** tab (and
a "Runs…" button on each loaded database's row).

`GET /api/admin/runs` lists the runs of every loaded database (by database name, then oldest
first); `GET /api/admin/databases/<name>/runs` those of one database (`loaded: false` and no
runs when it is not loaded; `404` for an unknown name, `409` offline). A run:

| Field | Type | Meaning |
|---|---|---|
| `database` | string | the database it runs on |
| `id` | number | its id, unique within the database (not across databases) |
| `client` | string | the client's key: `page:<id>` (a page's `X-Graphersal-Client`), `mcp:<session>` or `mcp`, ... |
| `alias` | string or null | the client's own id of the run (a page's `runId`, an MCP request id) |
| `kind` | string | `query`, `layout`, `tool` (an MCP tool call), `other` |
| `state` | string | `queued` (its job waits for the database's session) or `running` |
| `cancelled` | bool | stopped: it ends at its next check (or when it starts) |
| `startedAtMs` | number | when it was registered (Unix milliseconds) |
| `durationMs` | number | the time since then, waiting included |
| `text` | string or null | the first `textHeadChars` (200) characters of its query text |

`POST /api/admin/databases/<name>/runs/cancel` with `{"id": <n>}` stops that run of the
database, whatever its client: a waiting run stops when it starts, a running one at its next
check, and its changes roll back (its client gets an error answer). `cancelled: false` when the
database has no such run now (it ended, the id belongs to another database, or the database is
not loaded). A non-numeric `id` or another field is `400`. A client stopping its own run keeps
using its database's `POST /db/<name>/api/cancel`.

### Users and grants

The users and grants of `access.toml` (format: [`server-dir.md`](server-dir.md)). A user:

| Field | Type | Meaning |
|---|---|---|
| `name` | string | its name (the database name rule; `anonymous` and `operator` are reserved) |
| `roles` | [string] | its roles; `admin` = every database (write) and the admin API |
| `admin` | bool | it has the `admin` role |
| `hasToken` | bool | it can authenticate (`false` after a revoke) |
| `tokenLast4` | string or null | the last four characters of its token (to tell tokens apart: the server keeps only a SHA-256 of the token) |
| `grants` | [{`database`, `level`, `kind`, `target`}] | its own `user:<name>` grants (`kind`, `target` as for a grant below) |

A grant: `subject` (`user:<name>`, `anonymous` = everyone, also a request without
credentials, `role:<r>`, `group:<g>`), `database` (a database's real name, an alias, or `*` =
every database), `level` (`read` or `write`), and in every answer what `database` names now:
`kind` (`database`: a real database; `alias`: an alias, then `target` is the database it points
at now; `all`: `*`; `missing`: no database or alias of that name, e.g. one left by 0.2.0 on a deleted database's name; see [Grants left on a name](#grants-left-on-a-name)) and
`target` (`null` unless an alias). A principal reaches a database at the highest level of any of
its grants, capped by the database's mode (read-only or a base: `read`). A grant on an alias
reaches the alias's current database only through the alias's name; the real name stays hidden
without a grant of its own (`docs/access.md` "Grants on aliases").

- `POST users` makes the user with a new token (256 random bits as 64 hex digits). The token is
  in this answer only (`token`), never again: copy it. `admin: true` gives the `admin` role.
- `POST users/<name>/token` replaces the token (the old one stops working at once);
  `DELETE users/<name>/token` takes it away without a new one. Either change, and `DELETE
  users/<name>`, ends the user's browser sessions and MCP sessions at once
  (`mcpSessionsEnded`: how many MCP sessions ended). Removing a user removes its grants.
- `PUT users/<name>/admin {"admin": bool}` gives or takes the `admin` role and keeps the
  other roles; `PUT users/<name>/roles {"roles"}` sets them all.
- `POST grants`: `subject` may also be a bare user name (`bob` = `user:bob`); a user subject
  must name a user (`404`), `database` an existing database or alias (`404`). A second grant of the same
  subject and database replaces the level. `DELETE grants/<subject>/<database>` removes it
  (`revoked: false`: there was none); path segments may be percent-encoded (`user%3Abob`).
- The last admin (the `admin` role and a token) is never removed, demoted or left without a
  token: `409` with help (make another admin first; a lost token is replaced with `POST
  users/<name>/token`).
- Every change is recorded in the audit log with the actor (`user:<name>` or `operator`).

The console's `user` commands are these routes (offline the console edits `access.toml` itself,
as the operator); the web UI's Users tab uses them too.

### MCP

The runtime switches of the MCP endpoints (`mcp.toml`, [`mcp.md`](mcp.md#switches-at-runtime-the-mcp-tab-mcptoml))
and their usage statistics; the web UI's MCP tab and `graphersal-server mcp ...` use these
routes. A change takes effect at once (no restart) and may be based on the state's `revision`
(stale: `409` with the current one). The MCP state:

| Field | Meaning |
|---|---|
| `enabled` | the runtime switch (`mcp.toml` `enabled`); `false`: `/mcp` and `/db/<name>/mcp` answer `503`, open sessions are kept |
| `endpointBuilt` | `server.toml [mcp] enabled`: `false` = the server built no MCP endpoint (restart with it on); offline what `server.toml` says |
| `serving` | `enabled` and `endpointBuilt` |
| `online` | a running server answered (offline the console read `mcp.toml`; `usage` is `null`) |
| `endpoint`, `maxSessions` | `/mcp`; `server.toml [mcp] max_sessions` |
| `revision` | of the settings |
| `databases` | every database and alias, and every name with settings that names nothing now: `name`, `kind` (`database` \| `alias` \| `missing`), `target` (an alias's database), `exposed` and `readOnly` (the name's own settings), `effectiveExposed` (the name and its database exposed), `effectiveReadOnly` (read-only for MCP by the name, its database, or the database's own mode: its read-only flag, a base), `sessions` (open MCP sessions working on it), `calls`, `errors` (tool calls since the start) |
| `usage` | since the server started, in memory: `startedAtMs`, `sessions {open, byUser: [{user, sessions}], byDatabase: [{database, sessions}], withoutDatabase}`, `calls {total, errors, refused}` (`refused`: requests refused while MCP was off or the database hidden), `tools: [{tool, calls, errors}]`, `users: [{user, calls, errors}]`, `databases: [{database, calls, errors}]` (most calls first), `recentErrors: [{atMs, tool, user, database, message}]` (the last `recentErrorsMax` = 50, newest first); `null` offline |

`PUT /api/admin/mcp/databases/<name>` takes a database's or alias's name (a name with settings
that names nothing now too, to reset it); a missing value is kept; neither given is `400`, an
unknown name `404`. The changes are audited (`MCP on|off`, `MCP for '<name>': exposed ...,
read-only ...`). Users are named without the `user:` prefix; `anonymous` for requests without
a token, `operator` for the console's token.

### Server

`GET /api/admin/server` describes the running server itself (`graphersal-server status`):
version, process id, the bound address and its URL, the server directory, the start time
(Unix milliseconds) and uptime in seconds, the number of databases with the names of the
loaded and loading ones, the number of aliases, and the memory summary `memory` (the web UI's
Databases tab shows it above the table; `status` prints it). A figure the server cannot know is
`null`, never `0`:

| Field | Meaning |
|---|---|
| `processBytes` | the server process's resident memory (RSS): Linux `/proc/self/statm`, macOS `proc_pidinfo`, Windows `GetProcessMemoryInfo` (working set); `null` on other platforms |
| `liveBytes` | the engine's tracked live heap: `null` in this build, which installs no tracking allocator |
| `databasesBytes` | the loaded databases' graphs, each once: plain databases, the layers' own changes, and each base in the pool once (however many layers share it) |
| `machineTotalBytes`, `machineAvailableBytes` | the machine's physical memory and what it can give without swapping: Linux `/proc/meminfo` (`MemTotal`, `MemAvailable`), macOS `hw.memsize` and the VM statistics (free + inactive + purgeable pages, an approximation), Windows `GlobalMemoryStatusEx`; inside a container these are the host's |
| `limitBytes` | the engine's process memory ceiling in effect (`graphersal::exec::memory_room()`): a query, import or write that would take the process over it is refused; `null`: no ceiling |
| `limitSource` | where the ceiling comes from: `configured` (a number in `server.toml`), `cgroup` (a container's limit minus the headroom), `physical` (80 % of the machine's memory), `none` (`memory_limit = "none"`), `unknown` |
| `configuredLimit` | `server.toml [limits] memory_limit` as written: a number of bytes, `"auto"`, `"none"`, or `null` when not set (the engine default, `auto`) |
| `headroomBytes` | `[limits] memory_headroom` (what `auto` keeps free below a cgroup limit), `null` when not set (a tenth of the cgroup limit, at least 64 MiB) |
| `availableBytes` | the engine's room below the ceiling (the whole ceiling without a tracking allocator), `null` without one |
| `freeBytes`, `freeBasis` | "free under the limit": `limitBytes` minus the process's memory (`processBytes`, else `databasesBytes`) with `freeBasis: "limit"` when a ceiling is in effect, but never more than the machine's available memory (`machineAvailableBytes`): when that is smaller, it is the answer with `freeBasis: "machine-below-limit"`; without a ceiling the machine's available memory, `freeBasis: "machine"` |

`POST /api/admin/shutdown` stops the server (`graphersal-server stop`): it answers `202
{"stopping": true, "pid"}` and the server then stops exactly as on Ctrl+C or SIGTERM (running
requests get 5 s, every database is unloaded, `server.run` removed, the lock released). Only
the operator (the token of `server.run`) or a user with the admin role may; the principal
`everyone` of a server with access control off gets `403` (stopping needs real credentials
even then); a server embedded by a library caller without a stop hook answers `409`. The
request is audited (`server shutdown requested`).

### Help on refusals

Besides the cases above, a taken name (create, clone, restore, import, alias, alias rename) answers help naming the
listing; `PUT .../read-only {"readOnly": false}` on a base names the layer to create instead;
deleting a base that layers name names each layer's `DELETE`; a refused promotion names the
unload, the materialized export or the model to add (see Promote); unloading a base that loaded
layers use names each layer's `unload`.

## Console

```
graphersal-server db list | create NAME [...] | delete NAME | clone SOURCE NAME --dir DIR [--json]
graphersal-server db load NAME [--no-wait] | unload NAME | promote NAME --dir DIR [--json]
graphersal-server db read-only NAME on|off | auto-load NAME on|off --dir DIR [--json]
graphersal-server db alias set ALIAS DATABASE | alias remove ALIAS | alias rename OLD NEW --dir DIR [--json]
graphersal-server db backup NAME | backups NAME | restore NAME BACKUP [--name NEW] --dir DIR [--json]
graphersal-server db export NAME --out FILE [--no-models] [--materialize] --dir DIR [--json]
graphersal-server import DIR SOURCE --name NAME [--base BASE] [--format F] [--description TEXT] [--role normal|base] [--json]
graphersal-server import DIR --inbox FILE --name NAME [--base BASE] [--format F] [--description TEXT] [--role normal|base] [--json]
graphersal-server import DIR --list [--json]
graphersal-server models list | add SOURCE [--repo R] [--license L] | remove REPO --dir DIR [--json]
graphersal-server user list | add NAME [--admin] | token NAME [--revoke] | remove NAME --dir DIR [--json]
graphersal-server user admin NAME on|off | grant SUBJECT DATABASE read|write | revoke SUBJECT DATABASE --dir DIR [--json]
graphersal-server mcp status | on | off | db NAME on|off | db NAME read-only on|off --dir DIR [--json]
```

With a server running on `DIR` (its `server.lock` is held) the console sends these requests to
the address in `DIR/server.run` and notes so on stderr; otherwise it takes the lock itself and
answers them on the directory (then `load`/`unload` are refused: nothing would serve the
database). `--json` prints the answer above; errors go to stderr with `help:`; exit code `1`.

`db export` writes `FILE` (through `FILE.part`, renamed when complete): offline the admin core
writes it, through a server the console streams the download. Its `--json` answer is
`{"database", "file", "bytes", "snapshot": "full" | "layer", "models": "included" | "excluded"}`.

`import` takes the server directory first (as the design's command line does). Offline the
admin core imports `SOURCE` itself; through a running server the console sends the path form
with `SOURCE` made absolute (the server reads it; no upload). Its answer is the import answer
above; the text names the format, the kind and the state, and lists the notes.
`import DIR --inbox FILE` sends the inbox form (`{"inbox": FILE}`), `import DIR --list` is
`GET /api/admin/imports` (a table of name, size, kind, modified).

`models add SOURCE` (a model directory, a `.tar` or `.tar.gz` of one or a `.gsnap`) sends the path form
with `SOURCE` made absolute when it exists, else
`SOURCE` as a repository (`{"repo"}`). `models list` prints one line per model blob (repository,
bytes, short hash, the databases using it, a link warning) and the `needs-model` databases
whose model is missing.
