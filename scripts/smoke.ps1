# Smoke test of a built graphersal-server binary in PowerShell 7 (pwsh), for Windows (CI and
# release.yml); scripts/smoke.sh is the full end-to-end walk on Unix.
#
# Checks `--version`, `init` (the first admin's token), `serve` on 127.0.0.1:0 (the URL read from
# its stdout address line), /healthz, /readyz, /api/info, access (anonymous refused, a browser
# login and logout), a database created, loaded and queried through the admin and database APIs,
# the console through the running server, users (a read-only user reads its database, cannot
# write, does not see another one; "my databases"), the reserved name `jobs`, an import from the
# inbox, then stops the server and serves the same directory a second time (the lock of the
# stopped server is free again; a browser login made before the stop still works). Windows has no SIGTERM for a process outside the console's
# process group: the server is stopped with Stop-Process here; the clean stop (Ctrl+Break) is
# covered by tests/server_tests.rs.
#
#   pwsh scripts/smoke.ps1 -Bin target/dist/graphersal-server.exe [-Version 0.1.0]
param(
    [Parameter(Mandatory = $true)][string]$Bin,
    [string]$Version = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$Bin = (Resolve-Path $Bin).Path
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("graphersal-smoke-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $work | Out-Null
$dir = Join-Path $work "srv"
$script:server = $null

function Fail([string]$message) {
    Write-Host "FAIL $message"
    foreach ($log in @("server.err", "server.out")) {
        $path = Join-Path $work $log
        if (Test-Path $path) {
            Write-Host "--- $log ---"
            Get-Content $path -Tail 30 | ForEach-Object { Write-Host $_ }
        }
    }
    throw $message
}

function Ok([string]$step) { Write-Host "ok   $step" }

# Starts `serve` and returns its URL (with a trailing slash) from the stdout address line.
function Start-Server {
    $out = Join-Path $work "server.out"
    $err = Join-Path $work "server.err"
    $script:server = Start-Process -FilePath $Bin -PassThru -NoNewWindow `
        -ArgumentList @("serve", "--dir", "`"$dir`"", "--listen", "127.0.0.1:0") `
        -RedirectStandardOutput $out -RedirectStandardError $err
    $deadline = (Get-Date).AddSeconds(60)
    while ((Get-Date) -lt $deadline) {
        if ($script:server.HasExited) { Fail "serve exited with code $($script:server.ExitCode)" }
        if (Test-Path $out) {
            $line = Get-Content $out -TotalCount 1 -ErrorAction SilentlyContinue
            if ($line -and $line -match '^Graphersal server: (http://127\.0\.0\.1:[0-9]+/)$') {
                return $Matches[1]
            }
        }
        Start-Sleep -Milliseconds 200
    }
    Fail "no address line on serve's stdout within 60 s"
}

function Stop-Server {
    if ($script:server -and -not $script:server.HasExited) {
        Stop-Process -Id $script:server.Id -Force
        $script:server.WaitForExit(30000) | Out-Null
    }
    $script:server = $null
}

# GET or POST with a JSON body, as the first admin ($script:token, set after init), or with the
# token $Auth ("" = anonymous) and the web session $Session (a browser login's cookie, see
# New-CookieSession); returns @{ Status; Json; Text; Headers }. Never throws on an HTTP error
# status.
function Invoke-Api([string]$method, [string]$url, $body = $null, $Auth = $script:token, $Session = $null) {
    $params = @{ Method = $method; Uri = $url; SkipHttpErrorCheck = $true; TimeoutSec = 120 }
    if ($Auth) { $params.Headers = @{ Authorization = "Bearer $Auth" } }
    if ($Session) { $params.WebSession = $Session }
    if ($null -ne $body) {
        $params.ContentType = "application/json"
        $params.Body = ($body | ConvertTo-Json -Compress)
    }
    $response = Invoke-WebRequest @params
    $json = $null
    if ($response.Content) {
        try { $json = $response.Content | ConvertFrom-Json } catch { $json = $null }
    }
    return @{
        Status = [int]$response.StatusCode; Json = $json; Text = [string]$response.Content
        Headers = $response.Headers
    }
}

# The token a console command (`user add`, `user token`) printed on its `  token: ` line.
function Get-PrintedToken($lines) {
    $line = $lines | Where-Object { $_ -match '^  token: *([0-9a-f]{64})$' } | Select-Object -First 1
    Expect ($null -ne $line) "no token printed: $($lines -join ' / ')"
    $null = $line -match '^  token: *([0-9a-f]{64})$'
    return $Matches[1]
}

# The value of the gs_session cookie of a login answer's Set-Cookie header.
function Get-SessionCookie($r) {
    $setCookie = @($r.Headers["Set-Cookie"]) -join "`n"
    Expect ($setCookie -match 'gs_session=([^;]+); Max-Age=[0-9]+; HttpOnly; SameSite=Strict; Path=/') `
        "login cookie: $setCookie"
    return $Matches[1]
}

# A web session that sends the gs_session cookie $value to the server at $url, as a browser does.
function New-CookieSession([string]$url, [string]$value) {
    $session = New-Object Microsoft.PowerShell.Commands.WebRequestSession
    $session.Cookies.Add((New-Object System.Net.Cookie("gs_session", $value, "/", ([Uri]$url).Host)))
    return $session
}

function Expect([bool]$condition, [string]$what) {
    if (-not $condition) { Fail $what }
}

try {
    $versionText = (& $Bin --version) -join "`n"
    Expect ($LASTEXITCODE -eq 0) "--version exited with $LASTEXITCODE"
    if ($Version) { Expect ($versionText.Contains($Version)) "--version '$versionText' lacks $Version" }
    Ok "--version ($versionText)"

    $initErr = Join-Path $work "init.err"
    & $Bin init $dir 2> $initErr | Out-Null
    Expect ($LASTEXITCODE -eq 0) "init exited with $LASTEXITCODE"
    # The first admin's token, printed once in a framed block on stderr.
    $tokenLine = Get-Content $initErr | Where-Object { $_ -match '^\| token: *([0-9a-f]{64}) ' } |
        Select-Object -First 1
    Expect ($null -ne $tokenLine) "init printed no first admin token"
    $null = $tokenLine -match '^\| token: *([0-9a-f]{64}) '
    $script:token = $Matches[1]
    foreach ($part in @("server.toml", "access.toml", "databases", "models", "backups", "trash", "tmp", "imports")) {
        Expect (Test-Path (Join-Path $dir $part)) "init made no $part"
    }
    Ok "init"

    $url = Start-Server
    Expect (Test-Path (Join-Path $dir "server.run")) "no server.run while serving"
    Ok "serve ($url)"

    $r = Invoke-Api GET "${url}healthz"
    Expect ($r.Status -eq 200) "/healthz answers $($r.Status)"
    $ready = $false
    for ($i = 0; $i -lt 150 -and -not $ready; $i++) {
        $ready = (Invoke-Api GET "${url}readyz").Status -eq 200
        if (-not $ready) { Start-Sleep -Milliseconds 200 }
    }
    Expect $ready "/readyz never answered 200"
    $r = Invoke-Api GET "${url}api/info"
    Expect ($r.Status -eq 200) "/api/info answers $($r.Status): $($r.Text)"
    Expect ($r.Json.server -eq "graphersal-server") "/api/info server = $($r.Json.server)"
    Expect ($r.Json.protocol -eq "graphersal/1") "/api/info protocol = $($r.Json.protocol)"
    if ($Version) { Expect ($r.Json.version -eq $Version) "/api/info version = $($r.Json.version)" }
    Ok "probes and /api/info"

    $r = Invoke-Api GET "${url}api/info" -Auth ""
    Expect ($r.Status -eq 200 -and $r.Json.principal.kind -eq "anonymous") "anonymous /api/info: $($r.Text)"
    Expect ($r.Json.login -eq "/") "anonymous /api/info names no login: $($r.Text)"
    $r = Invoke-Api GET "${url}api/admin/databases" -Auth ""
    Expect ($r.Status -eq 401) "anonymous admin API: HTTP $($r.Status)"
    $r = Invoke-Api GET "${url}db/acme/api/info" -Auth ""
    Expect ($r.Status -eq 401) "anonymous database API: HTTP $($r.Status)"
    $r = Invoke-Api GET "${url}api/info"
    Expect ($r.Json.principal.kind -eq "user" -and $r.Json.principal.admin -eq $true) "admin /api/info: $($r.Text)"
    $r = Invoke-Api POST "${url}api/login" @{ token = "0000" } -Auth ""
    Expect ($r.Status -eq 401) "login with a wrong token: HTTP $($r.Status)"
    $r = Invoke-Api POST "${url}api/login" @{ token = $script:token } -Auth ""
    Expect ($r.Status -eq 200) "login: HTTP $($r.Status): $($r.Text)"
    $cookie = Get-SessionCookie $r
    $r = Invoke-Api GET "${url}api/admin/databases" -Auth "" -Session (New-CookieSession $url $cookie)
    Expect ($r.Status -eq 200) "admin API with the cookie: HTTP $($r.Status)"
    $r = Invoke-Api POST "${url}api/logout" -Auth "" -Session (New-CookieSession $url $cookie)
    Expect ($r.Status -eq 200 -and $r.Json.loggedOut -eq $true) "logout: HTTP $($r.Status): $($r.Text)"
    $r = Invoke-Api GET "${url}api/admin/databases" -Auth "" -Session (New-CookieSession $url $cookie)
    Expect ($r.Status -eq 401) "admin API after the logout: HTTP $($r.Status)"
    Ok "access: anonymous is refused, the first admin logs in and out"

    $r = Invoke-Api POST "${url}api/admin/databases" @{ name = "acme"; description = "smoke" }
    Expect ($r.Status -eq 201) "create acme: HTTP $($r.Status): $($r.Text)"
    $r = Invoke-Api POST "${url}api/admin/databases/acme/load"
    Expect ($r.Status -eq 200 -or $r.Status -eq 202) "load acme: HTTP $($r.Status): $($r.Text)"
    $state = ""
    for ($i = 0; $i -lt 300; $i++) {
        $state = (Invoke-Api GET "${url}api/admin/databases/acme").Json.state
        if ($state -ne "loading") { break }
        Start-Sleep -Milliseconds 200
    }
    Expect ($state -eq "loaded") "acme is $state, expected loaded"
    $r = Invoke-Api POST "${url}db/acme/api/execute" @{ script = 'g.add_v("person").property("name", "ada").iterate()' }
    Expect ($r.Status -eq 200 -and $r.Json.ok -eq $true) "write: HTTP $($r.Status): $($r.Text)"
    $r = Invoke-Api POST "${url}db/acme/api/execute" @{ script = 'g.v().has_label("person").count().next()' }
    Expect ($r.Status -eq 200 -and $r.Json.ok -eq $true) "read: HTTP $($r.Status): $($r.Text)"
    Expect ("$($r.Json.execution.results[0])" -eq "1") "acme counts $($r.Json.execution.results[0]) persons"
    Ok "create, load, write and read a database"

    $listing = (& $Bin db list --dir $dir --json 2>$null) -join "`n"
    Expect ($LASTEXITCODE -eq 0) "db list exited with $LASTEXITCODE"
    Expect (($listing | ConvertFrom-Json).databases[0].name -eq "acme") "db list: $listing"
    Ok "console through the running server"

    $r = Invoke-Api POST "${url}api/admin/databases" @{ name = "other" }
    Expect ($r.Status -eq 201) "create other: HTTP $($r.Status): $($r.Text)"
    $added = & $Bin user add reader --dir $dir 2>$null
    Expect ($LASTEXITCODE -eq 0) "user add exited with $LASTEXITCODE"
    $reader = Get-PrintedToken $added
    & $Bin user grant reader acme read --dir $dir 2>$null | Out-Null
    Expect ($LASTEXITCODE -eq 0) "user grant exited with $LASTEXITCODE"
    $r = Invoke-Api POST "${url}db/acme/api/execute" @{ script = 'g.v().has_label("person").count().next()' } -Auth $reader
    Expect ($r.Status -eq 200 -and $r.Json.ok -eq $true) "the reader's read: HTTP $($r.Status): $($r.Text)"
    Expect ("$($r.Json.execution.results[0])" -eq "1") "the reader counts $($r.Json.execution.results[0]) persons"
    $r = Invoke-Api POST "${url}db/acme/api/execute" @{ script = 'g.add_v("person").iterate()' } -Auth $reader
    Expect (-not ($r.Status -eq 200 -and $r.Json.ok -eq $true)) "the reader's write succeeded: $($r.Text)"
    $r = Invoke-Api POST "${url}db/acme/api/execute" @{ script = 'g.v().has_label("person").count().next()' }
    Expect ("$($r.Json.execution.results[0])" -eq "1") "acme counts $($r.Json.execution.results[0]) after the refused write"
    $r = Invoke-Api GET "${url}db/other/api/info" -Auth $reader
    Expect ($r.Status -eq 404) "the reader sees other: HTTP $($r.Status)"
    $r = Invoke-Api GET "${url}api/admin/databases" -Auth $reader
    Expect ($r.Status -eq 404) "the reader reaches the admin API: HTTP $($r.Status)"
    $r = Invoke-Api GET "${url}api/databases" -Auth $reader
    Expect ($r.Status -eq 200) "my databases: HTTP $($r.Status): $($r.Text)"
    $mine = @($r.Json.databases)
    Expect ($mine.Count -eq 1 -and $mine[0].name -eq "acme" -and $mine[0].level -eq "read") `
        "my databases of the reader: $($r.Text)"
    $users = (& $Bin user list --dir $dir 2>$null) -join "`n"
    # Only the last 4 characters: the console's ellipsis depends on the console's encoding.
    Expect ($users.Contains($reader.Substring(60, 4))) "user list lacks the reader's last 4: $users"
    Ok "users: a read-only user reads its database, cannot write, does not see another"

    $r = Invoke-Api POST "${url}api/admin/databases" @{ name = "jobs" }
    Expect ($r.Status -eq 400 -and $r.Text.Contains("reserved")) "create jobs: HTTP $($r.Status): $($r.Text)"
    Ok "the reserved name jobs is refused"

    $snapshot = Join-Path (Join-Path $dir "imports") "acme.gsnap"
    & $Bin db export acme --out $snapshot --dir $dir 2>$null | Out-Null
    Expect ($LASTEXITCODE -eq 0) "db export exited with $LASTEXITCODE"
    $r = Invoke-Api GET "${url}api/admin/imports"
    Expect ($r.Status -eq 200 -and @($r.Json.imports | Where-Object { $_.name -eq "acme.gsnap" }).Count -eq 1) `
        "the inbox listing: $($r.Text)"
    $r = Invoke-Api POST "${url}api/admin/import" @{ inbox = "acme.gsnap"; name = "acme-copy" }
    Expect ($r.Status -eq 201) "import from the inbox: HTTP $($r.Status): $($r.Text)"
    Ok "import from the inbox"

    $r = Invoke-Api POST "${url}api/login" @{ token = $script:token } -Auth ""
    Expect ($r.Status -eq 200) "login before the restart: HTTP $($r.Status): $($r.Text)"
    $cookie = Get-SessionCookie $r

    Stop-Server
    Ok "stop"

    # The OS released the lock with the process: the directory serves again, with its database.
    $url = Start-Server
    $r = Invoke-Api GET "${url}api/admin/databases/acme"
    Expect ($r.Status -eq 200) "acme after the restart: HTTP $($r.Status): $($r.Text)"
    $r = Invoke-Api GET "${url}api/admin/databases" -Auth "" -Session (New-CookieSession $url $cookie)
    Expect ($r.Status -eq 200) "the browser login after the restart: HTTP $($r.Status): $($r.Text)"
    Stop-Server
    Ok "serve the same directory again (the browser login survives)"

    Write-Host "smoke: all steps passed"
}
finally {
    Stop-Server
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
