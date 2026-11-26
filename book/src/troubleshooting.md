# Troubleshooting

Task recipes for things that go wrong. Each recipe starts with what you see, then the steps.
The commands use the server directory `./srv`; replace it with yours. Run them as the OS user
who owns the directory.

Every console error has an `Error:` line and, most of the time, a `Help:` line with the next
command. Read the `Help:` line first: it usually names the fix.

## Lost the admin token

A token is never stored, only its SHA-256 hash, so a lost token cannot be shown again. Make a
new one, as the directory's owner:

```bash
graphersal-server user token admin --dir ./srv
```

The new token is printed once. The old token stops working, and the user's browser and MCP
sessions end. It works with the server running or stopped, and needs no token: the owner of
the directory is the operator.

Every admin token lost and no shell on the server's machine? Someone with access to the OS user
who owns the directory must run the command above; there is no other way in, by design.

Not sure which user is an admin?

```bash
graphersal-server user list --dir ./srv
```

## Restore a database from a backup

1. Find the backup:

   ```bash
   graphersal-server db backups acme --dir ./srv
   ```

2. Restore it next to the current database first, to look at it:

   ```bash
   graphersal-server db restore acme 2026-11-22T10-00-00Z --name acme-restored --dir ./srv
   graphersal-server db load acme-restored --dir ./srv
   ```

3. Either point clients at the copy (an alias), or replace the database:

   ```bash
   graphersal-server db unload acme --dir ./srv
   graphersal-server db restore acme 2026-11-22T10-00-00Z --dir ./srv
   graphersal-server db load acme --dir ./srv
   ```

