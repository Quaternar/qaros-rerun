<#
.SYNOPSIS
Packages a release build of the qaros-rerun viewer as a release asset and records it in manifest.json.

.DESCRIPTION
Run after the dist build on the platform being packaged (the .pdb files the Windows build writes
next to the executables stay out of the zip):

    cargo build --profile dist -p qaros-rerun                                          # Linux
    cargo build --profile dist -p qaros-rerun --config .cargo/dist-windows.toml        # Windows

Writes to -OutDir:

- qaros-rerun-<version>-<platform>.zip: qaros-rerun and log-test-scene, LICENSE, the third-party
  licenses of every crate compiled into them, a README and BUILD.txt, at the root of the zip;
- manifest.json: the release version and one entry per asset with its platform, SHA-256, size,
  the viewer version and its compatibility: min_qaros_version (the QAROS version of the C API
  header the viewer is built against) and c_api_header_sha256. A manifest.json already in
  -OutDir for the same release gets this platform's entry added or replaced.

With -MergeManifests, merges the manifests of several platforms (the release workflow builds
each platform in a job of its own) into -OutDir/manifest.json instead.

Runs on Windows PowerShell 5.1 and PowerShell 7 (Windows and Linux).

.EXAMPLE
./scripts/package-release.ps1 -OutDir out/release

.EXAMPLE
./scripts/package-release.ps1 -MergeManifests dist/windows-x64/manifest.json, dist/linux-x64/manifest.json -OutDir dist
#>
[CmdletBinding(DefaultParameterSetName = 'Package')]
param(
    # The viewer version: X.Y.Z, X.Y.Z-rc.N, X.Y.Z-dev.N+commit, or X.Y.Z-pr.N for a pull
    # request build. Default: derived from VERSION
    # and the git tags (scripts/version.ps1), as the build derives the version the viewer reports.
    [Parameter(ParameterSetName = 'Package')]
    [string]$Version,

    # windows-x64, linux-x64, ... Default: this machine's.
    [Parameter(ParameterSetName = 'Package')]
    [string]$Platform,

    # The cargo profile the binaries were built with. Default: dist.
    [Parameter(ParameterSetName = 'Package')]
    [string]$CargoProfile = 'dist',

    # Where cargo put the binaries. Default: target/<CargoProfile> in the repository.
    [Parameter(ParameterSetName = 'Package')]
    [string]$TargetDir,

    # The commit the binaries were built from, for BUILD.txt. Default: git rev-parse HEAD.
    [Parameter(ParameterSetName = 'Package')]
    [string]$Commit,

    [Parameter(ParameterSetName = 'Merge', Mandatory = $true)]
    [string[]]$MergeManifests,

    [string]$OutDir = 'out/release'
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'qaros-common.ps1')

$ManifestFormatVersion = 1
$VersionPattern = '^\d+\.\d+\.\d+(-rc\.\d+|-pr\.\d+|-dev\.\d+\+[0-9a-z]+)?$'
$Binaries = @('qaros-rerun', 'log-test-scene')

$repoRoot = Get-QarosRepoRoot
if (-not [System.IO.Path]::IsPathRooted($OutDir)) { $OutDir = Join-Path (Get-Location).Path $OutDir }
$OutDir = [System.IO.Path]::GetFullPath($OutDir)
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$manifestPath = Join-Path $OutDir 'manifest.json'

# manifest.json: { version, release, package, assets: [ { name, sha256, size, platform,
# executable, app_version, min_qaros_version, c_api_header_sha256 } ] }, the field names of the
# qaros-ros-rviz release manifest.
function ConvertTo-ManifestJson($Manifest) {
    return ($Manifest | ConvertTo-Json -Depth 8)
}

function New-Manifest([string]$Release) {
    return [ordered]@{
        version = $ManifestFormatVersion
        release = $Release
        package = $QarosAppName
        assets  = @()
    }
}

function Read-Manifest([string]$Path) {
    $manifest = [System.IO.File]::ReadAllText($Path) | ConvertFrom-Json
    if ($manifest.version -ne $ManifestFormatVersion) { throw "$Path has manifest format version $($manifest.version), expected $ManifestFormatVersion." }
    if ($manifest.package -ne $QarosAppName) { throw "$Path is for package '$($manifest.package)', expected '$QarosAppName'." }
    return $manifest
}

