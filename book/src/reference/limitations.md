# Known Limitations

What this version does not do, or does differently than you might expect.


- A **base is frozen forever** (by design: layers depend on its exact state). A base with data
  comes from an import with role base (`import ... --role base`, `POST /api/admin/import?...
  &role=base`) or from promoting an unloaded plain database (`db promote NAME`, `POST
  /api/admin/databases/<name>/promote`). Promotion is one way: no command turns a base back
  into an ordinary database. To change a base's data, build a new base (clone it before
  promoting, or import again) and rebuild the layers over it. A base cannot get an embedding
  model later, so a source naming a model that neither it nor `models/` carries is refused as
  a base: add the model (`models add`) and import again, or import it as an ordinary database,
  add the model, then promote it.
- A **Store import lacking a model blob** that `models/` does not hold either becomes a
  `needs-model` database (since Graphersal 0.7.1, like a snapshot naming a model it does not
  carry): add the model to `models/` (`models add`, or the Models tab) and it is resolved. As a
  base (`--role base`) such a source is refused (`409`): a base cannot get a model later.
- The **run administration** (the Runs tab, `/api/admin/runs`) is under the admin surface and
  is for admins only: an admin sees every client's query texts and can stop them. The databases' own pages (`/db/<name>/`) do not offer it. A run id
  is unique within its database only.
- A model archive is a **ustar `.tar`**, uncompressed or gzip-compressed (`tar -cf model.tar
  -C <model dir> .`, `tar -czf model.tar.gz ...`); zstd, bzip2, xz and `.zip` are not taken.
- A **layer `.gsnap` export carries no models** (an engine rule); export the layer with
  `materialize=true` (`--materialize`) for a standalone snapshot with the models.
- **A query limit change applies at the database's next load** (unload + load, or a restart):
  the engine fixes a loaded database's session limits when it loads it. The settings
  (`db info`, the Database settings tab) show both the limits in effect now and those of the
  next load. The Store's creation parameters are shown read-only; the server creates every
  database with the engine's defaults, and a backend's container version is not reported by the
  engine (0.8.0).
- **Memory figures are the engine's estimates** of each graph's data (loaded embedding models
  are not in a database's figure, but in the process's resident memory). The process's
  resident memory is read on Linux, macOS and Windows only (`null` elsewhere).
- No admin tools over MCP. Browser and MCP sessions survive a restart (`sessions/`, hashes
  only; [sessions/](server_directory.md#sessions)). The MCP usage statistics live in memory and start again at every
  restart.
- **Windows: no permission check of the server directory** (unix only): its ACL is the
  boundary; create it where only the account running the server may write.
- **Plain HTTP** (HTTP/1.1 and h2c, no TLS, so no HTTP/2 negotiation by ALPN): tokens and
  session cookies travel in clear text; keep the server on the loopback or put a TLS reverse
  proxy in front (`[http] public_scheme = "https"`). On HTTP/2 an answer of a request whose body
  the route did not read resets that stream (HTTP/1.1 closes the connection with a lingering
  close instead).
- **Prebuilt binary for Windows x86_64 only**; other platforms build from source (see
  [Download / build](../getting_started/installation.md)). On Windows the clean stop is Ctrl+C or Ctrl+Break in
  the server's console (there is no SIGTERM); a killed server leaves `server.run` behind, and the
  next `serve` replaces it (the lock is released by the OS) and checks the databases it had
  open ([After a crash](../troubleshooting.md#a-server-was-killed-by-force)).

