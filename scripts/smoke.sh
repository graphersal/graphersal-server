#!/usr/bin/env bash
# End-to-end smoke test of graphersal-server (EXPERIMENTAL).
#
# Builds the binary, makes a server directory in a temporary directory, serves it on
# 127.0.0.1:0 (the URL read from server.run) and walks through the admin API, the database
# pages (with a promotion to a base and a layer over it), a model upload (.tar), the MCP endpoint
# and a clean stop.
# Prints one `ok <step>` line per step, or `FAIL <step>: <why>` and exits 1 at the first
# failure. The server is killed on any exit.
#
# Needs only bash, curl, python3 (JSON parsing) and the standard tools.
#
#   scripts/smoke.sh                     # from the repository root (or anywhere)
#   SMOKE_BIN=path/to/graphersal-server  # skip the build, test this binary
#   SMOKE_KEEP=1                         # keep the temporary directory (printed at the end)
set -u

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=$(mktemp -d "${TMPDIR:-/tmp}/graphersal-smoke.XXXXXX")
DIR="$WORK/srv"
BODY="$WORK/body"
HEAD="$WORK/head"
LOG="$WORK/server.log"
SERVER_PID=""
URL=""
STEP="start"
FAILED=0

cleanup() {
  local code=$?
  if [ "$code" != 0 ] && [ "$FAILED" = 0 ]; then
    echo "FAIL $STEP: the script stopped (exit code $code)"
  fi
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill -TERM "$SERVER_PID" 2>/dev/null
    for _ in $(seq 1 50); do
      kill -0 "$SERVER_PID" 2>/dev/null || break
      sleep 0.2
    done
    kill -9 "$SERVER_PID" 2>/dev/null
  fi
  if [ "${SMOKE_KEEP:-0}" = 1 ]; then
    echo "kept $WORK"
  else
    rm -rf "$WORK"
  fi
}
trap cleanup EXIT
trap 'exit 130' INT TERM

ok() { echo "ok   $STEP"; }
fail() {
  FAILED=1
  echo "FAIL $STEP: $*"
  if [ -s "$LOG" ]; then
    echo "--- server log (last 20 lines) ---"
    tail -n 20 "$LOG"
  fi
  exit 1
}

# req METHOD PATH [curl args...]: the answer's status in $STATUS, its body in $BODY (or in the
# file $OUT when set), its head in $HEAD.
req() {
  local method=$1 path=$2
  shift 2
  STATUS=$(curl -sS -X "$method" -o "${OUT:-$BODY}" -D "$HEAD" -w '%{http_code}' --max-time 120 \
    "$@" "${URL}${path}") || fail "curl $method $path failed"
}

# jreq METHOD PATH JSON: a request with a JSON body.
jreq() {
  req "$1" "$2" -H 'Content-Type: application/json' --data-binary "$3"
}

# jget PATH [FILE]: the value at the dotted PATH (keys, list indexes) of the JSON body; strings
# bare, everything else as JSON (`null` when missing).
jget() {
  python3 -I -c '
import json, sys
value = json.load(open(sys.argv[1]))
for key in [k for k in sys.argv[2].split(".") if k]:
    if isinstance(value, list):
        value = value[int(key)] if int(key) < len(value) else None
    elif isinstance(value, dict):
        value = value.get(key)
    else:
        value = None
print(value if isinstance(value, str) else json.dumps(value))
' "${2:-$BODY}" "$1" 2>/dev/null || echo "<not json>"
}

expect_status() {
  [ "$STATUS" = "$1" ] || fail "HTTP $STATUS, expected $1: $(head -c 400 "$BODY")"
}

expect_json() {
  local got
  got=$(jget "$1")
  [ "$got" = "$2" ] || fail "$1 = $got, expected $2: $(head -c 400 "$BODY")"
}

expect_body_contains() {
  grep -q -- "$1" "$BODY" || fail "the answer lacks '$1': $(head -c 400 "$BODY")"
}

header() { # header NAME: the value of the header NAME in $HEAD
  tr -d '\r' <"$HEAD" | awk -v name="$(echo "$1" | tr 'A-Z' 'a-z')" -F': ' \
    'tolower($1) == name { print substr($0, length($1) + 3) }' | tail -n 1
}

# load NAME: loads the database NAME and waits until it is loaded.
load() {
  req POST "/api/admin/databases/$1/load"
  case "$STATUS" in 200 | 202) ;; *) fail "load $1: HTTP $STATUS: $(head -c 400 "$BODY")" ;; esac
  for _ in $(seq 1 300); do
    req GET "/api/admin/databases/$1"
    case "$(jget state)" in
      loaded) return 0 ;;
      loading) sleep 0.2 ;;
      *) fail "load $1: state $(jget state) ($(jget reason))" ;;
    esac
  done
  fail "load $1: still loading after 60 s"
}

