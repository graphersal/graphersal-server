# Concepts

The few ideas the rest of the book builds on.

## The server directory

Everything the server keeps lives in ONE directory: its configuration, its databases, the
shared embedding models, backups, deleted databases, users and sessions. `graphersal-server
init DIR` creates it; `graphersal-server serve --dir DIR` serves it. A directory is a server
directory when it holds `server.toml`.

```text
srv/
  server.toml      settings
  access.toml      users (token hashes) and grants
  aliases.toml     alias -> database
  mcp.toml         MCP switches
  databases/acme/  one directory per database: database.toml + store/
  models/          one copy of every embedding model
  backups/acme/    backups of one database
  imports/         the import inbox
  trash/           deleted databases
  sessions/        browser and MCP sessions (hashes only)
  logs/            the log of a server `start` runs in the background
```

There is no registry file: the databases ARE the directories under `databases/`. One server
serves one directory, and one directory is served by at most one server (`server.lock`).

The directory is private to the OS user who owns it. That user is the **operator**: the
console run by that user needs no password or token. Every file is described in [The Server
Directory](../reference/server_directory.md); every setting in the [Configuration
Reference](../reference/configuration.md).

## Databases

A database is a Graphersal Store in `databases/<name>/store/`, with its settings in
`databases/<name>/database.toml`. The server knows three kinds:

| Kind | What it is | Made by |
|---|---|---|
| plain | an ordinary Store: reads and writes | `db create`, `import`, `db clone`, `db restore` |
| base | a frozen, read-only database that layers are built over | `db create --role base` (empty), `import --role base`, `db promote` |
| layer | a Store over a base: it stores only its own changes; the base stays shared | `db create NAME --base BASE`, the import of a layer snapshot |

**Bases are frozen forever.** Layers depend on the exact state of their base, so a base never
changes again and is never turned back into a plain database. Many layers over one base share
the base in memory: loading a layer loads its base once.

### States

A database is in one state at a time. `db list`, the Databases tab and `GET
/api/admin/databases` show it.

| State | Meaning | What to do |
|---|---|---|
| `unloaded` | on disk, not served | `db load NAME` |
| `loading` | being loaded in the background | wait |
| `loaded` | served under `/db/NAME/` and over MCP | |
| `maintenance` | loaded read-only: its Store is damaged | [A database is in maintenance](../troubleshooting.md#a-database-is-in-maintenance) |
| `needs-model` | its Store names an embedding model the server does not hold | [A database needs a model](../troubleshooting.md#a-database-needs-a-model) |
| `failed` | its directory or Store could not be read, or a load failed; the reason is shown | fix the reason, then `db load NAME` |

A database is never loaded implicitly: a request for an unloaded database answers `409` with
the help `load it`. Databases with `auto_load` are loaded at every server start.

### Read-only

A database can be read-only for three reasons: its `read_only` flag (`db read-only NAME on`),
its role (a base is always read-only), or its state (`maintenance`). A user with a `read`
grant also gets every request read-only.

## Aliases

An alias is a second name for a database: `prod -> acme-2026-11`. Clients use the alias; an
admin re-points it to another database at once (a *flip*), for example after preparing a new
version as a clone:

```bash
graphersal-server db clone acme-2026-11 acme-2026-12 --dir ./srv
graphersal-server db alias set prod acme-2026-12 --dir ./srv
```

- `/db/prod/...` and the MCP endpoint resolve the alias on every request.
- An alias names a database, never another alias.
- Aliases and databases share one namespace: a name is either.
- A grant on an alias reaches the database only through the alias
  ([Grants on aliases](../guide/access.md#grants-on-aliases)).

Aliases are stored in `aliases.toml`.

## Principals

Every request is answered for one principal:

| Principal | Who |
|---|---|
| a user | a request with a user token (`Authorization: Bearer <token>`), or a browser signed in with one |
| the operator | the console of the OS user who owns the server directory; an admin |
| anonymous | a request without credentials; it reaches only what is granted to `anonymous` |
| everyone | every request while access control is off (`[access] enabled = false`); an admin |

A user has roles and grants. The `admin` role reaches every database and the admin API. Other
users reach the databases they have a grant on, at the level of the grant (`read` or `write`).
Details in [Users, Tokens and Grants](../guide/access.md).

## Names

A new database, alias or user name:

- has 1 to 32 characters: lower-case letters, digits, `.`, `_` and `-`;
- starts and ends with a letter or digit;
- holds no `..`;
- is not a Windows device name (`con`, `nul`, `com1`, ...).

`jobs` is reserved for databases and aliases (the default backup root is `backups/jobs/`).
`anonymous` and `operator` are reserved for users. Names are case-insensitive: `ACME` finds
`acme`.

Names made by 0.2.0 or earlier under the older rule (up to 63 characters, a trailing `-` or
`_`, `..` inside) keep working everywhere: served, granted, aliased, restored under their own
name. Only new names follow the new rule.

On Windows keep the server directory itself short (for example `C:\gs\srv`): a Store adds its
own directories and file names below `databases\<name>\`, and a path over 260 characters
(`MAX_PATH`) fails unless long paths are enabled in Windows.

## Where things run

- **The console** (`graphersal-server db ...`) talks to the running server's admin API when a
  server runs on the directory. Otherwise it works on the directory itself. Both paths use the
  same admin core.
- **The web UI** at `/` is the Graphersal playground showing the server's own UI profile: the
  login page, "My databases", or the admin tabs, by who asks.
- **Each loaded database** keeps the ordinary Graphersal playground under `/db/<name>/`.
