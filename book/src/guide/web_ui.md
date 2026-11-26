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
  failure of an action started from a table row appear as small notices in the top-right
  corner, over the page: the page never moves. An outcome hides itself after about 6 seconds, a
  warning after about 12 seconds, an error stays until you close it (×); a notice stays while
  the pointer is on it. Several stack; the same notice again counts up (×2) instead of
  stacking. They stay when you switch tabs.
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
  Database settings tab), Runs… (a loaded database: opens the Runs tab filtered), Clone…,
  Backups…, Export…, Verify, Repair…, Promote to base…, "OpenAPI description" (the database's
  HTTP API description, in a new browser tab) and Delete…; an alias row has Settings of its
  database, Point to…, Rename…, its OpenAPI description and Remove…. An action that does not
  apply to the row is greyed out, its tooltip says why. The menu works with the keyboard (Enter
  or ↓ opens it, ↑/↓/Home/End move, Escape closes it), closes on a click outside, and opens
  upward near the bottom of the window. Read-only and auto-load are the row's checkboxes. The
  table fits the window without a horizontal scroll bar (long names and descriptions wrap).
- **Create** a database: plain, base, or layer over a base.
- **Aliases:** set, flip, rename, remove.
- **Memory summary** above the table: the server process, the loaded databases (each graph
  once), the machine's total and available memory, the memory limit and what is free under it.
- **Bases and layers** are explained in the Kind column's tooltips, a box under the table and the
  confirmation of "Promote to base…" (one way, forever).
- **Damage:** a database in the state `maintenance` shows its damage report. "Verify" per row,
  "Verify all", and "Repair…" for a plain database: a repaired copy as a new database, then "Let
  it take the name…" (confirmed; the damaged one moves to `trash/`) or "Keep both", with the hint
  to restore a backup instead. Above the table: the last unclean shutdown and the check of each
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
model. Add one by uploading a `.gsnap` or a `.tar`/`.tar.gz` of a model directory (with
progress), or from a path on the server. Remove one ([Embedding Models](models.md)).

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
- Per database or alias: offered to MCP or hidden, and read-only for MCP.
- "Expose new databases and aliases": whether a database or alias created from now on is offered
  to MCP (off by default: a new name is hidden until exposed here). The create, clone, new alias
  and import forms have their own "Expose through MCP" checkbox.
- Usage since the start: open sessions and tool calls per database, tool calls per tool and per
  user, the recent errors.
- The endpoint URL and the `claude mcp add` command.

More in [MCP](mcp.md).

## Database settings

One database, picked in the tab or opened by a Databases row's "Settings…":

- identity (kind, role, base, aliases, created), state and sizes;
- the description, read-only and auto-load, edited in place;
- the query limits: the database's own override, the server's default, the next load's value
  and, while loaded, the value in effect; each override edited in place; "Unload and load now…"
  when the values differ;
- MCP, grants and backups, with links to those tabs;
- "Damage and recovery": a database in maintenance with its damage report, and its check after
  the last unclean shutdown;
- the Store's creation parameters, read-only.

Every change is based on the view's revision.
