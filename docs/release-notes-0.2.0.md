# graphersal-server 0.2.0

**EXPERIMENTAL.** Everything here (commands, files, routes, answers) may change without notice
in later versions.

0.2.0 adds **access control**: users with tokens, grants per database, browser logins and MCP
per principal. It also adds the settings needed to run the server behind a TLS proxy, query
limits, sessions that survive a restart, an import inbox, a confined backup root and alias
renames. It builds on **Graphersal 0.8.0** (the four crates graphersal, graphersal-serve,
graphersal-session and graphersal-mcp, pinned exactly at `=0.8.0`).

**Upgrading from 0.1.0:** the server is closed by default. The first `serve` of a 0.1.0
directory creates the first admin and prints its token once on stderr. Scripts and agents that
called the HTTP API or `/mcp` without credentials now get `401`: give them a user token
([`access.md`](access.md)). `[mcp] token` in `server.toml` is refused at start; remove it.

## Access control

The new page [`docs/access.md`](access.md) describes the whole model.

- **`access.toml`** in the server directory holds the users (roles, the SHA-256 of the token
  and its last four characters, never the token) and the grants: `subject` (`user:<name>`,
  `role:<role>`, `anonymous` = everyone), `database` (a name or `*`), `level` (`read` or
  `write`). It is written atomically, mode `0600` on unix.
- **The first admin.** `init`, or the first `serve` of a directory without `access.toml`,
  creates the user `admin` and prints its token ONCE on stderr in a framed block (colour on a
  terminal). stdout still carries only the address line.
- **Levels.** A principal reaches a database at its highest grant, capped by the database's
  mode (read-only or a base: `read`). The `admin` role means write on every database plus the
  admin API.
  - A database without a grant is hidden: `404` like an unknown name, and never listed.
  - A `read` grant makes every request on the database read-only.
- **Enforcement on every route.** Only the probes, the root `GET /api/info` (minimal for
  anonymous) and the UI files stay open.
  - The admin API (Runs and Users included) is for admins: `401` for anonymous, `404` for a
    user without the admin role.
  - For `/db/<name>/...` without a grant, anonymous gets `401` for every name (a page gets
    `303` to the login page with `?next=`), and a user gets `404`.
  - Wrong credentials are `401`, never downgraded to anonymous.
- **Tokens:** 256 random bits (64 hex digits). Only hashes are kept and tokens are compared in
  constant time. Listings show `…last4`.
- **The operator token:** made fresh at every `serve` and written to `server.run` (now mode
  `0600`). The console sends it automatically, so console commands on the server's machine
  need no token.
- **Users and grants:**
  - new admin routes `/api/admin/users...` and `/api/admin/grants...`;
  - console `graphersal-server user list|add|token [--revoke]|remove|admin|grant|revoke`
    (through the running server, or on the directory offline);
  - a Users tab in the web UI (the token shown once, then `…last4`).
  The last admin cannot be removed, demoted or left without a token. A new token, a revoke or
  a removal ends the user's browser and MCP sessions at once.
- **Browser login:**
  - `POST /api/login {"token"}` sets the `gs_session` cookie (`HttpOnly; SameSite=Strict;
    Path=/`, `Max-Age` from `[sessions] lifetime_secs` or 400 days, `Secure` on https), and
    `POST /api/logout` ends the session;
  - the web UI shows "Sign in" to anonymous visitors, "My databases" to users (new route
    `GET /api/databases`) and the management tabs plus Users to admins;
  - the top bar shows who is signed in, with "Log out";
  - the playground pages under `/db/<name>/` send a `401` to the login page and come back
    after the login.
- **MCP per principal.** `/mcp` lists and serves only the databases the caller reaches.
  - With a token: `claude mcp add --transport http --header "Authorization: Bearer <token>" ...`.
  - Without one, the caller is anonymous and sees the anonymous grants.
  - A `read` level gets the read-only tool profile.
  - `/db/<name>/mcp` without a grant answers `401` to anonymous and `404` to a user.
  - An MCP session belongs to the principal that initialized it; anyone else using its id
    gets `404`.
  - The old `[mcp] token` is gone.
- **Audit:** every administrative event (databases, flags, aliases, users, tokens, grants,
  logins) is one log line on the target `graphersal_server::audit`, with the actor. The lines
  replace the earlier info lines and never carry a secret.

## Running behind a proxy, modes

New `server.toml` keys, read at start, also in the file `init` writes:

- `[access] enabled` (default `true`). With `false`, every request is an admin (the principal
  `everyone`), and the start block warns. When the address is not a loopback address, the
  warning is in the banner too.
