# graphersal-server 0.3.0 (unreleased)

**EXPERIMENTAL.** Everything here (commands, files, routes, answers) may change without notice
in later versions. This is a draft: it grows with every change until the release.

**Upgrading from 0.2.0:** `serve` and the console now refuse a server directory that is not
private to its owner. A directory made by 0.2.0 usually has `sessions/` (and maybe the
directory itself) open to group or others: run `graphersal-server init DIR` once as the
directory's owner; it tightens the modes and lists them under `owner only`. The error of a
refused command names the exact `chmod`/`chown` too.

## The server directory is the boundary

The OS user owning the server directory is the operator (PostgreSQL's peer-authentication
model): whoever can write the directory could edit `access.toml` or read the operator token
anyway, so the console needs no credentials there. 0.3.0 enforces that the directory really is
private ([`access.md`](access.md#the-security-model)):

- **`init`** creates the directory with mode `0700`, `sessions/` with `0700` and the secret
  files (`access.toml`, `server.run`, `sessions/*.json`) with `0600`; every later write keeps
  these modes. On an existing directory owned by the current user it tightens the modes it
  finds too open (`owner only` in its output).
- **`serve` and every console command** (`db`, `import`, `models`, `user`; offline and through
  a running server with the operator token) refuse, with exit code 1, a directory or file
  owned by another OS user (root on another user's directory included), a directory,
  `server.toml` or `aliases.toml` writable by group or others, and a secret (`access.toml`,
  `server.run`, `sessions/` and its files) readable or writable by group or others. The error
  lists every problem and the command fixing it (`chmod 600 ...`, `sudo -u OWNER ...`,
  `sudo chown -R ...`).
- **Windows:** no check (it would need the Win32 security API); the directory's ACL is the
  boundary.
- **Audit:** the operator's records name the OS user, `actor="operator (os:<name>)"`: the
  console's own user offline, and the user the server runs as for the operator token through
  the server. An offline console command logs its audit lines on stderr when `RUST_LOG` is set
  (`RUST_LOG=graphersal_server::audit=info`).
- **`--token TOKEN`** (or `GRAPHERSAL_SERVER_TOKEN`) for `db`, `import`, `models` and `user`:
  the command goes to the running server with that user's admin token (address `--server
  ADDRESS`, else the directory's `server.run`) and touches no file of the directory, so the
  permission checks do not apply; the audit names the token's user. Without a running server
  it is refused. `--server` without a token is a usage error.