# Adds each asset to $Manifest, replacing an entry for the same platform.
function Add-ManifestAssets($Manifest, [string]$Release, $Assets, [string]$SourceName) {
    if ($Manifest.release -ne $Release) {
        throw "$SourceName is for release '$Release', the manifest being written for '$($Manifest.release)'. Clean the output folder when packaging another version."
    }
    foreach ($asset in $Assets) {
        $Manifest.assets = @($Manifest.assets | Where-Object { $_.platform -ne $asset.platform }) + @(
            [ordered]@{
                name                = $asset.name
                sha256              = $asset.sha256
                size                = [long]$asset.size
                platform            = $asset.platform
                executable          = $asset.executable
                app_version         = $asset.app_version
                min_qaros_version   = $asset.min_qaros_version
                c_api_header_sha256 = $asset.c_api_header_sha256
            })
    }
    $Manifest.assets = @($Manifest.assets | Sort-Object { $_.platform })
}

if ($PSCmdlet.ParameterSetName -eq 'Merge') {
    $merged = $null
    foreach ($path in $MergeManifests) {
        $source = Read-Manifest (Resolve-Path -LiteralPath $path).Path
        if ($null -eq $merged) { $merged = New-Manifest $source.release }
        foreach ($asset in $source.assets) {
            $existing = @($merged.assets | Where-Object { $_.platform -eq $asset.platform })
            if ($existing.Count -gt 0 -and $existing[0].sha256 -ne $asset.sha256) {
                throw "Two manifests list different $($asset.platform) assets ($($existing[0].name), $($asset.name))."
            }
        }
        Add-ManifestAssets $merged $source.release $source.assets $path
    }
    Write-QarosFileAtomically $manifestPath (ConvertTo-ManifestJson $merged)
    Write-Host "Wrote $manifestPath with $(@($merged.assets).Count) assets."
    return
}

# --- Package ---------------------------------------------------------------------------------

if ($Version) {
    $Version = $Version.Trim()
    if ($Version.StartsWith('v')) { $Version = $Version.Substring(1) }
    $planned = Get-QarosPlannedVersion $repoRoot
    if (($Version -split '[-+]', 2)[0] -ne $planned) { throw "Version $Version does not match VERSION ($planned)." }
} else {
    $Version = Get-QarosRerunVersion $repoRoot
}
if ($Version -notmatch $VersionPattern) { throw "Version '$Version' is not X.Y.Z, X.Y.Z-rc.N, X.Y.Z-pr.N or X.Y.Z-dev.N+commit." }
if (-not $Platform) { $Platform = Get-QarosPlatform }
$triple = Get-QarosTargetTriple $Platform

if (-not $TargetDir) { $TargetDir = Join-Path $repoRoot (Join-Path 'target' $CargoProfile) }
if (-not $Commit) {
    $Commit = (& git -C $repoRoot rev-parse HEAD 2>$null)
    if ($LASTEXITCODE -ne 0 -or -not $Commit) { $Commit = 'unknown' }
}

$executables = foreach ($binary in $Binaries) {
    $path = Join-Path $TargetDir (Get-QarosExecutableName $binary $Platform)
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "$path is missing; build it first with: cargo build --profile $CargoProfile -p qaros-rerun (on Windows add --config .cargo/dist-windows.toml)"
    }
    $path
}

if (Test-Path -LiteralPath $manifestPath) {
    # Refuse before writing anything when the output folder holds another release.
    $earlier = Read-Manifest $manifestPath
    if ($earlier.release -ne $Version) {
        throw "$manifestPath is for release $($earlier.release), this package is $Version. Clean the output folder when packaging another version."
    }
}

