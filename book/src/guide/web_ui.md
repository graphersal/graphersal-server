# The Web UI

Open the server's address in a browser, for example <http://127.0.0.1:8484/>. The page is the
Graphersal playground showing graphersal-server's own UI profile. What it shows depends on who
asks:

| Visitor | Sees |
|---|---|
| not signed in | "Sign in": paste a user token |
| a user | "My databases": the databases and aliases the user reaches |
| an admin | the admin tabs below |

After signing in the page returns to where you came from (only a path on this server) or to
`/`. "Log out" in the top bar ends the session. The login is a cookie (`gs_session`) that
survives a browser restart and a server restart ([Browser login and
logout](access.md#browser-login-and-logout)).

Each loaded database keeps the ordinary Graphersal playground under `/db/<name>/`. Its header
links back to the server's UI.

## Common behaviour

- **Fresh data.** Every tab reads its data again each time it is shown, and when the window gets
  the focus back. A change made elsewhere (another browser, a script, a console command such as
  `graphersal-server user add`) shows up on the next visit. A refresh never touches an open
  dialog or form.
- **Sortable tables.** Click a column header to sort, click again to reverse. Text sorts
  naturally (`db2` before `db10`), sizes and dates by value, missing values last. Each table
  remembers its sort in the browser; until you pick one, the Databases, Aliases and MCP tables
  sort by name.
- **Notices.** The outcome of an action ("Loaded …", "… verifies: no problem found") and the
  failure of an action started from a table row appear as small notices in the bottom-right
  corner, over the page: the page never moves, and the top bar and the tab bar stay free. An
  outcome hides itself after about 6 seconds, a warning after about 12 seconds, an error stays
  until you close it (×); a notice stays while the pointer is on it. A long explanation shows
  its first three lines; "More" shows the rest. Several stack (the newest at the bottom); the
  same notice again counts up (×2) instead of stacking. They stay when you switch tabs.
- **Form errors** appear right under the form's own button ("Grant", "Create", "Restore",
  "Save description", "Sign in", ...), where you just clicked, not at the top of the page.
- **Conflicts.** Every change is based on what the tab showed. When someone else changed the
  same thing meanwhile, the server refuses with "changed by someone else: reload"; the tab
  reloads and you try again ([409: changed by someone
  reload](../troubleshooting.md#409-changed-by-someone-else)).
- **Names.** Every field that takes a new name shows the name rule under it and checks it before
  sending ([Names](../concepts/concepts.md#names)).
- **Refusals.** A refused request sends the page to the login page (`401`), or to the root when
  you lost the admin role (`404`).

## My databases (users)

The databases and aliases you reach, with your level (`read` or `write`), whether the name is
an alias (and of which database), a link to its playground and a link to its OpenAPI
description.

## Databases

The list of databases and aliases with their state, kind, flags, size on disk and size in
memory (hover for the exact bytes and a layer's shared base).

- **Actions per row** sit behind the row's "…" button: Load or Unload…, Settings… (opens the
  Settings tab), Runs… (a loaded database: opens the Runs tab filtered), Clone…,
  Backups…, Export…, Verify, Repair…, Promote to base…, Reset layer… (layer rows only),
  "OpenAPI description" (the database's
  HTTP API description, in a new browser tab) and Delete…; an alias row has Settings of its
  database, Point to…, Rename…, its OpenAPI description and Remove…. An action that does not
  apply to the row is greyed out, its tooltip says why. The menu works with the keyboard (Enter
  or ↓ opens it, ↑/↓/Home/End move, Escape closes it), closes on a click outside, and opens
  upward near the bottom of the window. Read-only and auto-load are the row's checkboxes. The
  table fits the window without a horizontal scroll bar (long names and descriptions wrap).
- **Create** a database: plain, base, or layer over a base.
- **Reset layer…** discards a layer's changes over its base after a confirmation that names
  what is lost (its added, changed and removed elements, its own schema and saved queries) and
  what is kept (name, base, description, flags, query limits, grants, aliases, MCP settings,
  history, scheduled jobs); the reset is one commit of the layer's Store and a loaded layer
  stays loaded. The outcome notice counts what was discarded ([Reset a
  layer](databases.md#reset-a-layer)).
- **Aliases:** set, flip, rename, remove.
- **Memory summary** above the table: the server process, the loaded databases (each graph
  once), the machine's total and available memory, the memory limit and what is free under it.
- **Kind** is one word: `plain`, `layer` or `base`; a layer shows `over <base>` on a second line
  (cut with "…" when the name is long). The tooltip says it in full ("layer over products") and
  what the kind means. Names never break in the middle.
- **Bases and layers** are explained in the Kind column's tooltips, a box under the table and the
  confirmation of "Promote to base…" (one way, forever).
- **Damage:** a database in the state `maintenance` shows its damage report. "Verify" per row,
  "Verify all", and "Repair…" for a plain database: a repaired copy as a new database, then "Let
  it take the name…" (confirmed; the damaged one moves to `trash/`) or "Keep both", with the hint
  to restore a backup instead. "Verify all" verifies one database after the other and shows its
  progress on the button (`Verifying 3/10…`); "Cancel" beside it stops before the next database
  (the one being verified finishes). Its result is one notice listing each database with
  problems and its problem lines. A missing blob of an embedding model is named as that model
  ("the embedding model acme/tiny (blob f0ed7648) is missing") with the remedy: download or import
  the model on the Models tab; such a database is `needs-model`, not damaged. A row whose last
  verify found something shows a small badge ("verify: 3 problems", "verify: needs model"). Above the table: the last unclean shutdown and the check of each
  database ([A server killed by force](../troubleshooting.md#a-server-was-killed-by-force)).

Programs talking to the server's HTTP API: [HTTP Clients and OpenAPI](http_clients.md).

## Backups

Back up a database, list its backups, restore one over the database or into a new one.

"Server backup": download a backup of the whole server's configuration, optionally with every
database, beside the warning that it holds the users' token hashes
([Back up the whole server](backups.md#back-up-the-whole-server)).

## Import

A new database from:

- **a file from this computer**, uploaded with a progress bar (up to `[limits] upload_bytes`);
- **a file of the server's import inbox** `imports/`, picked from a list;
- **a path on the server**.

Options: name, format, description, role (normal or base), the base of a layer snapshot.

## Export

Download a database as a packed snapshot (`.gsnap`), with or without its models; a layer as a
layer snapshot or materialized.

## Models

The shared embedding models with their size, licence and users, and the databases waiting for a
model. **Download from Hugging Face**: a repository (and a revision) downloaded by the server,
with the file being fetched and its progress, Cancel, and at the end the model's licence with
its warnings (a toast too); a waiting database's "Download…" puts its model into the form. Add
one by uploading a `.gsnap` or a `.tar`/`.tar.gz` of a model directory (with progress), or from
a path on the server. Remove one ([Embedding Models](models.md)).

## Runs

The queries that wait or run on every loaded database, whatever their client, refreshed every
2 seconds while the tab is shown, with a Stop button per run. A database's "Runs…" opens the
tab filtered to it.

## Users

The users with their roles and `…last4` of their token. Add a user (its token shown once), give
a new token, revoke it, give or take the admin role, remove the user. The grants, `anonymous`
included, on databases and aliases; an alias is shown with its current database. The grant
form's Database list shows the aliases first (`cur → acme (alias)`), then the databases, each
alphabetical, then "every database". The last usable admin cannot be removed, demoted or left
without a token.

## MCP

- MCP on or off for the whole server. Off, every MCP request is refused with `503`, also those
  of agents connected before; their sessions are not ended, and when MCP is switched on again
  they continue with the same session and selected database, without connecting again. The
  HTTP API and the web pages are not affected.
- Per database or alias: offered to MCP or hidden, and read-only for MCP. A base, a database
  with its own Read-only flag and an alias of a database read-only for MCP are read-only for MCP
  anyway: their checkbox is ticked and greyed out, its tooltip says why ("A base is always
  read-only").
- "Expose new databases and aliases": whether a database or alias created from now on is offered
  to MCP (off by default: a new name is hidden until exposed here). The create, clone, new alias
  and import forms have their own "Expose through MCP" checkbox.
- Usage since the start: open sessions and tool calls per database, tool calls per tool and per
  user, the recent errors.
- Profiles: what agents see (tools, descriptions, instructions, saved queries as tools,
  resources). "Edit…" beside "Server default profile" or on a row opens the profile editor; the
  Profile column says where each name's profile comes from. See
  [Editing a profile in the web UI](mcp.md#editing-a-profile-in-the-web-ui).
- The endpoint URL and the `claude mcp add` command.

More in [MCP](mcp.md).

## Settings

The tab's picker shows, in this order: **Server defaults (every database)**, **Server
(server.toml)**, then every database. A database is also opened by its Databases row's
"Settings…".

### Server defaults

What every database inherits or starts with ([Configuration Reference](../reference/configuration.md),
"Server defaults and inheritance"):

- MCP: "Offer new databases and aliases to MCP" (`mcp.toml` `expose_new`; the same switch as
  the MCP tab's);
- New databases: `chunk_bytes` and `damage_policy`, marked "at create only" (creation
  parameters, fixed for each database's life), and `auto_load` and `read_only`, the first value
  of each new database's own flags (the create form starts from them);
- Query limits of every database: each key inherited by every database that does not override
  it, applied at once to every loaded database's new runs.

### Server

Every key of `server.toml`, grouped by section, each with its value (or its default, marked
"(default)"), what it means, and when a change applies: "applies now", "new databases" / "at
create only", or "after restart". A key is set in place (a checkbox, a choice,
or a value with "Set") or removed with "Reset to default". The file keeps its comments; a
toast says what was applied at once and what waits for a restart. A restart key shows
"restart pending" with the value the running server still uses ("running: …"), a note above the tables
lists all of them, and the top bar shows a "restart pending" badge (for admins, on every tab;
it opens this view) until the server is restarted. When the server listens on another address
than the file's `listen` because it was started with `--listen` (or the file's port 0 picked a
port), the row says "running: 127.0.0.1:18731 (command line)" (or "(picked port)"): that is not
a restart pending. A change made from a stale page is refused, and the view is read again.

### A database

- identity (kind, role, base, aliases, created), state and sizes;
- the description, read-only and auto-load, edited in place, with what a new database would get
  from the server defaults;
- the query limits: per key "inherited" (the server's default applies) or "override" (this
  database's own value), the server's default, the effective value and, while loaded, the
  value in effect (a change applies at once to the database's new runs); each override set in
  place, "Inherit" removes it (reset to inherited); "Unload and load now…" when a hand edit of
  a file is not applied yet;
- MCP, grants and backups, with links to those tabs;
- "Damage and recovery": a database in maintenance with its damage report; its last verify in
  one wording ("Last verify <time>: no problem found", "…: the embedding model NAME is missing
  (the graph data is intact; not damage)" with the remedy and a link to the Models tab, "…: N
  problems: its Store is damaged" with the first problems, or "Never verified: Verify … checks
  the whole Store"); a `needs-model` database names its missing model there too; and its check
  after the last unclean shutdown;
- the Store's creation parameters, read-only.

Every change is based on the view's revision.
