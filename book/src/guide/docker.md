# Running in Docker

The repository has a `Dockerfile` that builds graphersal-server into a small image: the static
Linux binary (the build of the release binaries: profile `dist`, `--locked`) on Alpine, running
as the non-root user `graphersal` (uid and gid 65532), with the server directory as the volume
`/data`, port `8484`, a `HEALTHCHECK` on `/readyz` and `STOPSIGNAL SIGTERM`.

**No image is published yet.** Build it from a checkout of the repository.

## Build the image

```bash
docker build -t graphersal-server .            # in the repository checkout
```

The build stage is `rust:<RUST_VERSION>-alpine` (musl: a static binary, symbols stripped). It
links with thin LTO by default: the fat LTO of the release binaries needs more memory than a
default Docker Desktop VM may give (the linker is killed). Options:

```bash
docker build --build-arg LTO=fat -t graphersal-server .          # exactly the release binary
docker build --build-arg RUST_VERSION=1.91 -t graphersal-server . # another toolchain (>= 1.88)
```

The base image tags in the `Dockerfile` are examples: bump them when you build. Dependencies and
the target directory live in BuildKit cache mounts, so a source change rebuilds only the
server's own crate; `docker builder prune` removes that cache. The build context is the
workspace manifest, `Cargo.lock`, `crates/` and the entry point (`.dockerignore`): no git
history, no `target/`, no server directory you made by hand.

## Run it

With a named volume (the simplest):

```bash
docker volume create gsrv-data
docker run -d --name gsrv -p 127.0.0.1:8484:8484 -v gsrv-data:/data graphersal-server
docker logs gsrv
```

The entry point does this:

1. When `/data` is empty (a new volume), it runs `graphersal-server init /data --set
   listen=0.0.0.0:8484`: the server directory with its `server.toml`, private to the
   `graphersal` user (`0700`, secrets `0600`), and the first admin user `admin`.
2. Then always `graphersal-server serve --dir /data --listen 0.0.0.0:8484`, in the foreground
   as PID 1, so `docker stop` (SIGTERM) stops it cleanly.

