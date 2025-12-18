$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location (Join-Path $root "..")

if (-not (Test-Path "target\release\mcosu-importer.exe")) {
    Write-Host "Building release binary..."
    cargo build --release
}

$dist = "dist"
if (Test-Path $dist) { Remove-Item $dist -Recurse -Force }
New-Item -ItemType Directory -Path $dist | Out-Null

$bundle = Join-Path $dist "mcosu-importer"
New-Item -ItemType Directory -Path $bundle | Out-Null

Copy-Item "target\release\mcosu-importer.exe" $bundle
Copy-Item "README.md" $bundle
Copy-Item "CHANGELOG.md" $bundle
Copy-Item "LICENSE" $bundle
Copy-Item "assets" $bundle -Recurse

Write-Host "Bundle ready in $bundle"

# Optional zip
if (Get-Command Compress-Archive -ErrorAction SilentlyContinue) {
    $zipPath = Join-Path $dist "mcosu-importer.zip"
    if (Test-Path $zipPath) { Remove-Item $zipPath -Force }
    Compress-Archive -Path "$bundle\*" -DestinationPath $zipPath
    Write-Host "Zip created at $zipPath"
}
