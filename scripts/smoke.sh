#!/usr/bin/env bash
# End-to-end smoke test of graphersal-server (EXPERIMENTAL).
#
# Builds the binary, makes a server directory in a temporary directory, serves it on
# 127.0.0.1:0 (the URL read from server.run) and walks through the admin API, the database
# pages (with a promotion to a base and a layer over it), users and a read-only user, the run
# administration, the import inbox, a model upload (.tar, .tar.gz), the MCP endpoint, a clean
# stop, a restart that keeps the browser and MCP sessions, and start/status/stop in the
# background.
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
# file $OUT when set), its head in $HEAD. With $AUTH set (the first admin's token after init),
# the request carries it as `Authorization: Bearer`; `AUTH= req ...` sends it anonymously.
req() {
  local method=$1 path=$2
  shift 2
  local auth=()
  [ -n "${AUTH:-}" ] && auth=(-H "Authorization: Bearer $AUTH")
  STATUS=$(curl -sS -X "$method" -o "${OUT:-$BODY}" -D "$HEAD" -w '%{http_code}' --max-time 120 \
    ${auth[@]+"${auth[@]}"} "$@" "${URL}${path}") || fail "curl $method $path failed"
}

# jreq METHOD PATH JSON [curl args...]: a request with a JSON body.
jreq() {
  req "$1" "$2" -H 'Content-Type: application/json' --data-binary "$3" "${@:4}"
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
for part in server.toml databases models/blobs backups trash tmp imports; do
  [ -e "$DIR/$part" ] || fail "init made no $part"
done
grep -q "^  graphersal-server serve --dir " "$WORK/init.out" ||
  fail "init does not show the serve command on its own line: $(cat "$WORK/init.out")"
# The first admin's token, printed once in a framed block on stderr: every request below goes
# as that admin unless it says otherwise.
AUTH=$(sed -n 's/^| token: *\([0-9a-f]\{64\}\) .*$/\1/p' "$WORK/init.err")
[ -n "$AUTH" ] || fail "init printed no first admin token: $(cat "$WORK/init.err")"
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

STEP="access: anonymous is refused, the first admin logs in and out"
AUTH= req GET /api/info
expect_status 200
expect_json principal.kind anonymous
expect_json login /
expect_json uiProfile.name server
expect_json uploadBytes null
AUTH= req GET /api/admin/databases
expect_status 401
expect_json login /
AUTH= req GET /db/acme/api/info
expect_status 401
AUTH= req GET /db/acme/
expect_status 303
[ "$(header Location)" = "/?next=/db/acme/" ] || fail "no login redirect: $(header Location)"
req GET /api/info
expect_json principal.kind user
expect_json principal.admin true
AUTH= jreq POST /api/login '{"token": "0000"}'
expect_status 401
ADMIN_TOKEN=$AUTH
AUTH= jreq POST /api/login "{\"token\": \"$ADMIN_TOKEN\"}"
expect_status 200
COOKIE=$(header Set-Cookie)
case "$COOKIE" in
  gs_session=*"; HttpOnly; SameSite=Strict; Path=/") ;;
  *) fail "login cookie: $COOKIE" ;;
esac
COOKIE=${COOKIE%%;*}
AUTH= req GET /api/admin/databases -H "Cookie: $COOKIE"
expect_status 200
AUTH= req POST /api/logout -H "Cookie: $COOKIE"
expect_status 200
expect_json loggedOut true
AUTH= req GET /api/admin/databases -H "Cookie: $COOKIE"
expect_status 401
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

STEP="the server directory is private to its owner; --token goes through the server"
[ "$(stat -c %a "$DIR" 2>/dev/null || stat -f %Lp "$DIR")" = 700 ] || fail "init did not make $DIR 0700"
chmod 770 "$DIR"
"$BIN" db list --dir "$DIR" >"$WORK/perm.out" 2>"$WORK/perm.err" &&
  fail "db list on a group-writable directory succeeded"