The port inside the container must be on `0.0.0.0` to be reachable through `-p`. Publish it on
the host's loopback address (`-p 127.0.0.1:8484:8484`) unless a TLS proxy on another machine
must reach it: tokens travel in clear text over plain HTTP ([Behind a TLS
proxy](#behind-a-tls-proxy)).

Environment variables of the entry point:

| Variable | Default | Meaning |
|---|---|---|
| `GRAPHERSAL_SERVER_DIR` | `/data` | the server directory |
| `GRAPHERSAL_SERVER_PORT` | `8484` | the port inside the container (`--listen 0.0.0.0:<port>`, and the health check). It overrides `server.toml`'s `listen` at every start |
| `RUST_LOG` | `info` | the log level (the log goes to stderr: `docker logs`) |

Options after the image name are appended to `serve` (`docker run ... graphersal-server
--max-connections 64`). Any other arguments run `graphersal-server` with them instead, for
example a one-off console command on a stopped server's volume:

```bash
docker run --rm -v gsrv-data:/data graphersal-server db list --dir /data
```

## The first admin token

The first start prints the first admin's token **once**, in a frame, into the container log
(stderr):

```bash
docker logs gsrv 2>&1 | grep -A 12 'FIRST ADMIN TOKEN'
```

Copy it into a password manager: only its SHA-256 hash is kept in `/data/access.toml`. Open
<http://127.0.0.1:8484/> and paste it on the "Sign in" page, or send it as `Authorization:
Bearer <token>`. The log keeps the token as long as the container exists; remove the container
(`docker rm`) or rotate the token once you have copied it if others can read the Docker log.

Lost it? Print a new one (the old one stops working):

```bash
docker exec gsrv graphersal-server user token admin --dir /data
```

## The console inside the container

`docker exec` runs a command as the image's user, which owns `/data`: the console is the
server's operator and needs no token ([Security
Model](security.md#the-directory-owner-is-the-operator)). It finds the running server through
`/data/server.run`:

```bash
docker exec gsrv graphersal-server db create acme --dir /data
docker exec gsrv graphersal-server db load acme --dir /data
docker exec gsrv graphersal-server user add bob --dir /data
docker exec gsrv graphersal-server status --dir /data
```

From the host (with the binary installed there), an admin token works too, as for any remote
server; `--dir` then names no real directory ([The console with an admin
token](security.md#the-console-with-an-admin-token)):

```bash
graphersal-server db list --dir gsrv --server 127.0.0.1:8484 --token-file ~/.gsrv-token
```

Files to import must be inside the container: copy them into the import inbox
`/data/imports/`, or upload them in the web UI (Import tab):

```bash
docker cp acme.gsnap gsrv:/data/imports/
docker exec gsrv graphersal-server import /data --inbox acme.gsnap --name acme
docker exec gsrv rm /data/imports/acme.gsnap
```

## A bind mount and the owner check

The server refuses a server directory owned by another OS user or open to group or others
([Security Model](security.md#the-directory-owner-is-the-operator)). The image runs as uid
65532, so:

- A **named volume** mounted at `/data` starts as a copy of the image's `/data`: empty, owned by
  uid 65532, mode `0700`. Nothing to do.
- A **bind mount** of a host directory keeps the host's owner and mode. Give the directory to
  uid 65532 and make it private before the first start:

  ```bash
  sudo install -d -o 65532 -g 65532 -m 700 /srv/gsrv
  docker run -d --name gsrv -p 127.0.0.1:8484:8484 -v /srv/gsrv:/data graphersal-server
  ```

  Or run the container as the directory's owner instead, `--user "$(id -u):$(id -g)"` with a
  directory of your own (`mkdir -m 700 ./gsrv`).

A directory of another owner gives a clear refusal on the first start (and on every later
one), and the container exits with code 1:

```text
graphersal-server: /data is empty: creating the server directory (first start)
Error: refused: the server directory /data is not private to its owner: /data is owned by root (uid 0), not by graphersal (uid 65532): only the directory's owner is its operator
Help: the OS user owning the server directory is its operator: ...
```

The `Help:` lines name the commands outside a container (`sudo -u`, `chown`); in Docker the fix
is the `install -d -o 65532 -g 65532 -m 700` above, or a named volume. A container started with
`--user` another uid than the directory's owner is refused the same way ("owned by graphersal
(uid 65532), not by uid 1000"). On Docker Desktop (macOS, Windows) a bind-mounted host
directory shows up owned by `root` inside the container, whatever its owner on the host: use a
named volume there. The server's lock (`server.lock`) is an
OS file lock: serve one server directory from one container at a time, and keep it on a local
volume, not a network file system.

## Behind a TLS proxy

The server speaks plain HTTP. To reach it from other machines, put a reverse proxy that ends TLS
in front (Caddy, nginx, Traefik, a cloud load balancer) and tell the server its public name and
scheme:

```bash
docker exec gsrv graphersal-server config set http.allowed_hosts db.example.com --dir /data
docker exec gsrv graphersal-server config set http.public_scheme https --dir /data
docker restart gsrv          # allowed_hosts applies at the next start
```

- `allowed_hosts`: the server refuses a request whose `Host` (or browser `Origin`) is a name
  with a dot it does not know with `403` (the DNS rebinding check). IP addresses, `localhost`
  and names without a dot (a container name such as `gsrv` on a Docker network) pass; the
  proxy's public name (`db.example.com`) must be listed.
- `public_scheme = "https"`: the login cookie gets `Secure`, and the OpenAPI descriptions name
  `https://` URLs.

Let only the proxy reach the published port: publish it on the loopback address of the proxy's
machine (`-p 127.0.0.1:8484:8484`), or put the server and the proxy on one Docker network
without `-p`. With Caddy in a container on the same network:

```text
db.example.com {
    reverse_proxy gsrv:8484
}
```

The keys: [Security Model](security.md#modes), [Configuration
Reference](../reference/configuration.md).

## Health checks

The image's `HEALTHCHECK` asks `GET /readyz` on `127.0.0.1` inside the container every 10
seconds (busybox `wget`): `200` once the server serves, `503` while it starts or stops.
Databases with `auto_load` load in the background and do not hold it back. The probes need no
token and are exempt from the `Host` check, so orchestrators can use them too: `GET /healthz`
for liveness, `GET /readyz` for readiness ([HTTP Routes](../reference/http_routes.md)).

```bash
docker inspect --format '{{.State.Health.Status}}' gsrv     # starting, then healthy
```

## Stopping and restarting

`docker stop` sends SIGTERM: the server unloads its databases (each Store closed cleanly),
removes `server.run`, releases its lock and exits with code 0. Give a server with large loaded databases time to close them (`docker stop -t
60`). Everything lives in `/data`: a new container on the same volume (`docker rm gsrv`, then
the same `docker run`) serves the same databases, users and tokens; `init` does not run again,
because `/data` is not empty. Browser sessions survive a restart.

A container killed by force (`docker kill`, an OOM kill) leaves the directory as after a crash:
the next start checks the databases that were open ([A server was killed by
force](../troubleshooting.md#a-server-was-killed-by-force)).

## Upgrades

Build the image of the new version, then replace the container; the volume stays:

```bash
git pull && docker build -t graphersal-server .
docker stop gsrv && docker rm gsrv
docker run -d --name gsrv -p 127.0.0.1:8484:8484 -v gsrv-data:/data graphersal-server
```

Read the [release notes](../release_notes/index.md) of the new version first: an upgrade step
they name (for example `init` once on an existing directory) runs as a console command in the
container (`docker exec gsrv graphersal-server init /data`). Back up the server before an
upgrade (below).

## Backups of the volume

The supported way is the server backup, through the running server:

```bash
docker exec gsrv graphersal-server backup --dir /data --with-databases --out /data/tmp/full.zip
docker cp gsrv:/data/tmp/full.zip ./gsrv-backup.zip
docker exec gsrv rm /data/tmp/full.zip
```

The archive holds the users' token hashes: keep it like a password (it is written with mode
`0600`). Restore it into a new, empty volume with a one-off container, then serve that volume
(on Linux the mounted archive must be readable for uid 65532: `sudo chown 65532
gsrv-backup.zip` for the restore):

```bash
docker volume create gsrv-restored
docker run --rm -v gsrv-restored:/data -v "$PWD/gsrv-backup.zip:/backup.zip:ro" \
  graphersal-server restore /backup.zip --dir /data
docker run -d --name gsrv2 -p 127.0.0.1:8484:8484 -v gsrv-restored:/data graphersal-server
```

Database backups (`db backup NAME`, backup jobs) land in `/data/backups/`, inside the volume:
copy them out with `docker cp`, or set `[jobs] backup_root` to another mounted directory. A
copy of the whole volume (`docker run --rm -v gsrv-data:/data -v "$PWD:/out" alpine tar -C
/data -czf /out/gsrv-data.tgz .`) is consistent only while the server is stopped. More in
[Backups and Restore](backups.md).
