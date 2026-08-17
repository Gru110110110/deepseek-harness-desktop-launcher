# SPDX-License-Identifier: MIT
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Executable
)

$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($env:WINDOWS_SIGNING_CERT_BASE64)) {
    Write-Host "WINDOWS_SIGNING_CERT_BASE64 is not configured; leaving this build unsigned."
    exit 0
}
if ([string]::IsNullOrWhiteSpace($env:WINDOWS_SIGNING_CERT_PASSWORD)) {
    throw "WINDOWS_SIGNING_CERT_PASSWORD is required when a signing certificate is configured."
}

$resolvedExecutable = (Resolve-Path -LiteralPath $Executable).Path
$kitsRoot = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin"
$signTool = Get-ChildItem -Path $kitsRoot -Filter "signtool.exe" -File -Recurse |
    Where-Object { $_.FullName -match "\\x64\\signtool\.exe$" } |
    Sort-Object FullName -Descending |
    Select-Object -First 1
if ($null -eq $signTool) {
    throw "SignTool.exe was not found in the Windows SDK."
}

$temporaryRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }
$certificatePath = Join-Path $temporaryRoot ("dsh-signing-{0}.pfx" -f [guid]::NewGuid())
$timestampUrl = if ($env:WINDOWS_TIMESTAMP_URL) {
    $env:WINDOWS_TIMESTAMP_URL
} else {
    "http://timestamp.digicert.com"
}

try {
    [IO.File]::WriteAllBytes(
        $certificatePath,
        [Convert]::FromBase64String($env:WINDOWS_SIGNING_CERT_BASE64)
    )

    & $signTool.FullName sign `
        /fd SHA256 `
        /td SHA256 `
        /tr $timestampUrl `
        /f $certificatePath `
        /p $env:WINDOWS_SIGNING_CERT_PASSWORD `
        /d "DSH Launcher" `
        $resolvedExecutable
    if ($LASTEXITCODE -ne 0) {
        throw "SignTool failed to sign the launcher."
    }

    & $signTool.FullName verify /pa /v $resolvedExecutable
    if ($LASTEXITCODE -ne 0) {
        throw "SignTool could not verify the launcher signature."
    }
} finally {
    if (Test-Path -LiteralPath $certificatePath) {
        Remove-Item -LiteralPath $certificatePath -Force
    }
}
