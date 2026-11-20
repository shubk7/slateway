#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Clean rebuild of the host workspace and the Android app. Removes the
# Rust targets (including fuzz/ and android/native/) and the Gradle
# build outputs, then builds from scratch. The Android half is
# required.
#
#   scripts/rebuild.sh [--release] [--no-clean]
#
#   --release   build the host crates with optimizations (hardware runs)
#   --no-clean  skip the clean step (incremental rebuild)

set -euo pipefail
# shellcheck source-path=SCRIPTDIR source=lib.sh
. "$(dirname "$0")/lib.sh"

RELEASE=0
CLEAN=1

usage() {
    cat <<'EOF'
Clean rebuild of the host workspace and the Android app.

  scripts/rebuild.sh [--release] [--no-clean]

  --release   build the host crates with optimizations (hardware runs)
  --no-clean  skip the clean step (incremental rebuild)
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --release) RELEASE=1 ;;
        --no-clean) CLEAN=0 ;;
        -h | --help)
            usage
            exit 0
            ;;
        *) die "unknown argument $1 (see --help)" ;;
    esac
    shift
done

[ "$(uname -s)" = Linux ] || die "rebuild.sh is for Linux; use scripts/rebuild.ps1 on Windows"
cd "$ROOT"

require_android

profile=debug
build_args=(build --workspace)
if [ "$RELEASE" = 1 ]; then
    profile=release
    build_args+=(--release)
fi

if [ "$CLEAN" = 1 ]; then
    step "Cleaning"
    cargo clean
    # fuzz/ and android/native/ are outside the workspace, so cargo
    # clean does not reach them; Gradle's clean leaves the facade too.
    rm -rf "$ROOT/fuzz/target"
    (
        cd "$ROOT/android/native"
        cargo clean
    )
    (
        cd "$ROOT/android"
        ./gradlew clean
    )
fi

step "Building the host workspace ($profile)"
cargo "${build_args[@]}"

step "Building the Android app (debug)"
(
    cd "$ROOT/android"
    ./gradlew assembleDebug "-Pslateway.nativeRequired=true"
)

step "Done"
ok "host:    target/$profile/slatewayd"
ok "mock:    target/$profile/mock-tablet"
ok "apk:     android/app/build/outputs/apk/debug/app-debug.apk"
