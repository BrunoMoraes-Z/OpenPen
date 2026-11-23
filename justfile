# Windows PowerShell without loading user profiles or modules.
set shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command"]

# Build the app and the CLI (used by MCP) in release mode.
default: build

# Release build of the app (openpen.exe) and CLI (openpen-cli.exe).
build:
    cargo build --release -p openpen -p openpen-cli
    Write-Host "Binaries in target\release: openpen.exe, openpen-cli.exe"
