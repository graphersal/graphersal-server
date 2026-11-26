# Command Cheat Sheet

Every console command as a line to copy. The server directory is `./srv` and the address
`127.0.0.1:8484`; replace them with yours. Every command takes `--json` for the admin API's JSON
answer instead of text. The full options are in [Console Commands](reference/console.md) and
`graphersal-server --help`.

With a server running on the directory, a `db`, `import`, `models`, `user`, `mcp` or `backup`
command goes through the server's admin API. Without one it works on the directory itself
(then `db load` and `db unload` are refused).

## Server

```bash
graphersal-server init ./srv                               # create a server directory, print the first admin token
graphersal-server init ./srv                               # on an existing directory: add missing parts, tighten modes
graphersal-server init ./srv --config my-server.toml       # a new directory with your server.toml (checked first)
graphersal-server init ./srv --set listen=127.0.0.1:9000 --set limits.query.timeout_ms=30000
graphersal-server start --dir ./srv                        # serve in the background, wait until ready
graphersal-server start --dir ./srv --listen 127.0.0.1:0   # on a free port
graphersal-server start --dir ./srv --timeout 120          # wait up to 120 s for readiness
graphersal-server status --dir ./srv                       # running (exit 0) or stopped (exit 3)
graphersal-server status --dir ./srv --json                # the same as JSON
graphersal-server stop --dir ./srv                         # clean stop, waits until stopped
graphersal-server serve --dir ./srv                        # in the foreground (Ctrl+C stops it)
graphersal-server serve --dir ./srv --listen 0.0.0.0:8484  # every interface (prints a warning)
graphersal-server serve --dir ./srv --max-connections 512 --header-timeout 10 --idle-timeout 120
tail -f ./srv/logs/server.log                              # the log of a server `start` runs
graphersal-server --version
graphersal-server --help
```

## Server settings (server.toml)

```bash
graphersal-server config get --dir ./srv                               # every key: value, default, when it applies, restart pending
graphersal-server config get limits.upload_bytes --dir ./srv           # one key
graphersal-server config set limits.upload_bytes 1073741824 --dir ./srv  # through a running server: applied at once
graphersal-server config set limits.query.timeout_ms 30000 --dir ./srv # every database without its own value, at its next load
graphersal-server config set defaults.auto_load true --dir ./srv       # what new databases start with
graphersal-server config set http.allowed_hosts db.example.com --dir ./srv   # after a restart (restart pending)
graphersal-server config unset sessions.lifetime_secs --dir ./srv      # back to the default
graphersal-server config check ./my-server.toml                        # check a file, no server needed
```

## Databases

```bash
graphersal-server db list --dir ./srv                                  # databases, aliases, states, sizes
graphersal-server db info acme --dir ./srv                             # everything about one database
graphersal-server db create acme --description "Acme" --dir ./srv      # a new empty database
graphersal-server db create acme --auto-load --dir ./srv               # ... loaded at every start
graphersal-server db create acme --no-read-only --dir ./srv            # ... writable even when defaults.read_only is on
graphersal-server db create acme --schema schema.json --dir ./srv      # ... with a schema
graphersal-server db create acme --mcp --dir ./srv                     # ... offered to MCP at once (also clone, alias set, restore, import)
graphersal-server db load acme --dir ./srv                             # load (running server only), waits
graphersal-server db load acme --no-wait --dir ./srv                   # load without waiting
graphersal-server db unload acme --dir ./srv                           # unload (running server only)
graphersal-server db clone acme acme-copy --dir ./srv                  # a fork of acme's latest state
graphersal-server db delete acme --dir ./srv                           # an unloaded database, to trash/
graphersal-server db read-only acme on --dir ./srv                     # | off
graphersal-server db auto-load acme on --dir ./srv                     # | off
graphersal-server db set acme description "Acme's graph" --dir ./srv
graphersal-server db set acme limit timeout_ms 5000 --dir ./srv        # a query limit override
graphersal-server db set acme limit timeout_ms --clear --dir ./srv     # back to the server's default
graphersal-server db export acme --out acme.gsnap --dir ./srv          # a packed snapshot with models
graphersal-server db export acme --out acme.gsnap --no-models --dir ./srv
```

A query limit change applies at the database's next load:

