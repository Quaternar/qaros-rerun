<#
.SYNOPSIS
Prints the full version of qaros-rerun, checks the version files, or checks a release tag.

.DESCRIPTION
VERSION at the repository root holds the last released X.Y.Z; CI writes it when it releases.
The full version is derived:

    HEAD tagged vX.Y.Z           X.Y.Z
    HEAD tagged vX.Y.Z-rc.N      X.Y.Z-rc.N
    anything else                X.Y.(Z+1)-dev.<commits since the last v* tag>+<commit>
    a pull request build         X.Y.(Z+1)-pr.<number> (the release workflow)

The build (crates/build_version.rs) applies the same rules, so the viewer reports the version
this script prints.

Without arguments, prints the full version. -Check also fails when the [workspace.package]
version in Cargo.toml differs from VERSION (CI runs it). -Tag fails unless the tag is vX.Y.Z or
vX.Y.Z-rc.N with the X.Y.Z of VERSION, and prints the version it names (the release workflow
runs it). -NextPatch prints X.Y.(Z+1), the X.Y.Z of the dev and pull request versions.

Runs on Windows PowerShell 5.1 and PowerShell 7.

.EXAMPLE
./scripts/version.ps1 -Check

.EXAMPLE
./scripts/version.ps1 -Tag v0.1.0
#>
[CmdletBinding()]
param(
    [switch]$Check,
    [string]$Tag,
    [switch]$NextPatch
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'qaros-common.ps1')

$repoRoot = Get-QarosRepoRoot
$released = Get-QarosReleasedVersion $repoRoot

if ($Check) {
    $cargo = Get-QarosWorkspaceVersion $repoRoot
    if ($cargo -ne $released) {
        throw "Cargo.toml has version $cargo, VERSION has $released; keep them equal."
    }
}

if ($Tag) {
    if (-not (Test-QarosReleaseTag $Tag)) { throw "Tag '$Tag' is not vX.Y.Z or vX.Y.Z-rc.N." }
    $version = $Tag.Substring(1)
    $core = ($version -split '-', 2)[0]
    if ($core -ne $released) { throw "Tag $Tag names $core, VERSION has $released; a release tag goes on the commit that sets VERSION." }
    Write-Output $version
    return
}

if ($NextPatch) {
    Write-Output (Get-QarosNextPatch $released)
    return
}

Write-Output (Get-QarosRerunVersion $repoRoot)