# execute DB SCRIPT: runs SCRIPT on /db/DB/api/execute (status 200, ok true); the first result
# in $RESULT.
execute() {
  jreq POST "/db/$1/api/execute" "$(python3 -I -c 'import json,sys; print(json.dumps({"script": sys.argv[1]}))' "$2")"
  expect_status 200
  expect_json ok true
  RESULT=$(jget execution.results.0)
}

# mcp ID METHOD [PARAMS]: one JSON-RPC request to /mcp in the session $MCP_SESSION.
mcp() {
  local message
  message=$(python3 -I -c '
import json, sys
m = {"jsonrpc": "2.0", "method": sys.argv[2]}
if sys.argv[1]:
    m["id"] = int(sys.argv[1])
if len(sys.argv) > 3:
    m["params"] = json.loads(sys.argv[3])
print(json.dumps(m))' "$@")
  local session=()
  [ -n "${MCP_SESSION:-}" ] && session=(-H "Mcp-Session-Id: $MCP_SESSION")
  req POST /mcp -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -H 'MCP-Protocol-Version: 2025-06-18' ${session[@]+"${session[@]}"} --data-binary "$message"
}

# tool NAME ARGS: tools/call; the result's text in $TOOL_TEXT, its isError in $TOOL_ERROR.
tool() {
  mcp 3 tools/call "{\"name\": \"$1\", \"arguments\": $2}"
  expect_status 200
  TOOL_TEXT=$(jget result.content.0.text)
  TOOL_ERROR=$(jget result.isError)
}

# --- build, init, serve -------------------------------------------------------------------

STEP="build"
if [ -n "${SMOKE_BIN:-}" ]; then
  BIN=$SMOKE_BIN
else
  (cd "$ROOT" && cargo build -q -p graphersal-server) >"$WORK/build.log" 2>&1 ||
    fail "cargo build: $(tail -n 20 "$WORK/build.log")"
  BIN="$ROOT/target/debug/graphersal-server"
fi
[ -x "$BIN" ] || fail "no binary at $BIN"
ok

STEP="init"
"$BIN" init "$DIR" >"$WORK/init.out" 2>"$WORK/init.err" || fail "$(cat "$WORK/init.err")"
for part in server.toml databases models/blobs backups trash tmp; do
  [ -e "$DIR/$part" ] || fail "init made no $part"
done
grep -q "^  graphersal-server serve --dir " "$WORK/init.out" ||
  fail "init does not show the serve command on its own line: $(cat "$WORK/init.out")"
ok

STEP="serve"
"$BIN" serve --dir "$DIR" --listen 127.0.0.1:0 >"$WORK/server.out" 2>"$LOG" &
SERVER_PID=$!
for _ in $(seq 1 150); do
  [ -s "$DIR/server.run" ] && break
  kill -0 "$SERVER_PID" 2>/dev/null || fail "the server exited"
  sleep 0.2
done
URL=$(sed -n 's/^url = "\(.*\)"$/\1/p' "$DIR/server.run" 2>/dev/null)
URL=${URL%/}
[ -n "$URL" ] || fail "no url in server.run"
for _ in $(seq 1 150); do
  STATUS=$(curl -s -o /dev/null -w '%{http_code}' "$URL/readyz")
  [ "$STATUS" = 200 ] && break
  sleep 0.2
done
[ "$STATUS" = 200 ] || fail "/readyz answers $STATUS"
# stdout: the address line only; stderr: the start block, no colour codes (not a terminal).
for _ in $(seq 1 50); do
  [ -s "$WORK/server.out" ] && break
  sleep 0.1
done
[ "$(cat "$WORK/server.out")" = "Graphersal server: $URL/" ] ||
  fail "serve's stdout is not the address line: $(cat "$WORK/server.out")"
grep -q "^── Databases (0) ─" "$LOG" || fail "no start block on stderr: $(head -c 600 "$LOG")"
# No log line repeats the start block's address before it.
if grep -q " on $URL/" "$LOG"; then fail "a log line repeats the start block: $(head -c 600 "$LOG")"; fi
if grep -q "$(printf '\033')" "$LOG"; then fail "ANSI codes in the log"; fi
req GET /api/info
expect_status 200
expect_json server graphersal-server
expect_json protocol graphersal/1
expect_json dev false
expect_json uiProfile.name server
expect_json uiProfile.styles.0 server-ui.css
expect_json admin null
expect_json product null
ok

STEP="connection limits in the start block, h2c"
grep -q "connections: *up to 256 open (HTTP/1.1, h2c)$" "$LOG" ||
  fail "no connections row in the start block: $(head -c 900 "$LOG")"
grep -q "timeouts: *request head 30 s, idle connection 60 s$" "$LOG" ||
  fail "no timeouts row in the start block: $(head -c 900 "$LOG")"
# HTTP/2 with prior knowledge on the same port (skipped when this curl has no HTTP/2).
if curl -V | grep -q HTTP2; then
  req GET /api/info --http2-prior-knowledge
  expect_status 200
  expect_json protocol graphersal/1
  head -n 1 "$HEAD" | grep -q '^HTTP/2 200' || fail "not an HTTP/2 answer: $(head -n 1 "$HEAD")"
fi
ok

STEP="the server UI at /"
req GET /
expect_status 200
expect_body_contains '<html'
grep -q 'server-ui/' "$BODY" && fail "/ is not the playground's index.html unchanged"
req GET /server-ui/server-ui.css
expect_status 200
[ "$(header Content-Type)" = "text/css; charset=utf-8" ] || fail "server-ui.css: $(header Content-Type)"
req GET /server-ui/profile.json
expect_status 200
expect_json name server
req GET /server-ui/databases-tab.js
expect_status 200
[ "$(header Content-Type)" = "text/javascript; charset=utf-8" ] || fail "databases-tab.js: $(header Content-Type)"
ok

# --- databases ----------------------------------------------------------------------------

STEP="create plain, base and layer"
jreq POST /api/admin/databases '{"name": "acme", "description": "smoke"}'
expect_status 201
expect_json kind plain
jreq POST /api/admin/databases '{"name": "product", "role": "base"}'
expect_status 201
expect_json role base
expect_json readOnly true
jreq POST /api/admin/databases '{"name": "acme-l", "base": "product"}'
expect_status 201
expect_json kind layer
expect_json base product
req GET /api/admin/databases
expect_status 200
expect_json databases.0.name acme
ok

STEP="load"
load acme
load acme-l
req GET /api/admin/databases/product
expect_json pooled true
ok

STEP="database page info links home"
req GET /db/acme/api/info
expect_status 200
expect_json database acme
expect_json home /
expect_json admin null
expect_json server graphersal-server
expect_json protocol graphersal/1
expect_json dev false
ok

STEP="write and read through /db/<name>/api/execute"
execute acme 'g.add_v("person").property("name", "ada").iterate()'
execute acme 'g.v().has_label("person").count().next()'
[ "$RESULT" = 1 ] || fail "acme holds $RESULT persons, expected 1"
execute acme-l 'g.add_v("person").property("name", "bob").add_v("person").property("name", "cy").iterate()'
execute acme-l 'g.v().has_label("person").count().next()'
[ "$RESULT" = 2 ] || fail "acme-l holds $RESULT persons, expected 2"
ok

STEP="console through the running server"
"$BIN" db list --dir "$DIR" --json >"$WORK/list.json" 2>"$WORK/list.err" ||
  fail "db list: $(cat "$WORK/list.err")"
[ "$(jget databases.0.name "$WORK/list.json")" = acme ] || fail "db list: $(head -c 300 "$WORK/list.json")"
grep -q "admin API" "$WORK/list.err" || fail "db list did not note the server: $(cat "$WORK/list.err")"
# A load through the server shows where the database is served.
"$BIN" db load acme --dir "$DIR" >"$WORK/load.out" 2>"$WORK/load.err" ||
  fail "db load: $(cat "$WORK/load.err")"
grep -q "^  served at: $URL/db/acme/\$" "$WORK/load.out" ||
  fail "db load does not show the database's URL: $(cat "$WORK/load.out")"
# A refusal's help names the console command, not the admin route.
"$BIN" db delete acme --dir "$DIR" >"$WORK/delete.out" 2>"$WORK/delete.err" &&
  fail "db delete of a loaded database succeeded"
grep -q "^Help: unload it first: graphersal-server db unload acme --dir " "$WORK/delete.err" ||
  fail "the help is not the console command: $(cat "$WORK/delete.err")"
if grep -q "/api/admin" "$WORK/delete.err"; then fail "the console help names a route"; fi
ok

STEP="alias flip"
jreq PUT /api/admin/aliases/current '{"target": "acme"}'
expect_status 200
execute current 'g.v().has_label("person").count().next()'
[ "$RESULT" = 1 ] || fail "current -> acme counts $RESULT"
jreq PUT /api/admin/aliases/current '{"target": "acme-l"}'
expect_status 200
execute current 'g.v().has_label("person").count().next()'
[ "$RESULT" = 2 ] || fail "current -> acme-l counts $RESULT"
ok

STEP="read-only refuses a write"
jreq PUT /api/admin/databases/acme/read-only '{"readOnly": true}'
expect_status 200
expect_json readOnly true
jreq POST /db/acme/api/execute '{"script": "g.add_v(\"person\").iterate()"}'
if [ "$STATUS" = 200 ] && [ "$(jget ok)" = true ]; then
  fail "a write on a read-only database succeeded"
fi
expect_body_contains "read-only"
jreq PUT /api/admin/databases/acme/read-only '{"readOnly": false}'
expect_status 200
expect_json readOnly false
execute acme 'g.v().has_label("person").count().next()'
[ "$RESULT" = 1 ] || fail "acme counts $RESULT after the refused write"
ok

STEP="backup and restore into a new name"
req POST /api/admin/databases/acme/backup
expect_status 201
BACKUP=$(jget backup)
[ -f "$DIR/$(jget file)" ] || fail "no backup file $(jget file)"
req GET /api/admin/databases/acme/backups
expect_status 200
expect_json backups.0.backup "$BACKUP"
jreq POST /api/admin/databases/acme/restore "{\"backup\": \"$BACKUP\", \"name\": \"acme-restored\"}"
expect_status 201
expect_json name acme-restored
expect_json replaced false
load acme-restored
execute acme-restored 'g.v().has_label("person").count().next()'
[ "$RESULT" = 1 ] || fail "acme-restored counts $RESULT"
ok

STEP="export and re-import by upload"
OUT="$WORK/acme.gsnap" req GET /api/admin/databases/acme/export
expect_status 200
[ "$(header X-Graphersal-Snapshot)" = full ] || fail "X-Graphersal-Snapshot: $(header X-Graphersal-Snapshot)"
[ -s "$WORK/acme.gsnap" ] || fail "empty export"
req POST "/api/admin/import?name=acme-imported&description=re-imported" \
  -H 'Content-Type: application/octet-stream' --data-binary "@$WORK/acme.gsnap"
expect_status 201
expect_json import.format snapshot
expect_json state unloaded
load acme-imported
execute acme-imported 'g.v().has_label("person").count().next()'
[ "$RESULT" = 1 ] || fail "acme-imported counts $RESULT"
ok

STEP="promote + layer sees base data"
req POST /api/admin/databases/acme-restored/promote
expect_status 409
expect_body_contains "unload"
req POST /api/admin/databases/acme-restored/unload
expect_status 200
req POST /api/admin/databases/acme-restored/promote
expect_status 200
expect_json role base
expect_json readOnly true
jreq POST /api/admin/databases '{"name": "restored-l", "base": "acme-restored"}'
expect_status 201
load restored-l
execute restored-l 'g.v().has_label("person").count().next()'
[ "$RESULT" = 1 ] || fail "restored-l sees $RESULT persons of its base, expected 1"
execute restored-l 'g.add_v("person").property("name", "dan").iterate()'
execute restored-l 'g.v().has_label("person").count().next()'
[ "$RESULT" = 2 ] || fail "restored-l counts $RESULT after its own write, expected 2"
req POST "/api/admin/import?name=acme-base&role=base" \
  -H 'Content-Type: application/octet-stream' --data-binary "@$WORK/acme.gsnap"
expect_status 201
expect_json role base
expect_json readOnly true
ok

STEP="models list"
req GET /api/admin/models
expect_status 200
[ "$(jget models)" = "[]" ] || fail "models: $(jget models)"
[ "$(jget missing)" = "[]" ] || fail "missing: $(jget missing)"
ok

STEP="model upload (.tar of a model directory)"
# A tiny synthetic Model2Vec model (4 tokens x 3 dimensions), as the unit tests' test_models.rs.
mkdir -p "$WORK/tiny-model"
python3 -I - "$WORK/tiny-model" <<'PY' || fail "cannot write the tiny model"
import json, struct, sys, os
d = sys.argv[1]
vocab = ["[UNK]", "graph", "vector", "model"]
open(os.path.join(d, "config.json"), "w").write('{"normalize": true}')
tok = {"version": "1.0", "truncation": None, "padding": None, "added_tokens": [],
       "normalizer": {"type": "Lowercase"}, "pre_tokenizer": {"type": "Whitespace"},
       "post_processor": None, "decoder": None,
       "model": {"type": "WordLevel", "vocab": {t: i for i, t in enumerate(vocab)}, "unk_token": "[UNK]"}}
open(os.path.join(d, "tokenizer.json"), "w").write(json.dumps(tok))
data = b"".join(struct.pack("<f", 1.0 + i) for i in range(len(vocab) * 3))
header = json.dumps({"embeddings": {"dtype": "F32", "shape": [len(vocab), 3], "data_offsets": [0, len(data)]}})
header += " " * (-len(header) % 8)
open(os.path.join(d, "model.safetensors"), "wb").write(struct.pack("<Q", len(header)) + header.encode() + data)
open(os.path.join(d, "README.md"), "w").write("---\nlicense: mit\n---\n# A tiny model\n")
PY
COPYFILE_DISABLE=1 tar -cf "$WORK/tiny-model.tar" --format ustar -C "$WORK" tiny-model \
  || fail "cannot write the model archive"
req POST "/api/admin/models?repo=smoke%2Ftiny&license=mit&format=tar" \
  -H 'Content-Type: application/octet-stream' --data-binary "@$WORK/tiny-model.tar"
expect_status 201
expect_json added.0.repo smoke/tiny
req GET /api/admin/models
expect_json models.0.repo smoke/tiny
ok

# --- MCP ----------------------------------------------------------------------------------

STEP="mcp initialize"
MCP_SESSION=""
mcp 1 initialize '{"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "smoke", "version": "1"}}'
expect_status 200
MCP_SESSION=$(header Mcp-Session-Id)
[ -n "$MCP_SESSION" ] || fail "no Mcp-Session-Id"
mcp "" notifications/initialized
expect_status 202
ok

STEP="mcp tools/list"
mcp 2 tools/list
expect_status 200
for name in list_graphs use_graph gql; do
  expect_body_contains "\"$name\""
done
ok

STEP="mcp list_graphs"
tool list_graphs '{}'
[ "$TOOL_ERROR" != true ] || fail "$TOOL_TEXT"
for name in acme acme-l product current; do
  echo "$TOOL_TEXT" | grep -q "\"$name\"" || fail "list_graphs lacks $name: $TOOL_TEXT"
done
ok

STEP="mcp use_graph"
tool use_graph '{"name": "current"}'
[ "$TOOL_ERROR" != true ] || fail "$TOOL_TEXT"
echo "$TOOL_TEXT" | grep -q '"selected": *"acme-l"' || fail "use_graph: $TOOL_TEXT"
ok

STEP="mcp gql"
tool gql '{"query": "MATCH (p:person) RETURN count(p) AS n"}'
[ "$TOOL_ERROR" != true ] || fail "$TOOL_TEXT"
echo "$TOOL_TEXT" | grep -q '"n": *2' || fail "gql on acme-l: $TOOL_TEXT"
tool gql '{"query": "MATCH (p:person) RETURN count(p) AS n", "graph": "acme"}'
[ "$TOOL_ERROR" != true ] || fail "$TOOL_TEXT"
echo "$TOOL_TEXT" | grep -q '"n": *1' || fail "gql on acme: $TOOL_TEXT"
req DELETE /mcp -H "Mcp-Session-Id: $MCP_SESSION"
case "$STATUS" in 200 | 202 | 204) ;; *) fail "DELETE /mcp: HTTP $STATUS" ;; esac
ok

