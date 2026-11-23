<#
.SYNOPSIS
  Prints the workspace version from the root Cargo.toml ([workspace.package] version).

.DESCRIPTION
  With -Expect (a tag such as v0.1.0, or a bare 0.1.0), fails when the two differ.
  Used by the release workflow to make sure the pushed tag matches Cargo.toml.

.EXAMPLE
  ./scripts/version.ps1                 # 0.1.0
  ./scripts/version.ps1 -Expect v0.1.0  # 0.1.0, or an error
#>
[CmdletBinding()]
param([string]$Expect)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$cargo = Join-Path $PSScriptRoot '../Cargo.toml'
$inPackage = $false
$version = $null
foreach ($line in Get-Content $cargo) {
    if ($line -match '^\s*\[(.+)\]\s*$') { $inPackage = ($Matches[1].Trim() -eq 'workspace.package'); continue }
    if ($inPackage -and $line -match '^\s*version\s*=\s*"([^"]+)"') { $version = $Matches[1]; break }
}
if (-not $version) { throw "no [workspace.package] version in $cargo" }
if ($version -notmatch '^\d+\.\d+\.\d+$') {
    throw "workspace version '$version' is not X.Y.Z (the MSI and the updater need plain numeric versions)"
}

if ($Expect) {
    $want = $Expect.Trim() -replace '^refs/tags/', '' -replace '^v', ''
    if ($want -ne $version) {
        throw "tag/version mismatch: '$Expect' vs Cargo.toml [workspace.package] version '$version'. Bump Cargo.toml (and Cargo.lock) or fix the tag."
    }
}

$version
