# Admin API (EXPERIMENTAL)

The admin HTTP API of `graphersal-server`, below `/api/admin/`. The playground's admin panel
and the console (`graphersal-server db ...`, when a server runs on the directory) are its
clients. Both the API and the console's offline mode call the same admin core, so the answers
below are also what `graphersal-server db ... --json` prints.

There is NO access control in this version: the server listens on the loopback by default.
Every request is checked by `Host` and `Origin` (DNS rebinding, cross-site pages): send
`Host: <the server's address>`; a browser page of another origin is refused with `403`.

## Conventions

- Request bodies are JSON objects with `Content-Type: application/json` (else `415`), at most
  16 MiB (else `413`). Unknown fields are refused (`400`), so a typo is never ignored.
- Answers are JSON. An error is `{"error": "<sentence>", "help": "<what to do>"?}`.
- The actions take **real database names**; an alias is not followed (`404`). Only
  `GET /api/admin/databases/<name>` also answers an alias. Names are case-insensitive and
  answered in lower case; the name rule is `[a-z0-9][a-z0-9._-]{0,62}` (no Windows device
  names); databases and aliases share one namespace.
- Status codes: `200`/`201`/`202` success; `400` invalid name, body or schema; `404` no such
  database, alias or endpoint; `405` method not allowed on an existing endpoint; `409` a rule
  refuses it (name taken, database loaded, named by an alias, base of layers, a base's
  read-only, a failed database); `413`/`415` body; `500` a Store or file operation failed;
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

A `failed` database whose `database.toml` cannot be read has only `name`, `state`, `reason`.

## Routes

| Method | Path | Body | Answer |
|---|---|---|---|
| GET | `/api/admin/databases` | | `200 {"databases": [entry...], "aliases": {alias: database}, "problems": [string...]}` (rows sorted by name, alias rows included) |
| POST | `/api/admin/databases` | `{"name", "kind"?, "role"?, "base"?, "schema"?, "description"?, "autoLoad"?, "readOnly"?}` | `201` entry (state `unloaded`) |
| GET | `/api/admin/databases/<name>` | | `200` entry (`<name>` may be an alias: an alias row) |
| DELETE | `/api/admin/databases/<name>` | | `200 {"name", "trash": "trash/<name>-<UTC time>"}` |
| POST | `/api/admin/databases/<name>/clone` | `{"name"}` | `201` entry of the new database |
| POST | `/api/admin/databases/<name>/load` | | `202` entry + `"load": "started"` or `"already-loading"`; `200` entry + `"load": "already-loaded"` |
| POST | `/api/admin/databases/<name>/unload` | | `200` entry + `"unloaded": true` (was loaded) or `false` (was not) |
| PUT | `/api/admin/databases/<name>/read-only` | `{"readOnly": bool}` | `200` entry |
| PUT | `/api/admin/databases/<name>/auto-load` | `{"autoLoad": bool}` | `200` entry |
| POST | `/api/admin/databases/<name>/backup` | | `201` backup |
| GET | `/api/admin/databases/<name>/backups` | | `200 {"database", "backups": [backup...]}` (oldest first; also for the name of a deleted database) |
| POST | `/api/admin/databases/<name>/restore` | `{"backup", "name"?}` | `201` entry + `"restoredFrom": {"database", "backup"}`, `"replaced": bool`, `"trash"` (when replaced) |
| GET | `/api/admin/databases/<name>/export?models=true\|false&materialize=true\|false` | | `200` the `.gsnap` file as a download |
| GET | `/api/admin/aliases` | | `200 {"aliases": {alias: database}}` |
| PUT | `/api/admin/aliases/<alias>` | `{"target"}` | `200 {"alias", "target"}` (new alias or flip) |
| DELETE | `/api/admin/aliases/<alias>` | | `200 {"alias", "removed": true}` |

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

### Read-only and auto-load

`read-only` is persisted in the database's `database.toml` and, when the database is loaded,
applied at once: writes through its API and MCP endpoint are refused with a read-only error;
a running request finishes. A base refuses `false` (`409`). `auto-load` is persisted only.

### Backup

`POST .../<name>/backup` writes a verified ZIP backup of the database's latest state into
`backups/<name>/<UTC time>.zip` (the time with dashes instead of colons, such as
`2026-11-22T10-00-00Z`; `-2`, `-3`, ... when that exists). Every entry is read back and checked;
damage fails the backup and nothing is kept. It works on a loaded database (its open Store; an
unload waits for it) and on an unloaded one. The archive is written in `tmp/` and renamed into
place; a sidecar `<backup>.toml` beside it records the database's `kind`, `role`, `base`,
`description` and, for a layer, its base's identity. A layer's backup holds only the layer: it
restores only over the same base.

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

## Console

```
graphersal-server db list|create|delete|clone|load|unload|read-only|auto-load|alias ... --dir DIR [--json]
graphersal-server db backup NAME | backups NAME | restore NAME BACKUP [--name NEW] --dir DIR [--json]
graphersal-server db export NAME --out FILE [--no-models] [--materialize] --dir DIR [--json]
```

With a server running on `DIR` (its `server.lock` is held) the console sends these requests to
the address in `DIR/server.run` and notes so on stderr; otherwise it takes the lock itself and
answers them on the directory (then `load`/`unload` are refused: nothing would serve the
database). `--json` prints the answer above; errors go to stderr with `help:`; exit code `1`.

`db export` writes `FILE` (through `FILE.part`, renamed when complete): offline the admin core
writes it, through a server the console streams the download. Its `--json` answer is
`{"database", "file", "bytes", "snapshot": "full" | "layer", "models": "included" | "excluded"}`.