Write-Host "Reading cargo metadata for $triple ..."
$metadata = Get-QarosCargoMetadata $repoRoot $triple
$headerPath = Get-QarosCapiHeaderPath $metadata
$headerSha256 = Get-QarosCapiHeaderSha256 $headerPath
$qarosCheckout = Split-Path (Split-Path (Split-Path $headerPath))
$qarosPackage = @($metadata.packages | Where-Object { $_.name -eq 'qaros' -and $_.source -like 'git+*' })[0]
$qarosRevision = ($qarosPackage.source -split '#', 2)[-1]
# The QAROS version of the header: VERSION of the pinned qaros revision.
$minQarosVersion = ([System.IO.File]::ReadAllText((Join-Path $qarosCheckout 'VERSION'))).Trim()
if ($minQarosVersion -notmatch '^\d+\.\d+\.\d+') { throw "The pinned qaros revision has VERSION '$minQarosVersion'." }

# Third-party licenses: every package in the normal dependency graph of qaros-rerun for the
# target, apart from the workspace's own crates (LICENSE covers those).
function New-ThirdPartyLicenses($Metadata, [string]$RepoRoot) {
    $packagesById = @{}
    foreach ($package in $Metadata.packages) { $packagesById[$package.id] = $package }
    $nodesById = @{}
    foreach ($node in $Metadata.resolve.nodes) { $nodesById[$node.id] = $node }
    $workspaceMembers = @{}
    foreach ($id in $Metadata.workspace_members) { $workspaceMembers[$id] = $true }

    $rootId = @($Metadata.workspace_members | Where-Object { $packagesById[$_].name -eq $QarosAppName })[0]
    $seen = @{ $rootId = $true }
    $queue = New-Object System.Collections.Generic.Queue[string]
    $queue.Enqueue($rootId)
    while ($queue.Count -gt 0) {
        $node = $nodesById[$queue.Dequeue()]
        foreach ($dep in $node.deps) {
            $normal = @($dep.dep_kinds | Where-Object { $null -eq $_.kind }).Count -gt 0
            if ($normal -and -not $seen.ContainsKey($dep.pkg)) {
                $seen[$dep.pkg] = $true
                $queue.Enqueue($dep.pkg)
            }
        }
    }

    $licenseName = '^(licen[cs]e|copying|notice|copyright|unlicense|ofl|ufl)([-_.].*)?$|[-_.]licen[cs]e([-_.].*)?$'
    $skippedDirectory = '(^|[\\/])(tests?|benches|examples|fuzz|target|\.git)([\\/]|$)'
    $rerunLicenses = @('LICENSE-MIT', 'LICENSE-APACHE') | ForEach-Object { Join-Path $RepoRoot (Join-Path 'patches' (Join-Path 're_renderer' $_)) }
    $utf8 = New-Object System.Text.UTF8Encoding($false)

    $texts = [ordered]@{}   # sha256 -> @{ Text; Users }
    $rows = New-Object System.Collections.Generic.List[object]
    $packages = @($seen.Keys | Where-Object { -not $workspaceMembers.ContainsKey($_) } | ForEach-Object { $packagesById[$_] } |
            Sort-Object { $_.name }, { $_.version })
    foreach ($package in $packages) {
        $directory = Split-Path -Parent $package.manifest_path
        $files = @(Get-ChildItem -LiteralPath $directory -Recurse -Depth 3 -File -ErrorAction SilentlyContinue | Where-Object {
                $relative = $_.FullName.Substring($directory.Length)
                $_.Name -match $licenseName -and $_.Name -notmatch '\.(rs|toml|json|ya?ml|py|sh|ps1|c|h|cpp)$' -and
                $relative -notmatch $skippedDirectory -and $_.Length -lt 262144
            } | Sort-Object FullName)
        if ($package.license_file) {
            $declared = Join-Path $directory $package.license_file
            if ((Test-Path -LiteralPath $declared -PathType Leaf) -and -not ($files | Where-Object { $_.FullName -eq (Resolve-Path -LiteralPath $declared).Path })) {
                $files = @(Get-Item -LiteralPath $declared) + $files
            }
        }
        $note = ''
        if ($files.Count -eq 0 -and ($package.name -eq 'rerun' -or $package.name -like 're_*') -and "$($package.repository)" -like '*rerun-io/rerun*') {
            # Rerun publishes its crates without license files; its repository's are in patches/.
            $files = @($rerunLicenses | ForEach-Object { Get-Item -LiteralPath $_ })
            $note = ' (the license files of the Rerun repository)'
        }
        if ($files.Count -eq 0 -and "$($package.source)" -like 'git+*') {
            # A crate from a git checkout: the license sits at the repository root, above it.
            $parent = $directory
            for ($level = 0; $level -lt 4 -and $files.Count -eq 0; $level++) {
                $parent = Split-Path -Parent $parent
                if (-not $parent) { break }
                $files = @(Get-ChildItem -LiteralPath $parent -File -ErrorAction SilentlyContinue | Where-Object { $_.Name -match $licenseName } | Sort-Object FullName)
            }
            if ($files.Count -gt 0) { $note = ' (from the root of its git repository)' }
        }
        if ($files.Count -eq 0 -and "$($package.license)" -match 'Apache-2\.0') {
            # The Apache License names no copyright holder, so its standard text stands in.
            $files = @(Get-Item -LiteralPath $rerunLicenses[1])
            $note = ' (the standard Apache-2.0 text; the package ships no license file)'
        }
        $ids = foreach ($file in $files) {
            $text = ([System.IO.File]::ReadAllText($file.FullName, $utf8)).Replace("`r`n", "`n").Trim()
            $sha = Get-QarosSha256OfBytes ($utf8.GetBytes($text))
            if (-not $texts.Contains($sha)) {
                $texts[$sha] = @{ Number = $texts.Count + 1; Text = $text; Users = New-Object System.Collections.Generic.List[string] }
            }
            $texts[$sha].Users.Add("$($package.name) $($package.version)")
            "#$($texts[$sha].Number)"
        }
        $license = if ($package.license) { $package.license } else { 'not declared' }
        $textsColumn = if ($ids) { (@($ids) -join ', ') + $note } else { 'no license file in the published package; see its repository' }
        $rows.Add([pscustomobject]@{
                Name       = $package.name
                Version    = $package.version
                License    = $license
                Repository = if ($package.repository) { $package.repository } else { '' }
                Texts      = $textsColumn
            })
    }

    $builder = New-Object System.Text.StringBuilder
    [void]$builder.AppendLine('Third-party licenses of qaros-rerun')
    [void]$builder.AppendLine('===================================')
    [void]$builder.AppendLine()
    [void]$builder.AppendLine('qaros-rerun and log-test-scene contain the Rust crates listed below, compiled in')
    [void]$builder.AppendLine('statically, and the Rust standard library (MIT OR Apache-2.0,')
    [void]$builder.AppendLine('https://github.com/rust-lang/rust). Each crate is under the license its package')
    [void]$builder.AppendLine('declares; the license texts its package ships follow the list, each text once, with')
    [void]$builder.AppendLine('the crates that ship it. qaros-rerun itself is MIT licensed, see LICENSE.')
    [void]$builder.AppendLine()
    [void]$builder.AppendLine("Crates ($($rows.Count))")
    [void]$builder.AppendLine('------')
    [void]$builder.AppendLine()
    foreach ($row in $rows) {
        [void]$builder.AppendLine("$($row.Name) $($row.Version)")
        [void]$builder.AppendLine("    License: $($row.License)")
        if ($row.Repository) { [void]$builder.AppendLine("    Repository: $($row.Repository)") }
        [void]$builder.AppendLine("    License texts: $($row.Texts)")
    }
    foreach ($entry in $texts.Values) {
        [void]$builder.AppendLine()
        [void]$builder.AppendLine(('=' * 80))
        [void]$builder.AppendLine("License text #$($entry.Number)")
        [void]$builder.AppendLine("Shipped by: $($entry.Users -join ', ')")
        [void]$builder.AppendLine(('=' * 80))
        [void]$builder.AppendLine()
        [void]$builder.AppendLine($entry.Text)
    }
    return @{ Text = $builder.ToString(); Crates = $rows.Count; Texts = $texts.Count }
}

