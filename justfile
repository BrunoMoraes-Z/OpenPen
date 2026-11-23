# Windows PowerShell without loading user profiles or modules.
set shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command"]

# Build the app and the CLI (used by MCP) in release mode.
default: build

# Release build of the app (openpen.exe) and CLI (openpen-cli.exe).
build:
    cargo build --release -p openpen -p openpen-cli
    Write-Host "Binaries in target\release: openpen.exe, openpen-cli.exe"

# `just dist 0.1.1` names the assets with another version, e.g. to test an in-place upgrade.
# Release assets in target\dist (MSI, .sha256, portable zip), as in CI. Needs WiX 5 (docs/release.md).
dist version="": build
    ./scripts/smoke-test.ps1 -BinDir target/release
    ./scripts/package.ps1 -BinDir target/release {{ if version == "" { "" } else { "-Version " + version } }}
