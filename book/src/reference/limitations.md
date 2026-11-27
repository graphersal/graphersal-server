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
- **A layer reset waits for running queries** at most 30 s (the graph's write lock), then
  refuses and names the runs to cancel; it is undone by a rollback of the layer's Store (its
  playground's Store menu) or a backup, not by a console command.
- A **layer `.gsnap` export carries no models** (an engine rule); export the layer with
  `materialize=true` (`--materialize`) for a standalone snapshot with the models.
- **A query already running keeps its limits** when they change; only the runs that start
  afterwards use the new ones. A hand edit of a limit in a file applies at the next load. The
  Store's creation parameters are shown read-only; the server creates every
  database with the engine's defaults, and a backend's container version is not reported by the
  engine (0.9.0).
- **Memory figures are the engine's estimates** of each graph's data (loaded embedding models
  are not in a database's figure, but in the process's resident memory). The process's
  resident memory is read on Linux, macOS and Windows only (`null` elsewhere).
- **MCP profiles reach a connected agent at its next request**, but the server cannot push
  `notifications/tools/list_changed` over HTTP: an agent that keeps the tool list of its session
  start sees new tools after it reconnects. On `/mcp` saved queries are never tools of their own
  (graphersal-mcp's directory mode), and `initialize`, `tools/list` and `resources/list` serve
  the server default profile for the whole session: a database's own profile (its instructions,
  its tools, its saved-query tools) reaches the agent only as tool-result text, the block
  "Instructions for this database" after `use_graph` and on the first call on that database
  ([On /mcp](../guide/mcp.md#on-mcp)); a tool only that database's profile offers is callable
  there but not in the session's tool list. A tool's `graph_info` answer on `/db/<name>/mcp`
  may say `mcpReadOnly: false` for a reader whose `readOnly` is `true` (the server serves the
  endpoint itself; graphersal-serve sets that field from its own request state); the
  `readOnly` field and the tools offered are right.
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

