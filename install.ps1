# Installs the graphersal-server binary from a GitHub Release (Windows).
#
#   irm https://github.com/graphersal/graphersal-server/releases/latest/download/install.ps1 | iex
#   & ([scriptblock]::Create((irm .../install.ps1))) -Version v0.3.1 -Dir C:\Tools
#
# It downloads the Windows archive, checks it against the release's SHA256SUMS, and puts
# graphersal-server.exe into %LOCALAPPDATA%\Programs\graphersal-server (or -Dir). That directory
# is added to your user PATH (the registry's HKCU\Environment, its other entries kept as they
# are) and to this session's, so `graphersal-server` runs in this window and every new one;
# -NoPathUpdate leaves PATH alone. It changes nothing else: no server directory, no service.
# There is no prebuilt binary for Windows on Arm: it runs the x86_64 build through its emulation.
#
# Parameters (or environment variables):
#   -Version vX.Y.Z  the release to install (GRAPHERSAL_SERVER_VERSION; default: the latest)
#   -Dir DIR         where to put the binary (GRAPHERSAL_SERVER_INSTALL_DIR)
#   -BaseUrl URL     the releases URL (GRAPHERSAL_SERVER_BASE_URL; default
#                    https://github.com/graphersal/graphersal-server/releases); it holds
#                    latest/download/<file> and download/<tag>/<file>, as GitHub does
#   -NoPathUpdate    do not add the directory to PATH (GRAPHERSAL_SERVER_NO_PATH_UPDATE=1)

param(
    [string]$Version = $env:GRAPHERSAL_SERVER_VERSION,
    [string]$Dir = $env:GRAPHERSAL_SERVER_INSTALL_DIR,
    [string]$BaseUrl = $env:GRAPHERSAL_SERVER_BASE_URL,
    [switch]$NoPathUpdate = ($env:GRAPHERSAL_SERVER_NO_PATH_UPDATE -eq '1')
)