```bash
graphersal-server db unload acme --dir ./srv && graphersal-server db load acme --dir ./srv
```

## Bases and layers

```bash
graphersal-server db promote products --dir ./srv                      # an unloaded plain database becomes a base, for good
graphersal-server db reset-layer scratch --yes --dir ./srv             # discard a layer's changes; base untouched, old layer to trash/
graphersal-server db create products --role base --dir ./srv           # an empty base
graphersal-server db create acme --base products --dir ./srv           # a layer over the base
graphersal-server db export acme --out acme-full.gsnap --materialize --dir ./srv   # a layer as a standalone snapshot
```

## Aliases

```bash
graphersal-server db alias set prod acme-2026-11 --dir ./srv           # create, or flip to another database
graphersal-server db alias rename prod live --dir ./srv                # grants and MCP sessions move along
graphersal-server db alias remove prod --dir ./srv                     # its grants go with it
```

## Import

```bash
graphersal-server import ./srv /data/acme.gsnap --name acme                      # a packed snapshot
graphersal-server import ./srv /data/acme-store --name acme                      # a Store directory (or .gstore)
graphersal-server import ./srv /data/acme.json --name acme --format graphson     # GraphSON (.json, .jsonl, .graphson)
graphersal-server import ./srv /data/acme.graphml --name acme                    # GraphML (.xml, .graphml)
graphersal-server import ./srv /data/acme.layer.gsnap --name acme --base products   # a layer snapshot over its base
graphersal-server import ./srv /data/products.gsnap --name products --role base  # import as a base
graphersal-server import ./srv /data/acme.gsnap --name acme --description "Acme"
graphersal-server import ./srv --list                                            # the import inbox ./srv/imports/
graphersal-server import ./srv --inbox acme.gsnap --name acme                    # import one inbox entry
```

## Backups and restore

```bash
graphersal-server db backup acme --dir ./srv                           # a verified ZIP in ./srv/backups/acme/
graphersal-server db backups acme --dir ./srv                          # list them (also of a deleted database)
graphersal-server db restore acme 2026-11-22T10-00-00Z --dir ./srv     # over acme (unload it first)
graphersal-server db restore acme 2026-11-22T10-00-00Z --name acme-old --dir ./srv   # into a new database
graphersal-server backup --dir ./srv                                   # the server's configuration, one ZIP (a secret)
graphersal-server backup --dir ./srv --with-databases --out /secure/srv-full.zip   # configuration + every database
graphersal-server restore /secure/srv-full.zip --dir ./srv-new         # a NEW server directory from a server backup
graphersal-server restore old.zip --dir ./srv-new --force-version      # an archive of a newer server version
```

## After a crash: verify, repair, swap

```bash
graphersal-server status --dir ./srv                                   # the last unclean shutdown, per database
graphersal-server db verify acme --dir ./srv                           # scrub one Store (exit 1 on damage)
graphersal-server db verify --all --dir ./srv                          # scrub every Store
graphersal-server db repair acme --dir ./srv                           # a repaired copy: the new database acme-repaired
graphersal-server db repair acme --name acme-fixed --dir ./srv         # ... under another name
graphersal-server db swap acme acme-repaired --yes --dir ./srv         # the copy takes the name; the old one to trash/
```

## Embedding models

```bash
graphersal-server models list --dir ./srv
graphersal-server models add /models/potion-base-8M --dir ./srv                  # a Model2Vec model directory
graphersal-server models add /models/model.tar.gz --repo org/model --license mit --dir ./srv   # a .tar or .tar.gz of one
graphersal-server models add /data/with-models.gsnap --dir ./srv                 # the models a .gsnap carries
graphersal-server models add org/model --dir ./srv                               # a model models/ already holds
graphersal-server models remove org/model --dir ./srv                            # refused while a database uses it
```

## Users, tokens and grants

