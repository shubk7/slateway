#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Runs the same checks as CI on Linux: fmt, tests, clippy and
# cargo deny for the workspace, then the Android assemble, unit tests
# and lint. The Android half is required (AGENTS.md "Definition of
# done"), so a missing SDK/NDK fails the script.
#
#   scripts/test.sh

set -euo pipefail
# shellcheck source-path=SCRIPTDIR source=lib.sh
. "$(dirname "$0")/lib.sh"

[ "$(uname -s)" = Linux ] || die "test.sh is for Linux; use scripts/test.ps1 on Windows"
cd "$ROOT"

have cargo-deny || die "cargo-deny missing; cargo install cargo-deny (or run scripts/setup.sh)"

step "cargo fmt --all --check"
cargo fmt --all --check

step "cargo test --workspace"
cargo test --workspace

step "cargo clippy --workspace --all-targets -- -D warnings"
cargo clippy --workspace --all-targets -- -D warnings

step "cargo deny check"
cargo deny check

# Resolved after the host checks so their results are visible even
# when the Android toolchain is incomplete.
require_android

step "Android: assembleDebug testDebugUnitTest lintDebug"
(
    cd "$ROOT/android"
    ./gradlew assembleDebug testDebugUnitTest lintDebug \
        "-Pslateway.nativeRequired=true"
)

step "All checks passed"
