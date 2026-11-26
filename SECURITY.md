# Security policy

graphersal-server holds graph databases, user tokens (as hashes) and grants, and answers HTTP and
MCP requests. We take reports seriously.

## Supported versions

| Version | Supported |
|---|---|
| 0.x | the latest release only |

Before 1.0 only the latest release receives fixes. Please upgrade to the newest version before
reporting.

## Reporting a vulnerability

Please report vulnerabilities **privately**, in one of two ways:

- through GitHub's private vulnerability reporting:
  [Report a vulnerability](https://github.com/graphersal/graphersal-server/security/advisories/new)
  (the repository's **Security** tab, **Report a vulnerability**);
- by e-mail to **security@graphersal.dev**.

Do not open a public issue, discussion or pull request for a suspected vulnerability.

Include the version (`graphersal-server --version`), your platform, the relevant `server.toml`
settings (never a real token or `access.toml`), the requests or commands that reproduce the
problem, and the impact you expect. You will get an acknowledgement within a week and an
assessment after that. Fixes are released as patch versions and announced in a GitHub security
advisory, which credits you unless you ask otherwise. Please give us a reasonable time to release
a fix before you disclose the issue publicly.

## Scope

The security model is described in the book: [Security Model](book/src/guide/security.md) and
[Users, Tokens and Grants](book/src/guide/access.md). In scope, among others:

- **Authentication and grants.** A request that reaches a database, an admin route or an MCP
  tool without the credentials or the grant it needs; a database revealed to a user without a
  grant on it; a token, a session cookie or the operator token leaked through an answer, a log
  line, an audit record or a file with too open permissions.
- **Browser attacks.** A web page of another site, or a DNS rebinding name, that makes a
  signed-in browser act on the server (the `Host` and `Origin` checks), or script injection
  into the web UI.
- **Files.** A request that reads or writes outside the server directory where it should not
  (database, alias, backup and import names, uploads, restores), or a crafted upload, backup or
  import file that crashes the server or corrupts another database.
- **Denial of service** by an unauthenticated client despite the server's limits (body sizes,
  sessions, timeouts).
- **The server directory check.** A way around the refusal of a directory that is not private
  to its owner (unix).

Out of scope:

- what an admin can do: an admin (and the operator, the OS user owning the server directory) is
  trusted by design, for example to import files the server's OS user can read;
- running with `[access] enabled = false`, which makes every request an admin, as documented and
  announced by the server;
- tokens and cookies sent over plain HTTP to an address that is not the loopback address: the
  server warns, and the book says to put a TLS reverse proxy in front;
- vulnerabilities in the graph engine, its queries, the Store format, the playground or the MCP
  tools themselves: they belong to
  [Graphersal's security policy](https://github.com/graphersal/graphersal/security/policy).
