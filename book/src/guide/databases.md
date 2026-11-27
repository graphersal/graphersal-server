# Databases and Aliases

How to make, load, change, copy and remove databases, and how to give them second names. Every
task works three ways: the console (shown here), the web UI's Databases, Import and Export
tabs, and the [Admin API](../reference/admin_api.md). The kinds and states are explained in
[Concepts](../concepts/concepts.md).

## List and inspect

```bash
graphersal-server db list --dir ./srv
```

```text
── Databases (1) ─────────────────────────────────────────────────────────────
  NAME  STATE     KIND   FLAGS  ON DISK  MEMORY  DESCRIPTION
  acme  unloaded  plain         9.5 KiB          Acme
```

`ON DISK` is the database's directory (snapshots, journal, models). `MEMORY` is a loaded
database's graph (a layer's own changes; its shared base counts once). A Problems section lists
failed databases and missing models.

Everything about one database (or the database an alias names):

```bash
graphersal-server db info acme --dir ./srv
```

`db info` shows the kind, role, base and aliases; the state, flags and sizes; the query limits;
the MCP switches; the grants; the backups; the damage and recovery report; and the Store's
creation parameters (store id, damage policy, chunk size, created, backend, format version).
The web UI's Settings tab shows the same.

## Create

```bash
graphersal-server db create acme --description "Acme" --dir ./srv
```

Options:

| Option | Meaning |
|---|---|
| `--description TEXT` | one line of free text |
| `--auto-load` | load it at every server start |
| `--read-only` | serve it read-only |
| `--schema FILE.json` | a Graphersal schema for the new graph |
| `--role base` | an empty base (frozen, read-only) |
| `--base BASE` | a layer over the base database `BASE` |
| `--kind plain\|layer` | the kind; `--base` alone already makes a layer |

The database is built in `tmp/` and renamed into `databases/` in one step: a failure leaves no
database. Its Store takes `server.toml`'s `[defaults]` (`chunk_bytes`, `damage_policy`). A new
database is `unloaded`.

## Load and unload

Only a running server loads a database:

```bash
graphersal-server db load acme --dir ./srv
graphersal-server db unload acme --dir ./srv
```

`db load` waits until the load ends (`--no-wait` returns at once) and prints the address
(`served at: http://127.0.0.1:8484/db/acme/`). The result is `loaded`, `failed` (with the
reason) or `needs-model`. Loading a layer loads its base first, shared by every layer over it.

`db unload` gives running requests and jobs up to 30 seconds, then closes the Store. A base
that loaded layers use cannot be unloaded.

Load it at every start, or stop doing so:

```bash
graphersal-server db auto-load acme on --dir ./srv
graphersal-server db auto-load acme off --dir ./srv
```

## Read-only

```bash
graphersal-server db read-only acme on --dir ./srv
graphersal-server db read-only acme off --dir ./srv
```

Applies at once to a loaded database: writes through its HTTP API and MCP endpoint are refused;
a running request finishes. A base stays read-only for good.

## Description and query limits

```bash
graphersal-server db set acme description "Acme's graph" --dir ./srv
graphersal-server db set acme limit timeout_ms 5000 --dir ./srv
graphersal-server db set acme limit timeout_ms --clear --dir ./srv
```

