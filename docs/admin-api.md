# Admin API (EXPERIMENTAL)

The admin HTTP API of `graphersal-server`, below `/api/admin/`. The server's web UI at `/` (the
`server` UI profile, `crates/graphersal-server/ui/`, model `ui/admin-api.js`) and the console (`graphersal-server db ...`, when a server runs on the directory) are its
clients. Both the API and the console's offline mode call the same admin core, so the answers
below are also what `graphersal-server db ... --json` prints.

The MCP endpoints (`/mcp`, `/db/<name>/mcp`) are documented in [`mcp.md`](mcp.md), the server
directory and its files (`server.toml`, `database.toml`, `models/`, `backups/`) in
[`server-dir.md`](server-dir.md).

There is NO access control in this version: the server listens on the loopback by default.
Every request is checked by `Host` and `Origin` (DNS rebinding, cross-site pages): send
`Host: <the server's address>`; a browser page of another origin is refused with `403`.

## Conventions

- Request bodies are JSON objects with `Content-Type: application/json` (else `415`), at most
  16 MiB (else `413`); the one exception is an import upload (the file itself as the body, see
  Import). Unknown fields are refused (`400`), so a typo is never ignored.
- Answers are JSON. An error is `{"error": "<sentence>", "help": "<what to do>"?}`.
- Every `GET` route but the export also answers `HEAD`.
- The actions take **real database names**; an alias is not followed (`404`). Only
  `GET /api/admin/databases/<name>` also answers an alias. Names are case-insensitive and
  answered in lower case; the name rule is `[a-z0-9][a-z0-9._-]{0,62}` (no Windows device
  names); databases and aliases share one namespace.
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
| `sizeBytes` | number | listing rows | the database's directory on disk (every file once; a model blob linked from `models/` counts in full in each database holding it, so the sum over databases exceeds the disk use) |
| `pooled` | bool | listing rows of a pooled base | `true`: the base is held in memory in the shared pool (by its loaded layers or itself) |
| `pooledBy` | [string] | with `pooled` | the loaded layers holding it (empty: only the base itself, loaded for reading) |
| `linkWarning` | string | listing rows | some of its model blobs are copies, not hard links to `models/` (the file system has no links there: correct, no disk saving; unix only) |

Listing rows are the answers of `GET /api/admin/databases` and `GET /api/admin/databases/<name>`.
There is no `memoryBytes`: a loaded database's memory is known only inside its session thread,
which a listing does not wait for.

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
| POST | `/api/admin/databases/<name>/promote` | | `200` entry (`role: base`, `readOnly: true`; one way) |
| PUT | `/api/admin/databases/<name>/read-only` | `{"readOnly": bool}` | `200` entry |
| PUT | `/api/admin/databases/<name>/auto-load` | `{"autoLoad": bool}` | `200` entry |
| POST | `/api/admin/databases/<name>/backup` | | `201` backup |
| GET | `/api/admin/databases/<name>/backups` | | `200 {"database", "backups": [backup...]}` (oldest first; also for the name of a deleted database) |
| POST | `/api/admin/databases/<name>/restore` | `{"backup", "name"?}` | `201` entry + `"restoredFrom": {"database", "backup"}`, `"replaced": bool`, `"trash"` (when replaced) |
| GET | `/api/admin/databases/<name>/export?models=true\|false&materialize=true\|false` | | `200` the `.gsnap` file as a download |
| POST | `/api/admin/import?name=<n>&base=<b>&format=<f>&description=<d>&role=<normal\|base>` | the file itself (`Content-Type: application/octet-stream`, or anything but JSON) | `201` entry + `"import"` |
| POST | `/api/admin/import` | JSON `{"path", "name", "base"?, "format"?, "description"?, "role"?}` | `201` entry + `"import"` |
| GET | `/api/admin/models` | | `200 {"models": [model...], "missing": [{"database", "model"}...]}` |
| POST | `/api/admin/models` | `{"path"?, "repo"?, "license"?}` | `201` (from a path) or `200` (a repository) `{"added": [model...], "resolved": [database...], "stillMissing": [{"database", "model"}...], "notes": [string...]}` |
| DELETE | `/api/admin/models/<repo or blob hash>` | | `200 {"model", "removed": [blob...], "bytes"}` |
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

