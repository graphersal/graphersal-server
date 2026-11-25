# Access control (EXPERIMENTAL)

Who may do what on a graphersal-server: the principals, the grants in `access.toml`, tokens,
browser logins, MCP, the modes of `server.toml` and the limits an admin sets. The file formats
are in [`server-dir.md`](server-dir.md), the routes in [`admin-api.md`](admin-api.md) and
[`mcp.md`](mcp.md).

Access control is on by default. Every request except the health probes (`/healthz`,
`/readyz`), the root `GET /api/info` and the files of the web UI needs a principal that is
allowed to make it. Tokens and session cookies travel in clear text over plain HTTP (see
[Modes](#modes)). On the server's machine, the boundary is the OS permissions of the server
directory (see [The security model](#the-security-model)).

## The security model

The OS user that owns the server directory is the operator (as PostgreSQL's peer
authentication trusts the OS user). Whoever can write the directory can edit `access.toml` and
make themselves an admin, read `server.run` and use the operator token, or copy the Stores,
whatever the server checks over HTTP. So the console needs no credentials on the directory:
the OS already decided who may touch it. graphersal-server enforces that the directory really
is private, the way ssh treats `~/.ssh`:

- `init` creates the directory with mode `0700`, `sessions/` with `0700` and every secret
  file (`access.toml`, `server.run`, `sessions/*.json`) with `0600`. Every later write keeps
  these modes. `init` on an existing server directory owned by you tightens the modes it finds
  too open and lists them under `owner only` (the upgrade path for a directory made before
  0.3.0).
- `serve` and every console command (`db`, `import`, `models`, `user`; offline and through a
  running server with the operator token) check first, and refuse with exit code 1 when:
  - the directory, or any file checked in it, is owned by another OS user than the effective
    user of the process. `root` on a directory owned by another user is refused too: run the
    command as the owner (`sudo -u OWNER graphersal-server ...`);
  - the directory, `server.toml` or `aliases.toml` is writable by group or others;
  - `access.toml`, `server.run`, `sessions/` or a file in it is readable or writable by group
    or others.

  The error names each problem and the exact command fixing it:

  ```text
  Error: refused: the server directory /srv/graphs is not private to its owner: /srv/graphs/access.toml is open to group or others (mode 0644) (it holds token hashes or tokens)
  Help: the OS user owning the server directory is its operator: whoever else can change it, or read its secrets, could make themselves an admin; keep it private (as ssh keeps ~/.ssh)
    # owner only: mode 0600
    chmod 600 /srv/graphs/access.toml
    # or let init set every mode at once
    graphersal-server init /srv/graphs
  ```

  A directory readable (not writable) by group or others passes the check, but lets them read
  the databases' files; `init` sets `0700`.
- Run a shared server under a dedicated service account (for example `graphersal`) that owns
  the directory; administrators act as that user (`sudo -u graphersal graphersal-server user
  add bob --dir /srv/graphs`) or with an admin token (below).
- Windows: no permission check (it would need the Win32 security API). The directory's ACL is
  the boundary: create the directory where only the account running the server may write, and
  the files inherit its ACL.

Provisional (may change): root is refused on a directory it does not own, even though root
could read it anyway; the check runs only when the console or `serve` starts, not while the
server runs.

### The console with an admin token

`--token TOKEN` (or the environment variable `GRAPHERSAL_SERVER_TOKEN`, which keeps the token
out of the process list) sends a console command (`db`, `import`, `models`, `user`) to the
running server with that user's token, instead of acting as the operator. The server's address
is `--server ADDRESS` (`IP:PORT` or `http://IP:PORT/`), else the address in the directory's
`server.run`. Nothing in the directory is written, so the permission checks do not apply: the
token is the credential, and the server checks it like any request (a wrong token is refused
with `401`; a user without the admin role sees no admin API). The audit names the token's user.
Without a running server a token is refused (no command acts on the directory offline with a
token). `--server` without a token is a usage error. `--dir DIR` is still required; with
`--server` it only names the directory in the printed text.

```bash
# another OS user, or another machine through an SSH tunnel to the loopback address
export GRAPHERSAL_SERVER_TOKEN=<an admin's token>
graphersal-server db list --dir /srv/graphs --server 127.0.0.1:8484
graphersal-server user grant bob acme read --dir /srv/graphs --server 127.0.0.1:8484
```

## Principals

Every request is answered for ONE principal:

| Principal | Who | Identity (audit, MCP sessions) |
|---|---|---|
| a user | a request with `Authorization: Bearer <user token>`, or with the browser's `gs_session` cookie after a login | `user:<name>` |
| the operator | the console of the OS user owning the server directory: offline on the directory, or through the server with the per-run operator token from `server.run` (`Authorization: Bearer`); an admin | `operator`; the audit adds the OS user: `operator (os:<name>)` |
| anonymous | a request without credentials | none |
| everyone | every request while access control is off (`[access] enabled = false`); an admin | `everyone` |

Wrong credentials (a token nobody holds, an ended session's cookie) are `401`. They never
fall back to anonymous. `GET /api/info` names the request's principal: `principal {id, kind,
admin}`.

The operator token is made fresh at every `serve` and written to `server.run` (mode `0600` on
unix; removed at a clean stop). The console reads it from there, so whoever can read
`server.run` can act as an admin while the server runs: only the directory's owner can
([The security model](#the-security-model)). The operator token cannot log in through the
browser.

## Grants and levels

`access.toml` holds the users (each with roles, the SHA-256 of its token and the token's
last four characters) and the grants. A grant has three parts:

- `subject`: `user:<name>`, `role:<role>`, or `anonymous`. `anonymous` means everyone,
  including requests without a token, so a logged-in user always reaches at least what
  anonymous reaches.
- `database`: the real name of a database, an alias, or `*` for every database.
- `level`: `read` or `write`.

A grant on a database's real name also reaches the database through every alias that points at
it. A grant on an **alias** works the other way round
([Grants on aliases](#grants-on-aliases)).

A principal's level on a database is computed in two steps:

1. Its granted level is the highest grant of any of its subjects. A user with the `admin`
   role, the operator and `everyone` get `write` on every database.
2. That level is capped by the database's mode. On a read-only database or a base the
   effective level is at most `read`.

| Level | What the principal gets on `/db/<name>/...` and over MCP |
|---|---|
| none | the database is **hidden**: `404` like an unknown name, never listed (`GET /api/databases`, MCP `list_graphs`), and no alias leads to it unless the alias itself is granted |
| `read` | every request is read-only: queries run under the reading authorizer, write routes answer `409`, MCP offers the read-only tools |
| `write` | everything the database's own mode allows |

### Grants on aliases

A grant may name an alias (`graphersal-server user grant bob prod read` with `prod` an alias of
`prod-2026-11`). Such a grant:

- reaches the alias's **current** target, but only when the request names the alias:
  `/db/prod/...`, MCP `use_graph prod` or `"graph": "prod"`, the alias's row in the server-wide
  `/mcp` `list_graphs` and in "My databases";
- keeps the real name hidden: `/db/prod-2026-11/...` answers like an unknown name (`404`, or `401`
  for anonymous), and `prod-2026-11` is not listed as a database of its own, unless the
  principal holds a grant on it too. The alias's row still shows the database it points at
  (`aliasOf`), as every alias row does;
- **moves with the alias**: a flip (`db alias set prod prod-2026-12`) moves the access at once, for
  new requests and for open MCP sessions that selected `prod`;
- **follows a rename**: `db alias rename prod live` moves the alias's grants to `live` in
  `access.toml` (the answer says how many: `grantsMoved`);
- **goes with the alias**: `db alias remove prod` removes the grants on `prod` too
  (`grantsRemoved`), so a later alias or database named `prod` inherits nothing.

Through an alias the granted level is the higher of the grants on the alias and on its target;
the database's mode still caps it (a read-only target gives `read` at most). A `*` grant works
as before: every database, by its own name and through every alias.

Grants name names. A grant left on the name of a deleted database stays in `access.toml` and
applies again to whatever takes that name later; the listings mark such a grant as missing.

Every grant listing says what the name is now: the Users tab, `graphersal-server user list`
(`prod → prod-2026-11 (alias)`, a name that names nothing `gone (missing)`), "My databases"
(`alias → prod-2026-11` or `database`) and the admin API's grant objects (`kind`: `database`,
`alias`, `all` for `*`, `missing`; `target`: the alias's current database).

`admin` is the only role with built-in meaning: write on every database, plus the admin API
(`/api/admin/...`, which includes Runs and Users). Other roles get access only through
`role:<role>` grants.

User names follow the database name rule: a new user name has at most 32 characters,
starts and ends with a lower-case letter or digit and holds no `..` (a user named under the
older rule by 0.2.0 still logs in and is granted). `anonymous` and `operator` are reserved. The last
admin (a user with the `admin` role and a token) cannot be removed, demoted or left without a
token.

### 401, 404 and 303

| Request | Anonymous | Logged in without the right |
|---|---|---|
| `/api/admin/...` (admin API, Runs, Users) | `401 {"error", "login": "/"}` | `404`, as if the API did not exist |
| `/db/<name>/...` API without a grant | `401` for **every** name, existing or not, so nothing about existence leaks | `404`, the unknown-name answer |
| `GET` of a `/db/<name>/` page or static file | `303` to the login page `/?next=/db/<name>/...` | `404` |
| `/db/<name>/mcp` without a grant | `401` | `404` |
| `/mcp` | served, over the anonymous grants (often none) | served, over the user's grants |
| wrong credentials, anywhere | `401` | `401` |

Every `401` carries `WWW-Authenticate: Bearer` and the body `{"error", "login": "/"}`. When the
request came with a refused session cookie, the answer also clears that cookie. The playground
under `/db/<name>/` follows `login` to the login page and returns to its page afterwards.

## The first admin

`graphersal-server init DIR`, or the first `serve` of a directory without `access.toml`,
creates the user `admin` (role `admin`) with a new token. The token is printed ONCE, on stderr,
in a framed block (bold and colour on a terminal, an ASCII frame otherwise). stdout keeps only
its usual line. The block names the user, the token, the file that keeps the hash, how to send
the token, and how to replace a lost one:

```text
+--------------------------------------------------------------------------+
| FIRST ADMIN TOKEN - shown once, never again                              |
|                                                                          |
| user:   admin (role admin: every database, the admin API)                |
| token:  <64 hex digits>                                                  |
| ...                                                                      |
| Lost it? This prints a new one (the old one stops working):              |
|   graphersal-server user token admin --dir ./srv                         |
+--------------------------------------------------------------------------+
```

The console on the server's machine never needs this token (it uses the operator token).
Copy it for the web UI or for an MCP client.

## Tokens

A user token is 256 random bits from the OS, written as 64 lower-case hex digits. The server
keeps only its SHA-256 (`token_sha256`) and its last four characters (`token_last4`), and
compares tokens in constant time. The token is shown once, in the answer that made it:
`user add`, `user token`, or the Users tab. After that, listings show only `…a1b2` (console
`user list`, the Users tab, `tokenLast4` in the admin API), which is enough to tell which token
a user holds.

```bash
graphersal-server user add bob --dir ./srv                # prints bob's token once
graphersal-server user grant bob acme read --dir ./srv    # read on acme
graphersal-server user grant anonymous demo read --dir ./srv   # demo is public
graphersal-server user token bob --dir ./srv              # a new token; the old one stops working
graphersal-server user token bob --revoke --dir ./srv     # no token at all
graphersal-server user list --dir ./srv
```

With a server running on the directory, these commands go through its admin API (actor
`operator`) and take effect at once. Without one, they edit `access.toml` directly. Either way
only the OS user owning the directory can run them ([The security model](#the-security-model));
anyone else needs an admin token (`--token`). A new
token, a revoke and a user's removal end that user's browser and MCP sessions at once.

Send a token as `Authorization: Bearer <token>`:

```bash
curl -s http://127.0.0.1:8484/db/acme/api/execute -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' -d '{"script": "g.v().count().next()"}'
```

## Browser login and logout

The web UI at `/` shows the "Sign in" page to anonymous visitors. To log in, paste a user
token. The page calls `POST /api/login {"token"}`, and the server answers with the cookie:

```text
Set-Cookie: gs_session=<session id>; Max-Age=<seconds>; HttpOnly; SameSite=Strict; Path=/[; Secure]
```

- `HttpOnly`: page scripts cannot read the cookie. `SameSite=Strict`: other sites cannot send
  it.
- `Max-Age` is `[sessions] lifetime_secs` when that is set, else 400 days, so a browser
  restart keeps the login.
- `Secure` is added when `[http] public_scheme = "https"`.

After logging in, the page goes to its `?next=` path (only a path from the server root) or to
`/`. There an admin sees the management tabs and Users, and any other user sees "My databases"
(`GET /api/databases`: the databases it reaches, with its level). `POST /api/logout` (the top
bar's "Log out") ends the session and clears the cookie (`Max-Age=0`).

### Persistent sessions

Browser sessions survive a restart or an upgrade. They are kept in `sessions/browser.json`,
which holds only the SHA-256 of each session id and token. Restarts are always kept; there is
no setting to turn this off. A session ends at:

- the logout;
- a new token or a revoke for its user, or the user's removal (at once, also through the
  offline console);
- `[sessions] lifetime_secs` after its login, or `[sessions] idle_timeout_secs` after its last
  request (both unset, so no limit, by default; `server-dir.md`).

A session's last request is recorded coarsely, at most once a minute. Deleting `sessions/`
while no server runs logs every browser out.

## MCP

`/mcp` and `/db/<name>/mcp` follow the same grants ([`mcp.md`](mcp.md)). With a token, an
agent sees the databases its user reaches:

```bash
claude mcp add --transport http --header "Authorization: Bearer <token>" \
  graphersal http://127.0.0.1:8484/mcp
```

Without the header, the agent is anonymous. It sees only the databases granted to
`anonymous`, and only on `/mcp`; `/db/<name>/mcp` answers `401` to anonymous requests.

- A hidden database answers exactly like an unknown name, in `list_graphs`, `use_graph` and a
  tool's `graph` argument.
- A `read` level gets the read-only profile, with the reading tools only.
- An MCP session belongs to the principal that sent its `initialize`. A request with another
  principal's credentials and that session's id gets `404`.
- MCP sessions survive a restart (`sessions/mcp.json`). A new token, a revoke or a user's
  removal ends that user's MCP sessions; the client gets `404` and initializes again.

There are no admin tools over MCP.

## Modes

All of these are keys in `server.toml`, read at start ([`server-dir.md`](server-dir.md) has
the table):

- **`[access] enabled`** (default `true`). With `false`, nothing is refused: every request is
  an admin. Valid admin credentials keep their identity; everything else runs as `everyone`,
  and the audit names that actor. The start block warns. When the address is not a loopback
  address, the banner says so too: "ANYONE WHO REACHES IT IS AN ADMIN". `access.toml` is still
  read and the first admin is still made, so turning access back on needs nothing else.
- **`[http] public_scheme`** (default `"http"`). Set `"https"` when a reverse proxy in front
  ends TLS, which makes the login cookie `Secure`. On `http` with an address that is not a
  loopback address, the start block warns that tokens and cookies travel in clear text. On
  `https` it warns to let only the proxy reach the address.
- **`[http] allowed_hosts`** (default none). These are the public names a request's `Host` (and
  `Origin`) may carry besides the loopback names and the listen address, for example
  `["db.example.com"]` behind a proxy. Other names get `403`; this is the DNS rebinding check,
  not authentication.
- **`listen`**. The default `127.0.0.1:8484` is reachable from this machine only. Any other
  address gets a warning in the start block.

## Limits an admin sets

- **Query limits** (`[limits.query]` in `server.toml`, and per database in its
  `database.toml`, read at every load) are **ceilings**. A query may lower a limit for itself
  (`g.with("evaluationTimeout", 1000)`), but it can never raise or remove one: that query
  fails with `OptionAboveCeiling`. A key left unset is no ceiling: the engine's default then
  applies, and a query may raise it. To cap a limit at the engine's default, set the key to
  that value explicitly (for example `max_traversers = 10000000`). `0` means no limit (for
  `max_value_depth`, the default).
- **The memory ceiling** (`[limits] memory_limit`, server-wide) is the process memory limit.
  A query, import or write that would cross it fails and rolls back.
- **The backup root** (`[jobs] backup_root`, default `backups/jobs/`) is the only directory
  backup jobs, and the Store panel's fork, attic fork and backup `dir`, write into. Paths are
  relative to it; absolute or escaping paths are refused. Jobs found outside it at load are
  dropped with a warning. Queries never touch files. The name `jobs` is reserved for databases
  and aliases, because admin backups of a database named `jobs` would land in the default
  root.
- **The import inbox** (`imports/`): files put there by any means (scp, a mounted volume) are
  imported by an admin by name (`import DIR --inbox FILE`, the Import tab,
  `POST /api/admin/import {"inbox"}`). The inbox is flat, and a link leading outside it is
  refused. The source path form (`{"path"}`) is admin-only as well.

## Audit

Every administrative event is one log line on the tracing target `graphersal_server::audit`,
with the actor (`user:<name>`, `operator (os:<name>)`, `everyone`, or `-` when unknown). The
operator's lines name the OS user who acted: the console's own user offline, and through the
server the user the server runs as (only the directory's owner can read the operator token).
A command sent with `--token` is logged as the token's user. Events are logged
at `info`; refusals, such as a failed login, at `warn`. The events cover databases (create,
delete, clone, import, restore, promote, backup), the read-only and auto-load flags, aliases
(set, remove, rename), users (add, remove, roles, the first admin), tokens (issued, showing
only `…last4`, and revoked), grants (set, revoked), and logins (succeeded, refused, logout).
No line carries a token or a session id. The lines go to stderr with the rest of the log
(redirect it to keep a file). An offline console command writes its audit lines to its stderr
only when `RUST_LOG` is set (for example `RUST_LOG=graphersal_server::audit=info`): there is no
audit journal yet. `RUST_LOG` selects them like any target:

```bash
# warnings, plus every audit line
RUST_LOG=warn,graphersal_server::audit=info graphersal-server serve --dir ./srv 2>> server.log
grep graphersal_server::audit server.log
```