# --- unload, delete, stop -----------------------------------------------------------------

STEP="unload"
req DELETE /api/admin/databases/acme-imported
expect_status 409
req POST /api/admin/databases/acme-imported/unload
expect_status 200
expect_json unloaded true
req GET /api/admin/databases/acme-imported
expect_json state unloaded
req GET /db/acme-imported/api/info
expect_status 409
ok

STEP="delete"
req DELETE /api/admin/databases/acme-imported
expect_status 200
[ -d "$DIR/$(jget trash)" ] || fail "no trash directory $(jget trash)"
[ ! -e "$DIR/databases/acme-imported" ] || fail "databases/acme-imported is still there"
req GET /api/admin/databases/acme-imported
expect_status 404
ok

STEP="stop"
kill -TERM "$SERVER_PID"
for _ in $(seq 1 300); do
  kill -0 "$SERVER_PID" 2>/dev/null || break
  sleep 0.2
done
kill -0 "$SERVER_PID" 2>/dev/null && fail "the server still runs 60 s after SIGTERM"
wait "$SERVER_PID"
CODE=$?
SERVER_PID=""
[ "$CODE" = 0 ] || fail "exit code $CODE"
[ ! -e "$DIR/server.run" ] || fail "server.run is still there"
if grep -q "panicked" "$LOG"; then fail "the log shows a panic"; fi
grep -q "^Stopped: " "$LOG" || fail "no stop line on stderr: $(tail -c 400 "$LOG")"
ok

echo "smoke: all steps passed"
