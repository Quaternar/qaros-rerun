# Helpers shared by package-release.ps1 and register-qaros-app.ps1. Dot-source it.
# Runs on Windows PowerShell 5.1 and on PowerShell 7 (Windows and Linux).

Set-StrictMode -Version 3.0

$script:QarosAppName = 'qaros-rerun'

function Get-QarosRepoRoot {
    return (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
}

function Test-QarosIsWindows {
    # $IsWindows exists only on PowerShell 6 and later; Windows PowerShell 5.1 runs on Windows only.
    if ($PSVersionTable.PSEdition -eq 'Desktop') { return $true }
    return [bool]$IsWindows
}

# The platform names of the release assets: windows-x64, linux-x64 (and -arm64 where it applies).
function Get-QarosPlatform {
    $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    $cpu = switch ($arch) {
        'X64' { 'x64' }
        'Arm64' { 'arm64' }
        default { throw "Unsupported CPU architecture: $arch" }
    }
    if (Test-QarosIsWindows) { return "windows-$cpu" }
    if ($IsLinux) { return "linux-$cpu" }
    if ($IsMacOS) { return "macos-$cpu" }
    throw 'Unsupported operating system.'
}

function Get-QarosExecutableName([string]$Name, [string]$Platform) {
    if ($Platform -like 'windows-*') { return "$Name.exe" }
    return $Name
}

# `cargo metadata` of the workspace, for the target triple when one is given.
function Get-QarosCargoMetadata([string]$RepoRoot, [string]$TargetTriple) {
    $arguments = @('metadata', '--format-version', '1', '--locked', '--manifest-path', (Join-Path $RepoRoot 'Cargo.toml'))
    if ($TargetTriple) { $arguments += @('--filter-platform', $TargetTriple) }
    $json = (& cargo @arguments) -join "`n"
    if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed with exit code $LASTEXITCODE" }
    # Crate feature maps have keys that differ only in case ("USB" and "usb"), which plain
    # ConvertFrom-Json refuses. Both parsers below return dictionaries, read with $x.key.
    if ($PSVersionTable.PSEdition -eq 'Desktop') {
        Add-Type -AssemblyName System.Web.Extensions
        $serializer = New-Object System.Web.Script.Serialization.JavaScriptSerializer
        $serializer.MaxJsonLength = [int]::MaxValue
        $serializer.RecursionLimit = 1024
        return $serializer.DeserializeObject($json)
    }
    return ($json | ConvertFrom-Json -AsHashtable)
}

# The Rust target triple of a release platform.
function Get-QarosTargetTriple([string]$Platform) {
    $triples = @{
        'windows-x64'   = 'x86_64-pc-windows-msvc'
        'windows-arm64' = 'aarch64-pc-windows-msvc'
        'linux-x64'     = 'x86_64-unknown-linux-gnu'
        'linux-arm64'   = 'aarch64-unknown-linux-gnu'
        'macos-x64'     = 'x86_64-apple-darwin'
        'macos-arm64'   = 'aarch64-apple-darwin'
    }
    if (-not $triples.ContainsKey($Platform)) { throw "Unknown platform '$Platform'; expected one of $(($triples.Keys | Sort-Object) -join ', ')." }
    return $triples[$Platform]
}

# qar_streaming.h, the QAROS C API header, from the qaros revision pinned in Cargo.toml.
# Cargo checks the qaros repository out as a whole, so the header sits next to the Rust binding.
function Get-QarosCapiHeaderPath($Metadata) {
    $qaros = @($Metadata.packages | Where-Object {
            $_.name -eq 'qaros' -and $_.source -and $_.source -like 'git+https://github.com/Quaternar/qaros*'
        })
    if ($qaros.Count -eq 0) { throw 'The qaros package is not in cargo metadata; is it still a git dependency in Cargo.toml?' }
    # <checkout>/qar-streaming-rust/crates/qaros/Cargo.toml
    $checkout = Split-Path (Split-Path (Split-Path (Split-Path $qaros[0].manifest_path)))
    $header = Join-Path $checkout (Join-Path 'qar-streaming-c' (Join-Path 'include' 'qar_streaming.h'))
    if (-not (Test-Path -LiteralPath $header -PathType Leaf)) {
        throw "The QAROS C API header is not at $header; run 'cargo fetch' first."
    }
    return $header
}

# SHA-256 of the header with CRLF line endings read as LF, so a checkout with Windows line
# endings hashes the same as one with Unix line endings. Lower-case hex.
function Get-QarosCapiHeaderSha256([string]$HeaderPath) {
    # Latin-1 maps every byte to one character and back, so only CR LF pairs change.
    $latin1 = [System.Text.Encoding]::GetEncoding(28591)
    $text = $latin1.GetString([System.IO.File]::ReadAllBytes($HeaderPath)).Replace("`r`n", "`n")
    return Get-QarosSha256OfBytes $latin1.GetBytes($text)
}

function Get-QarosSha256OfBytes([byte[]]$Bytes) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        return (($sha.ComputeHash($Bytes) | ForEach-Object { $_.ToString('x2') }) -join '')
    } finally {
        $sha.Dispose()
    }
}

function Get-QarosFileSha256([string]$Path) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    $stream = [System.IO.File]::OpenRead($Path)
    try {
        return (($sha.ComputeHash($stream) | ForEach-Object { $_.ToString('x2') }) -join '')
    } finally {
        $stream.Dispose()
        $sha.Dispose()
    }
}

