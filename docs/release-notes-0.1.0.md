# graphersal-server 0.1.0

**EXPERIMENTAL.** The first version of graphersal-server. Everything here (commands, files,
routes, answers) may change without notice in later versions.

graphersal-server serves many [Graphersal](https://github.com/graphersal/graphersal) graph
databases from ONE process and ONE server directory, over HTTP:

- **Databases.** A database is a plain Graphersal Store, or a layer over a shared, frozen base
  database (many layers, one base in memory). Create, load, unload, clone, promote to a base,
  delete (into `trash/`), read-only and auto-load flags, aliases that flip at once.
- **Per database:** the Graphersal playground and its HTTP API under `/db/<name>/` (Gremlin in
  Rhai and GQL), resolved per request (also through an alias).
- **Transfer:** verified ZIP backups and restores, `.gsnap` exports, imports of snapshots,
  Stores, GraphSON and GraphML (by upload or by a path on the server).
- **Embedding models:** shared Model2Vec models in the server directory (`models/`), added by
  upload or from a directory, hard-linked into the databases that use them.
- **MCP:** one endpoint, `/mcp`, lets an AI agent list the databases and pick one per session;
  `/db/<name>/mcp` serves one database. An optional token guards both.
- **Administration:** the admin HTTP API (`/api/admin/...`), the console
  (`graphersal-server db ...`, `import`, `models ...`, which goes through a running server's
  admin API or works on the directory itself) and the server's web UI at `/`.
- **Operations:** health and readiness probes (`/healthz`, `/readyz`), one server per directory
  (`server.lock`), the bound address in `server.run`, a clean stop on Ctrl+C / SIGTERM (Ctrl+C
  or Ctrl+Break on Windows).

## Getting started

Windows x86_64: download `graphersal-server-v0.1.0-windows-x86_64.zip` from this release, check
it against `SHA256SUMS`, unpack it, then in a console:

```text
graphersal-server init .\srv
graphersal-server serve --dir .\srv
```

Open `http://127.0.0.1:8484/` for the web UI. Other platforms build from source (Rust 1.88 or
newer; the Graphersal 0.7.0 crates come from crates.io):

```bash
git clone https://github.com/graphersal/graphersal-server.git
cd graphersal-server
cargo build --release
target/release/graphersal-server init ./srv
target/release/graphersal-server serve --dir ./srv
```

The README covers the console, the HTTP routes, the web UI and the server directory; `docs/`
holds the admin API (`admin-api.md`), MCP (`mcp.md`) and the server directory
(`server-dir.md`) in detail.

## Known limits

- **No access control.** Anyone who reaches the port can read, change and delete every
  database. The server listens on the loopback (`127.0.0.1:8484`) by default and warns when told
  to listen elsewhere: keep it on the loopback. The `Host` and `Origin` checks guard against DNS
  rebinding and cross-site pages; they are not authentication. The MCP token guards only the MCP
  endpoints.
- **HTTP/1.1 only**, plain HTTP: no TLS, no HTTP/2.
- **Prebuilt binary for Windows x86_64 only**; Linux, macOS and the rest build from source.
- A **base is frozen forever**; promotion to a base is one way.
- No admin tools over MCP; MCP sessions live in memory (a restart forgets them).
- No memory figures per database in the listings (only the size on disk).
- Model archives are uncompressed ustar `.tar` files only.
- The README's "Known limitations" lists the rest.

## Licence

Apache-2.0 (`LICENSE-APACHE`, `NOTICE`). The licences of the third-party crates built into the
binary are in `THIRD-PARTY-LICENSES.html` inside the archive.