The replaced database moves to `trash/`. Everything written after the backup is lost. A
restore over the database keeps its grants; the restore of a DELETED database does not bring
its grants back: grant access again (`user grant`). Details: [Backups and
Restore](guide/backups.md#restore-one-database).

`Error: database 'acme' has no backup '...'`: the name is the one `db backups` prints (the UTC
time with dashes), without the `.zip`.

## Restore a whole server on a new machine

From a full server backup (`backup --with-databases`):

```bash
graphersal-server restore /secure/graphs-full.zip --dir /srv/graphs
graphersal-server status --dir /srv/graphs
graphersal-server start --dir /srv/graphs
graphersal-server db list --dir /srv/graphs
graphersal-server user list --dir /srv/graphs
```

Check `server.toml` before the start (listen address, `[http] allowed_hosts`, an absolute `[jobs]
backup_root`). Users log in with their old tokens; browser and MCP sessions start afresh. The
whole recipe, with the backup on the old machine: [Move a server to a new
machine](guide/backups.md#move-a-server-to-a-new-machine).

`restore` refuses:

| Message | Fix |
|---|---|
| a restore builds a NEW server directory and never writes over an existing one | give a new or empty directory; `restore` never writes over an existing one |
| `... checksum mismatch` (or an entry the manifest does not list) | the archive is damaged or was changed: copy it again |
| an archive of a newer graphersal-server or format | use that version, or try `--force-version` |

A database whose data the archive does not hold (a configuration-only backup, or one whose
backup failed) is skipped and reported. Copy its directory `databases/NAME/` from the old
machine with `rsync -aH` while both servers are stopped, or restore its own backup.

## The directory is refused by the permission check

You see:

```text
Error: refused: the server directory ./srv is not private to its owner: ./srv is writable by group or others (mode 0775); ./srv/access.toml is open to group or others (mode 0644) (it holds token hashes or tokens)
Help: the OS user owning the server directory is its operator: whoever else can change it, or read its secrets, could make themselves an admin; keep it private (as ssh keeps ~/.ssh)
  # owner only: mode 0700
  chmod 700 ./srv
  # owner only: mode 0600
  chmod 600 ./srv/access.toml
  # or let init set every mode at once
  graphersal-server init ./srv
```

`serve` and every console command check the directory first ([Security
Model](guide/security.md#the-directory-owner-is-the-operator)).

**Wrong modes**, for example a directory made by 0.2.0 or earlier (they did not set modes):
let `init` fix every mode at once. On an existing server directory `init` only adds missing
parts, keeps `server.toml` and lists what it tightened under `owner only`:

```bash
graphersal-server init ./srv
```

**Another owner**, for example you run the command as yourself but the directory belongs to the
service user `graphersal`: run it as the owner.

```bash
sudo -u graphersal graphersal-server db list --dir /srv/graphs
```

`root` is refused too on a directory it does not own. To give the directory to another user
for good:

```bash
sudo chown -R graphersal:graphersal /srv/graphs
```

**Another OS user who should administer**, without access to the owner's account: give them an
admin token and let them use `--token` ([The console with an admin
token](guide/security.md#the-console-with-an-admin-token)).

On Windows there is no permission check: the directory's ACL is the boundary.

## The server does not start

| Message | Cause and fix |
|---|---|
| `cannot listen on 127.0.0.1:8484: Address already in use` | another program uses the port: [The port is in use](#the-port-is-in-use) |
| `another graphersal-server already serves ./srv (its server.lock is held)` | a server already runs on this directory: `graphersal-server status --dir ./srv`, then `stop` it or use it |
| `a graphersal-server already runs on ./srv (pid N, URL)` (from `start`) | the same: see it with `status`, stop it with `stop` |
| `./srv/server.toml: unknown field ...` (with the line) | a typo or a key of another version in `server.toml`: fix the line it names. Every key is in the [Configuration Reference](reference/configuration.md) |
| `invalid listen address "...": ...` | `listen` must be `IP:PORT` (or `localhost:PORT`), such as `127.0.0.1:8484`; port `0` picks a free one |
| `./srv is not a server directory (no server.toml)` | wrong `--dir`, or the directory was never made: `graphersal-server init ./srv` |
| `refused: the server directory ... is not private to its owner` | [The directory is refused by the permission check](#the-directory-is-refused-by-the-permission-check) |
| `./srv/mcp.toml: ...` | a broken `mcp.toml` stops the server with the line it points at: fix it, or delete it while stopped (the defaults apply) |
| `the server exited before it was ready` (from `start`) | the last lines of the log follow the message; the whole log is `./srv/logs/server.log` |
| `the server (pid N) did not get ready within 60 s; it keeps running` | a large directory may still be scanning: watch `graphersal-server status --dir ./srv` and the log, or give `start --timeout 300` next time |

Run it in the foreground to see everything at once:

```bash
graphersal-server serve --dir ./srv
```

A broken `database.toml` or Store never stops the server: that database is listed as `failed`
with the reason ([A database is failed](#a-database-is-failed)).

## The port is in use

```text
Error: cannot listen on 127.0.0.1:8484: Address already in use (os error 48)
```

Find what holds the port:

```bash
lsof -nP -iTCP:8484 -sTCP:LISTEN
```

On Linux also:

```bash
ss -ltnp 'sport = :8484'
```

If it is an old graphersal-server, stop it with `graphersal-server stop --dir <its directory>`.
Otherwise use another port, for one start or for good:

```bash
graphersal-server start --dir ./srv --listen 127.0.0.1:8485
```

Or set `listen = "127.0.0.1:8485"` in `server.toml`. Port `0` picks a free port; `server.run`
and the start block name it.

## The server does not stop

`graphersal-server stop` asks the server to stop through its admin API, the same clean stop as
Ctrl+C: every database is unloaded and its Store closed. Unloading waits up to 30 seconds per
database for running requests and jobs, so a busy server takes a while.

| Message | Fix |
|---|---|
| `the server (pid N) did not stop within 60 s; it may still be unloading its databases` | wait, then `graphersal-server status --dir ./srv`; look at `./srv/logs/server.log`; give `stop --timeout 300` next time |
| `the server (pid N) could not be asked to stop: ...` (Windows) | Ctrl+C in the server's console, or `taskkill /PID N /F` (a hard stop) |
| `not running` | nothing to stop; a stale `server.run` was removed |

On Unix `stop` sends SIGTERM when the admin API cannot be reached (same clean stop). The last
resort is `kill -9 PID`: no clean unload, but no data loss either: every acknowledged commit is
in the journal, and the next start checks the databases ([A server was killed by
force](#a-server-was-killed-by-force)).

A server run by systemd or launchd: stop it with the service manager (`systemctl stop
graphersal-server`), else it may be restarted ([Running as a Service](guide/service.md)).

## A stale `server.run`

`status` says the server is stopped but reports a stale `server.run`, or a tool finds
`server.run` while nothing runs. The file is left behind by a server that did not stop cleanly
(killed, crashed, power loss). It means nothing without the lock: `server.lock` is held only
by a running server, and the OS releases it when the process ends.

Nothing to do: the next `serve` or `start` overwrites it. To remove it now:

```bash
graphersal-server stop --dir ./srv
```

`stop` on a stopped directory removes a stale `server.run` (holding the lock meanwhile) and
prints `not running`. It never removes `server.open`, so the next start still checks the
databases that were open. Do not delete `server.run` by hand while a server might run.

## A server was killed by force

A server killed by force (`kill -9`, an OOM kill, a power loss, a crash) leaves its databases'
Stores open. Nothing has to be repaired by hand first:

1. **The lock.** The OS releases `server.lock` with the process; `server.run` (and
   `server.open`) stay behind. `status` reports the stale `server.run` and, under "Last
   unclean shutdown", what the next start will check.
2. **The journal.** Every commit a client got an answer for is in its database's WAL; the next
   open of the Store replays it onto the last snapshot and cuts an interrupted last write (a
   commit in flight at the kill may or may not be there; it was never acknowledged).
3. **The check at the next start.** `serve` (or `start`) finds the stale files, says so in the
   start block (`UNCLEAN SHUTDOWN` and an "Unclean shutdown" section) and the log, records an
   audit event, and checks every database of `server.open` on a background thread: it is loaded
   again (also without `auto_load`: the server serves again what it served before), its Store's
   open report is kept, and the open Store is verified (`Store::verify`: every checksum,
   snapshot and WAL record). Each result is logged (`unclean shutdown: database 'x' is ok: ...`),
   audited (`database 'x' checked: ok (...)`) and written to `recovery.json`. Without
   `server.open` (a crash of a server older than 0.3.0) the open set is unknown and the
   `auto_load` databases are checked.
4. **Damage.** A Store the engine finds damaged at the open opens in maintenance mode, and one
   whose verify finds damage under the damage policy `maintenance` (the default) is frozen by
   the engine: the database is served **read-only** in the state `maintenance` with its damage
   report (`db list`, `db info`, `status`, the Databases tab, the Database settings). Queries read
   the intact data; writes are refused.
5. **Per database:** `db verify NAME` (or `--all`) scrubs a Store again at any time (exit 1 on
   damage). `db repair NAME [--name NEW]` builds a repaired copy as a NEW database (default
   `NAME-repaired`; the engine's `repair_to`: damaged snapshot chunks rebuilt from older
   snapshots and the WAL, what has no donor reported as lost, elements whose state differs from
   the journal reported as diverged; the copy is verified before it is registered; the damaged
   database is never written). Then either `db swap NAME NEW --yes` (the copy takes the name;
   the damaged one moves to `trash/`; grants, aliases, MCP settings and `database.toml` of the
   name stay) or keep both. A repaired copy starts a new history: older snapshots, marks and
   the store id (so the backup chain) are not carried. Layers and bases are not repaired.
6. **Or a backup.** `db backups NAME` lists the verified backups; `db restore NAME BACKUP`
   (unloaded) or `--name NEW` brings one back: everything after the backup is lost, so prefer a
   lossless repair when the repair report says so.

The same routes for scripts: `POST /api/admin/databases/<name>/verify`, `POST
/api/admin/verify`, `POST /api/admin/databases/<name>/repair`, `POST
/api/admin/databases/<name>/swap`, `GET /api/admin/recovery`
([Admin API](reference/admin_api.md#verify-repair-and-swap)).

## A database is in maintenance

`db list` shows the state `maintenance`: the database is loaded **read-only** because its Store
is damaged. Queries read the intact data; writes are refused. `db info` (and the Database
settings tab) show the damage report.

1. Read the report and check the Store again:

   ```bash
   graphersal-server db info acme --dir ./srv
   graphersal-server db verify acme --dir ./srv
   ```

2. Repair a plain database into a NEW one (the damaged one is never written):

   ```bash
   graphersal-server db repair acme --dir ./srv
   ```

   The output says what was rebuilt, what is lost and what diverged. The copy, `acme-repaired`,
   is verified before it is registered.

3. Check the copy, then let it take the name:

   ```bash
   graphersal-server db verify acme-repaired --dir ./srv
   graphersal-server db swap acme acme-repaired --yes --dir ./srv
   ```

   `acme` moves to `trash/`; the copy becomes `acme` with acme's grants, aliases, MCP settings
   and `database.toml`. It is loaded again when acme was loaded. Or keep both and point an alias
   at the copy.

4. Or restore a backup instead ([Restore a database from a
   backup](#restore-a-database-from-a-backup)). A backup loses everything after it; prefer the
   repair when its report shows nothing lost.

Layers and bases are not repaired: restore a backup. A repaired copy starts a new history: older
snapshots, marks and the store id (so the backup chain) are not carried; make a new backup
after the swap.

## A database needs a model

`db list` shows `needs-model` and the model's repository. The database's Store names an
embedding model whose blob neither it nor the server's `models/` holds (typically an import of
a snapshot exported without models). It cannot load until the model is there:

```bash
graphersal-server models add /models/potion-base-8M --dir ./srv
graphersal-server db load acme --dir ./srv
```

`models add` takes a model directory, a `.tar`/`.tar.gz` of one, a `.gsnap` carrying the model,
or the repository name when `models/` already holds it. Every waiting plain database is
resolved at once and becomes `unloaded`. More in [Embedding Models](guide/models.md).

## A database is failed

`db list` shows `failed` with a reason. Its directory, its `database.toml` or its Store could
not be read, or its last load failed. It never stops the server.

- `database.toml` does not parse (an unknown key, a typo in a hand edit): fix the line the reason
  names, then `db load NAME`.
- A query limit in `database.toml` or `server.toml` is invalid: the load fails with the reason;
  fix it and load again.
- The Store cannot be opened: `db verify NAME`, then repair or restore as for [a database in
  maintenance](#a-database-is-in-maintenance).


## 409: changed by someone else

```text
Error: the users and grants changed by someone else since you read them (now revision 3f2a...); nothing was changed
Help: changed by someone else: reload
```

Two people (or a person and a script) changed the same object at once: users and grants,
aliases, a database's settings, or the MCP settings. The later change was refused and nothing
changed. Read the current state again (the web UI does it for you) and repeat the change if it
is still needed. The console reads the current state on every command, so it meets this only
in a race. Details: [Revisions](reference/admin_api.md#revisions-concurrent-changes).

## A new name is refused

```text
Error: invalid name "acme-": a name ends with a lower-case letter or a digit
```

A new database, alias or user name has at most 32 characters (`[a-z0-9._-]`), starts and ends
with a letter or digit, holds no `..` and is no Windows device name (`con`, `nul`, ...). `jobs` is
reserved for databases and aliases. Pick a name that fits ([Names](concepts/concepts.md#names)).

Names made by 0.2.0 or earlier under the older rule keep working: they are served, granted,
aliased and restored under their own name. Only making a NEW name is refused.

`the name 'acme' is taken by a database (databases and aliases share one namespace,
case-insensitive)`: pick another name; `db list` lists the taken ones.

## A database is not loaded

```json
{"error": "database 'acme' is not loaded", "help": "load it: POST /api/admin/databases/acme/load", "state": "unloaded"}
```

Requests never load a database implicitly. An admin loads it:

```bash
graphersal-server db load acme --dir ./srv
graphersal-server db auto-load acme on --dir ./srv     # and at every start
```

`it is loading`: retry in a moment (`Retry-After: 1`). `it failed (...)`: [A database is
failed](#a-database-is-failed). `it needs the embedding model ...`: [A database needs a
model](#a-database-needs-a-model).

## A user gets 401 or 404

- `401 {"error", "login": "/"}`: no credentials (and no anonymous grant), or a token nobody holds
  (a revoked or replaced token, a typo). Check the token, or give the user a new one: `user token
  NAME`.
- `404` for a database that exists: the user has no grant on it. A database without a grant does
  not exist for that user. Grant it: `user grant NAME DATABASE read`.
- `404` for a database's real name while the alias works: the grant names the alias, and an
  alias grant reaches the database only through the alias ([Grants on
  aliases](guide/access.md#grants-on-aliases)).
- `404` on `/api/admin/...`: the user is not an admin. `user admin NAME on`.
- `409` "the graph is served read-only": a `read` grant, a read-only database or a base.

## Requests through a proxy get 403

Every request is checked by its `Host` (and a browser's `Origin`). Behind a reverse proxy the
public name is not one of the server's own names. Add it, then restart:

```toml
[http]
public_scheme = "https"
allowed_hosts = ["db.example.com"]
```

## A query fails at a limit

- `OptionAboveCeiling`: the query tried to raise or remove a configured limit
  (`g.with("evaluationTimeout", ...)` above the ceiling, or `0`). A query may only lower a
  configured limit. Raise the limit for the database (`db set NAME limit KEY VALUE`, then
  unload and load) or in `server.toml`.
- A `memory.limit` diagnostic: the query, import or write would take the process over its
  memory ceiling (`[limits] memory_limit`) and was rolled back. Look at `graphersal-server status
  --dir ./srv` (memory rows), unload unused databases, or raise the limit and restart.
- A limit change made with `db set` did not apply: it applies at the database's next load.

## An upload is refused with 413

The body is larger than `server.toml`'s `[limits] upload_bytes` (default 4 GiB). Raise it and
restart, or copy the file into the import inbox `./srv/imports/` and import it by name (no size
limit):

```bash
graphersal-server import ./srv --inbox big.gsnap --name big
```

## The last admin cannot be removed

`'admin' is the last admin: no other user with the admin role and a token would be left`. Make
another admin first, then remove, demote or revoke the first:

```bash
graphersal-server user add carol --admin --dir ./srv
graphersal-server user remove admin --dir ./srv
```

## A delete is refused

- the database is loaded: `db unload NAME` first;
- an alias names it: remove or flip the alias first (`db alias remove ALIAS`);
- it is a base that layers name: delete the layers first.

## MCP clients get 503, 404 or lose their session

- `503 "MCP is switched off on this server"`: an admin switched MCP off. `graphersal-server mcp
  on --dir ./srv`. Sessions are kept and go on.
- `404 "MCP is switched off for 'acme'"`: the database is hidden from MCP. `graphersal-server mcp
  db acme on --dir ./srv`.
- `404` on `/mcp` and `/db/<name>/mcp` while the rest works: `server.toml [mcp] enabled =
  false`. Set it to `true` and restart.
- `404` for a session id: the session ended (a new token for its user, the user removed, more than
  `[mcp] max_sessions` sessions). The client initializes again and selects its database again.

More in [MCP](guide/mcp.md).

## `--token` is refused

- `cannot reach the running graphersal-server at ...`: no server runs, or `--server` names
  another address. A token works only through a running server.
- `401`: the token is wrong or was replaced.
- `404` on admin commands: the token's user is not an admin.
- `--server` without a token is a usage error: set `--token` or `GRAPHERSAL_SERVER_TOKEN`.

## Everybody is logged out after a restart

Sessions survive a restart in `sessions/`. A warning at start that a sessions file cannot be read
or parsed means it was damaged: the server starts without those sessions (users sign in again,
MCP clients initialize again) and overwrites the file with the next change. Sessions also end
by `[sessions] lifetime_secs` and `idle_timeout_secs`.

A restored server backup never holds sessions: every user signs in again.

## A backup job was dropped at load

```text
WARN graphersal_server: database 'acme': dropped the backup job 'nightly': it writes '/mnt/old' outside the backup root (...); define it again with a path inside the root (server.toml [jobs] backup_root)
```

A stored backup job of a database (imported, restored or cloned from elsewhere, or after the
backup root moved) wrote outside `server.toml`'s `[jobs] backup_root`. It was removed from the
database's catalog before the database was served. Define it again in the database's
playground (Jobs panel) with a path relative to the backup root ([Backup
jobs](guide/backups.md#backup-jobs)).

## Model blobs are copied, not linked

A `linkWarning` in a model's or database's row, and a warning in the log: the file system
refuses hard links (another file system, FAT/exFAT, most SMB shares, FUSE mounts), so the model
blob was copied. Correct, but no disk saving. Keep `models/` and `databases/` on one file system
that supports hard links.

## Keep a log of a foreground server

`serve` logs to stderr. Keep it, with every audit line:

```bash
RUST_LOG=warn,graphersal_server::audit=info graphersal-server serve --dir ./srv 2>> ./srv/logs/serve.log
```

A server run by `start` already logs to `./srv/logs/server.log`. Nothing rotates the log:
truncate or rotate it while the server is stopped (or use `copytruncate`).
