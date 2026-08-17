# SPDX-License-Identifier: MIT
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$SourceDirectory,

    [Parameter(Mandatory = $true)]
    [string]$DestinationArchive
)

$ErrorActionPreference = "Stop"
$source = (Resolve-Path -LiteralPath $SourceDirectory).Path
$destination = [System.IO.Path]::GetFullPath($DestinationArchive)

if (-not (Test-Path -LiteralPath (Join-Path $source "DSHLauncher.exe") -PathType Leaf)) {
    throw "Packaged launcher not found in $source"
}

$destinationParent = Split-Path -Parent $destination
New-Item -ItemType Directory -Force -Path $destinationParent | Out-Null
if (Test-Path -LiteralPath $destination) {
    Remove-Item -LiteralPath $destination -Force
}

# Passing the directory itself (not its contents) preserves the top-level
# DSHLauncher folder when the user extracts the archive.
Compress-Archive -LiteralPath $source -DestinationPath $destination -CompressionLevel Optimal
Write-Host "Created $destination"
