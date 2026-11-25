<#
.SYNOPSIS
Authenticode-signs the Windows executables of a release with Azure Artifact Signing.

.DESCRIPTION
Runs in the release workflow's signing job (environment release-signing) after azure/login, so the
Artifact Signing client takes the job's Azure CLI login: no certificate or secret is stored anywhere.
Signs each file with signtool, the Artifact Signing dlib and Microsoft's time-stamping authority,
then checks that Windows reports every signature as Valid.

Without -Account the script signs nothing and says so: a release built before Artifact Signing is
set up for this repository ships unsigned executables, as before. See docs/releases.md#code-signing.

.EXAMPLE
./scripts/sign-windows.ps1 -Files target/dist/qaros-rerun.exe, target/dist/log-test-scene.exe `
    -Endpoint https://weu.codesigning.azure.net -Account quaternar -CertificateProfile qaros-public
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string[]]$Files,

    # The Artifact Signing endpoint of the account's region, e.g. https://weu.codesigning.azure.net.
    [string]$Endpoint,

    # The Artifact Signing account. Empty: sign nothing.
    [string]$Account,

    # The certificate profile of the account.
    [string]$CertificateProfile,

    # Identifies the signing request in the account's logs.
    [string]$CorrelationId = 'qaros-rerun',

    # The Microsoft.ArtifactSigning.Client NuGet package that holds the dlib.
    [string]$ClientVersion = '1.0.146',

    [string]$TimestampUrl = 'http://timestamp.acs.microsoft.com'
)

$ErrorActionPreference = 'Stop'

foreach ($file in $Files) {
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { throw "$file is missing." }
}

if (-not $Account) {
    Write-Host '::warning::Artifact Signing is not set up for this repository: the Windows executables stay unsigned (docs/releases.md#code-signing).'
    return
}
if (-not $Endpoint -or -not $CertificateProfile) { throw 'Artifact Signing needs -Endpoint and -CertificateProfile with -Account.' }

function Find-SignTool {
    $roots = @(${env:ProgramFiles(x86)}, $env:ProgramFiles) | Where-Object { $_ }
    foreach ($root in $roots) {
        $found = Get-ChildItem -Path (Join-Path $root 'Windows Kits\10\bin') -Filter signtool.exe -Recurse -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match '\\x64\\' } | Sort-Object FullName -Descending | Select-Object -First 1
        if ($found) { return $found.FullName }
    }
    $onPath = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($onPath) { return $onPath.Source }
    throw 'signtool.exe not found: install the Windows SDK.'
}

$work = Join-Path ([System.IO.Path]::GetTempPath()) "qaros-rerun-signing-$PID"
New-Item -ItemType Directory -Force -Path $work | Out-Null
try {
    $package = Join-Path $work 'client.zip'
    Invoke-WebRequest -Uri "https://www.nuget.org/api/v2/package/Microsoft.ArtifactSigning.Client/$ClientVersion" -OutFile $package
    Expand-Archive -Path $package -DestinationPath (Join-Path $work 'client') -Force
    $dlib = Get-ChildItem -Path (Join-Path $work 'client') -Recurse -Filter 'Azure.CodeSigning.Dlib.dll' |
        Where-Object { $_.FullName -match '\\x64\\' } | Select-Object -First 1
    if (-not $dlib) { throw "No x64 Azure.CodeSigning.Dlib.dll in Microsoft.ArtifactSigning.Client $ClientVersion." }

    # Only the Azure CLI login of azure/login is tried, so a missing login fails at once.
    $metadata = [ordered]@{
        Endpoint               = $Endpoint
        CodeSigningAccountName = $Account
        CertificateProfileName = $CertificateProfile
        CorrelationId          = $CorrelationId
        ExcludeCredentials     = @(
            'EnvironmentCredential', 'WorkloadIdentityCredential', 'ManagedIdentityCredential',
            'SharedTokenCacheCredential', 'VisualStudioCredential', 'VisualStudioCodeCredential',
            'AzurePowerShellCredential', 'AzureDeveloperCliCredential', 'InteractiveBrowserCredential')
    }
    $metadataPath = Join-Path $work 'metadata.json'
    $metadata | ConvertTo-Json | Set-Content -Path $metadataPath -Encoding utf8

    $signTool = Find-SignTool
    Write-Host "Signing $($Files.Count) files with $signTool"
    & $signTool sign /v /fd SHA256 /tr $TimestampUrl /td SHA256 /dlib $dlib.FullName /dmdf $metadataPath @Files
    if ($LASTEXITCODE -ne 0) { throw "signtool failed with exit code $LASTEXITCODE." }

    foreach ($file in $Files) {
        $signature = Get-AuthenticodeSignature -LiteralPath $file
        if ($signature.Status -ne 'Valid') {
            throw "$file is $($signature.Status) after signing: $($signature.StatusMessage)"
        }
        Write-Host "$file signed by $($signature.SignerCertificate.Subject), timestamp $($signature.TimeStamperCertificate.Subject)"
    }
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}
