# Backups and Restore

Three kinds of backup, from small to large:

| What | Command | Holds | Restores into |
|---|---|---|---|
| one database | `db backup NAME` | the database's Store (with its saved queries, backup jobs and models) | the same server: over the database or as a new one |
| scheduled backups of one database | a backup job in the database's playground | incremental directory backups | (the engine's Store tools) |
| the whole server | `backup [--with-databases]` | the configuration, users, grants, aliases, settings; with `--with-databases` every database too | a NEW server directory, on any machine |

## Back up one database

```bash
graphersal-server db backup acme --dir ./srv
```

```text
Backed up database 'acme'
  file: ./srv/backups/acme/2026-11-26T02-18-34Z.zip
  size: 10.4 KiB (10681 bytes)
  # restore it into a new database
  graphersal-server db restore acme 2026-11-26T02-18-34Z --name NEW --dir ./srv
```

The backup is a verified ZIP: every entry is read back and checked, and damage fails the
backup (nothing is kept). The database may be loaded: the backup is taken from its open Store.
A sidecar `<UTC time>.toml` beside the archive records the database's kind, role, base,
description and, for a layer, its base's identity.

List the backups of a database (also of a deleted one):

```bash
graphersal-server db backups acme --dir ./srv
```

Backups stay when their database is deleted. Nothing removes them automatically: delete old
ones from `backups/<name>/` yourself (the `.zip` with its `.toml`).

The web UI's Backups tab does the same: back up, list, restore.

## Restore one database

Over the database itself (unload it first):

```bash
graphersal-server db unload acme --dir ./srv
graphersal-server db restore acme 2026-11-22T10-00-00Z --dir ./srv
graphersal-server db load acme --dir ./srv
```

The replaced database moves to `trash/acme-<UTC time>/`. The database keeps its auto-load
and read-only flags, its description and its grants.

Into a new database, keeping the current one:

```bash
graphersal-server db restore acme 2026-11-22T10-00-00Z --name acme-old --dir ./srv
```

Everything written after the backup is lost in the restored copy.

- **A deleted database** can be restored under its own name: `db restore acme BACKUP` makes it
  again. Its grants do NOT come back (they were removed with it): grant access again.
- **A layer's backup** holds only the layer. It restores only over a base database with the
  same identity as when the backup was taken.
- **A base that layers name** is not replaced: restore it into a new name.

## Backup jobs

