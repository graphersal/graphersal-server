# Introduction

**graphersal-server** serves many [Graphersal](https://graphersal.dev) graph databases from one
process. All of them live in one directory on disk, the server directory. Clients reach them
over HTTP.

> **EXPERIMENTAL.** Everything in this book (commands, files, routes, answers) may change
> without notice until a stable version. The source code is on GitHub:
> <https://github.com/graphersal/graphersal-server>.

## What it does

- **Many databases, one process.** Each database is a Graphersal Store. A database can also be
  a *layer* over a shared, frozen *base* database: many layers, one base in memory.
- **The Graphersal playground for each database.** A loaded database `acme` gets the
  playground and its HTTP API under `/db/acme/`.
- **One MCP endpoint for AI agents.** `/mcp` lets an agent list the databases and pick one per
  session.
- **Administration three ways.** The same operations (create, load, clone, back up, restore,
  export, import, aliases, embedding models, users and grants, running queries) are a console
  (`graphersal-server db ...`), an HTTP admin API and the server's own web UI at `/`.
- **Access per user.** Users hold tokens; grants say which database each user may read or
  write. A database a user has no grant on does not exist for that user.

## Who it is for

- **Operators** who run Graphersal databases for a team or a service. Start with
  [Installation](getting_started/installation.md) and [First Start](getting_started/first_start.md).
- **Users and programs** that query the databases. Read [HTTP Clients and
  OpenAPI](guide/http_clients.md) and [MCP](guide/mcp.md).
- **Anyone fixing a problem.** Go to [Troubleshooting](troubleshooting.md).

The [Command Cheat Sheet](cheat_sheet.md) lists every console command as a copy-paste line.

## Relation to Graphersal

Graphersal is an in-memory graph engine modelled on Apache TinkerPop Gremlin, with persistent Stores,
a browser playground and an MCP endpoint. Its documentation is at
<https://graphersal.dev>. The Graphersal command line has a *dev server* for ONE graph on a
developer's machine. graphersal-server is the server for many graphs: it adds the server
directory, the registry of databases, users and grants, backups and the admin tools.

graphersal-server uses the Graphersal crates from crates.io (`graphersal`, `graphersal-serve`,
`graphersal-session`, `graphersal-mcp`), pinned to one exact version. The queries, the
playground, the Store format and the MCP tools are Graphersal's: see its book for the query
language and the Store.

## Reading this book

The book is not published on a website. Read it in the repository on GitHub
(`book/src/`), or build and open it locally:

```bash
mdbook serve book --open
```
