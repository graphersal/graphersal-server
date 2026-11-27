#!/usr/bin/env bash
# A server directory to try the MCP profiles by hand (web UI, MCP tab): MCP on, the database
# `demo` (a few people, saved queries in the folders `kb`, `kb/admin` and `memory`, one at the
# root), the database `notes`, the alias `current` -> demo, a user `reader` with read grants;
# the server started in the background on 127.0.0.1:PORT (`graphersal-server start`).
# Prints the admin's and the reader's tokens and how to stop it.
#
# Usage: scripts/mcp-profile-demo.sh DIR [PORT]     (DIR missing or empty; PORT default 18484)
# BIN: the binary (default target/debug/graphersal-server; build it first: cargo build).
# Needs bash, curl and python3 only. Synthetic data only.
set -euo pipefail

DIR=${1:?usage: scripts/mcp-profile-demo.sh DIR [PORT]}
PORT=${2:-18484}
BIN=${BIN:-target/debug/graphersal-server}
[ -x "$BIN" ] || { echo "no binary at $BIN: run cargo build (or set BIN)" >&2; exit 2; }
if [ -e "$DIR" ] && [ -n "$(ls -A "$DIR" 2>/dev/null)" ]; then
  echo "$DIR is not empty: give a new directory" >&2
  exit 2
fi
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

"$BIN" init "$DIR" >/dev/null 2>"$WORK/init.err"
ADMIN=$(sed -n 's/^| token: *\([^ |]*\) *|$/\1/p' "$WORK/init.err")
[ -n "$ADMIN" ] || { echo "init printed no admin token: $(cat "$WORK/init.err")" >&2; exit 1; }
"$BIN" mcp on --dir "$DIR" >/dev/null
"$BIN" db create demo --mcp --auto-load --description "MCP profile demo" --dir "$DIR" >/dev/null
"$BIN" db create notes --mcp --auto-load --dir "$DIR" >/dev/null
"$BIN" db alias set current demo --mcp --dir "$DIR" >/dev/null
"$BIN" user add reader --dir "$DIR" >"$WORK/reader.out"
READER=$(sed -n 's/^  token: *//p' "$WORK/reader.out")
for db in demo notes current; do
  "$BIN" user grant reader "$db" read --dir "$DIR" >/dev/null
done

"$BIN" start --dir "$DIR" --listen "127.0.0.1:$PORT" >/dev/null 2>"$WORK/start.err" ||
  { cat "$WORK/start.err" >&2; exit 1; }
URL="http://127.0.0.1:$PORT"
# Wait until both databases are loaded (auto-load).
for _ in $(seq 1 120); do
  states=$(curl -s -H "Authorization: Bearer $ADMIN" "$URL/api/admin/databases" |
    python3 -I -c 'import json,sys; d=json.load(sys.stdin); print(" ".join(e["state"] for e in d["databases"]))' 2>/dev/null || true)
  [ "$states" = "loaded loaded" ] && break
  sleep 0.5
done

# execute DB SCRIPT: runs SCRIPT on /db/DB/api/execute as the admin.
execute() {
  local body
  body=$(python3 -I -c 'import json,sys; print(json.dumps({"script": sys.argv[1]}))' "$2")
  curl -sf -H "Authorization: Bearer $ADMIN" -H 'Content-Type: application/json' \
    "$URL/db/$1/api/execute" -d "$body" >/dev/null ||
    { echo "execute on $1 failed: $2" >&2; exit 1; }
}
execute demo 'g.add_v("person").property("name", "ada").property("age", 36).add_v("person").property("name", "bob").property("age", 29).add_v("person").property("name", "cy").property("age", 41).iterate()'
execute demo 'g.define_query(#{name: "people", folder: "kb", description: "The names of all people.", body: "fn people() { g.v().has_label(\"person\").values(\"name\") }"})'
execute demo 'g.define_query(#{name: "older_than", folder: "kb", description: "People older than min.", params: #{min: #{schema: #{type: "integer"}, default: 30}}, body: "fn older_than(min) { g.v().has_label(\"person\").has(\"age\", P.gt(min)).values(\"name\") }"})'
execute demo 'g.define_query(#{name: "add_person", folder: "kb/admin", description: "Adds a person.", params: #{name: #{schema: #{type: "string"}}}, body: "fn add_person(name) { g.add_v(\"person\").property(\"name\", name).values(\"name\") }"})'
execute demo 'g.define_query(#{name: "recent", folder: "memory", description: "The people, newest first.", body: "fn recent() { g.v().has_label(\"person\").values(\"name\").order().by(Order.desc) }"})'
execute demo 'g.define_query(#{name: "count_all", description: "How many vertices.", body: "fn count_all() { g.v().count() }"})'

cat <<EOF
Server:        $URL/   (stop it: $BIN stop --dir $DIR)
Admin token:   $ADMIN
Reader token:  $READER   (read grants on demo, notes, current)
Databases:     demo (saved queries in kb, kb/admin, memory, one at the root), notes, alias current -> demo
EOF
