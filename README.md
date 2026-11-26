# graphersal-server

**EXPERIMENTAL. Private until its first public version.** Everything here (commands, files,
routes, answers) may change without notice.

A server for [Graphersal](https://graphersal.dev) graph databases: ONE process serving MANY
databases from one server directory, over HTTP.

- A database is a plain Graphersal Store, or a layer over a shared, frozen base database (many
  layers, one base in memory).
- Each loaded database gets the Graphersal playground and its HTTP API under `/db/<name>/`.
- One MCP endpoint (`/mcp`) lets an AI agent list the databases and pick one per session.
- The admin operations (create, load, clone, back up, restore, export, import, aliases, shared
  embedding models, users and grants, running queries) are a console (`graphersal-server db
  ...`), an HTTP API and the server's own web UI at `/`.
- Access per user: `access.toml` holds the users (token hashes) and their grants per database;
  a database a user has no grant on does not exist for that user.

## Documentation

The book in [`book/`](book/src/SUMMARY.md) is the documentation: installation, the command
cheat sheet, concepts, databases, backups, models, users and grants, the security model, the web
UI, MCP, HTTP clients, configuration, running as a service, troubleshooting, the admin API and
the release notes. It is not published on a website. Read it on GitHub under
[`book/src/`](book/src/SUMMARY.md), or build and open it with
[mdBook](https://rust-lang.github.io/mdBook/):

```bash
mdbook serve book --open
```

Start with [First Start](book/src/getting_started/first_start.md), the [Command Cheat
Sheet](book/src/cheat_sheet.md) and [Troubleshooting](book/src/troubleshooting.md).

## Download / build

**Windows x86_64:** each release on GitHub carries a prebuilt binary,
`graphersal-server-v<version>-windows-x86_64.zip` (`graphersal-server.exe` with
`LICENSE-APACHE`, `NOTICE`, this README and `THIRD-PARTY-LICENSES.html`), and `SHA256SUMS` to
verify it. Unpack it and run `graphersal-server.exe` from a console; Ctrl+C stops it cleanly.

**Every other platform** builds from source, with Rust 1.88 or newer:

```bash
git clone https://github.com/graphersal/graphersal-server.git
cd graphersal-server
cargo build --release --locked          # target/release/graphersal-server
```

## Quick start

```bash
graphersal-server init ./srv                     # create a server directory; prints the first admin token ONCE
graphersal-server start --dir ./srv              # serve it in the background on 127.0.0.1:8484
graphersal-server db create acme --dir ./srv     # a new database
graphersal-server db load acme --dir ./srv       # serve it under /db/acme/
graphersal-server user add bob --dir ./srv       # a user; prints its token once
graphersal-server user grant bob acme read --dir ./srv
graphersal-server status --dir ./srv
graphersal-server stop --dir ./srv
```

Open `http://127.0.0.1:8484/` and sign in with the first admin's token. Lost it?
`graphersal-server user token admin --dir ./srv` prints a new one. The console of the OS user
owning `./srv` needs no token. `graphersal-server --help` lists every command.

The server listens on the loopback address by default: tokens travel in clear text over plain
HTTP. To reach it from other machines put a TLS reverse proxy in front (the book's [Security
Model](book/src/guide/security.md)).

## Development

- `scripts/smoke.sh`: an end-to-end smoke test (build, init, serve, the admin API, the database
  pages, MCP, users, the import inbox, a restart keeping the sessions, a clean stop); needs only
  bash, curl and python3 (Unix).
- `scripts/smoke.ps1`: the smoke test of a built binary in PowerShell 7 for Windows (CI and the
  release build): `pwsh scripts/smoke.ps1 -Bin <path to graphersal-server.exe>`.
- CI (`.github/workflows/ci.yml`) and the release build (`release.yml`) run on Windows only for
  now. The release text is the book's page `book/src/release_notes/<version>.md`.

## Licence

Apache-2.0, see `LICENSE-APACHE` and `NOTICE`.
