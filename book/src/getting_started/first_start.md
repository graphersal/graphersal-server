# First Start

This page takes you from nothing to a running server with one database and one user. The
examples use the server directory `./srv` and the default address `127.0.0.1:8484`.

## 1. Create the server directory

```bash
graphersal-server init ./srv
```

`init` creates the directory with its `server.toml`, makes it private to you (mode `0700`) and
creates the first admin user, `admin`. stdout says what it made:

```text
Created the server directory ./srv
  config: ./srv/server.toml (listen 127.0.0.1:8484: edit it before serving)
  MCP:    off, and new databases are not offered to it (switch it on: graphersal-server mcp on --dir ./srv)
  # start it
  graphersal-server serve --dir ./srv
  # or create a database first
  graphersal-server db create NAME --dir ./srv
```

## 2. Copy the first admin token

stderr shows the first admin's token in a frame, **once**:

```text
+----------------------------------------------------------------------------+
| FIRST ADMIN TOKEN - shown once, never again                                |
|                                                                            |
| user:   admin (role admin: every database, the admin API)                  |
| token:  <64 hex digits>                                                    |
|                                                                            |
| Copy it now: only its SHA-256 hash is kept, in                             |
|   ./srv/access.toml                                                        |
| Send it as "Authorization: Bearer <token>", or paste it on the login page. |
| Lost it? This prints a new one (the old one stops working):                |
|   graphersal-server user token admin --dir ./srv                           |
| Server directory: ./srv                                                    |
+----------------------------------------------------------------------------+
```

Keep it in a password manager. You need it for the web UI, for HTTP clients and for MCP. The
console on this machine never needs it: the OS user who owns `./srv` is the server's operator
([Security Model](../guide/security.md)).

Lost it? Print a new one (the old one stops working):

```bash
graphersal-server user token admin --dir ./srv
```

## 3. Look at `server.toml`

`init` writes every setting with its default, commented. The defaults are safe for a first
try: the server listens on the loopback address only. The keys are in the
[Configuration Reference](../reference/configuration.md).

## 4. Start the server

In the background:

```bash
graphersal-server start --dir ./srv
```

`start` waits until the server is ready. Then it prints the start block on stderr (the
sections Server, Databases, Embedding models and MCP) and the address on stdout:

```text
Graphersal server: http://127.0.0.1:8484/
```

The server's log goes to `./srv/logs/server.log`.

Is it running?

```bash
graphersal-server status --dir ./srv
```

```text
── Server ────────────────────────────────────────────────────────────────────
  directory:    ./srv
  state:        running
  pid:          40483
  address:      http://127.0.0.1:8484/
  version:      0.3.0
  uptime:       11 s (since 2026-11-26T02:16:00Z)
  databases:    0 (0 loaded)
  memory:       process 21.0 MiB resident, loaded databases 0 B
  ...
```

`status` exits with `0` when the server runs and `3` when it is stopped.

Stop it cleanly:

```bash
graphersal-server stop --dir ./srv
```

In the foreground instead (Ctrl+C stops it; this is also how a service manager runs it):

```bash
graphersal-server serve --dir ./srv
```

On another port (`0` picks a free one; `server.run` then names the port):

```bash
graphersal-server start --dir ./srv --listen 127.0.0.1:0
```

## 5. Sign in to the web UI

Open <http://127.0.0.1:8484/>. The "Sign in" page asks for a token: paste the first admin's.
You see the admin tabs: Databases, Backups, Import, Export, Models, Runs, Users, MCP and
Database settings ([The Web UI](../guide/web_ui.md)).

## 6. Create and load the first database

From a second terminal, while the server runs:

```bash
graphersal-server db create acme --description "Acme" --dir ./srv
graphersal-server db load acme --dir ./srv
```

The console finds the running server through `./srv/server.run` and sends the command to its
admin API. A loaded database's note shows its address: `served at:
http://127.0.0.1:8484/db/acme/`. Open it: that is the Graphersal playground on `acme`.

A database must be loaded before anyone can query it. To load it at every start:

```bash
graphersal-server db auto-load acme on --dir ./srv
```

## 7. Run a query over HTTP

```bash
TOKEN=<the first admin's token>
curl -s -X POST http://127.0.0.1:8484/db/acme/api/execute -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"script": "g.add_v(\"person\").property(\"name\", \"ada\").iterate()"}'
curl -s -X POST http://127.0.0.1:8484/db/acme/api/execute -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' -d '{"script": "g.v().count().next()"}'
```

The query language is Graphersal's: see <https://graphersal.dev>.

## 8. Add the first user

A user who may only read `acme`:

```bash
graphersal-server user add bob --dir ./srv
graphersal-server user grant bob acme read --dir ./srv
```

`user add` prints bob's token once:

```text
Added user 'bob'
  token:      <64 hex digits>
  shown once: copy it now: only its SHA-256 hash is kept (listings show its last 4 characters)
  use it:     "Authorization: Bearer <token>", or paste it on the login page
```

Give bob the token. Signed in with it, bob sees "My databases" with `acme` only, and every
request of bob's on `acme` is read-only. More in [Users, Tokens and Grants](../guide/access.md).

## 9. Connect an AI agent (optional)

A new server has MCP switched off and offers no database to it. Switch it on and offer `acme`
(or create a database with `--mcp`):

```bash
graphersal-server mcp on --dir ./srv
graphersal-server mcp db acme on --dir ./srv
```

The start block prints the command for Claude Code. With bob's token:

```bash
claude mcp add --transport http --header "Authorization: Bearer <token>" graphersal-server http://127.0.0.1:8484/mcp
```

More in [MCP](../guide/mcp.md).

## Next steps

- Make backups: [Backups and Restore](../guide/backups.md).
- Run it for good: [Running as a Service](../guide/service.md).
- Reach it from other machines: put a TLS proxy in front ([Security Model](../guide/security.md#modes)).