```bash
graphersal-server user list --dir ./srv                                # users, token ends, grants
graphersal-server user add bob --dir ./srv                             # prints bob's token once
graphersal-server user add carol --admin --dir ./srv                   # an admin
graphersal-server user token bob --dir ./srv                           # a new token (the old one stops working)
graphersal-server user token bob --revoke --dir ./srv                  # no token at all
graphersal-server user token admin --dir ./srv                         # lost the admin token: a new one
graphersal-server user admin bob on --dir ./srv                        # | off: the admin role
graphersal-server user remove bob --dir ./srv                          # the user and its grants
graphersal-server user grant bob acme read --dir ./srv                 # | write
graphersal-server user grant bob prod read --dir ./srv                 # on an alias: reached only through the alias
graphersal-server user grant bob '*' read --dir ./srv                  # every database
graphersal-server user grant anonymous demo read --dir ./srv           # demo is public
graphersal-server user revoke bob acme --dir ./srv
```

## MCP

```bash
graphersal-server mcp status --dir ./srv                               # switches, databases, usage statistics
graphersal-server mcp on --dir ./srv                                   # | off: MCP for the whole server (a new server: off)
graphersal-server mcp expose-new on --dir ./srv                        # | off: offer NEW databases and aliases (default off)
graphersal-server mcp db acme off --dir ./srv                          # | on: hide acme from MCP
graphersal-server mcp db acme read-only on --dir ./srv                 # | off: MCP only reads acme
claude mcp add --transport http --header "Authorization: Bearer <token>" graphersal http://127.0.0.1:8484/mcp
claude mcp add --transport http --header "Authorization: Bearer <token>" acme http://127.0.0.1:8484/db/acme/mcp
```

## OpenAPI

```bash
graphersal-server openapi --dir ./srv --out server-openapi.json        # the server's own API
graphersal-server openapi --dir ./srv --db acme --out acme-openapi.json   # a database's API
graphersal-server openapi --dir ./srv --db acme --server https://db.example.com
curl -s http://127.0.0.1:8484/openapi.json                             # the same from a running server
```

## As another administrator: a token instead of the directory

```bash
export GRAPHERSAL_SERVER_TOKEN=<an admin's token>                      # keeps the token out of the process list
graphersal-server db list --dir ./srv                                  # through the running server
graphersal-server db list --dir ./srv --server 127.0.0.1:8484          # at this address
graphersal-server user list --dir ./srv --token <an admin's token>
graphersal-server status --dir ./srv --server 127.0.0.1:8484
```

## HTTP with curl

```bash
TOKEN=<a user token>
curl -s http://127.0.0.1:8484/readyz                                   # 200 when ready
curl -s http://127.0.0.1:8484/api/databases -H "Authorization: Bearer $TOKEN"   # my databases
curl -s -X POST http://127.0.0.1:8484/db/acme/api/execute -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' -d '{"script": "g.v().count().next()"}'
curl -s http://127.0.0.1:8484/api/admin/databases -H "Authorization: Bearer $TOKEN"   # admin: every database
curl -s -X POST http://127.0.0.1:8484/api/admin/databases/acme/load -H "Authorization: Bearer $TOKEN"
```

## Common tasks

| Task | Commands |
|---|---|
| Lost the admin token | `graphersal-server user token admin --dir ./srv` (as the directory's owner) |
| Publish a new version of a database without changing clients | `db clone acme acme-v2`, change `acme-v2`, then `db alias set prod acme-v2` |
| Make a database public | `user grant anonymous demo read` |
| Freeze a database as a base for layers | `db unload products`, `db promote products`, `db create acme --base products` |
| Throw away everything a layer changed, keep its settings | `db reset-layer scratch --yes` ([Reset a layer](guide/databases.md#reset-a-layer)) |
| Move a server to a new machine | `backup --with-databases --out FILE`, copy FILE, `restore FILE --dir NEWDIR`, `serve --dir NEWDIR` ([recipe](guide/backups.md#move-a-server-to-a-new-machine)) |
| A database is in `maintenance` | `db repair NAME`, `db verify NAME-repaired`, `db swap NAME NAME-repaired --yes` ([recipe](troubleshooting.md#a-database-is-in-maintenance)) |
| A database is `needs-model` | `models add <repo or file>` ([recipe](troubleshooting.md#a-database-needs-a-model)) |
| A refused directory | `graphersal-server init ./srv` as its owner ([recipe](troubleshooting.md#the-directory-is-refused-by-the-permission-check)) |
| Keep the audit lines | `RUST_LOG=warn,graphersal_server::audit=info graphersal-server serve --dir ./srv 2>> server.log` |
