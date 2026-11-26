# graphersal-server

**A multi-database server for [Graphersal](https://github.com/graphersal/graphersal): many graph
databases in one server directory, served by one process over HTTP and MCP, with a web UI, users
and grants.**

> **EXPERIMENTAL.** graphersal-server is young. Everything here (commands, files, routes,
> answers) may change without notice until a stable version.

Graphersal is an embeddable graph database and traversal engine in Rust. Its command line has a
dev server for ONE graph on a developer's machine. graphersal-server is the server for MANY
graphs: it adds the server directory, a registry of databases, users and grants, backups and the
admin tools.

## Features

- **Many databases, one process.** A database is a Graphersal Store, or a layer over a shared,
  frozen base database (many layers, one base in memory). Databases load and unload while the
  server runs.
- **The Graphersal playground per database.** A loaded database `acme` gets the playground and
  its HTTP API under `/db/acme/`.
- **One MCP endpoint for AI agents.** `/mcp` lets an agent list the databases it may see and pick
  one per session.
- **Administration three ways.** Create, load, clone, back up, restore, export, import, aliases,
  shared embedding models, users and grants, running queries: the same operations as a console
  (`graphersal-server db ...`), an HTTP admin API (with an OpenAPI description) and the server's
  own web UI at `/`.
- **Access per user.** Users hold tokens (only their SHA-256 hashes are stored); grants say which
  database each user may read or write. A database a user has no grant on does not exist for
  that user.
- **Backups.** Verified ZIP backups per database, restore into the same or a new name, and a
  backup of the whole server directory.

## Install

Prebuilt binaries for Linux x86_64 and aarch64 (static), macOS on Apple silicon and Windows
x86_64 are on every [GitHub Release](https://github.com/graphersal/graphersal-server/releases),
from 0.3.0 on. The installers verify the release's `SHA256SUMS` and install the binary only.

Linux and macOS (into `~/.local/bin`):

```bash
curl -fsSL https://github.com/graphersal/graphersal-server/releases/latest/download/install.sh | sh
```

Windows, in PowerShell (into `%LOCALAPPDATA%\Programs\graphersal-server`, added to your `PATH`):

```powershell
irm https://github.com/graphersal/graphersal-server/releases/latest/download/install.ps1 | iex
```

With Rust 1.88 or newer (macOS on Intel and other platforms build from source):

```bash
cargo install --locked graphersal-server     # from source
cargo binstall graphersal-server             # the release binary, through cargo-binstall
```

Manual download, checksum and attestation checks, and building from a clone:
[Installation](book/src/getting_started/installation.md).

In Docker: the repository's `Dockerfile` builds an image (no image is published yet); `/data` is
the server directory, and the first start prints the first admin token once into the container
log ([Running in Docker](book/src/guide/docker.md)):

```bash
docker build -t graphersal-server .
docker run -d --name gsrv -p 127.0.0.1:8484:8484 -v gsrv-data:/data graphersal-server
docker logs gsrv
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

## Documentation

The book in [`book/src/`](book/src/SUMMARY.md) is the documentation: installation, the command
cheat sheet, concepts, databases, backups, models, users and grants, the security model, the web
UI, MCP, HTTP clients, configuration, running as a service or in Docker, troubleshooting, the admin API and
the [release notes](book/src/release_notes/index.md). It is not published on a website: read it
on GitHub, or build and open it with [mdBook](https://rust-lang.github.io/mdBook/):

```bash
mdbook serve book --open
```

Start with [First Start](book/src/getting_started/first_start.md), the [Command Cheat
Sheet](book/src/cheat_sheet.md) and [Troubleshooting](book/src/troubleshooting.md). The query
language, the playground and the Store format are Graphersal's: see its
[documentation](https://graphersal.dev).

## Contributing and security

Issues and pull requests are welcome: [CONTRIBUTING.md](CONTRIBUTING.md) (build, tests, the
Contributor License Agreement). Please report vulnerabilities privately:
[SECURITY.md](SECURITY.md).

## Licence

Licensed under the [Apache License, Version 2.0](LICENSE-APACHE). See [NOTICE](NOTICE). The name
"Graphersal" is covered by the [trademark policy](TRADEMARKS.md).