# `irm | iex` runs this text in the caller's session: everything happens inside a function, so
# its preferences and variables stay out of that session.
function Install-GraphersalServer([string]$Version, [string]$Dir, [string]$BaseUrl, [bool]$NoPathUpdate) {
    $ErrorActionPreference = 'Stop'
    Set-StrictMode -Version Latest

    function Fail([string]$Message) {
        Write-Host "graphersal-server installer: $Message" -ForegroundColor Red
        throw "graphersal-server installer: $Message"
    }

    $fromSource = 'cargo install --locked graphersal-server'
    if (-not $BaseUrl) { $BaseUrl = 'https://github.com/graphersal/graphersal-server/releases' }
    $BaseUrl = $BaseUrl.TrimEnd('/')
    if (-not $Dir) {
        if (-not $env:LOCALAPPDATA) { Fail 'LOCALAPPDATA is not set: give the target directory with -Dir' }
        $Dir = Join-Path $env:LOCALAPPDATA 'Programs\graphersal-server'
    }
    if ($Version -eq 'latest') { $Version = '' }
    if ($Version -and -not $Version.StartsWith('v')) { $Version = "v$Version" }

    $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
    if ($arch -ne [System.Runtime.InteropServices.Architecture]::X64) {
        # Windows on Arm runs the x86_64 build through its emulation.
        if ($arch -ne [System.Runtime.InteropServices.Architecture]::Arm64) {
            Fail "no prebuilt binary for Windows on $arch; build from source: $fromSource"
        }
    }
    $platform = 'windows-x86_64'

    # Older Windows PowerShell versions do not offer TLS 1.2 by default.
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    } catch { }

    function Fetch([string]$Url, [string]$OutFile) {
        $previous = $ProgressPreference
        $ProgressPreference = 'SilentlyContinue'
        try {
            Invoke-WebRequest -Uri $Url -OutFile $OutFile -UseBasicParsing
        } finally {
            $ProgressPreference = $previous
        }
    }

    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("graphersal-server-" + [System.Guid]::NewGuid())
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        if ($Version) {
            $releaseUrl = "$BaseUrl/download/$Version"
            $what = "release $Version"
        } else {
            $releaseUrl = "$BaseUrl/latest/download"
            $what = 'the latest release'
        }
        $sums = Join-Path $tmp 'SHA256SUMS'
        try {
            Fetch "$releaseUrl/SHA256SUMS" $sums
        } catch {
            Fail "cannot download SHA256SUMS of $what from $releaseUrl (no such release, or no network; you can build from source: $fromSource)"
        }
        $pattern = "^([0-9a-f]{64})  (graphersal-server-(v[^ ]+)-$platform\.zip)$"
        $match = Get-Content $sums | ForEach-Object { [regex]::Match($_, $pattern) } |
            Where-Object { $_.Success } | Select-Object -First 1
        if (-not $match) { Fail "$what has no archive for $platform" }
        $expected = $match.Groups[1].Value
        $archive = $match.Groups[2].Value
        $foundVersion = $match.Groups[3].Value
        if ($Version -and $foundVersion -ne $Version) { Fail "SHA256SUMS of $Version names $archive" }
        # Download from the release that SHA256SUMS belongs to, even when "latest" moves meanwhile.
        if (-not $Version) { $releaseUrl = "$BaseUrl/download/$foundVersion" }

        Write-Host "Downloading graphersal-server $foundVersion for $platform"
        $zip = Join-Path $tmp $archive
        try {
            Fetch "$releaseUrl/$archive" $zip
        } catch {
            Fail "cannot download $releaseUrl/$archive"
        }
        $actual = (Get-FileHash -Algorithm SHA256 -Path $zip).Hash.ToLowerInvariant()
        if ($actual -ne $expected) {
            Fail "checksum mismatch for ${archive}: expected $expected, got $actual (the download is damaged or was changed; nothing was installed)"
        }
        Write-Host "Checksum verified (SHA-256 $expected)"

        Expand-Archive -Path $zip -DestinationPath $tmp -Force
        $binary = Join-Path $tmp "graphersal-server-$foundVersion-$platform\graphersal-server.exe"
        if (-not (Test-Path $binary)) { Fail "$archive holds no graphersal-server.exe" }

        New-Item -ItemType Directory -Force -Path $Dir | Out-Null
        $target = Join-Path $Dir 'graphersal-server.exe'
        $upgrade = Test-Path $target
        # A running graphersal-server.exe (a server started with `start`) cannot be overwritten, but it can be renamed:
        # move it aside, put the new one in place, and remove the old one when it is free.
        if (Test-Path $target) {
            $old = Join-Path $Dir 'graphersal-server.old.exe'
            Remove-Item -Force -ErrorAction SilentlyContinue $old
            Move-Item -Force $target $old
        }
        Copy-Item $binary $target
        Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $Dir 'graphersal-server.old.exe')
        Write-Host "Installed $target"
        & $target --version
        if ($LASTEXITCODE -ne 0) { Fail 'the installed binary does not run on this system' }
    } finally {
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $tmp
    }

    # The user PATH as the registry holds it: unexpanded (`%USERPROFILE%\...` stays a reference)
    # and with its value kind, so writing it back changes nothing but our entry.
    function Test-InPath([string]$List) {
        [bool](($List -split ';') | Where-Object {
            $_ -and ([Environment]::ExpandEnvironmentVariables($_).TrimEnd('\') -ieq $Dir.TrimEnd('\'))
        })
    }
    $program = 'graphersal-server'
    $inUserPath = $false
    $key = $null
    try {
        $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', -not $NoPathUpdate)
        if ($key) {
            $raw = [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            $inUserPath = Test-InPath $raw
            if (-not $inUserPath -and -not $NoPathUpdate) {
                $kind = if ($key.GetValueNames() -contains 'Path') { $key.GetValueKind('Path') } else { [Microsoft.Win32.RegistryValueKind]::ExpandString }
                $value = if ($raw.Trim(';')) { $raw.TrimEnd(';') + ';' + $Dir } else { $Dir }
                $key.SetValue('Path', $value, $kind)
                $inUserPath = $true
                Write-Host "Added $Dir to your user PATH"
                # Tell running programs (Explorer, new terminals) that the environment changed:
                # setting and removing a variable through .NET broadcasts WM_SETTINGCHANGE.
                [Environment]::SetEnvironmentVariable('GRAPHERSAL_SERVER_PATH_REFRESH', '1', 'User')
                [Environment]::SetEnvironmentVariable('GRAPHERSAL_SERVER_PATH_REFRESH', $null, 'User')
            }
        }
    } catch {
        Write-Host "Could not add $Dir to your user PATH: $($_.Exception.Message)" -ForegroundColor Yellow
    } finally {
        if ($key) { $key.Close() }
    }
    if ($inUserPath -and -not (Test-InPath $env:Path)) {
        $env:Path = $env:Path.TrimEnd(';') + ';' + $Dir
    }
    if (-not (Test-InPath $env:Path)) {
        Write-Host ''
        Write-Host "$Dir is not on your PATH. To add it for your user (new terminals see it):"
        Write-Host "  [Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path', 'User') + `";$Dir`", 'User')"
        $program = "& '$target'"
    }
    Write-Host ''
    if ($upgrade) {
        Write-Host 'Upgraded. A running server keeps the old version until it is restarted:'
        Write-Host "  $program stop --dir .\srv"
        Write-Host "  $program start --dir .\srv"
    } else {
        Write-Host 'Next step: create a server directory (it prints the first admin''s token once) and start it:'
        Write-Host "  $program init .\srv"
        Write-Host "  $program start --dir .\srv"
    }
}

Install-GraphersalServer -Version $Version -Dir $Dir -BaseUrl $BaseUrl -NoPathUpdate $NoPathUpdate.IsPresent