grep -q "^  chmod 700 " "$WORK/perm.err" || fail "the refusal names no chmod: $(cat "$WORK/perm.err")"
# An admin token needs no file access: it goes through the running server.
GRAPHERSAL_SERVER_TOKEN=$ADMIN_TOKEN "$BIN" db list --dir "$DIR" --server "${URL#http://}" --json \
  >"$WORK/token.json" 2>"$WORK/token.err" || fail "db list --token: $(cat "$WORK/token.err")"
[ "$(jget databases.0.name "$WORK/token.json")" = acme ] || fail "db list --token: $(head -c 300 "$WORK/token.json")"
chmod 700 "$DIR"
ok

STEP="users: the console adds a user, the API grants, a new token ends the old"
"$BIN" user add smokey --dir "$DIR" >"$WORK/user.out" 2>"$WORK/user.err" ||
  fail "user add: $(cat "$WORK/user.err")"
USER_TOKEN=$(sed -n 's/^  token: *//p' "$WORK/user.out")
[ ${#USER_TOKEN} = 64 ] || fail "user add printed no token: $(cat "$WORK/user.out")"
AUTH=$USER_TOKEN req GET /db/acme/api/info
expect_status 404
jreq POST /api/admin/grants '{"subject": "smokey", "database": "acme", "level": "read"}'
expect_status 200
expect_json subject user:smokey
AUTH=$USER_TOKEN req GET /db/acme/api/info
expect_status 200
AUTH=$USER_TOKEN req GET /api/admin/users
expect_status 404
req GET /api/admin/users/smokey
expect_status 200
expect_json tokenLast4 "${USER_TOKEN:60:4}"
"$BIN" user list --dir "$DIR" >"$WORK/users.out" 2>"$WORK/users.err" ||
  fail "user list: $(cat "$WORK/users.err")"
grep -q "…${USER_TOKEN:60:4}" "$WORK/users.out" || fail "user list: $(cat "$WORK/users.out")"
req POST /api/admin/users/smokey/token
expect_status 200
AUTH=$USER_TOKEN req GET /api/info
expect_status 401
"$BIN" user remove admin --dir "$DIR" >"$WORK/rm.out" 2>"$WORK/rm.err" &&
  fail "the last admin was removed"
grep -q "is the last admin" "$WORK/rm.err" || fail "last admin: $(cat "$WORK/rm.err")"
"$BIN" user remove smokey --dir "$DIR" >"$WORK/rm.out" 2>"$WORK/rm.err" ||
  fail "user remove: $(cat "$WORK/rm.err")"
ok

STEP="a read-only user: reads its database, cannot write, does not see another"
"$BIN" user add reader --dir "$DIR" >"$WORK/reader.out" 2>"$WORK/reader.err" ||
  fail "user add: $(cat "$WORK/reader.err")"
READER_TOKEN=$(sed -n 's/^  token: *//p' "$WORK/reader.out")
[ ${#READER_TOKEN} = 64 ] || fail "user add printed no token: $(cat "$WORK/reader.out")"
"$BIN" user grant reader acme read --dir "$DIR" >"$WORK/grant.out" 2>"$WORK/grant.err" ||
  fail "user grant: $(cat "$WORK/grant.err")"
AUTH=$READER_TOKEN execute acme 'g.v().has_label("person").count().next()'
[ "$RESULT" = 1 ] || fail "the reader counts $RESULT persons on acme, expected 1"
AUTH=$READER_TOKEN jreq POST /db/acme/api/execute '{"script": "g.add_v(\"person\").iterate()"}'
if [ "$STATUS" = 200 ] && [ "$(jget ok)" = true ]; then
  fail "a read-only user's write succeeded: $(head -c 400 "$BODY")"
fi
execute acme 'g.v().has_label("person").count().next()'
[ "$RESULT" = 1 ] || fail "acme counts $RESULT after the reader's refused write"
AUTH=$READER_TOKEN req GET /db/acme-l/api/info
expect_status 404
AUTH=$READER_TOKEN req GET /api/databases
expect_status 200
expect_json databases.0.name acme
expect_json databases.0.level read
expect_json databases.1 null
AUTH=$READER_TOKEN req GET /api/admin/databases
expect_status 404
ok

STEP="the reserved name jobs is refused"
jreq POST /api/admin/databases '{"name": "jobs"}'
expect_status 400
expect_body_contains "reserved"
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

STEP="a stale revision is refused (409), the current one passes"
req GET /api/admin/aliases
expect_status 200
REV=$(jget revision)
[ "$(header ETag)" = "\"$REV\"" ] || fail "ETag $(header ETag), revision $REV"
jreq PUT /api/admin/aliases/current "{\"target\": \"acme\", \"revision\": \"$REV\"}"
expect_status 200
jreq PUT /api/admin/aliases/current "{\"target\": \"acme-l\", \"revision\": \"$REV\"}"
expect_status 409
expect_json help "changed by someone else: reload"
NEW=$(jget revision)
req GET /api/admin/aliases
expect_json aliases.current acme
req DELETE /api/admin/aliases/current -H "If-Match: \"$REV\""
expect_status 409
jreq PUT /api/admin/aliases/current '{"target": "acme-l"}' -H "If-Match: \"$NEW\""
expect_status 200
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

STEP="database settings: view, a change based on the revision, 409, a limit at the next load (B7)"
req GET /api/admin/databases/acme/settings
expect_status 200
expect_json name acme
expect_json database.state loaded
expect_json store.damagePolicy maintenance
expect_json limits.loaded true
REV=$(jget revision)
[ "$(header ETag)" = "\"$REV\"" ] || fail "ETag $(header ETag), revision $REV"
jreq PUT /api/admin/databases/acme/settings "{\"description\": \"smoke settings\", \"revision\": \"$REV\"}"
expect_status 200
expect_json database.description "smoke settings"
jreq PUT /api/admin/databases/acme/settings "{\"autoLoad\": true, \"revision\": \"$REV\"}"
expect_status 409
expect_json help "changed by someone else: reload"
jreq PUT /api/admin/databases/acme/settings '{"limits": {"max_operations": -1}}'
expect_status 400
jreq PUT /api/admin/databases/acme/settings '{"limits": {"max_operations": 1000}}'
expect_status 200
expect_json limits.reloadNeeded true
HEAVY='let n = 0; while n < 20000 { n += 1; } n'
execute acme "$HEAVY"
[ "$RESULT" = 20000 ] || fail "not limited before the next load: $RESULT"
req POST /api/admin/databases/acme/unload
expect_status 200
load acme
jreq POST /db/acme/api/execute "$(python3 -I -c 'import json,sys; print(json.dumps({"script": sys.argv[1]}))' "$HEAVY")"
[ "$(jget ok)" = false ] || fail "the limit did not apply after the load: $(head -c 400 "$BODY")"
expect_body_contains "operations"
jreq PUT /api/admin/databases/acme/settings '{"limits": {"max_operations": null}}'
expect_status 200
req POST /api/admin/databases/acme/unload
expect_status 200
load acme
execute acme "$HEAVY"
ok

STEP="run administration (list and stop another client's query)"
req GET /api/admin/runs
expect_status 200
expect_json textHeadChars 200
req GET /db/acme/api/admin/runs
expect_status 404
# A page's endless query, in the background; the admin sees it and stops it.
curl -sS -o "$WORK/endless.json" --max-time 120 -H 'Content-Type: application/json' \
  -H 'X-Graphersal-Client: smoke' -H "Authorization: Bearer $AUTH" \
  --data-binary '{"script": "let n = 0; loop { n += 1; }", "runId": "endless"}' \
  "${URL}/db/acme/api/execute" &
ENDLESS_PID=$!
RUN_ID=""
for _ in $(seq 1 300); do
  req GET /api/admin/databases/acme/runs
  expect_status 200
  RUN_ID=$(python3 -I -c '
import json, sys
runs = json.load(open(sys.argv[1])).get("runs", [])
print(next((str(r["id"]) for r in runs if r.get("alias") == "endless" and r.get("state") == "running"), ""))
' "$BODY")
  [ -n "$RUN_ID" ] && break
  sleep 0.1
done
[ -n "$RUN_ID" ] || fail "the endless query never showed: $(head -c 400 "$BODY")"
req GET /api/admin/runs
expect_body_contains '"client":"page:smoke"'
jreq POST /api/admin/databases/acme/runs/cancel "{\"id\": $RUN_ID}"
expect_status 200
expect_json cancelled true
wait "$ENDLESS_PID" || fail "the endless query's request failed"
grep -q '"ok":false' "$WORK/endless.json" || fail "the stopped query answered: $(head -c 400 "$WORK/endless.json")"
jreq POST /api/admin/databases/acme/runs/cancel "{\"id\": $RUN_ID}"
expect_json cancelled false
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

STEP="import from the inbox"
cp "$WORK/acme.gsnap" "$DIR/imports/acme-inbox.gsnap"
req GET /api/admin/imports
expect_status 200
expect_json imports.0.name acme-inbox.gsnap
expect_json imports.0.kind snapshot
jreq POST /api/admin/import '{"inbox": "../server.toml", "name": "evil"}'
expect_status 400
jreq POST /api/admin/import '{"inbox": "acme-inbox.gsnap", "name": "acme-inbox"}'
expect_status 201
expect_json import.format snapshot
load acme-inbox
execute acme-inbox 'g.v().has_label("person").count().next()'
[ "$RESULT" = 1 ] || fail "acme-inbox counts $RESULT"
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

STEP="model upload (.tar and .tar.gz of a model directory)"
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
# The same model as a gzip-compressed tar (format detected): the same blob, added again.
gzip -c "$WORK/tiny-model.tar" > "$WORK/tiny-model.tar.gz" || fail "cannot gzip the archive"
req POST "/api/admin/models?repo=smoke%2Ftiny&license=mit" \
  -H 'Content-Type: application/octet-stream' --data-binary "@$WORK/tiny-model.tar.gz"
expect_status 201
expect_json added.0.repo smoke/tiny
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
echo "$TOOL_TEXT" | grep -q '"selected": *"current", *"graph": *"acme-l"' || fail "use_graph: $TOOL_TEXT"
ok

STEP="mcp gql"
tool gql '{"query": "MATCH (p:person) RETURN count(p) AS n"}'
[ "$TOOL_ERROR" != true ] || fail "$TOOL_TEXT"
echo "$TOOL_TEXT" | grep -q '"n": *2' || fail "gql on acme-l: $TOOL_TEXT"
tool gql '{"query": "MATCH (p:person) RETURN count(p) AS n", "graph": "acme"}'
[ "$TOOL_ERROR" != true ] || fail "$TOOL_TEXT"
echo "$TOOL_TEXT" | grep -q '"n": *1' || fail "gql on acme: $TOOL_TEXT"
ok

STEP="mcp switched off and on, a database hidden, statistics (B6)"
"$BIN" mcp off --dir "$DIR" >"$WORK/mcp.out" 2>"$WORK/mcp.err" || fail "mcp off: $(cat "$WORK/mcp.err")"
grep -q "^MCP is switched off\$" "$WORK/mcp.out" || fail "mcp off: $(cat "$WORK/mcp.out")"
mcp 2 tools/list
expect_status 503
"$BIN" mcp on --dir "$DIR" >"$WORK/mcp.out" 2>"$WORK/mcp.err" || fail "mcp on: $(cat "$WORK/mcp.err")"
mcp 2 tools/list
expect_status 200
"$BIN" mcp db product off --dir "$DIR" >"$WORK/mcp.out" 2>"$WORK/mcp.err" ||
  fail "mcp db product off: $(cat "$WORK/mcp.err")"
tool list_graphs '{}'
echo "$TOOL_TEXT" | grep -q '"product"' && fail "product still listed: $TOOL_TEXT"
jreq PUT /api/admin/mcp/databases/product '{"exposed": true}'
expect_status 200
req GET /api/admin/mcp
expect_status 200
expect_json serving true
[ "$(jget usage.calls.refused)" -ge 1 ] || fail "no refused request counted: $(cat "$BODY")"
ok

STEP="alias rename moves the MCP session"
"$BIN" db alias rename current live --dir "$DIR" >"$WORK/rename.out" 2>"$WORK/rename.err" ||
  fail "db alias rename: $(cat "$WORK/rename.err")"
grep -q "^Renamed alias 'current' to 'live' -> database 'acme-l'\$" "$WORK/rename.out" ||
  fail "db alias rename: $(cat "$WORK/rename.out")"
grep -q "^  MCP sessions moved: 1\$" "$WORK/rename.out" ||
  fail "the open MCP session did not move: $(cat "$WORK/rename.out")"
tool gql '{"query": "MATCH (p:person) RETURN count(p) AS n"}'
[ "$TOOL_ERROR" != true ] || fail "$TOOL_TEXT"
case "$TOOL_TEXT" in
  'note: alias "current" was renamed to "live"'*) ;;
  *) fail "no rename note: $TOOL_TEXT" ;;
esac
echo "$TOOL_TEXT" | grep -q '"n": *2' || fail "gql on live: $TOOL_TEXT"
tool gql '{"query": "MATCH (p:person) RETURN count(p) AS n"}'
case "$TOOL_TEXT" in note:*) fail "the note came twice: $TOOL_TEXT" ;; esac
jreq POST /api/admin/aliases/live/rename '{"name": "acme-l"}'
expect_status 409
req GET /db/current/api/info
expect_status 404
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

STEP="sessions: a browser login and an MCP session before a restart"
AUTH= jreq POST /api/login "{\"token\": \"$ADMIN_TOKEN\"}"
expect_status 200
KEPT_COOKIE=$(header Set-Cookie)
KEPT_COOKIE=${KEPT_COOKIE%%;*}
MCP_SESSION=""
mcp 1 initialize '{"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "smoke", "version": "1"}}'
expect_status 200
MCP_SESSION=$(header Mcp-Session-Id)
[ -n "$MCP_SESSION" ] || fail "no MCP session id"
tool use_graph '{"name": "acme"}'
[ "$TOOL_ERROR" != true ] || fail "use_graph: $TOOL_TEXT"
[ -s "$DIR/sessions/browser.json" ] && [ -s "$DIR/sessions/mcp.json" ] ||
  fail "no session files in $DIR/sessions"
if grep -q "${KEPT_COOKIE#gs_session=}" "$DIR/sessions/browser.json"; then
  fail "a clear session id in sessions/browser.json"
fi
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

STEP="restart: the browser and MCP sessions survive"
"$BIN" serve --dir "$DIR" --listen 127.0.0.1:0 >"$WORK/server2.out" 2>"$WORK/server2.log" &
SERVER_PID=$!
for _ in $(seq 1 150); do
  [ -s "$DIR/server.run" ] && break
  kill -0 "$SERVER_PID" 2>/dev/null || fail "the server exited: $(tail -c 400 "$WORK/server2.log")"
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
AUTH= req GET /api/admin/databases -H "Cookie: $KEPT_COOKIE"
expect_status 200
# The session keeps its selection: acme is not loaded after the restart, and the call names it.
tool gql '{"query": "MATCH (p:person) RETURN count(p) AS n"}'
[ "$TOOL_ERROR" = true ] || fail "acme answered although it is not loaded: $TOOL_TEXT"
case "$TOOL_TEXT" in
  *"database 'acme' is not loaded"*) ;;
  *) fail "the MCP session lost its selection: $TOOL_TEXT" ;;
