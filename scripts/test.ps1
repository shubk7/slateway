#Requires -Version 5.1
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Runs the same checks as CI on Windows: fmt, tests, clippy and
# cargo deny for the workspace, then the Android assemble, unit tests
# and lint. The Android half is required (AGENTS.md "Definition of
# done"), so a missing SDK/NDK fails the script.
#
#   powershell -ExecutionPolicy Bypass -File scripts\test.ps1

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\lib.ps1"

if (-not $IsWindows -and $env:OS -ne 'Windows_NT') {
    Fail 'test.ps1 is for Windows; use scripts/test.sh on Linux'
}
if (-not (Have 'cargo')) {
    Fail 'cargo not found; run scripts\setup.ps1'
}
if (-not (Have 'cargo-deny')) {
    Fail 'cargo-deny missing; cargo install cargo-deny (or run scripts\setup.ps1)'
}

Push-Location $Root
try {
    Resolve-Ffmpeg

    Step 'cargo fmt --all --check'
    cargo fmt --all --check
    Assert-Exit 'cargo fmt'

    Step 'cargo test --workspace'
    cargo test --workspace
    Assert-Exit 'cargo test'

    Step 'cargo clippy --workspace --all-targets -- -D warnings'
    cargo clippy --workspace --all-targets -- -D warnings
    Assert-Exit 'cargo clippy'

    Step 'cargo deny check'
    cargo deny check
    Assert-Exit 'cargo deny'

    # Resolved after the host checks so their results are visible even
    # when the Android toolchain is incomplete.
    Require-Android

    Step 'Android: assembleDebug testDebugUnitTest lintDebug'
    Push-Location (Join-Path $Root 'android')
    try {
        & .\gradlew.bat assembleDebug testDebugUnitTest lintDebug '-Pslateway.nativeRequired=true'
        Assert-Exit 'gradlew'
    }
    finally {
        Pop-Location
    }

    Step 'All checks passed'
}
finally {
    Pop-Location
}
