$ErrorActionPreference = "Stop"

$RootDir = Split-Path -Parent $PSScriptRoot
$DistDir = Join-Path $RootDir "dist"

Push-Location $RootDir
try {
    Write-Host "==> Building release executable for Windows..."
    cargo build --release

    New-Item -ItemType Directory -Force -Path $DistDir | Out-Null
    $ExeSrc = Join-Path $RootDir "target\release\mydisktest.exe"
    $ExeDst = Join-Path $DistDir "MyDiskTest-Windows.exe"
    Copy-Item $ExeSrc $ExeDst -Force

    Write-Host "==> Windows single-file executable ready: $ExeDst"
} finally {
    Pop-Location
}