esac
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
if grep -q "panicked" "$WORK/server2.log"; then fail "the log shows a panic"; fi
ok

STEP="start, status, stop in the background"
LOG="$DIR/logs/server.log"
"$BIN" start --dir "$DIR" --listen 127.0.0.1:0 >"$WORK/start.out" 2>"$WORK/start.err" ||
  fail "start: $(cat "$WORK/start.err")"
SERVER_PID=$(sed -n 's/^pid = //p' "$DIR/server.run")
[ -n "$SERVER_PID" ] || fail "start wrote no pid into server.run"
grep -q '^Graphersal server: http://127.0.0.1:' "$WORK/start.out" ||
  fail "start printed no address line: $(cat "$WORK/start.out")"
grep -q 'Started in the background' "$WORK/start.err" || fail "no start note: $(cat "$WORK/start.err")"
"$BIN" status --dir "$DIR" >"$WORK/status.out" 2>&1 || fail "status of a running server: $(cat "$WORK/status.out")"
grep -q 'running' "$WORK/status.out" || fail "status: $(cat "$WORK/status.out")"
"$BIN" stop --dir "$DIR" >"$WORK/stop.out" 2>&1 || fail "stop: $(cat "$WORK/stop.out")"
grep -q '^stopped: ' "$WORK/stop.out" || fail "stop: $(cat "$WORK/stop.out")"
kill -0 "$SERVER_PID" 2>/dev/null && fail "the server still runs after stop"
SERVER_PID=""
CODE=0
"$BIN" status --dir "$DIR" >"$WORK/status.out" 2>&1 || CODE=$?
[ "$CODE" = 3 ] || fail "status of a stopped server: exit code $CODE"
if grep -q "panicked" "$LOG"; then fail "the log shows a panic"; fi
ok

