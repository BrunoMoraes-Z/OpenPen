<#
.SYNOPSIS
  Builds the OpenPen per-user MSI (WiX 5) from release binaries.

.DESCRIPTION
  Renders the installer bitmaps from installer/branding.pen with openpen-cli, turns
  LICENSE into RTF, and runs `wix build` on installer/OpenPen.wxs.
  Requires WiX 5 (`dotnet tool install --global wix --version 5.0.2`) and the
  WixToolset.UI.wixext and WixToolset.Util.wixext extensions at the same version
  (`wix extension add -g WixToolset.UI.wixext/5.0.2 WixToolset.Util.wixext/5.0.2`).

.EXAMPLE
  ./installer/build-installer.ps1 -Version 0.1.0 -BinDir target/release
#>
[CmdletBinding()]
param(
    # MSI ProductVersion, X.Y.Z (numbers only).
    [Parameter(Mandatory)][string]$Version,
    # Folder with openpen.exe and openpen-cli.exe.
    [Parameter(Mandatory)][string]$BinDir,
    # Where the .msi goes (default: target/dist).
    [string]$OutDir,
    # Skip `wix msi validate` (ICE checks; they need the Windows Installer service).
    [switch]$SkipValidation
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $OutDir) { $OutDir = Join-Path $root 'target/dist' }

if ($Version -notmatch '^(\d+)\.(\d+)\.(\d+)$' -or [int]$Matches[1] -gt 255 -or [int]$Matches[2] -gt 255 -or [int]$Matches[3] -gt 65535) {
    throw "Version '$Version' is not a valid MSI version (X.Y.Z, X and Y <= 255, Z <= 65535)."
}

$BinDir = (Resolve-Path $BinDir).Path
foreach ($exe in 'openpen.exe', 'openpen-cli.exe') {
    if (-not (Test-Path (Join-Path $BinDir $exe))) { throw "$exe not found in $BinDir" }
}
$cli = Join-Path $BinDir 'openpen-cli.exe'

if (-not (Get-Command wix -ErrorAction SilentlyContinue)) {
    throw 'wix not found. Install it with: dotnet tool install --global wix --version 5.0.2'
}

New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path
$work = Join-Path $OutDir 'installer-obj'
if (Test-Path $work) { Remove-Item -Recurse -Force $work }
New-Item -ItemType Directory -Force $work | Out-Null

# 1. Bitmaps: render our own design (installer/branding.pen) and convert PNG -> 24-bit BMP.
Add-Type -AssemblyName System.Drawing
function ConvertTo-Bmp([string]$PngPath, [string]$BmpPath) {
    $img = [System.Drawing.Image]::FromFile($PngPath)
    try {
        $canvas = New-Object System.Drawing.Bitmap $img.Width, $img.Height, ([System.Drawing.Imaging.PixelFormat]::Format24bppRgb)
        try {
            $g = [System.Drawing.Graphics]::FromImage($canvas)
            $g.Clear([System.Drawing.Color]::White)
            $g.DrawImage($img, 0, 0, $img.Width, $img.Height)
            $g.Dispose()
            $canvas.Save($BmpPath, [System.Drawing.Imaging.ImageFormat]::Bmp)
        } finally { $canvas.Dispose() }
    } finally { $img.Dispose() }
}
$branding = Join-Path $PSScriptRoot 'branding.pen'
$bitmaps = @{}
foreach ($pair in @(@('dialog', 493, 312), @('banner', 493, 58))) {
    $name, $w, $h = $pair
    $png = Join-Path $work "$name.png"
    & $cli render $branding -o $png --node $name --bg '#FFFFFF' | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "openpen-cli render failed for '$name'" }
    $bmp = Join-Path $work "$name.bmp"
    ConvertTo-Bmp $png $bmp
    $size = [System.Drawing.Image]::FromFile($bmp)
    try {
        if ($size.Width -ne $w -or $size.Height -ne $h) { throw "$name.bmp is $($size.Width)x$($size.Height), expected ${w}x${h}" }
    } finally { $size.Dispose() }
    $bitmaps[$name] = $bmp
}

# 2. License as RTF (the WixUI license page only shows RTF).
$license = Get-Content -Raw (Join-Path $root 'LICENSE')
$escaped = $license.Replace('\', '\\').Replace('{', '\{').Replace('}', '\}')
$escaped = ($escaped -replace "`r`n", "`n").TrimEnd("`n") -replace "`n", "\par`r`n"
$rtf = "{\rtf1\ansi\ansicpg1252\deff0{\fonttbl{\f0\fswiss Segoe UI;}}\viewkind4\uc1\pard\f0\fs18`r`n$escaped\par`r`n}"
$licenseRtf = Join-Path $work 'License.rtf'
[System.IO.File]::WriteAllText($licenseRtf, $rtf, [System.Text.Encoding]::ASCII)

# 3. Skill files (everything in skills/openpen, flat).
$skillsDir = Join-Path $root 'skills/openpen'
$skillFiles = @(Get-ChildItem -File $skillsDir | Sort-Object Name | ForEach-Object Name)
if ($skillFiles.Count -eq 0) { throw "no files in $skillsDir" }
if (Get-ChildItem -Directory $skillsDir) { throw "$skillsDir has subfolders; OpenPen.wxs only installs its top-level files" }

# 4. wix build
$msi = Join-Path $OutDir "OpenPen-$Version-x64.msi"
$wixArgs = @(
    'build', (Join-Path $PSScriptRoot 'OpenPen.wxs'),
    '-arch', 'x64',
    '-culture', 'en-US',
    '-ext', 'WixToolset.UI.wixext',
    '-ext', 'WixToolset.Util.wixext',
    '-d', "Version=$Version",
    '-d', "BinDir=$BinDir",
    '-d', "SkillsDir=$skillsDir",
    '-d', "SkillFiles=$($skillFiles -join ';')",
    '-d', "IconFile=$(Join-Path $root 'assets/icon/openpen.ico')",
    '-d', "LicenseRtf=$licenseRtf",
    '-d', "BannerBmp=$($bitmaps['banner'])",
    '-d', "DialogBmp=$($bitmaps['dialog'])",
    '-intermediatefolder', $work,
    '-o', $msi
)
& wix @wixArgs | Out-Host
if ($LASTEXITCODE -ne 0) { throw "wix build failed ($LASTEXITCODE)" }
# wix leaves a .wixpdb next to the MSI; it is not a release asset.
Remove-Item -Force -ErrorAction SilentlyContinue ([System.IO.Path]::ChangeExtension($msi, '.wixpdb'))

# 5. ICE validation. Expected warnings: ICE61 (AllowSameVersionUpgrades) and ICE91
#    (per-user only package installing into the user profile).
if (-not $SkipValidation) {
    & wix msi validate $msi -sice ICE61 -sice ICE91 | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "wix msi validate failed ($LASTEXITCODE)" }
}

Write-Host "MSI: $msi"
$msi
