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

## Console

```
graphersal-server db list|create|delete|clone|load|unload|read-only|auto-load|alias ... --dir DIR [--json]
```

With a server running on `DIR` (its `server.lock` is held) the console sends these requests to
the address in `DIR/server.run` and notes so on stderr; otherwise it takes the lock itself and
answers them on the directory (then `load`/`unload` are refused: nothing would serve the
database). `--json` prints the answer above; errors go to stderr with `help:`; exit code `1`.