Scheduled backup jobs (the playground's Jobs panel of a database, `POST
/db/<name>/api/jobs/define` with `"action": "backup"`; incremental directory backups run by
the database's job ticker while it is loaded, or at once with `jobs/run`) write only into the
**backup root**, `server.toml [jobs] backup_root` (default `backups/jobs/`; it may name a
mounted storage):

- a job's `path` is relative to the root: `"path": "nightly"` writes into
  `<backup root>/nightly/`; the job stores the resolved path;
- an absolute `path` is refused when the job is defined, also one inside the root (`200
  {"ok": false, "errors": [{"field": "path", "message", "help"}]}`, the form's field error);
- a path leaving the root (`..`, a symbolic link inside the root pointing out of it) is
  refused the same way by the database's file root;
- a catalog loaded through the playground (`catalog/load`) with a job writing outside the root
  is refused (`403`, nothing loaded);
- the Store panel's fork, attic fork and backup take a `dir` inside the root only (relative to
  it; outside: `403` "paths must lie inside the server's file root"); without a `dir` they
  write next to the database's store as before;
- at load, a stored job writing outside the root (an imported, restored or cloned store, or
  a root moved in `server.toml`) is **dropped** from the database's catalog before the
  database is served, with a warning on stderr naming the job and its path: define it again
  with a path inside the root.

Queries (the playground's `execute`, the MCP tools) never touch files: they cannot define a
backup job at all. The admin backups above (`backups/<name>/`) are the server's own and are not
affected. Keep the root's other writers trusted: a symbolic link created inside it after a
job was checked is not seen.

## Back up the whole server

`graphersal-server backup --dir DIR [--out FILE] [--with-databases]` (also through a running
server, the admin API's `GET /api/admin/server-backup`, and the web UI's Backups tab, "Download
server backup") writes ONE ZIP archive of the server itself, mode `0600`, through `FILE.part`
renamed when complete (default `FILE`: `./<dir name>-server-backup-<UTC time>.zip`):

| Entry | What |
|---|---|
| `server.toml`, `aliases.toml`, `access.toml`, `mcp.toml` | the configuration, as the files are (each when the directory has it) |
| `databases/<name>/database.toml` | every database's settings (kind, role, base, flags, description, `[limits.query]`), not its data |
| `models.json` | the shared models: repository, identity, licence, blob hash and size, the databases using them |
| `databases/<name>/backup.zip`, `backup.toml` | with `--with-databases`: a fresh VERIFIED backup of the database (the same as `db backup`: its Store, with the catalog holding its saved queries and backup job definitions, and its model blobs) and its sidecar |
| `models/blobs/<2 hex>/<sha256>` | with `--with-databases`: every shared model blob |
| `manifest.json` (last) | `format` (`graphersal-server-backup`), `formatVersion` (1), `serverVersion`, `createdAt`, `sourceDir` (the directory's name, never its path), `withDatabases`, `files` (every other entry with `bytes` and `sha256`), `databases` (name, kind, role, base, state, `included`, `reason` when not), `excluded` (path and reason) |

Never in it (listed under `excluded` with the reason): `sessions/` (restoring sessions would
revive logins), `logs/`, `server.lock`, `server.run` (a run's operator token), `server.open` and
`recovery.json` (the state of a run), `tmp/`, `trash/`, `imports/`, `backups/` (the per-database
backups and the backup jobs' root: keep them on their own), `models/models.toml` (derived); and
without `--with-databases` every `databases/<name>/store/` and `models/blobs/`. A database's
backup jobs and saved queries live in its Store's catalog, so they come with its data.

A running server copies the configuration files under their writers' locks (the registry's read
lock for `aliases.toml` and every `database.toml`, then the access file's lock, then the MCP
settings' lock, the order of [Concurrent changes](../reference/server_directory.md#concurrent-changes)): the archive is one
consistent state. The databases' backups are taken afterwards, one at a time (a loaded one from
its open Store); one that fails (a damaged or failed database) is left out, named in the
manifest and in the command's output, and the backup still succeeds. The console on a stopped
directory holds the server lock. Every server backup is audited. The archive holds the token
hashes: keep it like a password ([Security Model](security.md#server-backups-are-secrets)).

`graphersal-server restore FILE --dir NEWDIR [--force-version]` builds a NEW server directory
from an archive:

1. **Refusals first, nothing written.** `NEWDIR` must be missing or an empty directory of this
   OS user (an existing server directory, a running server's included, is never written).
   Every entry's CRC-32 and SHA-256 must match the manifest; an entry the manifest does not
   list, a listed entry that is missing, a name outside the layout above or a changed manifest
   refuses the archive (`cannot restore the server backup ...: ... checksum mismatch`). An
   archive of a newer format version or written by a newer graphersal-server is refused unless
   `--force-version` (its files must still read: an unknown key is refused as at a start). Every
   configuration file is parsed as the server would.
2. **The layout**, owner only: `NEWDIR` and every directory of the layout `0700`, every file the
   restore writes `0600`; `server.toml` as it was.
3. **The databases** from their embedded backups (bases and plain databases before the layers
   over them; each through the ordinary restore of a backup, so a database is registered whole
   or not at all), then their `database.toml` as it was (flags, description, limits, creation
   time). The shared model blobs go into `models/blobs/`; the server links them at its start.
4. **The configuration that names databases and users**, last: `aliases.toml`, `mcp.toml`,
   `access.toml`.

A database whose data the archive does not hold (a configuration-only archive, or one whose
backup failed) is SKIPPED and reported, never half-registered: its aliases stay in
`aliases.toml` (dropped at start until the database exists), its grants and MCP settings stay
in their files. Copy its directory (`databases/<name>/`, with `rsync -aH` while both servers are
stopped) into the new directory to bring it back with them; a database created or imported
under that name starts without them ([Users, Tokens and Grants](access.md#grants-never-outlive-their-name)).
A failure removes everything the restore wrote. The users keep their tokens (the hashes came
along); browser and MCP sessions start afresh. The output ends with the next command, `serve
--dir NEWDIR`.

## Move a server to a new machine

```bash
# 1. on the old machine: a full backup (through the running server, or with it stopped)
graphersal-server backup --dir /srv/graphs --with-databases --out /secure/graphs-full.zip
# 2. copy it over a private channel; it holds the token hashes
scp /secure/graphs-full.zip new-host:/secure/
# 3. on the new machine, as the OS user that will own the server directory
graphersal-server restore /secure/graphs-full.zip --dir /srv/graphs
# 4. check server.toml (listen address, [http] allowed_hosts, an absolute [jobs] backup_root)
#    for the new machine, then serve it (or install the service unit: Running as a service)
graphersal-server serve --dir /srv/graphs
# 5. stop the old server; keep or remove the archive like a secret
```

Check after the start: `graphersal-server status --dir /srv/graphs`, `db list` (every database
there, none `failed`), `user list`. Users log in with their old tokens. Per-database backups
(`backups/`) and the import inbox do not move: copy `backups/` by hand when you want the
history. A configuration-only archive (without `--with-databases`) moves the settings, users
and grants only; then copy each database's directory `databases/NAME/` into the new
directory's `databases/` (`rsync -aH`) while both servers are stopped, or restore their own backups (`db restore`).

## Schedule a server backup

There is no scheduled server backup built in (the backup jobs belong to one database's engine).
Run `graphersal-server backup` from cron or a systemd timer, as the directory's owner. A
crontab line for a nightly full backup at 03:00 (`%` must be escaped in crontab):

```text
0 3 * * * /usr/local/bin/graphersal-server backup --dir /srv/graphs --with-databases --out /secure/graphs-$(date +\%F).zip
```

Remove old archives yourself, and keep them like secrets.

## Copy the whole directory

The server backup above is the supported way. Otherwise stop the server (or accept the state
of a running one), then copy the directory with
`rsync -aH` (`-H` keeps the hard links between `models/blobs/` and the databases). `cp -r` or a
zip duplicates every linked model blob: bigger, still correct. For a consistent copy of one
database while it is served, use `graphersal-server db backup NAME`.
