<#
.SYNOPSIS
  Builds the release assets from release binaries:
    OpenPen-X.Y.Z-x64.msi, OpenPen-X.Y.Z-x64.msi.sha256, OpenPen-X.Y.Z-x64-portable.zip

.DESCRIPTION
  The MSI comes from installer/build-installer.ps1 (WiX 5). The zip holds openpen.exe,
  openpen-cli.exe, skills/openpen/*, LICENSE and README.md at its root. The .sha256 file
  holds "<lowercase hex>  OpenPen-X.Y.Z-x64.msi" plus a newline (sha256sum format).
  The release workflow and local runs use this same script.

.EXAMPLE
  cargo build --release -p openpen -p openpen-cli
  ./scripts/package.ps1                      # version from Cargo.toml, output in target/dist
  ./scripts/package.ps1 -Version 0.1.1       # e.g. to test an in-place upgrade
#>
[CmdletBinding()]
param(
    # X.Y.Z (default: Cargo.toml [workspace.package] version).
    [string]$Version,
    [string]$BinDir,
    [string]$OutDir,
    # Only the zip (no WiX needed).
    [switch]$SkipMsi
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $Version) { $Version = & (Join-Path $PSScriptRoot 'version.ps1') }
if (-not $BinDir) { $BinDir = Join-Path $root 'target/release' }
if (-not $OutDir) { $OutDir = Join-Path $root 'target/dist' }
$BinDir = (Resolve-Path $BinDir).Path
New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path

$base = "OpenPen-$Version-x64"
$assets = @()

# Portable zip, entries with forward slashes at the archive root.
$zip = Join-Path $OutDir "$base-portable.zip"
Remove-Item -Force -ErrorAction SilentlyContinue $zip
$entries = [ordered]@{
    'openpen.exe'     = Join-Path $BinDir 'openpen.exe'
    'openpen-cli.exe' = Join-Path $BinDir 'openpen-cli.exe'
    'LICENSE'         = Join-Path $root 'LICENSE'
    'README.md'       = Join-Path $root 'README.md'
}
foreach ($f in Get-ChildItem -File (Join-Path $root 'skills/openpen') | Sort-Object Name) {
    $entries["skills/openpen/$($f.Name)"] = $f.FullName
}
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
$archive = [System.IO.Compression.ZipFile]::Open($zip, [System.IO.Compression.ZipArchiveMode]::Create)
try {
    foreach ($e in $entries.GetEnumerator()) {
        if (-not (Test-Path $e.Value)) { throw "missing $($e.Value)" }
        [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
            $archive, $e.Value, $e.Key, [System.IO.Compression.CompressionLevel]::Optimal)
    }
} finally { $archive.Dispose() }
Write-Host "zip: $zip"
$assets += $zip

if (-not $SkipMsi) {
    $msi = & (Join-Path $root 'installer/build-installer.ps1') -Version $Version -BinDir $BinDir -OutDir $OutDir |
        Select-Object -Last 1
    $hash = (Get-FileHash -Algorithm SHA256 $msi).Hash.ToLowerInvariant()
    $sha = "$msi.sha256"
    # sha256sum format, LF, no BOM: the updater parses this file.
    [System.IO.File]::WriteAllText($sha, "$hash  $([System.IO.Path]::GetFileName($msi))`n", (New-Object System.Text.UTF8Encoding $false))
    Write-Host "sha256: $hash"
    $assets += $msi, $sha
}

Write-Host ''
Write-Host "Release assets in ${OutDir}:"
foreach ($a in $assets) { Write-Host ("  {0,-40} {1,12:N0} bytes" -f [System.IO.Path]::GetFileName($a), (Get-Item $a).Length) }
