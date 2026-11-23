<#
.SYNOPSIS
  Quick checks of release binaries before packaging (no window is opened).

.DESCRIPTION
  - openpen-cli --version prints the expected version, --help works;
  - openpen.exe and openpen-cli.exe carry the version in their VERSIONINFO;
  - openpen-cli validate --strict and render (PNG and SVG) of installer/branding.pen.

.EXAMPLE
  ./scripts/smoke-test.ps1 -BinDir target/release
#>
[CmdletBinding()]
param(
    [string]$BinDir = (Join-Path $PSScriptRoot '../target/release'),
    # Version the binaries must report (default: Cargo.toml).
    [string]$Version
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $Version) { $Version = & (Join-Path $PSScriptRoot 'version.ps1') }
$BinDir = (Resolve-Path $BinDir).Path
$cli = Join-Path $BinDir 'openpen-cli.exe'
$app = Join-Path $BinDir 'openpen.exe'
foreach ($f in $cli, $app) { if (-not (Test-Path $f)) { throw "missing $f" } }

function Invoke-Cli {
    # Windows PowerShell 5.1 turns redirected stderr lines into errors; judge by exit code only.
    $ErrorActionPreference = 'Continue'
    $out = & $cli @args 2>&1 | ForEach-Object { "$_" } | Out-String
    if ($LASTEXITCODE -ne 0) { throw "openpen-cli $($args -join ' ') failed ($LASTEXITCODE):`n$out" }
    $out
}

$v = (Invoke-Cli --version).Trim()
if ($v -ne "openpen-cli $Version") { throw "openpen-cli --version printed '$v', expected 'openpen-cli $Version'" }
Write-Host "ok  $v"

$help = Invoke-Cli --help
foreach ($cmd in 'validate', 'render', 'mcp') {
    if ($help -notmatch "\b$cmd\b") { throw "openpen-cli --help does not list '$cmd'" }
}
Write-Host 'ok  openpen-cli --help'

foreach ($f in $cli, $app) {
    $info = (Get-Item $f).VersionInfo
    if ($info.ProductVersion -ne $Version -or $info.ProductName -ne 'OpenPen') {
        throw "$f VERSIONINFO is '$($info.ProductName) $($info.ProductVersion)', expected 'OpenPen $Version'"
    }
}
Write-Host "ok  VERSIONINFO $Version on both executables"

$fixture = Join-Path $root 'installer/branding.pen'
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) "openpen-smoke-$PID"
New-Item -ItemType Directory -Force $tmp | Out-Null
try {
    Invoke-Cli validate --strict $fixture | Write-Host
    $png = Join-Path $tmp 'dialog.png'
    Invoke-Cli render $fixture -o $png --node dialog | Write-Host
    $bytes = [System.IO.File]::ReadAllBytes($png)
    if ($bytes.Length -lt 1000 -or $bytes[0] -ne 0x89 -or $bytes[1] -ne 0x50) { throw "render did not produce a PNG ($png)" }
    $svg = Join-Path $tmp 'banner.svg'
    Invoke-Cli render $fixture -o $svg --node banner | Write-Host
    if ((Get-Content -Raw $svg) -notmatch '<svg') { throw "render did not produce an SVG ($svg)" }
    Write-Host 'ok  validate + render (png, svg)'
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
Write-Host 'smoke test passed'
