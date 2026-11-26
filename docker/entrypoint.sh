#!/bin/sh
# The entry point of the graphersal-server image (see the Dockerfile and the book page
# "Running in Docker").
#
# No arguments, or only options (starting with `-`): serve the server directory
# $GRAPHERSAL_SERVER_DIR (default /data) on 0.0.0.0:$GRAPHERSAL_SERVER_PORT (default 8484), the
# options appended to `serve`. When the directory is missing or empty (a new volume) it runs `init` first,
# which prints the first admin token ONCE on stderr, so into the container log.
#
# Any other arguments run graphersal-server with them (`docker run ... IMAGE db list --dir /data`,
# `docker exec CONTAINER graphersal-server ...` works too).
set -eu

DIR=${GRAPHERSAL_SERVER_DIR:-/data}
PORT=${GRAPHERSAL_SERVER_PORT:-8484}
BIN=/usr/local/bin/graphersal-server

case "${1:-}" in
  "" | -*) ;;
  *) exec "$BIN" "$@" ;;
esac

if [ ! -e "$DIR" ] || { [ -d "$DIR" ] && [ -z "$(ls -A "$DIR" 2>/dev/null)" ]; }; then
  echo "graphersal-server: $DIR is empty: creating the server directory (first start)" >&2
  "$BIN" init "$DIR" --set "listen=0.0.0.0:$PORT"
  echo "graphersal-server: the FIRST ADMIN TOKEN above is shown ONCE: copy it from this log now" >&2
fi

exec "$BIN" serve --dir "$DIR" --listen "0.0.0.0:$PORT" "$@"