A limit set here overrides `server.toml`'s `[limits.query]` for this database. The keys are
`timeout_ms`, `max_operations`, `max_string_size`, `max_array_size`, `max_map_size`,
`max_traversers`, `max_materialized_bytes` and `max_value_depth` ([Query
limits](../reference/configuration.md#query-limits)). `0` lifts the limit for this
database (for `max_value_depth`: the default, 128). `--clear` lets the server's default apply
again.

**A limit change applies at once.** A loaded database's runs that start afterwards use it; a
query already running keeps the limits it started with. A database that is not loaded uses it
from its next load. A hand edit of `database.toml` (or `server.toml`) applies at the next load,
or with the next limit change through the server; until then `db info` (and the Settings tab)
show the limits in effect now beside the files' values, and the tab offers "Unload and load
now…".

Every settings change names the revision it was based on. When someone else changed the same
database meanwhile, the change is refused with `409` "changed by someone else: reload" and
nothing changes ([409: changed by someone else](../troubleshooting.md#409-changed-by-someone-else)).

## Clone

```bash
graphersal-server db clone acme acme-copy --dir ./srv
```

A fork of `acme`'s latest state as the new database `acme-copy`: same kind, role, base,
description and read-only flag, `auto_load` off, a new Store identity, independent afterwards.
The source may be loaded.

## Promote to a base

```bash
graphersal-server db unload products --dir ./srv
graphersal-server db promote products --dir ./srv
graphersal-server db create acme --base products --dir ./srv
```

`db promote` turns an UNLOADED plain database into a base: read-only, frozen for good. Layers
can be created over it at once. Use it for a large shared data set (a catalogue, a reference
graph) that many databases build on: each layer stores only its own changes, and the base is in
memory once.

**There is no way back.** No command turns a base into an ordinary database. To change a base's
data, build a new base (clone the database before promoting it, or import again) and rebuild
the layers over it.

Refused: a loaded database (unload it first); a layer (export it with `--materialize` and
import the file with `--role base`); a `needs-model` database (add the model first: a base
cannot get a model later).

## Reset a layer

```bash
graphersal-server db reset-layer scratch --yes --dir ./srv
```

Discards every change the layer database `scratch` made over its base, and only those: the
vertices and edges it added, the base elements it changed or removed, and its own schema and
catalog changes (saved queries, index definitions) go back to the base's. The base and every
other layer over it stay exactly as they were. Afterwards `scratch` reads exactly its base.

**Kept:** the name, kind and base, the description, the read-only and auto-load flags, the
query limit overrides (`database.toml` is not touched), the grants (also those given through an
alias), the aliases, the MCP settings, the layer's scheduled job definitions, and the layer's
Store with its history. A loaded layer is reset in place and stays loaded (`applied: in place`):
its pages and MCP sessions see the base's data at their next request; a query running on it
finishes first (the reset waits up to 30 s, then refuses and names the runs to cancel). `--yes`
is required: the command discards data.

**The reset is one commit of the layer's own Store** (the engine's layer reset): nothing goes to
`trash/`, and the Store's history keeps the state before. A kill in the middle leaves the layer
either as it was or reset with its jobs, never in between. To go back, roll the layer's Store
back to the commit before the reset (its playground's Store menu, which keeps the rolled-back
commits in the attic as the undo), or restore a backup (`db restore`).

Refused: a plain database (restore a backup instead), a base (reset the layers over it), a
failed database, and a layer that is being loaded. Web UI: "Reset layer…" in the row's "…"
menu (layer rows only), with a confirmation naming what is lost and what is kept.

## Delete

```bash
graphersal-server db unload acme --dir ./srv
graphersal-server db delete acme --dir ./srv
```

Only an unloaded database. Its directory moves to `trash/acme-<UTC time>/`; nothing empties
`trash/` automatically. To bring it back by hand: stop the server and move the directory back
into `databases/`.

Refused while an alias names the database (remove or flip the alias first) and for a base that
layers name.

**Every grant on the database is removed with it** (the output says `grants removed: N`). A
later database or alias of that name inherits nothing, and a restore of its backup does not
bring the grants back ([Grants never outlive their
name](access.md#grants-never-outlive-their-name)).

## Aliases

```bash
graphersal-server db alias set prod acme-2026-11 --dir ./srv     # create
graphersal-server db alias set prod acme-2026-12 --dir ./srv     # flip: takes effect at once
graphersal-server db alias rename prod live --dir ./srv
graphersal-server db alias remove live --dir ./srv
```

- A **flip** re-points the alias for every new request, for open MCP sessions that selected
  it and for the grants on the alias.
- A **rename** moves the alias's grants and its MCP settings to the new name; open MCP sessions
  that selected the old name move too (their next answer carries one note). `/db/<old
  name>/...` answers `404` afterwards.
- A **removal** removes the grants on the alias and its MCP settings.

Databases have no rename: point an alias at a database instead. A broken alias (its database
is gone) is dropped at start and reported under the listing's problems.

## Import

A new database from a file or a Store:

```bash
graphersal-server import ./srv /data/acme.gsnap --name acme
```

| Source | Result |
|---|---|
| packed snapshot `.gsnap` | a plain database, with the embedding models it carries |
| layer snapshot `.layer.gsnap` | a layer over the base database with the snapshot's base identity (`--base` names it, else the only base that matches) |
| Store directory or `.gstore` file | a copy as a directory Store, with its creation parameters |
| GraphSON (`.json`, `.jsonl`, `.graphson`) | a plain database; data GraphSON cannot hold is skipped and noted |
| GraphML (`.xml`, `.graphml`) | a plain database |

The format is detected from the content; `--format snapshot|store|graphson|graphml` overrides
it. `--description TEXT` sets the description. `--role base` makes a base of a plain source.

Through a running server, the server reads `SOURCE` itself (same machine): the console sends
the absolute path.

**Embedding models.** A source may name a model it does not carry. When `models/` holds it, the
import takes it from there. Otherwise the database is created in the state `needs-model` and
loads once the model is added ([Embedding Models](models.md)). As a base such a source is
refused: a base cannot get a model later.

**Scheduled jobs.** A Store or snapshot from elsewhere may carry backup jobs writing outside
this server's backup root (an absolute path of the old machine). The import lists them under
`jobs:` with whether they run; such a job never runs here. `--rebase-jobs` moves each such backup
job under the backup root, so it keeps running ([Jobs from an imported
store](backups.md#jobs-from-an-imported-store)).

**From another machine.** Two ways:

- Upload in the web UI's Import tab ("A file from this computer"), with a progress bar. The
  size limit is `server.toml`'s `[limits] upload_bytes` (default 4 GiB).
- Copy the file into the server's **import inbox**, `./srv/imports/`, by any means (scp, a
  mounted volume, a shared folder), then import it by name:

```bash
scp acme.gsnap server:/srv/graphs/imports/
graphersal-server import ./srv --list
graphersal-server import ./srv --inbox acme.gsnap --name acme
```

The file stays in the inbox: remove it yourself when done. The inbox is flat (one entry name,
no paths). Entries starting with `.` (a copy in progress named `.acme.gsnap.part`) are not
listed. A symbolic link leading outside the inbox is refused.

Every import is built in `tmp/` and renamed into `databases/` in one step: a failed import
leaves nothing behind.

## Export

```bash
graphersal-server db export acme --out acme.gsnap --dir ./srv
```

A packed snapshot (`.gsnap`) of the database's latest state, with its embedding models.
`--no-models` leaves the model blobs out (an importer then resolves them by name). A layer is
exported as a layer snapshot, which loads only over the same base and never carries models;
`--materialize` writes a standalone full snapshot of everything the layer shows instead.

The database may be loaded. Through a running server, the console streams the download into
`FILE.part` and renames it when complete. The web UI's Export tab downloads the same file.

## Running queries

The Runs tab, and `GET /api/admin/runs`, list the queries that wait or run on every loaded
database, whatever their client, with a Stop button per run ([Runs](../reference/admin_api.md#runs)).
It is for admins only: an admin sees every client's query texts.