- `[http] public_scheme = "http" | "https"`. `https` makes the login cookie `Secure`. On
  `http`, an address that is not a loopback address gets a clear-text warning.
- `[http] allowed_hosts`: the public names a `Host`/`Origin` may carry behind a reverse proxy.
  The `403` for an unknown host now names this key.

The start block has new rows (`access`, `scheme` with how long the login cookie is kept,
`allowed hosts`), new banner variants, and an MCP `connect` line with the token header.

## Sessions survive a restart

Browser logins and MCP sessions (with their selection and a pending alias rename note) are
kept in the new `sessions/` directory (`browser.json`, `mcp.json`). The files hold hashes
only, are written atomically with mode `0600`, and are always on. New optional keys
`[sessions] lifetime_secs` and `idle_timeout_secs` limit how long a browser session lasts (no
limit by default). An expired session's `401` says "logged out, expired, or the token
changed".

## Query limits and the memory ceiling

- `[limits.query]` in `server.toml`, and per database in `database.toml` (key by key over the
  global ones): `timeout_ms`, `max_operations`, `max_string_size`, `max_array_size`,
  `max_map_size`, `max_traversers`, `max_materialized_bytes`, `max_value_depth`. The names
  follow the graphersal command line's flags.
  - Both files are read again at every load.
  - `GET /db/<name>/api/statistics` shows the limits under `limits.presets`, and so does the
    playground.
  - A configured limit is a **ceiling**: a query may lower it with `g.with(..)`, never raise or
    remove it (`OptionAboveCeiling`). A key left unset is no ceiling; set it to cap at the
    engine's default.
- `[limits] memory_limit` (bytes, `"auto"` or `"none"`) and `memory_headroom` set the process
  memory ceiling, server-wide.
- The start block shows `query limits` and `memory ceiling` rows when they are set. TOML errors
  now name the line they point at.

## Backup root

`[jobs] backup_root` (default `backups/jobs/`, created by `init` and `serve`, may name a mounted
storage) is the only directory backup jobs write into.

- A job's `path` is relative to the root. Absolute or escaping paths are refused when the job
  is defined.
- The Store panel's fork, attic fork and backup `dir` must lie inside the root.
- A stored job writing outside the root is dropped at load, with a warning naming it.
- **New database and alias name rule:** `jobs` is reserved (`400` with help at create, clone,
  import, restore under a new name, alias set and alias rename), because the admin backups of
  a database of that name would land in the default root. A database of that name made by
  0.1.0 is still served.

## Import inbox

The new `imports/` directory, created by `init`, is an inbox: put a `.gsnap`, Store, GraphSON or
GraphML file there by any means and import it by its name.

- admin API: `GET /api/admin/imports` lists it, and `POST /api/admin/import {"inbox", "name"}`
  imports an entry;
- console: `import DIR --list` and `import DIR --inbox FILE --name NAME`;
- web UI: the Import tab's third source.

The inbox is flat, and links leading outside it are refused.

## Aliases

- Rename an alias with `POST /api/admin/aliases/<alias>/rename {"name"}`, the console's
  `db alias rename OLD NEW` or "Rename…" in the web UI. The rename is atomic.
- Open `/mcp` sessions that selected the alias move with it, and their next answer carries one
  note line.
- `/db/<old name>/...` answers `404` afterwards.
- MCP `use_graph` now answers `"selected"` (the name the session keeps) and `"graph"` (the
  database it names now). `list_graphs` adds `selectedGraph` (Graphersal 0.8.0).

## Also new since the 0.1.0 notes

- **Run administration:**
  - `GET /api/admin/runs`, `GET /api/admin/databases/<name>/runs` and `POST
    /api/admin/databases/<name>/runs/cancel {"id"}`: every client's queued and running queries,
    with a way to stop one;
  - a Runs tab in the web UI, and "Runs…" on a loaded database's row.
- **Store imports lacking a model blob** that `models/` does not hold either become
  `needs-model` databases, like snapshots. A base is still refused.

## Known limits

- **Plain HTTP:** tokens and session cookies travel in clear text. Keep the server on the
  loopback, or put a TLS proxy in front and set `public_scheme = "https"`.
- No admin tools over MCP; no audit journal file (the audit lines go to the log); no UI
  editing of query limits.
- The import inbox cannot delete files, and imports run within the request.
- The memory ceiling uses the engine's size estimates; the binary installs no tracking
  allocator.
- The 0.1.0 limits still hold: a base is frozen forever; the prebuilt binary is for Windows
  x86_64 only; model archives are ustar `.tar` files (optionally gzip-compressed); there are no
  memory figures per database in the listings.

## Licence

Apache-2.0 (`LICENSE-APACHE`, `NOTICE`).
