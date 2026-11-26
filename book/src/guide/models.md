# Embedding Models

Graphersal databases can hold Model2Vec embedding models for vector search (see the Graphersal
book at <https://graphersal.dev>). A model is a blob inside a Store. graphersal-server keeps ONE
copy of each model for the whole server and shares it with every database that uses it.

## How the server keeps models

- One copy of every model blob lives in `models/blobs/<2 hex>/<sha256>` (the Store's own blob
  layout).
- Each database that holds the model gets a **hard link** to that file in its
  `store/blobs/`. For the engine it is an ordinary blob file: a database's delete or prune
  removes only its link, and an export embeds the bytes.
- Every create, clone, import and restore links the new database's model blobs before the
  database appears. At start the server does the same for every idle database.
- `models/models.toml` is a derived index, rebuilt at every listing and model change. Edits to
  it are lost.

Where the file system refuses hard links (another file system, FAT/exFAT, most SMB shares, FUSE
mounts) the blob is **copied** instead: correct, but no disk saving. The server logs a warning
and shows it in an import's notes, in the model's row and in the database's row.

## List the models

```bash
graphersal-server models list --dir ./srv
```

Each model shows its repository, identity, licence, size, the databases using it and the
`needs-model` databases waiting for it. The web UI's Models tab shows the same.

## Add a model

From a Model2Vec model directory on the server's machine:

```bash
graphersal-server models add /models/potion-base-8M --dir ./srv
```

A directory that does not name its repository and licence needs them:

```bash
graphersal-server models add /models/my-model --repo org/my-model --license mit --dir ./srv
```

From a tar archive of a model directory (uncompressed `.tar` or gzip-compressed `.tar.gz`):

```bash
tar -czf model.tar.gz -C /models/potion-base-8M .
graphersal-server models add model.tar.gz --dir ./srv
```

The archive must be POSIX ustar; the model directory is its root or its only top directory.
zstd, bzip2, xz and `.zip` are not taken. The archive is unpacked safely into `tmp/` first and
refused as a whole for absolute paths, `..`, links, devices, more than 1000 entries or nesting
deeper than 16 directories.

From a packed snapshot that carries models:

```bash
graphersal-server models add /data/with-models.gsnap --dir ./srv
```

From another computer: the web UI's Models tab uploads a `.gsnap` or a `.tar`/`.tar.gz` with a
progress bar ("A file from this computer"). The upload limit is `server.toml`'s `[limits]
upload_bytes`.

After a model is added, every plain `needs-model` database waiting for it gets it and becomes
`unloaded`, ready to load.

## Databases that need a model

A snapshot may name a model without carrying it (an export with `--no-models`), and a Store may
lack a model's blob. At the import:

- when `models/` holds the model, the import takes it from there;
- otherwise the database is created in the state **`needs-model`** (the model's repository is
  shown). It cannot load until the model is added.

Fix it by adding the model, from any source above, or by its repository when `models/` already
holds it:

```bash
graphersal-server models add org/model --dir ./srv
graphersal-server db load acme --dir ./srv
```

**Verify finds a missing model blob.** A Store that lost a model's blob (a link removed by hand,
an older import) fails to load with `needs-model`. `db verify NAME` (or Verify on the Databases
tab) names it: "the embedding model org/model (blob f0ed7648) is missing", with the command that
adds it (`models add org/model` when `models/` holds it, else `models add SOURCE`). This is not
damage: the graph data is intact and no repair is needed. A plain database that is not loaded
becomes `needs-model` (marked in its `database.toml`), so adding the model resolves it as for an
import. The Settings tab shows the last verify and the missing model.

**A base cannot get a model later.** An import with `--role base` of a source naming a model
that neither it nor `models/` carries is refused. Add the model first and import again, or
import it as an ordinary database, add the model, then promote it.

## Remove a model

```bash
graphersal-server models remove org/model --dir ./srv
```

Refused while any database holds one of its blobs, waits for it, or names it in its Store's
catalog. The refusal names those databases.

## Backups and copies

- A database backup (`db backup`) carries the database's model blobs.
- A server backup with `--with-databases` carries every shared model blob.
- A copy of the whole directory keeps the hard links only with `rsync -aH`; `cp -r` or a zip
  copies each linked blob again (bigger, still correct).
