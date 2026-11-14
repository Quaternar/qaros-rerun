<#
.SYNOPSIS
Registers a hand-built qaros-rerun viewer with QAROS, for this user.

.DESCRIPTION
Writes one record to the per-user QAROS app registry, apps.json in the QAROS user config folder
(%APPDATA%\Quaternar on Windows, $XDG_CONFIG_HOME/quaternar or ~/.config/quaternar on Linux).
The QAROS Visualizer's Sources catalog then shows the Rerun tile as Detected and launches this
executable. The record has source "local": it wins over a viewer QAROS downloaded for the same
platform. Other records in the file are kept. No admin rights needed.

-Unregister removes this platform's local record again.

Runs on Windows PowerShell 5.1 and PowerShell 7. The Linux counterpart is register-qaros-app.sh.

.EXAMPLE
cargo build --release -p qaros-rerun
.\scripts\register-qaros-app.ps1

.EXAMPLE
.\scripts\register-qaros-app.ps1 -Unregister
#>
[CmdletBinding()]
param(
    # The viewer to register. Default: target/release/qaros-rerun(.exe) in this repository.
    [string]$Executable,

    # Default: the full version derived from VERSION and the git tags (scripts/version.ps1),
    # the version the build gave the viewer.
    [string]$AppVersion,

    # SHA-256 of the QAROS C API header the viewer was built against. Default: computed from
    # the qaros revision pinned in Cargo.toml (needs cargo).
    [string]$CapiHeaderSha256,

    # windows-x64, linux-x64, ... Default: this machine's.
    [string]$Platform,

    [switch]$Unregister
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'qaros-common.ps1')

$RegistryFormatVersion = 1
$repoRoot = Get-QarosRepoRoot
if (-not $Platform) { $Platform = Get-QarosPlatform }

function Get-QarosUserConfigDirectory {
    if (Test-QarosIsWindows) {
        if (-not $env:APPDATA) { throw 'APPDATA is not set.' }
        return (Join-Path $env:APPDATA 'Quaternar')
    }
    $base = $env:XDG_CONFIG_HOME
    if (-not $base) {
        if (-not $env:HOME) { throw 'Neither XDG_CONFIG_HOME nor HOME is set.' }
        $base = Join-Path $env:HOME '.config'
    }
    return (Join-Path $base 'quaternar')
}

$registryPath = Join-Path (Get-QarosUserConfigDirectory) 'apps.json'

# The registry: { "version": 1, "apps": [ records ] }, other top-level fields kept as they are.
$registry = [ordered]@{ version = $RegistryFormatVersion; apps = @() }
if (Test-Path -LiteralPath $registryPath) {
    $text = [System.IO.File]::ReadAllText($registryPath)
    if ($text.Trim()) {
        try {
            # PowerShell 7 turns ISO 8601 strings into dates unless told not to; other records
            # must be written back as they were read.
            if ((Get-Command ConvertFrom-Json).Parameters.ContainsKey('DateKind')) {
                $existing = $text | ConvertFrom-Json -DateKind String
            } else {
                $existing = $text | ConvertFrom-Json
            }
        } catch { throw "$registryPath is not valid JSON; fix or remove it. $($_.Exception.Message)" }
        if ($existing.PSObject.Properties['version'] -and $existing.version -gt $RegistryFormatVersion) {
            throw "$registryPath has format version $($existing.version); this script writes version $RegistryFormatVersion. Update the repository."
        }
        foreach ($property in $existing.PSObject.Properties) { $registry[$property.Name] = $property.Value }
        $registry.version = $RegistryFormatVersion
        if ($null -eq $registry.apps) { $registry.apps = @() }
    }
}

function Test-ThisRecord($Record) {
    # Records from other writers may lack fields; StrictMode refuses a missing property.
    $fields = @{}
    foreach ($property in $Record.PSObject.Properties) { $fields[$property.Name] = $property.Value }
    return ($fields['app'] -eq $QarosAppName -and $fields['platform'] -eq $Platform -and $fields['source'] -eq 'local')
}

$others = @($registry.apps | Where-Object { -not (Test-ThisRecord $_) })
$removed = @($registry.apps).Count - $others.Count

if ($Unregister) {
    if ($removed -eq 0) {
        Write-Host "No local $QarosAppName registration for $Platform in $registryPath."
        return
    }
    $registry.apps = $others
    Write-QarosFileAtomically $registryPath ($registry | ConvertTo-Json -Depth 8)
    Write-Host "Unregistered the local $QarosAppName for $Platform from $registryPath."
    return
}

if (-not $Executable) {
    $Executable = Join-Path $repoRoot (Join-Path 'target' (Join-Path 'release' (Get-QarosExecutableName $QarosAppName $Platform)))
}
if (-not (Test-Path -LiteralPath $Executable -PathType Leaf)) {
    throw "$Executable does not exist. Build the viewer first (cargo build --release -p qaros-rerun) or pass -Executable."
}
$Executable = (Resolve-Path -LiteralPath $Executable).Path

if (-not $AppVersion) { $AppVersion = Get-QarosRerunVersion $repoRoot }
if (-not $CapiHeaderSha256) {
    $CapiHeaderSha256 = Get-QarosCapiHeaderSha256 (Get-QarosCapiHeaderPath (Get-QarosCargoMetadata $repoRoot (Get-QarosTargetTriple $Platform)))
}
$CapiHeaderSha256 = $CapiHeaderSha256.ToLowerInvariant()
if ($CapiHeaderSha256 -notmatch '^[0-9a-f]{64}$') { throw "'$CapiHeaderSha256' is not a SHA-256 in hex." }

$record = [ordered]@{
    app                 = $QarosAppName
    platform            = $Platform
    source              = 'local'
    executable          = $Executable
    app_version         = $AppVersion
    c_api_header_sha256 = $CapiHeaderSha256
    registered_at       = [DateTime]::UtcNow.ToString("yyyy-MM-dd'T'HH:mm:ss'Z'", [System.Globalization.CultureInfo]::InvariantCulture)
}
$registry.apps = @($others) + @([pscustomobject]$record)
Write-QarosFileAtomically $registryPath ($registry | ConvertTo-Json -Depth 8)

$verb = if ($removed -gt 0) { 'Updated' } else { 'Registered' }
Write-Host "$verb $QarosAppName $AppVersion ($Platform, local) in $registryPath"
Write-Host "  executable: $Executable"
Write-Host "  C API header sha256: $CapiHeaderSha256"
