# syntax=docker/dockerfile:1.7
#
# graphersal-server as a container image: a static musl binary (the build of the Linux release
# binaries of .github/workflows/release.yml: profile `dist`, `--locked`; thin LTO unless
# --build-arg LTO=fat) on Alpine (a shell for the entry point, busybox wget for the health
# check), as the non-root user `graphersal` (uid/gid 65532). No image is published; build it from
# a checkout.
#
#   docker build -t graphersal-server .
#   docker run -d --name gsrv -p 127.0.0.1:8484:8484 -v gsrv-data:/data graphersal-server
#   docker logs gsrv          # the first start prints the first admin token ONCE
#
# /data is the server directory (a volume). On the first start (an empty /data) the entry point
# runs `graphersal-server init /data`, then always `serve --dir /data --listen 0.0.0.0:8484`.
# Book: "Running in Docker" (book/src/guide/docker.md): bind mounts and the owner check, the
# token, a TLS proxy (`allowed_hosts`, `public_scheme`), upgrades, backups.
#
# The base image versions are examples: bump them (RUST_VERSION at least the MSRV 1.88) when you
# build.

ARG RUST_VERSION=1.88
ARG ALPINE_VERSION=3.22

# --- build: a static binary (musl is static by default on Alpine) ----------------------------
FROM rust:${RUST_VERSION}-alpine${ALPINE_VERSION} AS build
# ring (SHA-256, random tokens) compiles C code.
RUN apk add --no-cache musl-dev
WORKDIR /src
# The release binaries link with fat LTO, which needs more memory than a default Docker Desktop
# VM may give; thin LTO builds in less and runs within a few percent of it.
# `--build-arg LTO=fat` builds exactly the release binary.
ARG LTO=thin
ENV CARGO_PROFILE_DIST_LTO=${LTO}
COPY . .
# The registry, git checkouts and the target directory live in BuildKit cache mounts: a source
# change rebuilds only this crate, and no build artefact lands in an image layer.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --locked --profile dist --bin graphersal-server \
    && install -D -m 0755 target/dist/graphersal-server /out/graphersal-server

# --- runtime ---------------------------------------------------------------------------------
FROM alpine:${ALPINE_VERSION}
LABEL org.opencontainers.image.title="graphersal-server" \
      org.opencontainers.image.description="EXPERIMENTAL server for many Graphersal graph databases in one server directory, over HTTP, with a web UI and MCP" \
      org.opencontainers.image.source="https://github.com/graphersal/graphersal-server" \
      org.opencontainers.image.url="https://github.com/graphersal/graphersal-server" \
      org.opencontainers.image.documentation="https://github.com/graphersal/graphersal-server/blob/main/book/src/guide/docker.md" \
      org.opencontainers.image.licenses="Apache-2.0"
# The server's OS user owns the server directory: the directory-owner check refuses a /data
# owned by another user or open to group or others. /data is made 0700 for it, so a new named
# volume mounted there starts empty, owned by it and private.
RUN addgroup -S -g 65532 graphersal \
    && adduser -S -D -H -h /data -s /sbin/nologin -u 65532 -G graphersal graphersal \
    && install -d -m 0700 -o graphersal -g graphersal /data
COPY --from=build /out/graphersal-server /usr/local/bin/graphersal-server
COPY docker/entrypoint.sh /usr/local/bin/docker-entrypoint
COPY LICENSE-APACHE NOTICE /usr/share/doc/graphersal-server/
USER 65532:65532
ENV GRAPHERSAL_SERVER_DIR=/data \
    GRAPHERSAL_SERVER_PORT=8484
VOLUME ["/data"]
EXPOSE 8484
# The server stops cleanly on SIGTERM (it is PID 1: the entry point execs it): databases are
# closed, server.run removed.
STOPSIGNAL SIGTERM
# Readiness: /readyz answers 200 once the server serves (auto-loaded databases loading in the
# background do not hold it back); probes are exempt from the Host/Origin checks.
HEALTHCHECK --interval=10s --timeout=3s --start-period=30s --retries=3 \
    CMD wget -q -O /dev/null "http://127.0.0.1:${GRAPHERSAL_SERVER_PORT}/readyz" || exit 1
ENTRYPOINT ["/usr/local/bin/docker-entrypoint"]
