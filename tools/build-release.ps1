$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)
cargo build --release
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& .\target\release\patch-icon.exe .\target\release\clipnest.exe .\assets\icon.ico
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Write-Host "release build + icon patched"
