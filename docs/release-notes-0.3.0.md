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

## start, stop and status

A server in the background without hunting for its pid:

- **`graphersal-server start --dir DIR [serve options] [--timeout SECS]`** spawns `serve` as a
  detached process (Unix: a new session, no controlling terminal; Windows: `DETACHED_PROCESS |
  CREATE_NEW_PROCESS_GROUP`, no fork), stdin closed, stdout and stderr appended to
  `DIR/logs/server.log` (new: `logs/` mode `0700`, the log `0600`). It waits until the server
  answers `GET /readyz` (at most SECS, default 60), then prints the usual start block on
  stderr and the address line on stdout, and exits 0. A directory without `access.toml` gets
  its first admin from `start` itself, so the token is printed once on the terminal and never
  written into the log. Refused while a server runs on DIR (naming its pid and URL); a server
  that exits before it is ready, or is not ready in time, is exit code 1 with the last lines of
  its log (one still starting after the timeout keeps running, and the error says so).
- **`graphersal-server stop --dir DIR [--timeout SECS]`** asks the running server to stop
  through the new operator-only route `POST /api/admin/shutdown` (with the operator token of
  `server.run`); the server answers `202` and stops exactly as on Ctrl+C. On Unix, when the
  API cannot be reached, it falls back to SIGTERM (the same clean stop). It waits until
  `server.lock` is released (default 60 s; after that the error names `kill`/`taskkill`) and
  prints `stopped`. Without a server it prints `not running` and exits **0** (as LSB init
  scripts do for stopping a stopped service); a stale `server.run` a crashed server left is
  removed and named.
- **`graphersal-server status --dir DIR [--json]`**: running or stopped, the pid, address,
  version, uptime, databases (count, loaded ones by name), memory (the ceiling; the live heap
  is not measured by this build) and the log; a stale `server.run` is reported, not removed.
  Exit code **0** running, **3** stopped (the LSB convention).
- `stop` and `status` also take `--token TOKEN [--server ADDRESS]` like the other console
  commands.
- New admin routes: `GET /api/admin/server` (the server's facts, any admin) and `POST
  /api/admin/shutdown` (the operator or an admin user; `403` for `everyone` with access
  control off; audited). `server.run` gains `started_at_ms`.
- The library: `ServeOptions::with_stop_hook(StopHook)` (what the shutdown route calls; `serve`
  routes it into its Ctrl+C channel), `graphersal_server::daemon` (`start`, `stop`, `status`
  and their `--token` forms), `ServerError::Service`, `AuditEvent::ShutdownRequested`.
- The `serve` start block's `stop` row names `graphersal-server stop --dir DIR`.
- Under systemd or launchd run `serve` in the foreground: examples in
  [`server-dir.md`](server-dir.md#running-as-a-service).