$baseName = "$QarosAppName-$Version-$Platform"
$zipPath = Join-Path $OutDir "$baseName.zip"
$staging = Join-Path $OutDir (Join-Path '.staging' $baseName)
if (Test-Path -LiteralPath $staging) { Remove-Item -LiteralPath $staging -Recurse -Force }
New-Item -ItemType Directory -Force -Path $staging | Out-Null

foreach ($path in $executables) { Copy-Item -LiteralPath $path -Destination $staging }
Copy-Item -LiteralPath (Join-Path $repoRoot 'LICENSE') -Destination $staging
Copy-Item -LiteralPath (Join-Path $repoRoot (Join-Path '.github' (Join-Path 'release' 'README.md'))) -Destination $staging

Write-Host 'Collecting third-party licenses ...'
$licenses = New-ThirdPartyLicenses $metadata $repoRoot
Write-QarosFileAtomically (Join-Path $staging 'THIRD-PARTY-LICENSES.txt') $licenses.Text
Write-Host "  $($licenses.Crates) crates, $($licenses.Texts) distinct license texts"

$rustc = (& rustc --version 2>$null)
# The Windows viewer links the C runtime dynamically and the package does not carry it: the QAROS
# setup installs the Visual C++ Redistributable system wide.
$vcRuntime = if ($Platform -like 'windows-*') {
    'not included; needs the Microsoft Visual C++ 2015-2022 Redistributable (x64), which the QAROS setup installs from QAROS 0.10.0'
} else {
    'not used'
}
Write-QarosFileAtomically (Join-Path $staging 'BUILD.txt') (@(
        "app: $QarosAppName",
        "version: $Version",
        "min qaros version: $minQarosVersion",
        "platform: $Platform ($triple)",
        "cargo profile: $CargoProfile",
        "commit: $Commit",
        "qaros revision: $qarosRevision",
        "C API header sha256: $headerSha256",
        "rustc: $rustc",
        "visual c++ runtime: $vcRuntime"
    ) -join "`n")

