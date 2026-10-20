# Installs the dibs command line from a release: picks the archive for this machine, checks it
# against the release's SHA256SUMS, and puts dibs.exe in %LOCALAPPDATA%\Programs\dibs, which it adds
# to your user PATH.
#
#   irm https://github.com/hryvinskyi/dibs/releases/latest/download/install.ps1 | iex
#
# A particular release, or another directory, through the environment:
#
#   $env:DIBS_VERSION = '1.2.3'; irm https://github.com/hryvinskyi/dibs/releases/latest/download/install.ps1 | iex
#
# or through parameters, from a downloaded copy of this file:
#
#   .\install.ps1 -Version 1.2.3 -InstallDir C:\Tools\dibs
#
# Environment:
#   DIBS_VERSION       the release to install (1.2.3 or v1.2.3); the latest when absent
#   DIBS_INSTALL_DIR   where dibs.exe goes (default: %LOCALAPPDATA%\Programs\dibs)
#   DIBS_RELEASE_URL   where releases are downloaded from, laid out as GitHub lays them out
#                      (default: https://github.com/hryvinskyi/dibs/releases)
#
# Nothing is installed unless the archive's checksum matches the release's SHA256SUMS. This script
# does not check the release's signature of that file; `dibs self-update` does.
param(
    [string]$Version = $env:DIBS_VERSION,
    [string]$InstallDir = $env:DIBS_INSTALL_DIR
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$ProgressPreference = 'SilentlyContinue'

function Stop-Install([string]$Message) {
    throw "dibs install: $Message"
}

[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$releaseUrl = $env:DIBS_RELEASE_URL
if ([string]::IsNullOrWhiteSpace($releaseUrl)) {
    $releaseUrl = 'https://github.com/hryvinskyi/dibs/releases'
}
$releaseUrl = $releaseUrl.TrimEnd('/')

if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
        Stop-Install 'LOCALAPPDATA is not set; pass -InstallDir or set DIBS_INSTALL_DIR.'
    }
    $InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\dibs'
}

if ([string]::IsNullOrWhiteSpace($Version)) {
    $Version = ''
} else {
    $Version = $Version.Trim()
    if ($Version.StartsWith('v')) {
        $Version = $Version.Substring(1)
    }
    if ($Version -notmatch '^[0-9A-Za-z.+-]+$') {
        Stop-Install "'$Version' is not a release version."
    }
}

# The target triple of this machine's release archive.
$architecture = $env:PROCESSOR_ARCHITEW6432
if ([string]::IsNullOrWhiteSpace($architecture)) {
    $architecture = $env:PROCESSOR_ARCHITECTURE
}
switch ($architecture) {
    'AMD64' { $target = 'x86_64-pc-windows-msvc' }
    'ARM64' { $target = 'aarch64-pc-windows-msvc' }
    default { Stop-Install "there is no dibs release for Windows on $architecture." }
}

# Downloads a URL into a file.
function Get-Release([string]$Url, [string]$Path) {
    try {
        Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile $Path
    } catch {
        Stop-Install "could not download $Url"
    }
}

# The lines of a SHA256SUMS file as digest and name pairs.
function Read-Sums([string]$Path) {
    foreach ($line in Get-Content -LiteralPath $Path) {
        if ($line -match '^([0-9A-Fa-f]{64})\s+\*?(.+?)\s*$') {
            [pscustomobject]@{ Hash = $Matches[1].ToLowerInvariant(); Name = $Matches[2] }
        }
    }
}

$work = Join-Path ([IO.Path]::GetTempPath()) ('dibs-install-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
try {
    if ($Version -eq '') {
        $latestSums = Join-Path $work 'latest-sums'
        Get-Release "$releaseUrl/latest/download/SHA256SUMS" $latestSums
        $pattern = '^dibs-(.+)-' + [regex]::Escape($target) + '\.zip$'
        $listed = @(Read-Sums $latestSums | Where-Object { $_.Name -match $pattern })
        if ($listed.Count -eq 0) {
            Stop-Install "the latest release lists no archive for $target."
        }
        $listed[0].Name -match $pattern | Out-Null
        $Version = $Matches[1]
    }

    $base = "$releaseUrl/download/v$Version"
    $archive = "dibs-$Version-$target.zip"
    Write-Host "Installing dibs $Version for $target."

    $sums = Join-Path $work 'SHA256SUMS'
    Get-Release "$base/SHA256SUMS" $sums
    Write-Host "This install script does not verify the release's signature; it checks the archive against SHA256SUMS only."
    $entry = @(Read-Sums $sums | Where-Object { $_.Name -eq $archive })
    if ($entry.Count -eq 0) {
        Stop-Install "release $Version lists no archive named $archive."
    }

    $zip = Join-Path $work $archive
    Get-Release "$base/$archive" $zip
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $zip).Hash.ToLowerInvariant()
    if ($actual -ne $entry[0].Hash) {
        Stop-Install "the checksum of $archive does not match the release's SHA256SUMS; nothing was installed."
    }

    $unpacked = Join-Path $work 'unpacked'
    Expand-Archive -LiteralPath $zip -DestinationPath $unpacked
    $binary = Join-Path $unpacked "dibs-$Version-$target\dibs.exe"
    if (-not (Test-Path -LiteralPath $binary)) {
        Stop-Install "$archive holds no dibs.exe."
    }

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $installed = Join-Path $InstallDir 'dibs.exe'
    $previous = Join-Path $InstallDir 'dibs.old.exe'
    if (Test-Path -LiteralPath $previous) {
        Remove-Item -LiteralPath $previous -Force -ErrorAction SilentlyContinue
    }
    if (Test-Path -LiteralPath $installed) {
        # A running dibs.exe cannot be overwritten, but it can be renamed out of the way.
        try {
            Move-Item -LiteralPath $installed -Destination $previous -Force
        } catch {
            Stop-Install "could not replace $installed; close the programs that run it and try again."
        }
    }
    Copy-Item -LiteralPath $binary -Destination $installed
    Unblock-File -LiteralPath $installed
    Write-Host "Installed $installed."

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ($null -eq $userPath) {
        $userPath = ''
    }
    $entries = @($userPath -split ';' | Where-Object { $_ -ne '' })
    $onPath = $false
    foreach ($item in $entries) {
        if ($item.TrimEnd('\') -ieq $InstallDir.TrimEnd('\')) {
            $onPath = $true
        }
    }
    if (-not $onPath) {
        $newPath = ($entries + $InstallDir) -join ';'
        [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
        $env:Path = "$env:Path;$InstallDir"
        Write-Host "Added $InstallDir to your user PATH; terminals that are already open need restarting."
    }
    Write-Host 'Next: dibs login --server <url>, inside a checkout that has a .dibs.toml.'
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}
