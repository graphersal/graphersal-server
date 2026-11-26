# End-to-end smoke test of a built graphersal-server binary in PowerShell 7 (pwsh), for Windows
# (ci.yml and release.yml): the port of scripts/smoke.sh, step by step and in the same order, so
# the Windows release binary gets the same walk as the Unix ones. Keep the two in step: a step
# added to one goes into the other.
#
# Makes a server directory in a temporary directory, serves it on 127.0.0.1:0 (the URL read from
# server.run) and walks through the admin API, the database pages (with a promotion to a base and
# a layer over it), users and a read-only user, the run administration, the import inbox, a model
# upload (.tar, .tar.gz), the MCP endpoint, a clean stop, a restart that keeps the browser and
# MCP sessions, start/status/stop in the background, and a hard kill with the unclean-shutdown
# check. Prints one `ok   <step>` line per step, or `FAIL <step>: <why>` and stops at the first
# failure. Every server it started is stopped on any exit.
#
# Differences from smoke.sh, all on purpose:
# - No build step: -Bin names the binary to test.
# - Windows has no SIGTERM for a process outside the console's process group: a clean stop of
#   `serve` goes through `graphersal-server stop --dir` (the admin API's shutdown, the same path
#   as SIGTERM inside the server); `kill -9` is `Stop-Process -Force`.
# - The directory-owner check is unix only (the book's Security Model): the "private to its
#   owner" step keeps only its `GRAPHERSAL_SERVER_TOKEN` + `--server` half.
# - The h2c request is skipped (no HTTP/2 prior-knowledge client in PowerShell); the start
#   block's connection rows are still checked.
# - The console's `…` before a token's last 4 characters depends on the console's encoding: only
#   the last 4 are matched.
#
#   pwsh scripts/smoke.ps1 -Bin target/dist/graphersal-server.exe [-Version 0.3.0] [-Keep]
param(
    [Parameter(Mandatory = $true)][string]$Bin,
    [string]$Version = "",
    [switch]$Keep
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$Bin = (Resolve-Path $Bin).Path
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("graphersal-smoke-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $work | Out-Null
$dir = Join-Path $work "srv"
$script:log = Join-Path $work "server.log"
$script:server = $null       # the `serve` process started by Start-Server
$script:bgPid = $null        # the pid of a server started with `start`
$script:step = "start"
$script:url = ""
$script:AUTH = ""
$script:mcpSession = ""

function W([string]$name) { Join-Path $work $name }

function Fail([string]$message) {
    Write-Host "FAIL ${script:step}: $message"
    if ((Test-Path $script:log) -and (Get-Item $script:log).Length -gt 0) {
        Write-Host "--- server log (last 20 lines) ---"
        Get-Content $script:log -Tail 20 | ForEach-Object { Write-Host $_ }
    }
    throw "FAIL ${script:step}: $message"
}

function Ok { Write-Host "ok   $script:step" }

function Expect([bool]$condition, [string]$what) {
    if (-not $condition) { Fail $what }
}

function Text([string]$path) {
    if (Test-Path $path) { return [string](Get-Content -Raw $path) } else { return "" }
}

function Head400([string]$text) {
    if ($null -eq $text) { return "" }
    if ($text.Length -gt 400) { return $text.Substring(0, 400) } else { return $text }
}

# The value at the dotted PATH (keys, list indexes) of a parsed JSON value, as smoke.sh's jget:
# strings bare, everything else as JSON (`null` when missing).
function Get-J($value, [string]$path) {
    foreach ($key in ($path -split '\.' | Where-Object { $_ -ne "" })) {
        if ($null -eq $value) { break }
        if ($value -is [System.Array]) {
            $i = [int]$key
            if ($i -lt $value.Count) { $value = $value[$i] } else { $value = $null }
        } elseif ($value -is [System.Management.Automation.PSCustomObject]) {
            $property = $value.PSObject.Properties[$key]
            if ($property) { $value = $property.Value } else { $value = $null }
        } else {
            $value = $null
        }
    }
    if ($null -eq $value) { return "null" }
    if ($value -is [string]) { return $value }
    if ($value -is [bool]) { if ($value) { return "true" } else { return "false" } }
    if ($value -is [System.ValueType]) { return [string]$value }
    return (ConvertTo-Json -InputObject $value -Compress -Depth 20)
}

# One request, as smoke.sh's req: the status in $script:Status, the body in $script:Body (text)
# and $script:Json (parsed, or $null), the headers in $script:Head. -Auth "" sends it
# anonymously; the default is the first admin's token ($script:AUTH). -Json sends a JSON body,
# -InFile a file as application/octet-stream, -OutFile writes the body to a file.
function Req([string]$method, [string]$path, [string]$Json = $null, [string]$InFile = $null,
    [string]$OutFile = $null, [hashtable]$Headers = @{}, $Auth = $script:AUTH, $Session = $null) {
    $h = @{}
    foreach ($k in $Headers.Keys) { $h[$k] = $Headers[$k] }
    if ($Auth) { $h["Authorization"] = "Bearer $Auth" }
    $params = @{
        Method = $method; Uri = "$($script:url)$path"; SkipHttpErrorCheck = $true
        TimeoutSec = 120; MaximumRedirection = 0; Headers = $h
    }
    if ($Session) { $params.WebSession = $Session }
    if ($Json) { $params.ContentType = "application/json"; $params.Body = $Json }
    if ($InFile) { $params.ContentType = "application/octet-stream"; $params.InFile = $InFile }
    if ($OutFile) { $params.OutFile = $OutFile; $params.PassThru = $true }
    try {
        $response = Invoke-WebRequest @params
    } catch {
        Fail "request $method $path failed: $($_.Exception.Message)"
    }
    $script:Status = [int]$response.StatusCode
    $script:Head = $response.Headers
    if ($OutFile) { $script:Body = "" } else { $script:Body = [string]$response.Content }
    $script:Json = $null
    if ($script:Body) {
        try { $script:Json = $script:Body | ConvertFrom-Json } catch { $script:Json = $null }
    }
}

function JsonOf($object) { ConvertTo-Json -InputObject $object -Compress -Depth 20 }

function J([string]$path) { Get-J $script:Json $path }

function Expect-Status([int]$expected) {
    Expect ($script:Status -eq $expected) "HTTP $script:Status, expected ${expected}: $(Head400 $script:Body)"
}

function Expect-Json([string]$path, [string]$expected) {
    $got = J $path
    Expect ($got -ceq $expected) "$path = $got, expected ${expected}: $(Head400 $script:Body)"
}

function Expect-BodyContains([string]$text) {
    Expect ($script:Body.Contains($text)) "the answer lacks '$text': $(Head400 $script:Body)"
}

# The value of the header NAME of the last answer ("" when missing).
function Header([string]$name) {
    foreach ($key in $script:Head.Keys) {
        if ($key -ieq $name) { return (@($script:Head[$key]) | Select-Object -Last 1) }
    }
    return ""
}

# A web session that sends the gs_session cookie $value to the server, as a browser does
# (PowerShell's web cmdlets manage the Cookie header themselves).
function New-CookieSession([string]$value) {
    $session = New-Object Microsoft.PowerShell.Commands.WebRequestSession
    $session.Cookies.Add((New-Object System.Net.Cookie("gs_session", $value, "/", ([Uri]$script:url).Host)))
    return $session
}

# Runs the binary with ARGS, stdout into $out and stderr into $err (paths in the work
# directory); returns the exit code.
function Run-Bin([string[]]$arguments, [string]$out, [string]$err) {
    $outFile = W $out
    $errFile = W $err
    & $Bin @arguments > $outFile 2> $errFile
    return $LASTEXITCODE
}

# load NAME: loads the database NAME and waits until it is loaded.
function Load([string]$name) {
    Req POST "/api/admin/databases/$name/load"
    Expect ($script:Status -eq 200 -or $script:Status -eq 202) "load ${name}: HTTP $($script:Status): $(Head400 $script:Body)"
    for ($i = 0; $i -lt 300; $i++) {
        Req GET "/api/admin/databases/$name"
        switch (J state) {
            "loaded" { return }
            "loading" { Start-Sleep -Milliseconds 200 }
            default { Fail "load ${name}: state $(J state) ($(J reason))" }
        }
    }
    Fail "load ${name}: still loading after 60 s"
}

# execute DB SCRIPT: runs SCRIPT on /db/DB/api/execute (status 200, ok true); the first result
# in $script:Result.
function Execute([string]$db, [string]$text, $Auth = $script:AUTH) {
    Req POST "/db/$db/api/execute" -Json (JsonOf @{ script = $text }) -Auth $Auth
    Expect-Status 200
    Expect-Json ok true
    $script:Result = J execution.results.0
}

# mcp ID METHOD [PARAMS]: one JSON-RPC request to /mcp in the session $script:mcpSession.
function Mcp([string]$id, [string]$method, [string]$params = $null) {
    $message = [ordered]@{ jsonrpc = "2.0"; method = $method }
    if ($id) { $message.id = [int]$id }
    if ($params) { $message.params = $params | ConvertFrom-Json }
    $headers = @{ Accept = "application/json, text/event-stream"; "MCP-Protocol-Version" = "2025-06-18" }
    if ($script:mcpSession) { $headers["Mcp-Session-Id"] = $script:mcpSession }
    Req POST /mcp -Json (JsonOf $message) -Headers $headers
}

# tool NAME ARGS: tools/call; the result's text in $script:ToolText, its isError in
# $script:ToolError.
function Tool([string]$name, [string]$arguments) {
    Mcp 3 tools/call "{`"name`": `"$name`", `"arguments`": $arguments}"
    Expect-Status 200
    $script:ToolText = J result.content.0.text
    $script:ToolError = J result.isError
}

# Starts `serve` on 127.0.0.1:0 and waits for server.run and /readyz; sets $script:url (no
# trailing slash). Stdout goes to $out, stderr to $script:log.
function Start-Server([string]$out) {
    $script:server = Start-Process -FilePath $Bin -PassThru -NoNewWindow `
        -ArgumentList @("serve", "--dir", "`"$dir`"", "--listen", "127.0.0.1:0") `
        -RedirectStandardOutput (W $out) -RedirectStandardError $script:log
    # Keeps the process handle, so ExitCode is known after the exit.
    $null = $script:server.Handle
    $run = Join-Path $dir "server.run"
    for ($i = 0; $i -lt 150; $i++) {
        if ((Test-Path $run) -and (Get-Item $run).Length -gt 0) { break }
        if ($script:server.HasExited) { Fail "the server exited: $(Text $script:log)" }
        Start-Sleep -Milliseconds 200
    }
    $line = Get-Content $run -ErrorAction SilentlyContinue | Where-Object { $_ -match '^url = "(.*)"$' } | Select-Object -First 1
    Expect ($null -ne $line) "no url in server.run"
    $null = $line -match '^url = "(.*)"$'
    $script:url = $Matches[1].TrimEnd('/')
    $status = 0
    for ($i = 0; $i -lt 150; $i++) {
        try {
            $status = [int](Invoke-WebRequest -Uri "$($script:url)/readyz" -SkipHttpErrorCheck -TimeoutSec 10).StatusCode
        } catch { $status = 0 }
        if ($status -eq 200) { break }
        Start-Sleep -Milliseconds 200
    }
    Expect ($status -eq 200) "/readyz answers $status"
}

# Stops the `serve` process cleanly through `graphersal-server stop` and checks exit code 0.
function Stop-Server {
    $code = Run-Bin @("stop", "--dir", $dir) "stop-serve.out" "stop-serve.err"
    Expect ($code -eq 0) "stop: $(Text (W 'stop-serve.out')) $(Text (W 'stop-serve.err'))"
    Expect ($script:server.WaitForExit(60000)) "the server still runs 60 s after the stop"
    $exit = $script:server.ExitCode
    $script:server = $null
    Expect ($exit -eq 0) "exit code $exit"
}

function Background-Pid {
    $line = Get-Content (Join-Path $dir "server.run") | Where-Object { $_ -match '^pid = ([0-9]+)$' } | Select-Object -First 1
    Expect ($null -ne $line) "start wrote no pid into server.run"
    $null = $line -match '^pid = ([0-9]+)$'
    return [int]$Matches[1]
}

function Test-Running([int]$id) {
    return $null -ne (Get-Process -Id $id -ErrorAction SilentlyContinue)
}

try {
    # --- binary, init, serve ------------------------------------------------------------------

    $script:step = "binary"
    Expect (Test-Path $Bin) "no binary at $Bin"
    if ($Version) {
        $versionText = (& $Bin --version) -join "`n"
        Expect ($LASTEXITCODE -eq 0) "--version exited with $LASTEXITCODE"
        Expect ($versionText -ceq "graphersal-server $Version") "--version prints '$versionText', expected 'graphersal-server $Version'"
    }
    Ok

    $script:step = "init"
    $code = Run-Bin @("init", $dir) "init.out" "init.err"
    Expect ($code -eq 0) (Text (W "init.err"))
    foreach ($part in @("server.toml", "databases", "models\blobs", "backups", "trash", "tmp", "imports")) {
        Expect (Test-Path (Join-Path $dir $part)) "init made no $part"
    }
    Expect (@(Get-Content (W "init.out") | Where-Object { $_ -match '^  graphersal-server serve --dir ' }).Count -gt 0) `
        "init does not show the serve command on its own line: $(Text (W 'init.out'))"
    # The first admin's token, printed once in a framed block on stderr: every request below goes
    # as that admin unless it says otherwise.
    $tokenLine = Get-Content (W "init.err") | Where-Object { $_ -match '^\| token: *([0-9a-f]{64}) ' } | Select-Object -First 1
    Expect ($null -ne $tokenLine) "init printed no first admin token: $(Text (W 'init.err'))"
    $null = $tokenLine -match '^\| token: *([0-9a-f]{64}) '
    $script:AUTH = $Matches[1]
    # A new server has MCP off and hides new names from it (round 4 B-MCPDEF): the MCP steps below
    # need it on, with the databases created later offered.
    $code = Run-Bin @("mcp", "on", "--dir", $dir) "mcp.out" "mcp.err"
    Expect ($code -eq 0) "mcp on: $(Text (W 'mcp.err'))"
    $code = Run-Bin @("mcp", "expose-new", "on", "--dir", $dir) "mcp.out" "mcp.err"
    Expect ($code -eq 0) "mcp expose-new on: $(Text (W 'mcp.err'))"
    Ok

    $script:step = "serve"
    Start-Server "server.out"
    # stdout: the address line only; stderr: the start block, no colour codes (not a terminal).
    for ($i = 0; $i -lt 50; $i++) {
        if ((Text (W "server.out")).Trim()) { break }
        Start-Sleep -Milliseconds 100
    }
    $outText = (Text (W "server.out")).TrimEnd("`r", "`n")
    Expect ($outText -ceq "Graphersal server: $($script:url)/") "serve's stdout is not the address line: $outText"
    $logLines = Get-Content $script:log
    Expect (@($logLines | Where-Object { $_ -match '^── Databases \(0\) ─' }).Count -gt 0) "no start block on stderr: $(Head400 (Text $script:log))"
    # No log line repeats the start block's address before it.
    Expect (-not (Text $script:log).Contains(" on $($script:url)/")) "a log line repeats the start block: $(Head400 (Text $script:log))"
    Expect (-not (Text $script:log).Contains([string][char]27)) "ANSI codes in the log"
    Req GET /api/info
    Expect-Status 200
    Expect-Json server graphersal-server
    Expect-Json protocol graphersal/1
    Expect-Json dev false
    Expect-Json uiProfile.name server
    Expect-Json uiProfile.styles.0 server-ui.css
    Expect-Json admin null
    Expect-Json product null
    if ($Version) { Expect-Json version $Version }
    Ok

    $script:step = "access: anonymous is refused, the first admin logs in and out"
    Req GET /api/info -Auth ""
    Expect-Status 200
    Expect-Json principal.kind anonymous
    Expect-Json login /
    Expect-Json uiProfile.name server
    Expect-Json uploadBytes null
    Req GET /api/admin/databases -Auth ""
    Expect-Status 401
    Expect-Json login /
    Req GET /db/acme/api/info -Auth ""
    Expect-Status 401
    Req GET /db/acme/ -Auth ""
    Expect-Status 303
    Expect ((Header Location) -ceq "/?next=/db/acme/") "no login redirect: $(Header Location)"
    Req GET /api/info
    Expect-Json principal.kind user
    Expect-Json principal.admin true
    Req POST /api/login -Json '{"token": "0000"}' -Auth ""
    Expect-Status 401
    $adminToken = $script:AUTH
    Req POST /api/login -Json "{`"token`": `"$adminToken`"}" -Auth ""
    Expect-Status 200
    $setCookie = Header Set-Cookie
    Expect ($setCookie -match '^gs_session=([^;]+);.*; HttpOnly; SameSite=Strict; Path=/$') "login cookie: $setCookie"
    $cookie = $Matches[1]
    Req GET /api/admin/databases -Auth "" -Session (New-CookieSession $cookie)
    Expect-Status 200
    Req POST /api/logout -Auth "" -Session (New-CookieSession $cookie)
    Expect-Status 200
    Expect-Json loggedOut true
    Req GET /api/admin/databases -Auth "" -Session (New-CookieSession $cookie)
    Expect-Status 401
    Ok

    $script:step = "connection limits in the start block (h2c skipped on Windows)"
    $logLines = Get-Content $script:log
    Expect (@($logLines | Where-Object { $_ -match 'connections: *up to 256 open \(HTTP/1\.1, h2c\)$' }).Count -gt 0) `
        "no connections row in the start block: $(Head400 (Text $script:log))"
    Expect (@($logLines | Where-Object { $_ -match 'timeouts: *request head 30 s, idle connection 60 s$' }).Count -gt 0) `
        "no timeouts row in the start block: $(Head400 (Text $script:log))"
    Ok

    $script:step = "the server UI at /"
    Req GET /
    Expect-Status 200
    Expect-BodyContains '<html'
    Expect (-not $script:Body.Contains('server-ui/')) "/ is not the playground's index.html unchanged"
    Req GET /server-ui/server-ui.css
    Expect-Status 200
    Expect ((Header Content-Type) -ceq "text/css; charset=utf-8") "server-ui.css: $(Header Content-Type)"
    Req GET /server-ui/profile.json
    Expect-Status 200
    Expect-Json name server
    Req GET /server-ui/databases-tab.js
    Expect-Status 200
    Expect ((Header Content-Type) -ceq "text/javascript; charset=utf-8") "databases-tab.js: $(Header Content-Type)"
    Ok

    # --- databases ----------------------------------------------------------------------------

    $script:step = "server settings: a live key applies at once, a restart key is pending"
    Req GET /api/admin/config
    Expect-Status 200
    $rev = J revision
    Req PUT /api/admin/config -Json "{`"set`": {`"limits.upload_bytes`": 3221225472, `"limits.max_connections`": 300}, `"revision`": `"$rev`"}"
    Expect-Status 200
    Expect-Json applied '["limits.upload_bytes"]'
    Expect-Json restartPending '["limits.max_connections"]'
    Req GET /api/info
    Expect-Json uploadBytes 3221225472
    $code = Run-Bin @("config", "get", "limits.max_connections", "--dir", $dir) "config.out" "config.err"
    Expect ($code -eq 0) "config get: $(Text (W 'config.out')) $(Text (W 'config.err'))"
    Expect ((Text (W "config.out")) + (Text (W "config.err"))).Contains("restart pending") "config get: $(Text (W 'config.out'))"
    $code = Run-Bin @("config", "unset", "limits.max_connections", "--dir", $dir) "config.out" "config.err"
    Expect ($code -eq 0) "config unset: $(Text (W 'config.out')) $(Text (W 'config.err'))"
    Req GET /api/admin/config
    Expect-Json restartPending '[]'
    Ok

    $script:step = "create plain, base and layer"
    Req POST /api/admin/databases -Json '{"name": "acme", "description": "smoke"}'
    Expect-Status 201
    Expect-Json kind plain
    Req POST /api/admin/databases -Json '{"name": "product", "role": "base"}'
    Expect-Status 201
    Expect-Json role base
    Expect-Json readOnly true
    Req POST /api/admin/databases -Json '{"name": "acme-l", "base": "product"}'
    Expect-Status 201
    Expect-Json kind layer
    Expect-Json base product
    Req GET /api/admin/databases
    Expect-Status 200
    Expect-Json databases.0.name acme
    Ok

    $script:step = "load"
    Load acme
    Load acme-l
    Req GET /api/admin/databases/product
    Expect-Json pooled true
    Ok

    $script:step = "database page info links home"
    Req GET /db/acme/api/info
    Expect-Status 200
    Expect-Json database acme
    Expect-Json home /
    Expect-Json admin null
    Expect-Json server graphersal-server
    Expect-Json protocol graphersal/1
    Expect-Json dev false
    Ok

    $script:step = "write and read through /db/<name>/api/execute"
    Execute acme 'g.add_v("person").property("name", "ada").iterate()'
    Execute acme 'g.v().has_label("person").count().next()'
    Expect ($script:Result -eq "1") "acme holds $($script:Result) persons, expected 1"
    Execute acme-l 'g.add_v("person").property("name", "bob").add_v("person").property("name", "cy").iterate()'
    Execute acme-l 'g.v().has_label("person").count().next()'
    Expect ($script:Result -eq "2") "acme-l holds $($script:Result) persons, expected 2"
    Ok

    $script:step = "console through the running server"
    $code = Run-Bin @("db", "list", "--dir", $dir, "--json") "list.json" "list.err"
    Expect ($code -eq 0) "db list: $(Text (W 'list.err'))"
    Expect ((Get-J ((Text (W "list.json")) | ConvertFrom-Json) databases.0.name) -eq "acme") "db list: $(Head400 (Text (W 'list.json')))"
    Expect ((Text (W "list.err")).Contains("admin API")) "db list did not note the server: $(Text (W 'list.err'))"
    # A load through the server shows where the database is served.
    $code = Run-Bin @("db", "load", "acme", "--dir", $dir) "load.out" "load.err"
    Expect ($code -eq 0) "db load: $(Text (W 'load.err'))"
    Expect ((Get-Content (W "load.out")) -ccontains "  served at: $($script:url)/db/acme/") `
        "db load does not show the database's URL: $(Text (W 'load.out'))"
    # A refusal's help names the console command, not the admin route.
    $code = Run-Bin @("db", "delete", "acme", "--dir", $dir) "delete.out" "delete.err"
    Expect ($code -ne 0) "db delete of a loaded database succeeded"
    Expect (@(Get-Content (W "delete.err") | Where-Object { $_.StartsWith("Help: unload it first: graphersal-server db unload acme --dir ") }).Count -gt 0) `
        "the help is not the console command: $(Text (W 'delete.err'))"
    Expect (-not (Text (W "delete.err")).Contains("/api/admin")) "the console help names a route"
    Ok

    # The owner check of the server directory is unix only: just the token half here.
    $script:step = "--token goes through the server (the owner check is unix only)"
    $env:GRAPHERSAL_SERVER_TOKEN = $adminToken
    try {
        $code = Run-Bin @("db", "list", "--dir", $dir, "--server", $script:url.Substring("http://".Length), "--json") "token.json" "token.err"
    } finally {
        Remove-Item Env:GRAPHERSAL_SERVER_TOKEN
    }
    Expect ($code -eq 0) "db list --token: $(Text (W 'token.err'))"
    Expect ((Get-J ((Text (W "token.json")) | ConvertFrom-Json) databases.0.name) -eq "acme") "db list --token: $(Head400 (Text (W 'token.json')))"
    Ok

    $script:step = "users: the console adds a user, the API grants, a new token ends the old"
    $code = Run-Bin @("user", "add", "smokey", "--dir", $dir) "user.out" "user.err"
    Expect ($code -eq 0) "user add: $(Text (W 'user.err'))"
    $line = Get-Content (W "user.out") | Where-Object { $_ -match '^  token: *([0-9a-f]{64})$' } | Select-Object -First 1
    Expect ($null -ne $line) "user add printed no token: $(Text (W 'user.out'))"
    $null = $line -match '^  token: *([0-9a-f]{64})$'
    $userToken = $Matches[1]
    Req GET /db/acme/api/info -Auth $userToken
    Expect-Status 404
    Req POST /api/admin/grants -Json '{"subject": "smokey", "database": "acme", "level": "read"}'
    Expect-Status 200
    Expect-Json subject user:smokey
    Req GET /db/acme/api/info -Auth $userToken
    Expect-Status 200
    Req GET /api/admin/users -Auth $userToken
    Expect-Status 404
    Req GET /api/admin/users/smokey
    Expect-Status 200
    Expect-Json tokenLast4 $userToken.Substring(60, 4)
    $code = Run-Bin @("user", "list", "--dir", $dir) "users.out" "users.err"
    Expect ($code -eq 0) "user list: $(Text (W 'users.err'))"
    Expect ((Text (W "users.out")).Contains($userToken.Substring(60, 4))) "user list: $(Text (W 'users.out'))"
    Req POST /api/admin/users/smokey/token
    Expect-Status 200
    Req GET /api/info -Auth $userToken
    Expect-Status 401
    $code = Run-Bin @("user", "remove", "admin", "--dir", $dir) "rm.out" "rm.err"
    Expect ($code -ne 0) "the last admin was removed"
    Expect ((Text (W "rm.err")).Contains("is the last admin")) "last admin: $(Text (W 'rm.err'))"
    $code = Run-Bin @("user", "remove", "smokey", "--dir", $dir) "rm.out" "rm.err"
    Expect ($code -eq 0) "user remove: $(Text (W 'rm.err'))"
    Ok

    $script:step = "a read-only user: reads its database, cannot write, does not see another"
    $code = Run-Bin @("user", "add", "reader", "--dir", $dir) "reader.out" "reader.err"
    Expect ($code -eq 0) "user add: $(Text (W 'reader.err'))"
    $line = Get-Content (W "reader.out") | Where-Object { $_ -match '^  token: *([0-9a-f]{64})$' } | Select-Object -First 1
    Expect ($null -ne $line) "user add printed no token: $(Text (W 'reader.out'))"
    $null = $line -match '^  token: *([0-9a-f]{64})$'
    $readerToken = $Matches[1]
    $code = Run-Bin @("user", "grant", "reader", "acme", "read", "--dir", $dir) "grant.out" "grant.err"
    Expect ($code -eq 0) "user grant: $(Text (W 'grant.err'))"
    Execute acme 'g.v().has_label("person").count().next()' -Auth $readerToken
    Expect ($script:Result -eq "1") "the reader counts $($script:Result) persons on acme, expected 1"
    Req POST /db/acme/api/execute -Json '{"script": "g.add_v(\"person\").iterate()"}' -Auth $readerToken
    Expect (-not ($script:Status -eq 200 -and (J ok) -eq "true")) "a read-only user's write succeeded: $(Head400 $script:Body)"
    Execute acme 'g.v().has_label("person").count().next()'
    Expect ($script:Result -eq "1") "acme counts $($script:Result) after the reader's refused write"
    Req GET /db/acme-l/api/info -Auth $readerToken
    Expect-Status 404
    Req GET /api/databases -Auth $readerToken
    Expect-Status 200
    Expect-Json databases.0.name acme
    Expect-Json databases.0.level read
    Expect-Json databases.1 null
    Req GET /api/admin/databases -Auth $readerToken
    Expect-Status 404
    Ok

    $script:step = "the reserved name jobs is refused"
    Req POST /api/admin/databases -Json '{"name": "jobs"}'
    Expect-Status 400
    Expect-BodyContains "reserved"
    Ok

    $script:step = "alias flip"
    Req PUT /api/admin/aliases/current -Json '{"target": "acme"}'
    Expect-Status 200
    Execute current 'g.v().has_label("person").count().next()'
    Expect ($script:Result -eq "1") "current -> acme counts $($script:Result)"
    Req PUT /api/admin/aliases/current -Json '{"target": "acme-l"}'
    Expect-Status 200
    Execute current 'g.v().has_label("person").count().next()'
    Expect ($script:Result -eq "2") "current -> acme-l counts $($script:Result)"
    Ok

    $script:step = "a stale revision is refused (409), the current one passes"
    Req GET /api/admin/aliases
    Expect-Status 200
    $rev = J revision
    Expect ((Header ETag) -ceq "`"$rev`"") "ETag $(Header ETag), revision $rev"
    Req PUT /api/admin/aliases/current -Json "{`"target`": `"acme`", `"revision`": `"$rev`"}"
    Expect-Status 200
    Req PUT /api/admin/aliases/current -Json "{`"target`": `"acme-l`", `"revision`": `"$rev`"}"
    Expect-Status 409
    Expect-Json help "changed by someone else: reload"
    $new = J revision
    Req GET /api/admin/aliases
    Expect-Json aliases.current acme
    Req DELETE /api/admin/aliases/current -Headers @{ "If-Match" = "`"$rev`"" }
    Expect-Status 409
    Req PUT /api/admin/aliases/current -Json '{"target": "acme-l"}' -Headers @{ "If-Match" = "`"$new`"" }
    Expect-Status 200
    Ok

    $script:step = "read-only refuses a write"
    Req PUT /api/admin/databases/acme/read-only -Json '{"readOnly": true}'
    Expect-Status 200
    Expect-Json readOnly true
    Req POST /db/acme/api/execute -Json '{"script": "g.add_v(\"person\").iterate()"}'
    Expect (-not ($script:Status -eq 200 -and (J ok) -eq "true")) "a write on a read-only database succeeded"
    Expect-BodyContains "read-only"
    Req PUT /api/admin/databases/acme/read-only -Json '{"readOnly": false}'
    Expect-Status 200
    Expect-Json readOnly false
    Execute acme 'g.v().has_label("person").count().next()'
    Expect ($script:Result -eq "1") "acme counts $($script:Result) after the refused write"
    Ok

    $script:step = "database settings: view, a change based on the revision, 409, a limit at the next load (B7)"
    Req GET /api/admin/databases/acme/settings
    Expect-Status 200
    Expect-Json name acme
    Expect-Json database.state loaded
    Expect-Json store.damagePolicy maintenance
    Expect-Json limits.loaded true
    $rev = J revision
    Expect ((Header ETag) -ceq "`"$rev`"") "ETag $(Header ETag), revision $rev"
    Req PUT /api/admin/databases/acme/settings -Json "{`"description`": `"smoke settings`", `"revision`": `"$rev`"}"
    Expect-Status 200
    Expect-Json database.description "smoke settings"
    Req PUT /api/admin/databases/acme/settings -Json "{`"autoLoad`": true, `"revision`": `"$rev`"}"
    Expect-Status 409
    Expect-Json help "changed by someone else: reload"
    Req PUT /api/admin/databases/acme/settings -Json '{"limits": {"max_operations": -1}}'
    Expect-Status 400
    Req PUT /api/admin/databases/acme/settings -Json '{"limits": {"max_operations": 1000}}'
    Expect-Status 200
    Expect-Json limits.reloadNeeded true
    $heavy = 'let n = 0; while n < 20000 { n += 1; } n'
    Execute acme $heavy
    Expect ($script:Result -eq "20000") "not limited before the next load: $($script:Result)"
    Req POST /api/admin/databases/acme/unload
    Expect-Status 200
    Load acme
    Req POST /db/acme/api/execute -Json (JsonOf @{ script = $heavy })
    Expect ((J ok) -eq "false") "the limit did not apply after the load: $(Head400 $script:Body)"
    Expect-BodyContains "operations"
    Req PUT /api/admin/databases/acme/settings -Json '{"limits": {"max_operations": null}}'
    Expect-Status 200
    Req POST /api/admin/databases/acme/unload
    Expect-Status 200
    Load acme
    Execute acme $heavy
    Ok

    $script:step = "run administration (list and stop another client's query)"
    Req GET /api/admin/runs
    Expect-Status 200
    Expect-Json textHeadChars 200
    Req GET /db/acme/api/admin/runs
    Expect-Status 404
    # A page's endless query, in the background; the admin sees it and stops it.
    $endless = Start-ThreadJob -ArgumentList $script:url, $script:AUTH, (W "endless.json") -ScriptBlock {
        param($url, $token, $out)
        $r = Invoke-WebRequest -Method POST -Uri "$url/db/acme/api/execute" -SkipHttpErrorCheck -TimeoutSec 120 `
            -ContentType "application/json" -Headers @{ Authorization = "Bearer $token"; "X-Graphersal-Client" = "smoke" } `
            -Body '{"script": "let n = 0; loop { n += 1; }", "runId": "endless"}'
        Set-Content -Path $out -Value $r.Content -NoNewline
    }
    $runId = ""
    for ($i = 0; $i -lt 300; $i++) {
        Req GET /api/admin/databases/acme/runs
        Expect-Status 200
        $run = @($script:Json.runs | Where-Object { $_.PSObject.Properties["alias"] -and $_.alias -eq "endless" -and $_.state -eq "running" }) | Select-Object -First 1
        if ($run) { $runId = [string]$run.id; break }
        Start-Sleep -Milliseconds 100
    }
    Expect ($runId -ne "") "the endless query never showed: $(Head400 $script:Body)"
    Req GET /api/admin/runs
    Expect-BodyContains '"client":"page:smoke"'
    Req POST /api/admin/databases/acme/runs/cancel -Json "{`"id`": $runId}"
    Expect-Status 200
    Expect-Json cancelled true
    $null = Wait-Job $endless -Timeout 120
    Expect ($endless.State -eq "Completed") "the endless query's request failed: $($endless.State) $(Receive-Job $endless 2>&1)"
    Remove-Job $endless
    Expect ((Text (W "endless.json")).Contains('"ok":false')) "the stopped query answered: $(Head400 (Text (W 'endless.json')))"
    Req POST /api/admin/databases/acme/runs/cancel -Json "{`"id`": $runId}"
    Expect-Json cancelled false
    Ok

    $script:step = "backup and restore into a new name"
    Req POST /api/admin/databases/acme/backup
    Expect-Status 201
    $backup = J backup
    Expect (Test-Path (Join-Path $dir (J file))) "no backup file $(J file)"
    Req GET /api/admin/databases/acme/backups
    Expect-Status 200
    Expect-Json backups.0.backup $backup
    Req POST /api/admin/databases/acme/restore -Json "{`"backup`": `"$backup`", `"name`": `"acme-restored`"}"
    Expect-Status 201
    Expect-Json name acme-restored
    Expect-Json replaced false
    Load acme-restored
    Execute acme-restored 'g.v().has_label("person").count().next()'
    Expect ($script:Result -eq "1") "acme-restored counts $($script:Result)"
    Ok

    $script:step = "server backup (API download) restored into a new directory"
    Req GET "/api/admin/server-backup?withDatabases=1" -OutFile (W "server-backup.zip")
    Expect-Status 200
    Expect ((Header X-Graphersal-Server-Backup) -ceq "with-databases") "X-Graphersal-Server-Backup: $(Header X-Graphersal-Server-Backup)"
    Expect ((Get-Item (W "server-backup.zip")).Length -gt 0) "empty server backup"
    $restored = W "restored"
    $code = Run-Bin @("restore", (W "server-backup.zip"), "--dir", $restored) "restore.out" "restore.err"
    Expect ($code -eq 0) "restore: $(Text (W 'restore.out')) $(Text (W 'restore.err'))"
    Expect ((Get-FileHash (Join-Path $dir "access.toml")).Hash -eq (Get-FileHash (Join-Path $restored "access.toml")).Hash) "access.toml differs after the restore"
    Expect (Test-Path (Join-Path (Join-Path (Join-Path $restored "databases") "acme") "store")) "acme was not restored"
    Expect (((Text (W "restore.out")) + (Text (W "restore.err"))).Contains("serve --dir")) "no next command: $(Text (W 'restore.out'))"
    $code = Run-Bin @("restore", (W "server-backup.zip"), "--dir", $restored) "restore2.out" "restore2.err"
    Expect ($code -ne 0) "a restore into a non-empty directory was accepted"
    Ok

    $script:step = "export and re-import by upload"
    Req GET /api/admin/databases/acme/export -OutFile (W "acme.gsnap")
    Expect-Status 200
    Expect ((Header X-Graphersal-Snapshot) -ceq "full") "X-Graphersal-Snapshot: $(Header X-Graphersal-Snapshot)"
    Expect ((Get-Item (W "acme.gsnap")).Length -gt 0) "empty export"
    Req POST "/api/admin/import?name=acme-imported&description=re-imported" -InFile (W "acme.gsnap")
    Expect-Status 201
    Expect-Json import.format snapshot
    Expect-Json state unloaded
    Load acme-imported
    Execute acme-imported 'g.v().has_label("person").count().next()'
    Expect ($script:Result -eq "1") "acme-imported counts $($script:Result)"
    Ok

    $script:step = "import from the inbox"
    Copy-Item (W "acme.gsnap") (Join-Path (Join-Path $dir "imports") "acme-inbox.gsnap")
    Req GET /api/admin/imports
    Expect-Status 200
    Expect-Json imports.0.name acme-inbox.gsnap
    Expect-Json imports.0.kind snapshot
    Req POST /api/admin/import -Json '{"inbox": "../server.toml", "name": "evil"}'
    Expect-Status 400
    Req POST /api/admin/import -Json '{"inbox": "acme-inbox.gsnap", "name": "acme-inbox"}'
    Expect-Status 201
    Expect-Json import.format snapshot
    Load acme-inbox
    Execute acme-inbox 'g.v().has_label("person").count().next()'
    Expect ($script:Result -eq "1") "acme-inbox counts $($script:Result)"
    Ok

    $script:step = "promote + layer sees base data"
    Req POST /api/admin/databases/acme-restored/promote
    Expect-Status 409
    Expect-BodyContains "unload"
    Req POST /api/admin/databases/acme-restored/unload
    Expect-Status 200
    Req POST /api/admin/databases/acme-restored/promote
    Expect-Status 200
    Expect-Json role base
    Expect-Json readOnly true
    Req POST /api/admin/databases -Json '{"name": "restored-l", "base": "acme-restored"}'
    Expect-Status 201
    Load restored-l
    Execute restored-l 'g.v().has_label("person").count().next()'
    Expect ($script:Result -eq "1") "restored-l sees $($script:Result) persons of its base, expected 1"
    Execute restored-l 'g.add_v("person").property("name", "dan").iterate()'
    Execute restored-l 'g.v().has_label("person").count().next()'
    Expect ($script:Result -eq "2") "restored-l counts $($script:Result) after its own write, expected 2"
    Req POST "/api/admin/import?name=acme-base&role=base" -InFile (W "acme.gsnap")
    Expect-Status 201
    Expect-Json role base
    Expect-Json readOnly true
    Ok

    $script:step = "models list"
    Req GET /api/admin/models
    Expect-Status 200
    Expect ((J models) -eq "[]") "models: $(J models)"
    Expect ((J missing) -eq "[]") "missing: $(J missing)"
    Ok

    $script:step = "model upload (.tar and .tar.gz of a model directory)"
    # A tiny synthetic Model2Vec model (4 tokens x 3 dimensions), as the unit tests' test_models.rs.
    $model = W "tiny-model"
    New-Item -ItemType Directory -Path $model | Out-Null
    $vocab = @("[UNK]", "graph", "vector", "model")
    [System.IO.File]::WriteAllText((Join-Path $model "config.json"), '{"normalize": true}')
    $ids = [ordered]@{}
    for ($i = 0; $i -lt $vocab.Count; $i++) { $ids[$vocab[$i]] = $i }
    $tok = [ordered]@{
        version = "1.0"; truncation = $null; padding = $null; added_tokens = @()
        normalizer = @{ type = "Lowercase" }; pre_tokenizer = @{ type = "Whitespace" }
        post_processor = $null; decoder = $null
        model = [ordered]@{ type = "WordLevel"; vocab = $ids; unk_token = "[UNK]" }
    }
    [System.IO.File]::WriteAllText((Join-Path $model "tokenizer.json"), (JsonOf $tok))
    $data = New-Object System.IO.MemoryStream
    $dataWriter = New-Object System.IO.BinaryWriter($data)
    for ($i = 0; $i -lt $vocab.Count * 3; $i++) { $dataWriter.Write([single](1.0 + $i)) }
    $dataWriter.Flush()
    $dataBytes = $data.ToArray()
    $header = "{`"embeddings`": {`"dtype`": `"F32`", `"shape`": [$($vocab.Count), 3], `"data_offsets`": [0, $($dataBytes.Length)]}}"
    $header += " " * ((8 - ($header.Length % 8)) % 8)
    $file = [System.IO.File]::Create((Join-Path $model "model.safetensors"))
    $writer = New-Object System.IO.BinaryWriter($file)
    $writer.Write([uint64]$header.Length)
    $writer.Write([System.Text.Encoding]::ASCII.GetBytes($header))
    $writer.Write($dataBytes)
    $writer.Close()
    [System.IO.File]::WriteAllText((Join-Path $model "README.md"), "---`nlicense: mit`n---`n# A tiny model`n")
    # Windows' own tar.exe (bsdtar, Windows 10 1803 and later).
    & tar -cf (W "tiny-model.tar") --format ustar -C $work tiny-model
    Expect ($LASTEXITCODE -eq 0) "cannot write the model archive"
    Req POST "/api/admin/models?repo=smoke%2Ftiny&license=mit&format=tar" -InFile (W "tiny-model.tar")
    Expect-Status 201
    Expect-Json added.0.repo smoke/tiny
    Req GET /api/admin/models
    Expect-Json models.0.repo smoke/tiny
    # The same model as a gzip-compressed tar (format detected): the same blob, added again.
    & tar -czf (W "tiny-model.tar.gz") --format ustar -C $work tiny-model
    Expect ($LASTEXITCODE -eq 0) "cannot gzip the archive"
    Req POST "/api/admin/models?repo=smoke%2Ftiny&license=mit" -InFile (W "tiny-model.tar.gz")
    Expect-Status 201
    Expect-Json added.0.repo smoke/tiny
    Ok

    # --- MCP ----------------------------------------------------------------------------------

    $script:step = "mcp initialize"
    $script:mcpSession = ""
    Mcp 1 initialize '{"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "smoke", "version": "1"}}'
    Expect-Status 200
    $script:mcpSession = Header Mcp-Session-Id
    Expect ($script:mcpSession -ne "") "no Mcp-Session-Id"
    Mcp "" notifications/initialized
    Expect-Status 202
    Ok

    $script:step = "mcp tools/list"
    Mcp 2 tools/list
    Expect-Status 200
    foreach ($name in @("list_graphs", "use_graph", "gql")) { Expect-BodyContains "`"$name`"" }
    Ok

    $script:step = "mcp list_graphs"
    Tool list_graphs '{}'
    Expect ($script:ToolError -ne "true") $script:ToolText
    foreach ($name in @("acme", "acme-l", "product", "current")) {
        Expect ($script:ToolText.Contains("`"$name`"")) "list_graphs lacks ${name}: $($script:ToolText)"
    }
    Ok

    $script:step = "mcp use_graph"
    Tool use_graph '{"name": "current"}'
    Expect ($script:ToolError -ne "true") $script:ToolText
    Expect ($script:ToolText -match '"selected": *"current", *"graph": *"acme-l"') "use_graph: $($script:ToolText)"
    Ok

    $script:step = "mcp gql"
    Tool gql '{"query": "MATCH (p:person) RETURN count(p) AS n"}'
    Expect ($script:ToolError -ne "true") $script:ToolText
    Expect ($script:ToolText -match '"n": *2') "gql on acme-l: $($script:ToolText)"
    Tool gql '{"query": "MATCH (p:person) RETURN count(p) AS n", "graph": "acme"}'
    Expect ($script:ToolError -ne "true") $script:ToolText
    Expect ($script:ToolText -match '"n": *1') "gql on acme: $($script:ToolText)"
    Ok

    $script:step = "mcp switched off and on, a database hidden, statistics (B6)"
    $code = Run-Bin @("mcp", "off", "--dir", $dir) "mcp.out" "mcp.err"
    Expect ($code -eq 0) "mcp off: $(Text (W 'mcp.err'))"
    Expect ((Get-Content (W "mcp.out")) -ccontains "MCP is switched off") "mcp off: $(Text (W 'mcp.out'))"
    Mcp 2 tools/list
    Expect-Status 503
    $code = Run-Bin @("mcp", "on", "--dir", $dir) "mcp.out" "mcp.err"
    Expect ($code -eq 0) "mcp on: $(Text (W 'mcp.err'))"
    Mcp 2 tools/list
    Expect-Status 200
    $code = Run-Bin @("mcp", "db", "product", "off", "--dir", $dir) "mcp.out" "mcp.err"
    Expect ($code -eq 0) "mcp db product off: $(Text (W 'mcp.err'))"
    Tool list_graphs '{}'
    Expect (-not $script:ToolText.Contains('"product"')) "product still listed: $($script:ToolText)"
    Req PUT /api/admin/mcp/databases/product -Json '{"exposed": true}'
    Expect-Status 200
    Req GET /api/admin/mcp
    Expect-Status 200
    Expect-Json serving true
    Expect ([int](J usage.calls.refused) -ge 1) "no refused request counted: $($script:Body)"
    Ok

    $script:step = "alias rename moves the MCP session"
    $code = Run-Bin @("db", "alias", "rename", "current", "live", "--dir", $dir) "rename.out" "rename.err"
    Expect ($code -eq 0) "db alias rename: $(Text (W 'rename.err'))"
    $renamed = Get-Content (W "rename.out")
    Expect ($renamed -ccontains "Renamed alias 'current' to 'live' -> database 'acme-l'") "db alias rename: $(Text (W 'rename.out'))"
    Expect ($renamed -ccontains "  MCP sessions moved: 1") "the open MCP session did not move: $(Text (W 'rename.out'))"
    Tool gql '{"query": "MATCH (p:person) RETURN count(p) AS n"}'
    Expect ($script:ToolError -ne "true") $script:ToolText
    Expect ($script:ToolText.StartsWith('note: alias "current" was renamed to "live"')) "no rename note: $($script:ToolText)"
    Expect ($script:ToolText -match '"n": *2') "gql on live: $($script:ToolText)"
    Tool gql '{"query": "MATCH (p:person) RETURN count(p) AS n"}'
    Expect (-not $script:ToolText.StartsWith("note:")) "the note came twice: $($script:ToolText)"
    Req POST /api/admin/aliases/live/rename -Json '{"name": "acme-l"}'
    Expect-Status 409
    Req GET /db/current/api/info
    Expect-Status 404
    Req DELETE /mcp -Headers @{ "Mcp-Session-Id" = $script:mcpSession }
    Expect (@(200, 202, 204) -contains $script:Status) "DELETE /mcp: HTTP $($script:Status)"
    Ok

    # --- unload, delete, stop -----------------------------------------------------------------

    $script:step = "unload"
    Req DELETE /api/admin/databases/acme-imported
    Expect-Status 409
    Req POST /api/admin/databases/acme-imported/unload
    Expect-Status 200
    Expect-Json unloaded true
    Req GET /api/admin/databases/acme-imported
    Expect-Json state unloaded
    Req GET /db/acme-imported/api/info
    Expect-Status 409
    Ok

    $script:step = "delete"
    Req DELETE /api/admin/databases/acme-imported
    Expect-Status 200
    Expect (Test-Path -PathType Container (Join-Path $dir (J trash))) "no trash directory $(J trash)"
    Expect (-not (Test-Path (Join-Path (Join-Path $dir "databases") "acme-imported"))) "databases/acme-imported is still there"
    Req GET /api/admin/databases/acme-imported
    Expect-Status 404
    Ok

    $script:step = "sessions: a browser login and an MCP session before a restart"
    Req POST /api/login -Json "{`"token`": `"$adminToken`"}" -Auth ""
    Expect-Status 200
    $setCookie = Header Set-Cookie
    Expect ($setCookie -match '^gs_session=([^;]+);') "login cookie: $setCookie"
    $keptCookie = $Matches[1]
    $script:mcpSession = ""
    Mcp 1 initialize '{"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "smoke", "version": "1"}}'
    Expect-Status 200
    $script:mcpSession = Header Mcp-Session-Id
    Expect ($script:mcpSession -ne "") "no MCP session id"
    Tool use_graph '{"name": "acme"}'
    Expect ($script:ToolError -ne "true") "use_graph: $($script:ToolText)"
    $sessions = Join-Path $dir "sessions"
    $browserFile = Join-Path $sessions "browser.json"
    $mcpFile = Join-Path $sessions "mcp.json"
    Expect ((Test-Path $browserFile) -and (Get-Item $browserFile).Length -gt 0 -and (Test-Path $mcpFile) -and (Get-Item $mcpFile).Length -gt 0) `
        "no session files in $sessions"
    Expect (-not (Text $browserFile).Contains($keptCookie)) "a clear session id in sessions/browser.json"
    Ok

    $script:step = "stop"
    Stop-Server
    Expect (-not (Test-Path (Join-Path $dir "server.run"))) "server.run is still there"
    Expect (-not (Text $script:log).Contains("panicked")) "the log shows a panic"
    Expect (@(Get-Content $script:log | Where-Object { $_.StartsWith("Stopped: ") }).Count -gt 0) "no stop line on stderr: $(Text $script:log)"
    Ok

    $script:step = "restart: the browser and MCP sessions survive"
    $script:log = W "server2.log"
    Start-Server "server2.out"
    Req GET /api/admin/databases -Auth "" -Session (New-CookieSession $keptCookie)
    Expect-Status 200
    # The session keeps its selection: acme is not loaded after the restart, and the call names it.
    Tool gql '{"query": "MATCH (p:person) RETURN count(p) AS n"}'
    Expect ($script:ToolError -eq "true") "acme answered although it is not loaded: $($script:ToolText)"
    Expect ($script:ToolText.Contains("database 'acme' is not loaded")) "the MCP session lost its selection: $($script:ToolText)"
    Stop-Server
    Expect (-not (Text $script:log).Contains("panicked")) "the log shows a panic"
    Ok

    $script:step = "start, status, stop in the background"
    $script:log = Join-Path (Join-Path $dir "logs") "server.log"
    $code = Run-Bin @("start", "--dir", $dir, "--listen", "127.0.0.1:0") "start.out" "start.err"
    Expect ($code -eq 0) "start: $(Text (W 'start.err'))"
    $script:bgPid = Background-Pid
    Expect ((Text (W "start.out")) -match '(?m)^Graphersal server: http://127\.0\.0\.1:') "start printed no address line: $(Text (W 'start.out'))"
    Expect ((Text (W "start.err")).Contains("Started in the background")) "no start note: $(Text (W 'start.err'))"
    $code = Run-Bin @("status", "--dir", $dir) "status.out" "status.err"
    Expect ($code -eq 0) "status of a running server: $(Text (W 'status.out')) $(Text (W 'status.err'))"
    Expect (((Text (W "status.out")) + (Text (W "status.err"))).Contains("running")) "status: $(Text (W 'status.out'))"
    $code = Run-Bin @("stop", "--dir", $dir) "stop.out" "stop.err"
    Expect ($code -eq 0) "stop: $(Text (W 'stop.out')) $(Text (W 'stop.err'))"
    Expect (((Text (W "stop.out")) + (Text (W "stop.err"))) -match '(?m)^stopped: ') "stop: $(Text (W 'stop.out')) $(Text (W 'stop.err'))"
    Expect (-not (Test-Running $script:bgPid)) "the server still runs after stop"
    $script:bgPid = $null
    $code = Run-Bin @("status", "--dir", $dir) "status.out" "status.err"
    Expect ($code -eq 3) "status of a stopped server: exit code $code"
    Expect (-not (Text $script:log).Contains("panicked")) "the log shows a panic"
    Ok

    $script:step = "kill -9 (Stop-Process -Force) and restart: the unclean-shutdown check, verify (B-CRASH)"
    $code = Run-Bin @("db", "create", "crashy", "--dir", $dir) "crashy.out" "crashy.err"
    Expect ($code -eq 0) "db create crashy: $(Text (W 'crashy.err'))"
    $code = Run-Bin @("start", "--dir", $dir, "--listen", "127.0.0.1:0") "start2.out" "start2.err"
    Expect ($code -eq 0) "start: $(Text (W 'start2.err'))"
    $script:bgPid = Background-Pid
    $code = Run-Bin @("db", "load", "crashy", "--dir", $dir) "load.out" "load.err"
    Expect ($code -eq 0) "db load: $(Text (W 'load.err'))"
    $openFile = Join-Path $dir "server.open"
    Expect ((Text $openFile).Contains('"crashy"')) "server.open does not name crashy: $(Text $openFile)"
    Stop-Process -Id $script:bgPid -Force
    for ($i = 0; $i -lt 100 -and (Test-Running $script:bgPid); $i++) { Start-Sleep -Milliseconds 100 }
    $script:bgPid = $null
    Expect (Test-Path (Join-Path $dir "server.run")) "a killed server should leave server.run behind"
    $code = Run-Bin @("status", "--dir", $dir) "status.out" "status.err"
    Expect ($code -eq 3) "status after the kill: exit code $code"
    Expect (((Text (W "status.out")) + (Text (W "status.err"))).Contains("Last unclean shutdown")) "status: $(Text (W 'status.out'))"
    $code = Run-Bin @("start", "--dir", $dir, "--listen", "127.0.0.1:0") "start3.out" "start3.err"
    Expect ($code -eq 0) "start after the kill: $(Text (W 'start3.err'))"
    $script:bgPid = Background-Pid
    Expect ((Text (W "start3.err")).Contains("UNCLEAN SHUTDOWN")) "no unclean-shutdown notice: $(Text (W 'start3.err'))"
    $done = $false
    $status = $null
    for ($i = 0; $i -lt 300; $i++) {
        $null = Run-Bin @("status", "--dir", $dir, "--json") "status.json" "status.err"
        try { $status = (Text (W "status.json")) | ConvertFrom-Json } catch { $status = $null }
        if ((Get-J $status server.uncleanShutdown.done) -eq "true") { $done = $true; break }
        Start-Sleep -Milliseconds 200
    }
    Expect $done "the unclean-shutdown check did not end: $(Text (W 'status.json'))"
    $outcome = Get-J $status server.uncleanShutdown.databases.0.outcome
    Expect ($outcome -eq "ok") "crashy was not checked ok: $(Text (W 'status.json'))"
    $code = Run-Bin @("db", "verify", "--all", "--dir", $dir) "verify.out" "verify.err"
    Expect ($code -eq 0) "db verify --all: $(Text (W 'verify.out')) $(Text (W 'verify.err'))"
    $code = Run-Bin @("stop", "--dir", $dir) "stop.out" "stop.err"
    Expect ($code -eq 0) "stop"
    $script:bgPid = $null
    Expect (-not (Test-Path $openFile)) "a clean stop should remove server.open"
    Ok

    Write-Host "smoke: all steps passed"
}
finally {
    if ($script:server -and -not $script:server.HasExited) {
        Stop-Process -Id $script:server.Id -Force -ErrorAction SilentlyContinue
        $script:server.WaitForExit(30000) | Out-Null
    }
    if ($script:bgPid -and (Test-Running $script:bgPid)) {
        Stop-Process -Id $script:bgPid -Force -ErrorAction SilentlyContinue
    }
    if ($Keep) {
        Write-Host "kept $work"
    } else {
        Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
    }
}
