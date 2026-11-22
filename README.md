# graphersal-server

**EXPERIMENTAL. Private until its first public version.**

A server for [Graphersal](https://github.com/graphersal/graphersal) graph databases: one process
serving many databases (plain and layered over a shared frozen base) from one server directory,
over HTTP, with one MCP endpoint where a session picks its graph, and an admin panel in the
Graphersal playground.

> **No access control.** Anyone who reaches the port can read and change every database. The
> server listens on the loopback (`127.0.0.1:8484`) by default and logs a warning when it is
> told to listen on any other address.

## Status

This version holds `init`, `serve`, the server lock, the run file, the health probes,
`/api/info` and the database registry (read at start, no HTTP routes yet). Loading databases,
the admin API, the playground and MCP come in later versions.

## Use

```bash
cargo build --release
target/release/graphersal-server init ./srv              # create a server directory
target/release/graphersal-server serve --dir ./srv       # serve it (Ctrl+C stops it)
target/release/graphersal-server serve --dir ./srv --listen 127.0.0.1:0   # a free port
```

`serve` writes the address it actually bound into `<dir>/server.run` while it runs (removed at
stop); a second `serve` on the same directory is refused (`server.lock`).

## Server directory

```
<server-dir>/
  server.toml     listen address, MCP options (placeholder), defaults for new databases
  server.lock     one running server per directory
  server.run      the running server's address (only while it runs)
  aliases.toml    acme = "acme-2026.3"   (an alias names a database)
  databases/<name>/
    database.toml kind, role, base, auto_load, read_only, created, description
    store/        the Store
  models/blobs/  backups/  trash/  tmp/  logs/
```

There is no registry file: the databases are the directories under `databases/`. Names are
`[a-z0-9][a-z0-9._-]{0,62}`, unique case-insensitively across databases and aliases. A broken
database directory is listed as `failed` with the reason; it never stops the server. New
databases are built in `tmp/` and renamed into `databases/`; what a crash leaves in `tmp/` is
removed at the next start. A `role = "base"` database is created read-only and stays read-only
while a layer names it.

## HTTP

| Path | What |
|---|---|
| `GET /healthz` | liveness: `200 {"status":"ok"}` |
| `GET /readyz` | readiness: `200 ready`, `503 loading` / `503 stopping` |
| `GET /api/info` | `{"server":"graphersal-server","version":...,"experimental":true,"admin":true}` |
| `GET /` | a placeholder text (the playground comes later) |

## Building

The crate depends on Graphersal's hosting crates by path: a checkout of the `graphersal`
repository must sit beside this one (`../graphersal`).

## Licence

Apache-2.0, see `LICENSE-APACHE` and `NOTICE`.
