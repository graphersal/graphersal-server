# The Server Directory

Everything `graphersal-server` keeps lives in ONE directory: its configuration, its databases,
the shared embedding models, backups and deleted databases. `graphersal-server init DIR` creates
it; `graphersal-server serve --dir DIR` serves it. A directory is a server directory when it
holds `server.toml`.

The directory is private to the OS user that owns it, the operator: `init` creates it with mode
`0700` (unix), `sessions/` `0700` and the secret files (`access.toml`, `server.run`,
`sessions/*.json`) `0600`; `serve` and the console refuse a directory or secret owned by
another user or open to group or others ([Security Model](../guide/security.md#the-directory-owner-is-the-operator)).

```
<server-dir>/
  server.toml          configuration
  access.toml          users (token hashes), roles and grants (mode 0600)
  server.lock          one running server per directory (an OS file lock)
  server.run           the running server's address and operator token (mode 0600, only while it runs)
  server.open          the databases the running server has open (removed at a clean stop)
  recovery.json        the last unclean shutdown and its check of each database
  aliases.toml         alias -> database
  mcp.toml             the MCP switches (written by the server; missing = the defaults)
  databases/
    <name>/
      database.toml    the database's configuration
      store/           its graphersal Store (a directory Store)
      store-reset/     a layer reset's new Store, only during the reset (finished or removed at start)
  models/
    blobs/<2 hex>/<sha256>   one copy of every embedding model blob
    models.toml        derived index of the models (rebuilt, never read back)
  backups/<name>/      <UTC time>.zip + <UTC time>.toml per backup
  backups/jobs/        the backup root of backup jobs (server.toml [jobs] backup_root)
  trash/               deleted and replaced databases, reset layers: <name>-<UTC time>/
  tmp/                 builds and uploads in progress (emptied at start)
  imports/             the import inbox: files and Store directories an admin imports by name
  sessions/            (mode 0700)
    browser.json       browser sessions (hashes only, mode 0600): they survive a restart
    mcp.json           MCP sessions (hashes only, mode 0600): they survive a restart
  logs/                (mode 0700, made by the first `start`)
    server.log         stdout and stderr of a server `start` runs in the background (mode 0600)
```

`serve` logs to stderr (redirect it to keep a log file). `graphersal-server start` runs
`serve` in the background and appends its stdout and stderr to `logs/server.log` (created on
the first `start`: `logs/` mode `0700`, the log `0600` on unix; each start adds a line `== <UTC
time> graphersal-server start: serve --dir ... ==`). Nothing rotates the log: truncate or
rotate it while the server is stopped (or use `copytruncate`). A `logs/` directory an earlier
version's `init` created is reused.

`init DIR` accepts a missing or empty directory, or an existing server directory (then it only
creates the missing parts and keeps `server.toml` as it is).

## The configuration files

`server.toml`, `access.toml`, `aliases.toml`, `mcp.toml` and each `databases/<name>/database.toml`
are described key by key in the [Configuration Reference](configuration.md).

## `server.lock` and `server.run`

`server.lock` holds the OS's advisory file lock (`flock` on Unix, `LockFileEx` on Windows) while
a server or an offline console command works on the directory; the OS releases it when the
process ends, also on a crash. The file itself stays and means nothing when unlocked. A second
`serve` on a locked directory is refused.

`server.run` is written once the listener is bound (mode `0600` on unix) and removed at a
clean stop:

```toml
address = "127.0.0.1:51234"
url = "http://127.0.0.1:51234/"
pid = 12345
version = "0.3.0"
operator_token = "<64 hex digits>"
started_at_ms = 1764151975000
```

It holds the address actually bound (port `0` resolved) and the operator token of this run: a
new one at every start, an admin credential the console sends (`Authorization: Bearer`), so
whoever reads the file can act as an admin while the server runs ([Users, Tokens and Grants](../guide/access.md)). The console reads it to reach a
running server; it is trustworthy only while `server.lock` is held (a crash leaves it behind,
the next server overwrites it).

`graphersal-server status` reads both: a held lock is a running server (its facts come from
`GET /api/admin/server`, the uptime from `started_at_ms` when the server does not answer); a
`server.run` without the lock is reported as stale. `graphersal-server stop` removes a stale
one (holding the lock meanwhile) and `start` refuses while the lock is held.

## `server.open` and `recovery.json`

`server.open` (round 3) names the databases the running server has open: rewritten
atomically (with the directory's `fsync`) when a load starts, when it ends and after an unload,
and removed at a clean stop (after every Store closed cleanly). A database whose Store could not
be closed cleanly at an unload stays listed.

```toml
pid = 12345
started_at_ms = 1764151975000
databases = ["acme", "orders"]
```

A `server.open` (or a stale `server.run`) found by the next `serve` (with the lock held) means
the last server did not stop cleanly: see [After a crash](../troubleshooting.md#a-server-was-killed-by-force). `stop` on a
stopped directory removes a stale `server.run`, never `server.open`, so the next start still
checks those databases.

`recovery.json` is the last unclean shutdown and its check, written by the server (JSON, the
shape of `uncleanShutdown` in [Admin API](admin_api.md#server)): when it was found, the
pid of the server that did not stop, whether its open set was known, and per database the
outcome (`pending`, `checking`, `ok`, `damaged`, `failed`, `needs-model`, `missing`), the state
afterwards, what the open of its Store did (clean close, journal commits replayed, the position,
a torn tail cut, ...), what the verify found and the damage report. It stays until the next
unclean shutdown replaces it; deleting it while the server is stopped only forgets the report.

## `databases/<name>/`

There is no registry file: the databases ARE the directories under `databases/`, read at start.
Names are unique case-insensitively. A NEW database or alias name (and a new user name) has
at most 32 characters, `[a-z0-9]` first, `[a-z0-9._-]` inside, a letter or digit last, no `..`
and no Windows device name (`con`, `nul.db`, `com1`, ...). A new database or alias cannot be
named `jobs` either (reserved: the default backup root `backups/jobs/` would also hold the
admin backups of a database of that name). A directory, alias or user named by 0.2.0 or
earlier under the older rule `[a-z0-9][a-z0-9._-]{0,62}` (not ending in `.`; so up to 63
characters, a trailing `-` or `_`, `..` inside, or `jobs`) is still loaded, served, granted
and restored under its own name; only making a new one is refused.

**Long paths on Windows.** A database's files live at `<server-dir>\databases\<name>\store\...`,
and the Store adds directories and file names of its own below that (and backups, `tmp\`,
`trash\` add a time stamp to the name). Windows refuses a path over 260 characters
(`MAX_PATH`) unless long paths are enabled (the `LongPathsEnabled` policy). The 32-character
name limit keeps the server's part short; keep the server directory itself short as well
(e.g. `C:\gs\srv`, not a deep folder under the user profile). A directory whose `database.toml` or Store is broken is listed as `failed`
with the reason; it never stops the server.

`databases/<name>/verify.json` is the database's last verify (JSON, `version` 1: when it ended,
the verdict `ok` / `needs-model` / `damaged` / `failed`, the problem count and the first 100
problems, the missing models), written atomically after every verify. It goes with the
directory (a delete moves it to `trash/`, a swap brings the other directory's); a missing file
means "never verified", a broken one is ignored with a warning and replaced by the next verify.
A server backup does not carry it.

Every create, clone, import and restore builds the new database in `tmp/` and renames it into
`databases/` in one step: a failure leaves no database, and the debris of a crash in `tmp/` is
removed at the next start.

### `database.toml`

The database's settings: [Configuration Reference](configuration.md#databasetoml).

### `store/`

An ordinary graphersal directory Store (snapshot chunks, journal, `blobs/`). The graphersal
tools can open it while no server serves it.

### `store-reset/`

Exists only while a layer is being reset (`db reset-layer`): the new, empty layer Store is
built here beside `store/`, then `store/` moves to `trash/<name>-<UTC time>/store/` and
`store-reset/` is renamed to `store/`. After a crash the next start finishes it: a
`store-reset/` beside `store/` was never swapped in and is removed (the layer is unchanged); one
without `store/` is renamed to `store/` (the old layer is in `trash/` already). A database is
never left without a Store.

## `models/`

The server keeps ONE copy of every embedding model blob in `models/blobs/<2 hex>/<sha256>` (the
Store blob layout) and puts a HARD LINK of it into each database's `store/blobs/`. Store blobs
are content-addressed and immutable, so for the engine the link is an ordinary blob file: a
database's delete or prune removes only its link, an export embeds the bytes. Every create,
clone, import and restore links the new database's model blobs before the database appears; at
start the server does it for every idle database. An import whose source names a model without
carrying it gets the model from `models/` (linked the same way) when `models/` holds it. A
Store import whose source lacks a model blob gets the blob filled into its copy from
`models/blobs/` (checked by its hash), and the filled file is then replaced by a hard link like
any other; a blob `models/` lacks too refuses the import (add the model first), one `models/`
holds damaged refuses it naming the file.

Where the file system refuses hard links (another file system, FAT/exFAT, most SMB shares, FUSE
mounts) the blob is COPIED instead: correct, but no disk saving. The server logs a warning and
shows it in an import's `notes`, the model's row and the database's row (`linkWarning`).

`models/models.toml` is a derived index, one row per blob (repository, identity, licence and
size read from the model's own manifest inside the blob; the databases holding it; the
`needs-model` databases waiting for it; the users holding a copy). It is rebuilt at every
listing and model change and never read back: edits are lost.

`graphersal-server models list|add|remove` and `/api/admin/models` manage it
([Admin API](admin_api.md#models)): a model is added from a Model2Vec model directory, a `.tar` or `.tar.gz`
of one (unpacked safely into `tmp/` first) or a `.gsnap` carrying models, on the server's machine
or uploaded through the web UI's Models tab (the upload goes through `tmp/` too).

## `backups/`

`backups/<name>/<UTC time>.zip` is a verified ZIP backup of the database `<name>` (the time with
dashes instead of colons, such as `2026-11-22T10-00-00Z`; `-2`, `-3`, ... when that name exists),
with a sidecar `<UTC time>.toml` recording the database's kind, role, base, description and, for
a layer, its base's identity. A layer's backup holds only the layer and restores only over a
base with the same identity. Backups stay when their database is deleted; nothing removes them
automatically.

Scheduled backup jobs write into the backup root, `backups/jobs/` by default: [Backup
jobs](../guide/backups.md#backup-jobs). The server's own backup of the whole directory is in
[Backups and Restore](../guide/backups.md#back-up-the-whole-server).

## `trash/`

A deleted database, and the database a restore replaced, move to `trash/<name>-<UTC time>/`
(recoverable by hand: stop the server and move it back into `databases/`). A layer reset moves
the old layer there too: a copy of its `database.toml`, its `store/` and its `verify.json`, so
the entry is a whole database directory as well. Nothing empties `trash/` automatically.

## `tmp/`

New databases, backups, exports, upload bodies (imports and models) and unpacked model archives
are written in `tmp/` and renamed into place (or streamed or read, then removed). The server empties it at every start; leave it alone while a server
runs. (The temporary files of the atomic writes of configuration files live beside their file,
not here: [Concurrent changes](#concurrent-changes).)

## `imports/`

The import inbox: put a `.gsnap`, a `.gstore`, a Store directory, a GraphSON or a GraphML file
here by any means (scp, a mounted volume, a shared folder), and an admin imports it by its NAME:
the web UI's Import tab lists the inbox ("A file of the server's import inbox"), the admin API
lists it (`GET /api/admin/imports`) and imports one entry (`POST /api/admin/import {"inbox":
"<file>", "name", ...}`), the console as `graphersal-server import DIR --list` and
`graphersal-server import DIR --inbox FILE --name NAME`. The import copies what it needs: the
file stays in the inbox (remove it yourself when done); the import runs within the request, as
every import does.

The inbox is flat: an inbox name is ONE entry of `imports/` (no subdirectory paths; a name with
`/`, `\`, `:` or a leading `.`, `..` included, is refused). Entries starting with `.` (such as
`.DS_Store`, or a copy in progress named `.acme.gsnap.part`) are not listed. A symbolic link in
the inbox is followed only when it resolves inside `imports/`; one leading outside is neither
listed nor imported. `init` creates `imports/`; an older server directory gets it at its next
`init` or at the first listing.

## `sessions/`

The sessions that survive a restart (always on: there is no setting to turn it off), so a
release or a restart does not log browsers out or make MCP clients initialize again.

- `browser.json`: one row per browser session (`gs_session` cookie): the SHA-256 of the session
  id, the user, the SHA-256 of the token it logged in with, created and last seen (seconds since
  the Unix epoch; last seen is updated coarsely, at most every minute).
- `mcp.json`: one row per MCP session: the SHA-256 of its `Mcp-Session-Id`, the identity of its
  `initialize` (`user:<name>`, `operator`, or none for anonymous), the selected database or
  alias, a pending alias rename note, the protocol revision, the client's name and version, and
  when it was created.

```json
{
  "version": 1,
  "sessions": [
    { "id_sha256": "<64 hex>", "user": "alice", "token_sha256": "<64 hex>",
      "created_unix_s": 1795000000, "last_seen_unix_s": 1795003600 }
  ]
}
```

Neither file holds a usable id or token, only their hashes. Every change is written at once
(login, logout, a new MCP session, `use_graph`, a rename note taken, an end), the whole file
atomically, mode `0600` on unix. At start both are read back: a browser session whose user is
gone, whose user holds another token now, or that expired (`[sessions]` above) is dropped, and
so is an MCP session of a user that is gone or has no token. A missing file is no sessions; a
file that cannot be read or parsed is a warning on stderr and no sessions (the server starts;
browsers log in again, MCP clients initialize again) and is overwritten by the next change.

Revoking or reissuing a user's token, or removing the user, ends its browser and MCP sessions at
once, in memory and in these files, through the running server or with the console on the
directory (`graphersal-server user ...`). Deleting the files (while no server runs) logs every
browser out and ends every MCP session. The MCP endpoint keeps at most `[mcp] max_sessions`
sessions, the oldest forgotten first, restarts included.

## Concurrent changes

Many browsers, scripts and the console may change the server at the same time. The files stay
whole and correct:

- **Atomic writes.** Every configuration file is written to a temporary file beside it
  (`<file>.tmp-<pid>-<n>`, a new name for every write, so two writers never share one), flushed
  to the disk (`fsync`), renamed over the file, and on unix the directory is flushed too (not
  for `server.run`, which a power loss makes stale anyway): a reader, a crash or a power loss
  sees the old file or the whole new one, never a mix. A crash
  between the write and the rename leaves the temporary file; the next `serve` (and the console
  on a stopped server) removes it at start: only names this server makes
  (`<file>.tmp-<pid>-<n>`, `<file>.tmp` of the files below as written by 0.2, `access.toml.new-<pid>`),
  in the root, `sessions/`, `models/`, `databases/<name>/` and `backups/<name>/`.
- **One lock per file, held across read, change and write.** The change is made on a copy, the
  file is written, and only then does the server's memory take the new state: a failed write
  changes nothing.
- **Revisions against lost updates.** Users and grants, aliases, each database's settings and
  the MCP settings carry a `revision` that a change may name; a change based on an older state is refused
  (`409`, help `changed by someone else: reload`; [Admin API](admin_api.md#revisions-concurrent-changes)).

What writes which file at runtime, and under which lock:

| File | Writers | Lock |
|---|---|---|
| `server.toml` | `init` (`--config`/`--set`), the server settings change (`PUT /api/admin/config`, the Settings tab, `config set`/`unset`: read, revision check, write, then the running server applies its `now` keys) | `init`: the server lock (an existing directory is never rewritten); a change: the settings writer's mutex (a leaf), online; offline the server lock (one console command) |
| `server.lock` | `serve`, the offline console, `start` | the OS file lock itself |
| `server.run` | `serve` at start (removed at stop); `stop` removes a stale one | the server lock |
| `server.open` | the loader: each load start, load end and unload (removed at a clean stop) | its writer's mutex (a leaf, taken before the loader's state is read, never while it is held) |
| `recovery.json` | the unclean-shutdown check at start (one write per database checked) | its report's mutex (a leaf) |
| `access.toml` | user and grant changes (admin API, console), alias rename/remove and database delete (grants follow), a new name taking leftover grants, the first admin (`init`, `serve`: created new, never replaced) | the access file's lock (`Access`), online; offline the server lock (one console command) |
| `aliases.toml` | alias set/flip, rename, remove | the registry's write lock |
| `mcp.toml` | the MCP switches (admin API, MCP tab, console `mcp`), alias rename (settings move) and remove, database delete and a new name (settings dropped) | the MCP settings' write lock (a leaf: it takes no other lock; a change of a name's settings holds the registry's read lock around it while the name must exist) |
| `databases/<name>/database.toml` | read-only, auto-load, promote, the `needs_model` marker, the settings change (description, read-only, auto-load and the `[limits.query]` overrides in ONE write: `PUT /api/admin/databases/<name>/settings`, `db set`, the Settings tab); a new database's file is written in its build in `tmp/` before the rename into `databases/` | the registry's write lock, held across the revision check, the read of the file's current limits, the write and the memory update (read-only, also through the settings, also the loader's flip lock, taken before the registry) |
| `databases/<name>/verify.json` | every verify of the database (the last one replaces it) | none needed: each write is one whole file (`write_atomic`), the last verify wins |
| `databases/<name>/store/` | the graphersal engine (the Store, its WAL, the database's job definitions) | the engine's own (one open Store per database) |
| `databases/<name>/store-reset/`, `store/` swap | a layer reset: the new layer Store built beside `store/`, then the old `store/` to `trash/` and the new one renamed (`db reset-layer`, `POST .../reset-layer`) | the loader's lock for the whole reset (the layer unloaded, nothing loads it meanwhile), the registry's write lock for the two renames |
| `databases/` (add, remove), `trash/` | create, clone, import, restore (rename of a build), repair (a new database built in `tmp/`), delete, restore over a database, swap (the old directory to `trash/`, the copy renamed) | the registry's write lock; delete, promote and a restore over a database also hold the loader's lock (nothing loads meanwhile) |
| `sessions/browser.json` | login, logout, a session touched or ended | the browser session table's mutex |
| `sessions/mcp.json` | MCP sessions (new, `use_graph`, rename note, end), token changes | its writer's mutex (the MCP endpoint writes snapshots in order) |
| `models/blobs/`, `models/models.toml` | adopting blobs, the models listing (rebuilds the index), model add/remove | the models lock |
| `backups/<name>/<time>.zip`, `<time>.toml` | backups (built in `tmp/`, then renamed) | the backup naming lock (two backups in one second get `-2`) |
| `backups/jobs/` | backup jobs of the engine | the engine's job runner |
| `tmp/` | builds, uploads, exports, unpacked models, server backups being downloaded | none needed: every name is unique (a counter) |
| `imports/` | nothing (created only; the admin fills it) | |
| `logs/server.log` | `start` (one line, appended), then the server's stdout and stderr | append mode |

Lock order (a thread holding one lock takes only those after it, so no two requests can
deadlock): the loader's state lock, then the loader's read-only flip lock, then the registry
(write or read), then the access file, then a session table (browser or MCP); the models lock,
the backup naming lock, the MCP settings' lock, the `server.open` writer's lock and the
recovery report's lock take no other lock (the `server.open` writer reads the loader's state
inside its own lock, never the other way round). The audit records go to the log, no file of
their own.