# The zip: forward slashes in entry names, every file at the root, executables marked
# executable for unzip on Linux (external attributes carry the Unix mode).
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
if (Test-Path -LiteralPath $zipPath) { Remove-Item -LiteralPath $zipPath -Force }
$executableNames = $Binaries | ForEach-Object { Get-QarosExecutableName $_ $Platform }
$zipStream = [System.IO.File]::Open($zipPath, [System.IO.FileMode]::CreateNew)
try {
    $archive = New-Object System.IO.Compression.ZipArchive($zipStream, [System.IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($file in (Get-ChildItem -LiteralPath $staging -File | Sort-Object Name)) {
            $entry = $archive.CreateEntry($file.Name, [System.IO.Compression.CompressionLevel]::Optimal)
            $entry.LastWriteTime = $file.LastWriteTime
            $mode = if ($executableNames -contains $file.Name) { 0x81ED } else { 0x81A4 }   # 0100755, 0100644
            if ($Platform -notlike 'windows-*') { $entry.ExternalAttributes = $mode -shl 16 }
            $source = [System.IO.File]::OpenRead($file.FullName)
            $target = $entry.Open()
            try { $source.CopyTo($target) } finally { $target.Dispose(); $source.Dispose() }
        }
    } finally {
        $archive.Dispose()
    }
} finally {
    $zipStream.Dispose()
}
Remove-Item -LiteralPath (Join-Path $OutDir '.staging') -Recurse -Force


$zipItem = Get-Item -LiteralPath $zipPath
$asset = [pscustomobject]@{
    name                = $zipItem.Name
    sha256              = Get-QarosFileSha256 $zipPath
    size                = $zipItem.Length
    platform            = $Platform
    executable          = Get-QarosExecutableName $QarosAppName $Platform
    app_version         = $Version
    min_qaros_version   = $minQarosVersion
    c_api_header_sha256 = $headerSha256
}
$manifest = New-Manifest $Version
if (Test-Path -LiteralPath $manifestPath) {
    $existing = Read-Manifest $manifestPath
    Add-ManifestAssets $manifest $existing.release $existing.assets $manifestPath
}
Add-ManifestAssets $manifest $Version @($asset) $zipItem.Name
Write-QarosFileAtomically $manifestPath (ConvertTo-ManifestJson $manifest)

Write-Host "Wrote $zipPath ($($asset.size) bytes, sha256 $($asset.sha256))"
Write-Host "Wrote $manifestPath (QAROS $minQarosVersion, C API header sha256 $headerSha256)"