STEP="kill -9 and restart: the unclean-shutdown check, verify (B-CRASH)"
"$BIN" db create crashy --dir "$DIR" >/dev/null 2>&1 || fail "db create crashy"
"$BIN" start --dir "$DIR" --listen 127.0.0.1:0 >"$WORK/start2.out" 2>"$WORK/start2.err" ||
  fail "start: $(cat "$WORK/start2.err")"
SERVER_PID=$(sed -n 's/^pid = //p' "$DIR/server.run")
"$BIN" db load crashy --dir "$DIR" >/dev/null 2>"$WORK/load.err" || fail "db load: $(cat "$WORK/load.err")"
grep -q '"crashy"' "$DIR/server.open" || fail "server.open does not name crashy: $(cat "$DIR/server.open")"
kill -9 "$SERVER_PID"
for _ in $(seq 1 100); do kill -0 "$SERVER_PID" 2>/dev/null || break; sleep 0.1; done
SERVER_PID=""
[ -f "$DIR/server.run" ] || fail "a killed server should leave server.run behind"
CODE=0
"$BIN" status --dir "$DIR" >"$WORK/status.out" 2>&1 || CODE=$?
[ "$CODE" = 3 ] || fail "status after the kill: exit code $CODE"
grep -q 'Last unclean shutdown' "$WORK/status.out" || fail "status: $(cat "$WORK/status.out")"
"$BIN" start --dir "$DIR" --listen 127.0.0.1:0 >"$WORK/start3.out" 2>"$WORK/start3.err" ||
  fail "start after the kill: $(cat "$WORK/start3.err")"
