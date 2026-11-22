# Smoke test of a built graphersal-server binary in PowerShell 7 (pwsh), for Windows (CI and
# release.yml); scripts/smoke.sh is the full end-to-end walk on Unix.
#
# Checks `--version`, `init`, `serve` on 127.0.0.1:0 (the URL read from its stdout address line),
# /healthz, /readyz, /api/info, a database created, loaded and queried through the admin and
# database APIs, then stops the server and serves the same directory a second time (the lock of
# the stopped server is free again). Windows has no SIGTERM for a process outside the console's
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

# GET or POST with a JSON body; returns @{ Status; Json }. Never throws on an HTTP error status.
function Invoke-Api([string]$method, [string]$url, $body = $null) {
    $params = @{ Method = $method; Uri = $url; SkipHttpErrorCheck = $true; TimeoutSec = 120 }
    if ($null -ne $body) {
        $params.ContentType = "application/json"
        $params.Body = ($body | ConvertTo-Json -Compress)
    }
    $response = Invoke-WebRequest @params
    $json = $null
    if ($response.Content) {
        try { $json = $response.Content | ConvertFrom-Json } catch { $json = $null }
    }
    return @{ Status = [int]$response.StatusCode; Json = $json; Text = [string]$response.Content }
}

function Expect([bool]$condition, [string]$what) {
    if (-not $condition) { Fail $what }
}

try {
    $versionText = (& $Bin --version) -join "`n"
    Expect ($LASTEXITCODE -eq 0) "--version exited with $LASTEXITCODE"
    if ($Version) { Expect ($versionText.Contains($Version)) "--version '$versionText' lacks $Version" }
    Ok "--version ($versionText)"

    & $Bin init $dir | Out-Null
    Expect ($LASTEXITCODE -eq 0) "init exited with $LASTEXITCODE"
    foreach ($part in @("server.toml", "databases", "models", "backups", "trash", "tmp")) {
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

    Stop-Server
    Ok "stop"

    # The OS released the lock with the process: the directory serves again, with its database.
    $url = Start-Server
    $r = Invoke-Api GET "${url}api/admin/databases/acme"
    Expect ($r.Status -eq 200) "acme after the restart: HTTP $($r.Status): $($r.Text)"
    Stop-Server
    Ok "serve the same directory again"

    Write-Host "smoke: all steps passed"
}
finally {
    Stop-Server
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