# [workspace.package] version in Cargo.toml.
function Get-QarosWorkspaceVersion([string]$RepoRoot) {
    $inWorkspacePackage = $false
    foreach ($line in [System.IO.File]::ReadAllLines((Join-Path $RepoRoot 'Cargo.toml'))) {
        if ($line -match '^\s*\[(.+)\]\s*$') { $inWorkspacePackage = ($Matches[1] -eq 'workspace.package'); continue }
        if ($inWorkspacePackage -and $line -match '^\s*version\s*=\s*"([^"]+)"') { return $Matches[1] }
    }
    throw 'No [workspace.package] version in Cargo.toml.'
}

# The last released X.Y.Z, from VERSION at the repository root.
function Get-QarosReleasedVersion([string]$RepoRoot) {
    $released = ([System.IO.File]::ReadAllText((Join-Path $RepoRoot 'VERSION'))).Trim()
    if ($released -notmatch '^\d+\.\d+\.\d+$') { throw "VERSION holds '$released', expected X.Y.Z." }
    return $released
}

# X.Y.(Z+1): the core of every development and pull request version after release X.Y.Z.
function Get-QarosNextPatch([string]$Released) {
    $parts = $Released.Split('.')
    return "$($parts[0]).$($parts[1]).$([int64]$parts[2] + 1)"
}

# The X.Y.Z a full version must carry: VERSION for a release (or a candidate), the next patch for
# a -dev or -pr version.
function Get-QarosExpectedCore([string]$Version, [string]$Released) {
    if ($Version -match '^[^-+]+-(dev|pr)\.') { return Get-QarosNextPatch $Released }
    return $Released
}

# A release tag: vX.Y.Z or vX.Y.Z-rc.N.
function Test-QarosReleaseTag([string]$Tag) {
    return ($Tag -match '^v\d+\.\d+\.\d+(-rc\.\d+)?$')
}

function Invoke-QarosGit([string]$RepoRoot, [string[]]$Arguments) {
    # Windows PowerShell turns a native command's stderr into an error record, which 'Stop'
    # makes fatal; a failing git call here is an answer, not an error.
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $output = & git -C $RepoRoot @Arguments 2>$null
        $exitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previous
    }
    if ($exitCode -ne 0) { return $null }
    $text = (@($output) -join "`n").Trim()
    if ($text) { return $text }
    return $null
}

# The full version, by the same rules as crates/build_version.rs. VERSION holds the last release:
# - HEAD tagged vX.Y.Z or vX.Y.Z-rc.N: that version;
# - otherwise X.Y.(Z+1)-dev.<commits since the last v* tag>+<8-digit commit>, the next patch;
# - no git: X.Y.(Z+1)-dev.0+unknown.
# A release tag whose X.Y.Z differs from VERSION is an error.
function Get-QarosRerunVersion([string]$RepoRoot) {
    $released = Get-QarosReleasedVersion $RepoRoot
    $next = Get-QarosNextPatch $released
    $tags = Invoke-QarosGit $RepoRoot @('tag', '--points-at', 'HEAD', '--list', 'v[0-9]*')
    $tag = $null
    if ($tags) { $tag = $tags -split "`n" | ForEach-Object { $_.Trim() } | Where-Object { Test-QarosReleaseTag $_ } | Select-Object -First 1 }
    if ($tag) {
        $version = $tag.Substring(1)
    } else {
        $commit = Invoke-QarosGit $RepoRoot @('rev-parse', '--short=8', 'HEAD')
        if ($commit) {
            $lastTag = Invoke-QarosGit $RepoRoot @('describe', '--tags', '--abbrev=0', '--match', 'v[0-9]*')
            $range = if ($lastTag) { "$lastTag..HEAD" } else { 'HEAD' }
            $count = Invoke-QarosGit $RepoRoot @('rev-list', '--count', $range)
            if (-not $count) { $count = '0' }
            $version = "$next-dev.$count+$commit"
        } else {
            $version = "$next-dev.0+unknown"
        }
    }
    $core = ($version -split '[-+]', 2)[0]
    if ($core -ne (Get-QarosExpectedCore $version $released)) { throw "Version $version does not match VERSION ($released); a release tag must name the version in VERSION." }
    return $version
}

# UTF-8 without a byte order mark, LF line endings, written to a temporary file next to the
# target and then moved over it, so a reader never sees a half-written file.
function Write-QarosFileAtomically([string]$Path, [string]$Content) {
    $directory = Split-Path -Parent $Path
    if (-not (Test-Path -LiteralPath $directory)) { New-Item -ItemType Directory -Force -Path $directory | Out-Null }
    $temporary = Join-Path $directory ('.' + (Split-Path -Leaf $Path) + '.' + [System.Guid]::NewGuid().ToString('N') + '.tmp')
    $text = $Content.Replace("`r`n", "`n")
    if (-not $text.EndsWith("`n")) { $text += "`n" }
    [System.IO.File]::WriteAllText($temporary, $text, (New-Object System.Text.UTF8Encoding($false)))
    try {
        if (Test-Path -LiteralPath $Path) {
            [System.IO.File]::Replace($temporary, $Path, [NullString]::Value)
        } else {
            [System.IO.File]::Move($temporary, $Path)
        }
    } catch {
        Remove-Item -LiteralPath $temporary -Force -ErrorAction SilentlyContinue
        throw
    }
}
