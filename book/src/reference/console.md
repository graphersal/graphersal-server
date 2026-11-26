# Console Commands

The console is the `graphersal-server` binary itself. This page lists every command with its
options; the [Command Cheat Sheet](../cheat_sheet.md) has them as one-liners, and
`graphersal-server --help` prints the same on the terminal.

## How the console works


The console acts as the operator without credentials: the OS user that owns the server
directory is its operator. `init` makes the directory private (mode `0700`, the secret files
`0600`), and `serve` and every console command refuse a directory or secret owned by another
user or open to group or others, naming the `chmod`/`chown` that fixes it
([Security Model](../guide/security.md#the-directory-owner-is-the-operator)). Run a shared server under a dedicated
service user; other administrators run the console as that user, or send an admin token:
`--token TOKEN` (or `GRAPHERSAL_SERVER_TOKEN`) goes through the running server (address
`--server ADDRESS`, else from `DIR/server.run`) and touches no file of the directory. The
audit names the OS user of the operator's actions (`operator (os:<name>)`) and the token's
user otherwise.

## Output and the two paths

Every command takes `--dir DIR`; `--json` prints the admin API's JSON answer instead of text.
With a server running on `DIR` the command goes to its admin API (address from
`DIR/server.run`, noted on stderr); otherwise it works on the directory itself (then `load` and
`unload` are refused: nothing would serve the database). Both paths call the same admin core.
The text output follows the graphersal CLI's layout: a result is a headline with its details in
one column and every command on its own line under a `# comment` (an import's `source:` is the
absolute path without `.`/`..` segments); listings (`db list`, `db
backups`, `models list`) are sections of aligned columns, with a Problems section for failed
databases and missing models. Paths and commands are never wrapped.

## Commands

| Command | What |
|---|---|
| `init DIR [--config FILE] [--set KEY=VALUE]...` | create a server directory (missing or empty DIR), or add the missing parts of an existing one and tighten its modes; a directory without `access.toml` gets the first admin, its token printed once on stderr; a new server has MCP off and offers no database to it (`mcp.toml`). `--config FILE`: the new `server.toml` is FILE (checked like `config check`, refused if invalid; keys it leaves out keep their defaults; its comments kept); each `--set KEY=VALUE` (as `config set`) is applied after it; secrets are never in `server.toml` (the first admin's token is generated as always); both only for a new directory |
| `serve --dir DIR [--listen ADDR] [--max-connections N] [--header-timeout SECS] [--idle-timeout SECS]` | serve in the foreground (for service managers); `ADDR` overrides `server.toml`'s `listen` (port `0` = a free port), the others its `[limits]`; Ctrl+C, SIGTERM or `stop` stops it |
| `start [serve options] [--timeout SECS]` | `serve` in the background (log `DIR/logs/server.log`); waits until ready, prints the start block and the address |
| `stop [--timeout SECS]` | stops the running server cleanly and waits; `not running` (exit 0) when none runs |
| `status [--json]` | running or stopped: pid, address, version, uptime, databases, memory (process, loaded databases, machine, limit, free), the `server.toml` keys whose change waits for a restart (`restart pending`), the log, the last unclean shutdown (and, stopped, one the next start will check); exit code 0 running, 3 stopped |
| `db list` | the databases and aliases with their states |
| `db create NAME [--base BASE] [--kind plain\|layer] [--role normal\|base] [--schema FILE.json] [--description TEXT] [--auto-load\|--no-auto-load] [--read-only\|--no-read-only] [--mcp\|--no-mcp]` | a new empty database; `--base` makes a layer over a `role = base` database. Without the auto-load/read-only flags it takes `server.toml`'s `[defaults]` `auto_load`/`read_only` (an import too). `--mcp` offers it to MCP at once; without it a new database (or alias) is hidden from MCP unless `mcp expose-new` is on (`--no-mcp` hides it then); the same flags on `db clone`, `db alias set`, `db restore` and `import`, and the result says whether MCP offers it |
| `db delete NAME` | an unloaded database, moved to `DIR/trash/`; every grant on it is removed (a restore does not bring them back) |
| `db clone SOURCE NAME [--mcp\|--no-mcp]` | a fork of SOURCE's latest state (it may be loaded) |
| `db load NAME [--no-wait]`, `db unload NAME` | running server only; a loaded database's note shows its URL (`served at: http://.../db/NAME/`) |
| `db promote NAME` | an unloaded plain database becomes a base (read-only, frozen for good) |
| `db read-only NAME on\|off`, `db auto-load NAME on\|off` | flags, persisted; read-only applies at once |
| `db info NAME` | everything about a database (or the database an alias names): kind, role, base, aliases, created; state, flags, on disk, in memory; the query limits (its own overrides, the server's defaults, what the next load uses, what a loaded one runs under now); MCP, grants, backups; the Store's creation parameters (store id, damage policy, chunk size, created, backend, format version), read-only |
| `db set NAME description TEXT`, `db set NAME limit KEY VALUE`, `db set NAME limit KEY --clear` | the description, or a query limit override of `database.toml [limits.query]` (`timeout_ms`, `max_operations`, ...; `--clear`: the server's default applies again), checked like `server.toml`; a limit applies at the database's next load (unload + load) |
| `db alias set ALIAS DATABASE [--mcp\|--no-mcp]`, `db alias remove ALIAS`, `db alias rename OLD NEW` | aliases (a flip takes effect at once, also for the grants on the alias; a rename moves open MCP sessions and the alias's grants with it; a removal removes its grants) |
| `db backup NAME`, `db backups NAME` | verified ZIP backups in `DIR/backups/NAME/` |
| `db restore NAME BACKUP [--name NEW] [--mcp\|--no-mcp]` | over NAME (unloaded; it keeps its MCP setting) or into the new database NEW |
| `db verify NAME`, `db verify --all` | the engine's scrub of NAME's Store (a loaded one: its open Store; damage found makes it read-only, state `maintenance`) or of every database's; a missing embedding model blob is named as its model with the `models add` command that fixes it (state `needs-model`, not damage); the result is kept as the database's last verify (`db info` shows it); exit 1 when damage or a missing model is found |
| `db repair NAME [--name NEW]` | a repaired copy of the plain database NAME as the new database NEW (default `NAME-repaired`; never in place, NAME is not changed): what was rebuilt, lost or diverged; the copy is verified |
| `db swap NAME WITH --yes` | WITH (a repaired copy) takes the name NAME: NAME moves to `DIR/trash/`, WITH's name goes away; NAME's grants, aliases and settings stay; both unloaded first, NAME loaded again when it was |
| `db export NAME --out FILE [--no-models] [--materialize]` | a packed snapshot (`.gsnap`) |
| `import DIR SOURCE --name NAME [--base BASE] [--format F] [--description TEXT] [--role normal\|base] [--mcp\|--no-mcp]` | a new database from a `.gsnap`, a Store (directory or `.gstore`), GraphSON or GraphML; an embedding model it names without carrying comes from `models/` (else the database is `needs-model` until `models add`); a model blob a Store lacks is filled from `models/` (else the database is `needs-model` until `models add`, like a snapshot); `--role base` makes a base of a plain source |
| `import DIR --inbox FILE --name NAME [...]`, `import DIR --list` | the same from the import inbox `DIR/imports/` (FILE: one entry name there; a link leading outside is refused), and its listing |
| `models list`, `models add SOURCE [--repo R] [--license L]`, `models remove REPO` | the shared embedding models; SOURCE: a Model2Vec model directory, a `.tar` or `.tar.gz` of one, a `.gsnap` carrying models, or a repository `models/` holds |
| `user list` | the users of `access.toml`: roles, the last 4 characters of each token (`…a1b2`), what each reaches; then the grants of `anonymous` and roles; an alias is shown with its current database (`prod → prod-2026-11 (alias)`) |
| `user add NAME [--admin]` | a new user; its token is printed once on stdout (only a SHA-256 hash is kept) |
| `user token NAME [--revoke]` | a new token for NAME, printed once (the old one stops working), or with `--revoke` none; the user's browser and MCP sessions end at once |
| `user remove NAME` | the user and its grants (its sessions end) |
| `user admin NAME on\|off` | give or take the `admin` role |
| `user grant SUBJECT DATABASE read\|write`, `user revoke SUBJECT DATABASE` | grants; SUBJECT: a user name or `anonymous` (everyone, also without a token), DATABASE: a database, an alias (reached only through the alias, at its current database: [Grants on aliases](../guide/access.md#grants-on-aliases)) or `*`. The last admin (role and token) cannot be removed, demoted or revoked |
| `backup [--out FILE] [--with-databases]` | one ZIP (mode `0600`) of the server itself: `server.toml`, `aliases.toml`, `access.toml`, `mcp.toml`, every database's `database.toml`, the models list and a manifest with every file's SHA-256; `--with-databases` adds a verified backup of every database and the model blobs. Sessions, logs, run files, `tmp/`, `trash/`, `imports/` and `backups/` are left out. Through a running server like `db`. It holds the token hashes: keep it like a password ([Backups and Restore](../guide/backups.md#back-up-the-whole-server)) |
| `restore FILE --dir NEWDIR [--force-version]` | a NEW server directory from a server backup (missing or empty `NEWDIR` only; every checksum checked first; modes `0700`/`0600`; databases without data in the archive skipped and reported); then `serve --dir NEWDIR`. Recipe: "Move a server to a new machine" in [Backups and Restore](../guide/backups.md#move-a-server-to-a-new-machine) |
| `openapi [--db NAME] [--out FILE] [--server URL]` | the OpenAPI description of the server's HTTP API, or of the database or alias `NAME`'s, built from the directory alone (no server needed; nothing written in it): the same document a running server answers, `servers` = `URL`, else `server.toml`'s first `[http] allowed_hosts` name or its listen address ([HTTP Clients and OpenAPI](../guide/http_clients.md)) |
| `config get [KEY]`, `config set KEY VALUE`, `config unset KEY` | the keys of `DIR/server.toml` (dotted names: `listen`, `limits.upload_bytes`, `limits.query.timeout_ms`, `defaults.auto_load`, `http.allowed_hosts`, ...): every key with its value or default and when a change applies (`now`, `load`, `create`, `restart`; "restart pending" while the running server still uses the old value), one key, a new value (true/false or on/off, a whole number, a text; `allowed_hosts` names separated by commas; `memory_limit` bytes, `auto` or `none`), back to the default. Offline on the directory, or through the running server like `db` (`--token`/`--server` too), which applies the `now` keys at once; checked by the start-up parser, the file's comments kept ([Configuration Reference](configuration.md)) |
| `config check [FILE]` | check a `server.toml` (default `DIR/server.toml`, then `--dir DIR`) with the server's parser, without a server: exit 0 valid, 1 with the error naming the key and line |
| `mcp status`, `mcp on\|off`, `mcp expose-new on\|off`, `mcp db NAME on\|off`, `mcp db NAME read-only on\|off` | the MCP switches of `DIR/mcp.toml` (a running server follows at once): MCP for the whole server (off: `/mcp` and `/db/NAME/mcp` answer 503, open sessions are kept; a new server starts off), whether new databases and aliases are offered to MCP (default off; `status` shows it), a database or alias offered to MCP or hidden, MCP read-only whatever the caller's grant; `status` lists them with the usage statistics of a running server ([MCP](../guide/mcp.md)) |

## Exit codes

`graphersal-server --help` has the details. Exit codes: `0` ok, `1` failed (`Error: ...` and a
`Help: ...` line on stderr; where the admin API's help names a route, the console names the
console command instead: `graphersal-server db list --dir DIR lists ...`), `2` invalid invocation (`Error: ...` and the command that shows
the usage), `3` `status`: the server is not running, `141` stdout closed before the result was written (`| head`).

