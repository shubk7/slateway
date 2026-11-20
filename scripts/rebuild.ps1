#Requires -Version 5.1
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Clean rebuild of the host workspace and the Android app. Removes the
# Rust targets (including fuzz/) and the Gradle build outputs, then
# builds from scratch. The Android half is required.
#
#   powershell -ExecutionPolicy Bypass -File scripts\rebuild.ps1 [-Release] [-NoClean]
#
#   -Release  build the host crates with optimizations (hardware runs)
#   -NoClean  skip the clean step (incremental rebuild)

param(
    [switch]$Release,
    [switch]$NoClean
)

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\lib.ps1"

if (-not $IsWindows -and $env:OS -ne 'Windows_NT') {
    Fail 'rebuild.ps1 is for Windows; use scripts/rebuild.sh on Linux'
}
if (-not (Have 'cargo')) {
    Fail 'cargo not found; run scripts\setup.ps1'
}

Push-Location $Root
try {
    Resolve-Ffmpeg
    Require-Android

    $profile = 'debug'
    $buildArgs = @('build', '--workspace')
    if ($Release) {
        $profile = 'release'
        $buildArgs += '--release'
    }

    if (-not $NoClean) {
        Step 'Cleaning'
        cargo clean
        Assert-Exit 'cargo clean'
        # fuzz/ and android/native/ are outside the workspace, so cargo
        # clean does not reach them; Gradle's clean leaves the facade too.
        Remove-Item -Recurse -Force (Join-Path $Root 'fuzz\target') -ErrorAction SilentlyContinue
        Push-Location (Join-Path $Root 'android\native')
        try {
            cargo clean
            Assert-Exit 'cargo clean (android/native)'
        }
        finally {
            Pop-Location
        }
        Push-Location (Join-Path $Root 'android')
        try {
            & .\gradlew.bat clean
            Assert-Exit 'gradlew clean'
        }
        finally {
            Pop-Location
        }
    }

    Step "Building the host workspace ($profile)"
    cargo @buildArgs
    Assert-Exit 'cargo build'

    Step 'Building the Android app (debug)'
    Push-Location (Join-Path $Root 'android')
    try {
        & .\gradlew.bat assembleDebug '-Pslateway.nativeRequired=true'
        Assert-Exit 'gradlew assembleDebug'
    }
    finally {
        Pop-Location
    }

    Step 'Done'
    Ok "host:    target\$profile\slatewayd.exe"
    Ok "mock:    target\$profile\mock-tablet.exe"
    Ok 'apk:     android\app\build\outputs\apk\debug\app-debug.apk'
}
finally {
    Pop-Location
}
