# Security Model

Who can reach what, on the server's machine and over the network. The users, tokens and grants
themselves are in [Users, Tokens and Grants](access.md).

## The directory owner is the operator

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

### Server backups are secrets

A server backup (`graphersal-server backup`, `GET /api/admin/server-backup`, the Backups tab's
"Download server backup"; [Backups and Restore](backups.md#back-up-the-whole-server)) holds `access.toml`:
every user's token HASH (SHA-256) and last four characters, the roles and the grants, beside
every setting (and with `--with-databases` every database's data). A hash is not a token, but
whoever holds the archive can restore it as a server of their own where the old tokens work,
and can try to guess tokens offline (256-bit random tokens: not feasible, but keep it from
anyone who should not have the data anyway). Keep the archive like a password or a private
key:

- it is written with mode `0600` (`<FILE>.part`, renamed when complete); store it where only
  the operator can read it (an encrypted disk or vault), never in a shared folder or a ticket;
- only the operator token and admin users may download it; `everyone` of a server without
  access control is refused (`403`), as for the shutdown; every download is audited;
- it never holds a token, a session id or the run's operator token: sessions (`sessions/`) and
  `server.run` are left out, so a restored server logs every browser out and makes MCP clients
  initialize again; the users log in with their tokens as before;
- after a leak, give every user a new token (`graphersal-server user token NAME`): the archive's
  hashes then match nothing.

### The console with an admin token

`--token-file PATH` (`-` reads the token from standard input), the environment variable
`GRAPHERSAL_SERVER_TOKEN` or `--token TOKEN` sends a console command (`db`, `import`,
`models`, `user`, `mcp`, `config`, `backup`, `stop`, `status`) to the running server with that
user's token, instead of acting as the operator. A token on the command line (`--token`) is
visible to every user of the machine in the process list (`ps`): the console prints a warning
on stderr then; the file and the variable keep it out. The server's address is `--server
ADDRESS` (`IP:PORT` or `http://IP:PORT/`), else the address in the directory's `server.run`.
Nothing in the directory is written, so the permission checks do not apply: the token is the
credential, and the server checks it like any request (a wrong token is refused with `401`; a
user without the admin role sees no admin API). The audit names the token's user. Without a
running server a token is refused (no command acts on the directory offline with a token).
`--server` without a token is a usage error. `--dir DIR` is still required; with `--server` it
only names the directory in the printed text.

```bash
# another OS user, or another machine through an SSH tunnel to the loopback address
graphersal-server db list --dir /srv/graphs --server 127.0.0.1:8484 --token-file ~/.graphersal-token
echo "$TOKEN" | graphersal-server user grant bob acme read --dir /srv/graphs --token-file -
```

**Port squatting on a shared machine.** A port is not owned by anyone: when the server is down,
any local user may listen on its address and collect the `Authorization` header a console or a
browser sends there. So without `--server` the console sends a token to the address of
`server.run` only while a server holds `server.lock` of that directory and `server.run` belongs
to the directory's owner; a stale `server.run` (left by a crash) is refused with "no server
holds server.lock". With `--server ADDRESS` nothing can check who listens there: on a machine
other users log in to, name the address only while you know the server runs (`graphersal-server
status --dir DIR` as its owner), and never paste a token into a login page you reached after a
restart you did not see. A privileged port (below 1024) or a reverse proxy that ends TLS on its
own port keeps other users from taking the address.

## What access control off protects

With `[access] enabled = false` every request is the admin principal `everyone` (valid admin
credentials keep their identity). `everyone` is refused only what would hand out or use real
credentials, with `403`:

- the shutdown (`POST /api/admin/shutdown`, `stop` through the API) and the server backup
  (`GET /api/admin/server-backup`, it holds the token hashes);
- a new user (`POST /api/admin/users`) and a new token for a user (`POST
  /api/admin/users/<name>/token`): otherwise a request without credentials could make itself
  an admin token and get past the two refusals above.

Nothing else is refused. Whoever reaches the address reads, changes and deletes every database,
changes the grants and the settings, revokes tokens and imports files, as an admin would.
Access control off is for a server only you can reach (the loopback address of a one-user
machine, a container nobody else talks to); it protects no data.

## Admins have the service account's file access

An admin (the operator, a user with the admin role) acts with the file access of the OS account
the server runs as:

- `POST /api/admin/import {"path"}` and `POST /api/admin/models {"path"}` (the console's
  `import DIR SOURCE`, `models add SOURCE`) read any file or directory that account can read. Only
  what parses imports (GraphSON, GraphML, a snapshot, a Store directory, a model); errors never
  echo a file's content, but a readable Store of another server directory imports fine;
- `PUT /api/admin/config` with `jobs.backup_root` (applies at the next start) moves the file root
  of every database's backup jobs to any directory that account can write.

Give the admin role only to people you would let act as that account on its files. Run every
server directory under a service account of its own: then an admin of one server cannot import
another's databases by path.

## Limits for clients without credentials

A client without credentials can reach the login, the UI files, the probes and, with an
anonymous grant, the databases granted to anonymous. What it can make the server hold is
bounded:

- the login body is at most 4 KiB (`413` beyond, read no further);
- the body of a request that may run without credentials (the login; with access control on,
  any request naming no `Authorization` and no session cookie) must arrive within 10 seconds,
  and the unread rest of a refused request's body is drained for at most 5 seconds; a slower
  client gets `400` and its connection is closed. Authenticated bodies (a large upload) have no
  total deadline, only the 120-second wait for each next piece;
- an anonymous body to an MCP endpoint is at most 1 MiB (`413`);
- `/mcp` refuses anonymous with `401` unless `access.toml` grants anonymous something, and one
  principal holds at most `[mcp] max_sessions_per_identity` MCP sessions (default 16): a new one
  ends that principal's own oldest, so a flood of `initialize` requests never evicts another
  principal's sessions;
- `/healthz` and `/readyz` are answered at once, also when every connection place is taken (a
  liveness probe does not kill a server that is being flooded); the request threads are bounded
  (`[limits] max_connections` + 32, between 32 and 512), further requests wait for one;
- one user holds at most 32 browser sessions (a new login ends the oldest), and a session ends
  30 days after its login or 7 days after its last request by default (`[sessions]`; `0` lifts
  a limit).

The connection cap itself (`[limits] max_connections`, the 30-second header timeout) still
lets a client that opens many connections fill it for a while; put a reverse proxy with
per-client limits in front of a server reachable from untrusted networks.

## Runs and the change feed

A database's playground names its page with `X-Graphersal-Client`, and the engine keys a page's
runs (stopping them with `POST /db/<name>/api/cancel`) and names its changes (`GET
/db/<name>/api/changes`, `recent[].by`) by it. The header is chosen by the client, so the server
never passes it on: it replaces it with a keyed hash of the principal's identity (a key made at
each start, never the identity itself) followed by the page's own id. Another principal sending
the same id gets another key and stops nothing. The change feed shows a reader only its own
page ids; a change of another identity is `by: "another identity"` without its detail. The run
administration (`/api/admin/runs`) shows the rewritten ids.

## Pages and framing

Every answer carries `X-Frame-Options: DENY` and `Content-Security-Policy: frame-ancestors
'none'; object-src 'none'; base-uri 'none'`: no other site may frame the admin UI or a
database's playground (which share one origin), load plugins into them or change their base
URL. Scripts are not restricted further, because the embedded playground runs inline scripts,
an import map and modules from its CDNs.

## Modes

All of these are keys in `server.toml`, read at start ([The Server Directory](../reference/server_directory.md) has
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

## Audit

Every administrative event is one log line on the tracing target `graphersal_server::audit`,
with the actor (`user:<name>`, `operator (os:<name>)`, `everyone`, or `-` when unknown). The
operator's lines name the OS user who acted: the console's own user offline, and through the
server the user the server runs as (only the directory's owner can read the operator token).
A command sent with `--token` is logged as the token's user. Events are logged
at `info`; refusals, such as a failed login, at `warn`. The events cover databases (create,
delete, clone, import, restore, promote, backup), server backups (offline and downloaded), the read-only and auto-load flags, aliases
(set, remove, rename), users (add, remove, roles, the first admin), tokens (issued, showing
only `…last4`, and revoked), grants (set, revoked), logins (succeeded, refused, logout), a shutdown request, and
after a crash (round 3) the unclean shutdown found at start (actor `-`), each database's check
or verify (`database 'x' checked: ok|damaged (...)`), a repair into a new database and a swap.
No line carries a token or a session id. The lines go to stderr with the rest of the log
(redirect it to keep a file). An offline console command writes its audit lines to its stderr
only when `RUST_LOG` is set (for example `RUST_LOG=graphersal_server::audit=info`): there is no
audit journal yet. `RUST_LOG` selects them like any target:

```bash
# warnings, plus every audit line
RUST_LOG=warn,graphersal_server::audit=info graphersal-server serve --dir ./srv 2>> server.log
grep graphersal_server::audit server.log
```

## Plain HTTP and reverse proxies

The server speaks plain HTTP (HTTP/1.1 and h2c), no TLS. Tokens and session cookies travel in
clear text. So:

- keep it on the loopback address (the default `127.0.0.1:8484`) for use on one machine, or
  reach it through an SSH tunnel:

  ```bash
  ssh -N -L 8484:127.0.0.1:8484 server.example.com
  ```

- or put a reverse proxy that ends TLS in front (nginx, Caddy, ...), and set in `server.toml`:

  ```toml
  [http]
  public_scheme = "https"
  allowed_hosts = ["db.example.com"]
  ```

  Let only the proxy reach the server's address.

Every request but the health probes is checked by its `Host` header (the listen address's names
and `allowed_hosts`) and its `Origin` (a browser page of another site): `403` otherwise. This
stops DNS rebinding and cross-site pages; it is not authentication.