### Import

A new database from another one, in two forms:

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

Sources, detected from the content (a file name never decides a snapshot):

| Source | Result |
|---|---|
| packed snapshot `.gsnap`, full | a plain database; its embedding models come along when it carries them; when it names a model without carrying it, the model is taken from the server's shared `models/` when that holds it (hard-linked like any shared blob, listed in `modelsResolved`), else the database is created in the state `needs-model` (`model` names it; kept across restarts, cleared by `POST /api/admin/models` or the first successful load) |
| packed snapshot, layer (`.layer.gsnap`) | a layer database over the `role = "base"` database with the snapshot's base identity: `base` names it, else the only base database with it; none (or the named one does not match): `409` naming the identity |
| Store directory or single-file `.gstore` | copied file by file into a directory Store (verified; its creation parameters come along); a layer Store is matched with its base like a layer snapshot |
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
| `modelsResolved` | [string] | always | the models named but not carried, taken from `models/` at the import |
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
directory (`repo` and `license` for a directory that does not name them) or a `.gsnap` carrying
model blobs, read on the server's machine (a relative path `400`, a missing one `404`, one
that holds no model `400`); `{"repo"}` alone names a model `models/` already holds (`404` with
help when it does not). Then every plain `needs-model` database whose repository `models/` now
holds gets it (the blob its catalog references restored from `models/`, or, when its catalog
has no definition of the model, the model imported from the blob's files) and becomes
`unloaded` (`resolved`); the others stay (`stillMissing`). A base is never resolved (it is
frozen; only a hand-edited `database.toml` could mark one). An import of a snapshot that names
a model it does not carry takes it from `models/` right away when `models/` holds it (see
Import), so only a model `models/` lacks leaves a database `needs-model`.

`DELETE /api/admin/models/<repo>` (the repository's slash is part of the path) removes every
blob of the repository from `models/` (a blob hash removes that blob); refused (`409`, the
message names them, with help) while any database holds one of its blobs, waits for it, or
names it in its Store's catalog.

### Help on refusals

Besides the cases above, a taken name (create, clone, restore, alias) answers help naming the
listing; `PUT .../read-only {"readOnly": false}` on a base names the layer to create instead;
deleting a base that layers name names each layer's `DELETE`; a refused promotion names the
unload, the materialized export or the model to add (see Promote); unloading a base that loaded
layers use names each layer's `unload`.

## Console

```
graphersal-server db list | create NAME [...] | delete NAME | clone SOURCE NAME --dir DIR [--json]
graphersal-server db load NAME [--no-wait] | unload NAME | promote NAME --dir DIR [--json]
graphersal-server db read-only NAME on|off | auto-load NAME on|off --dir DIR [--json]
graphersal-server db alias set ALIAS DATABASE | alias remove ALIAS --dir DIR [--json]
graphersal-server db backup NAME | backups NAME | restore NAME BACKUP [--name NEW] --dir DIR [--json]
graphersal-server db export NAME --out FILE [--no-models] [--materialize] --dir DIR [--json]
graphersal-server import DIR SOURCE --name NAME [--base BASE] [--format F] [--description TEXT] [--role normal|base] [--json]
graphersal-server models list | add SOURCE [--repo R] [--license L] | remove REPO --dir DIR [--json]
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

`models add SOURCE` sends the path form with `SOURCE` made absolute when it exists, else
`SOURCE` as a repository (`{"repo"}`). `models list` prints one line per model blob (repository,
bytes, short hash, the databases using it, a link warning) and the `needs-model` databases
whose model is missing.