SERVER_PID=$(sed -n 's/^pid = //p' "$DIR/server.run")
grep -q 'UNCLEAN SHUTDOWN' "$WORK/start3.err" || fail "no unclean-shutdown notice: $(cat "$WORK/start3.err")"
DONE=""
for _ in $(seq 1 300); do
  "$BIN" status --dir "$DIR" --json >"$WORK/status.json" 2>/dev/null || true
  DONE=$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); u=(d.get("server") or {}).get("uncleanShutdown") or {}; print("yes" if u.get("done") else "")' "$WORK/status.json")
  [ -n "$DONE" ] && break
  sleep 0.2
done
[ -n "$DONE" ] || fail "the unclean-shutdown check did not end: $(cat "$WORK/status.json")"
OUTCOME=$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d["server"]["uncleanShutdown"]["databases"][0]["outcome"])' "$WORK/status.json")
[ "$OUTCOME" = ok ] || fail "crashy was not checked ok: $(cat "$WORK/status.json")"
"$BIN" db verify --all --dir "$DIR" >"$WORK/verify.out" 2>&1 || fail "db verify --all: $(cat "$WORK/verify.out")"
"$BIN" stop --dir "$DIR" >/dev/null 2>&1 || fail "stop"
SERVER_PID=""
[ ! -f "$DIR/server.open" ] || fail "a clean stop should remove server.open"
ok

echo "smoke: all steps passed"
