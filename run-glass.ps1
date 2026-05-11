#!/usr/bin/env pwsh
# Run Glass on Windows with optional CEF support
# Usage: .\run-glass.ps1 [release|debug]

param(
    [string]$BuildType = "debug"
)

$ErrorActionPreference = "Stop"

$Arch = "x86_64"
$Target = "$Arch-pc-windows-msvc"

if ($BuildType -eq "release") {
    $TargetDir = "release"
    $BuildFlag = "--release"
} else {
    $TargetDir = "debug"
    $BuildFlag = ""
    $env:CARGO_INCREMENTAL = "true"
}

$GlassExe = "target\$Target\$TargetDir\zed.exe"

Write-Host "Building Glass ($BuildType)..."
if ($BuildFlag) {
    cargo build --package zed --target $Target $BuildFlag
} else {
    cargo build --package zed --target $Target
}

if (-not (Test-Path $GlassExe)) {
    Write-Error "Build failed: $GlassExe not found"
    exit 1
}

# Set remote server path for development
$env:ZED_COPY_REMOTE_SERVER = "$PWD\target\$Target\$TargetDir\remote_server.gz"

# Check for CEF runtime
$CefPath = $env:CEF_PATH
if (-not $CefPath) {
    $CefPath = "$env:LOCALAPPDATA\glass\cef_runtime"
}

if (Test-Path "$CefPath\libcef.dll") {
    Write-Host "CEF runtime found at: $CefPath"
    # Ensure CEF DLLs are discoverable
    $env:PATH = "$CefPath;$env:PATH"
} else {
    Write-Host "No CEF runtime found — running in editor-only mode"
    Write-Host "Set CEF_PATH or place CEF files at: $CefPath"
}

Write-Host "Running Glass from: $GlassExe"
& $GlassExe @args
