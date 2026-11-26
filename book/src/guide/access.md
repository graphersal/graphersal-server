# Users, Tokens and Grants

Who may do what on a graphersal-server: the principals, the users and their tokens, the grants
in `access.toml`, browser logins and MCP. The file format is in the [Configuration
Reference](../reference/configuration.md#accesstoml), the routes in the [Admin
API](../reference/admin_api.md#users-and-grants), the protection of the directory itself in the
[Security Model](security.md).

Access control is on by default. Every request except the health probes (`/healthz`,
`/readyz`), the root `GET /api/info`, `GET /openapi.json` and the files of the web UI needs a
principal that is allowed to make it. Tokens and session cookies travel in clear text over
plain HTTP ([Modes](security.md#modes)).

## Everyday tasks

```bash
graphersal-server user add bob --dir ./srv                     # a user; its token is printed once
graphersal-server user grant bob acme read --dir ./srv         # bob reads acme
graphersal-server user grant bob acme write --dir ./srv        # bob writes acme
graphersal-server user revoke bob acme --dir ./srv
graphersal-server user admin bob on --dir ./srv                # bob becomes an admin
graphersal-server user token bob --dir ./srv                   # a new token for bob
graphersal-server user remove bob --dir ./srv
graphersal-server user list --dir ./srv
```

The web UI's Users tab does the same: users with the token shown once and as `…last4` after,
new token, revoke, admin role, remove; and the grants, `anonymous` included, on databases and
aliases.

### Lost the admin token

A token is never stored, only its hash: a lost token cannot be shown again. Make a new one.
As the OS user who owns the server directory:

```bash
graphersal-server user token admin --dir ./srv
```

The new token is printed once; the old one stops working, and the user's browser and MCP
sessions end. This works with the server running (through its admin API) or stopped (on
`access.toml` directly). It needs no token: the directory's owner is the operator.

Another admin can do the same from anywhere with their own token, in the Users tab ("New
token") or with `--token`.

### Make a database public

```bash
graphersal-server user grant anonymous demo read --dir ./srv
```

`anonymous` is everyone, also requests without a token. A signed-in user always reaches at
least what anonymous reaches.

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
([The security model](security.md#the-directory-owner-is-the-operator)). The operator token cannot log in through the
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
  (`grantsRemoved`), so a later alias or database named `prod` inherits nothing
  ([Grants never outlive their name](#grants-never-outlive-their-name)).

Through an alias the granted level is the higher of the grants on the alias and on its target;
the database's mode still caps it (a read-only target gives `read` at most). A `*` grant works
as before: every database, by its own name and through every alias.

### Grants never outlive their name

Grants name names, so a grant must not survive the thing it names: otherwise whatever takes the
name later (a new database, an alias, a rename onto it) would inherit it.

- **Deleting a database removes every grant on it** (`graphersal-server db delete`, `DELETE
  /api/admin/databases/<name>`, the Databases tab): each is audited as a revoked grant, the
  answer says how many (`grantsRemoved`), the console prints `grants removed: N` and the web UI
  names them in its notice. Removing an alias does the same for the alias's grants.
- **A restore does not bring grants back.** Restoring the backup of a deleted database (under
  its own name or a new one) gives a database without grants: grant access again with
  `graphersal-server user grant` or on the Users tab. A restore over an existing database keeps
  that database's grants (it is still the same name).
- **A name taken over leftovers drops them first.** `access.toml` may still hold grants on a
  name nothing holds: written by 0.2.0 or an earlier 0.3.0 build (which kept a deleted
  database's grants), or left when `access.toml` could not be rewritten during a delete or an
  alias removal. Every operation that gives a name to a new database or alias (create, clone,
  import, a restore into a new or a deleted database's name, a new alias, the new name of an
  alias rename) removes those grants before the name becomes reachable, audits each as a revoked
  grant and warns: the answer carries `leftoverGrantsRemoved: N`, the console prints
  `warning: removed N grants left on this name ...` and the web UI shows a warning notice. If
  `access.toml` cannot be rewritten then, the operation is refused and nothing changes.
- **Leftovers stay visible until then:** the listings mark such a grant as missing, and it can
  be revoked like any other (`graphersal-server user revoke`, `DELETE
  /api/admin/grants/<subject>/<name>`, the Users tab).

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
| `GET /db/<name>/openapi.json` (the database's API description) without a grant | `401` (a client's document, not a page: no redirect) | `404` |
| `GET /openapi.json` (the server's API description) | served (public: it carries no data) | served |
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
only the OS user owning the directory can run them ([The security model](security.md#the-directory-owner-is-the-operator));
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
  request (both unset, so no limit, by default; [Configuration Reference](../reference/configuration.md#servertoml)).

A session's last request is recorded coarsely, at most once a minute. Deleting `sessions/`
while no server runs logs every browser out.

## MCP

`/mcp` and `/db/<name>/mcp` follow the same grants ([MCP](mcp.md)). With a token, an
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
