#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# One-time development setup for Linux. Installs the system packages,
# the Rust toolchain pieces and the Android SDK components the
# workspace needs, then checks that everything is visible. Safe to
# re-run: it only installs what is missing.
#
#   scripts/setup.sh [--yes] [--host-only] [--android-sdk DIR]
#
# --yes          do not ask before installing anything
# --host-only    skip the Android SDK, NDK and Rust target (host work)
# --android-sdk  use this SDK instead of ANDROID_HOME / ~/Android/Sdk

set -euo pipefail
# shellcheck source-path=SCRIPTDIR source=lib.sh
. "$(dirname "$0")/lib.sh"

ASSUME_YES=0
HOST_ONLY=0
SDK_ARG=""

usage() {
    cat <<'EOF'
One-time development setup for Linux.

  scripts/setup.sh [--yes] [--host-only] [--android-sdk DIR]

  --yes          do not ask before installing anything
  --host-only    skip the Android SDK, NDK and Rust target (host work)
  --android-sdk  use this SDK instead of ANDROID_HOME / ~/Android/Sdk
EOF
}

install_cargo_tool() {
    local binary="$1" crate="$2"
    if have "$binary"; then
        ok "$binary already installed"
    else
        confirm "Install $crate with cargo install?" || die "aborted"
        cargo install "$crate"
    fi
}

while [ $# -gt 0 ]; do
    case "$1" in
        --yes) export ASSUME_YES=1 ;;
        --host-only) HOST_ONLY=1 ;;
        --android-sdk)
            [ $# -ge 2 ] || die "--android-sdk needs a directory"
            SDK_ARG="$2"
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *) die "unknown argument $1 (see --help)" ;;
    esac
    shift
done

[ "$(uname -s)" = Linux ] || die "setup.sh is for Linux; use scripts/setup.ps1 on Windows"

cd "$ROOT"

if [ -n "$SDK_ARG" ]; then
    [ -d "$SDK_ARG" ] || die "--android-sdk $SDK_ARG does not exist"
    export ANDROID_HOME="$SDK_ARG"
    export ANDROID_SDK_ROOT="$SDK_ARG"
fi

step "Checking the system packages"
if system_packages_present; then
    ok "protoc, pkg-config, clang, JDK, FFmpeg and PipeWire headers already present"
else
    install_system_packages
fi

step "Checking the Rust toolchain"
if ! have cargo; then
    confirm "Install Rust with rustup (https://rustup.rs)?" || die "aborted"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
fi
have rustup || die "rustup not found; install the Rust toolchain with rustup (https://rustup.rs)"
ok "$(cargo --version)"
rustup component add rustfmt clippy
if [ "$HOST_ONLY" = 0 ]; then
    rustup target add aarch64-linux-android
fi

step "Checking the Cargo tools"
install_cargo_tool cargo-deny cargo-deny
install_cargo_tool cargo-fuzz cargo-fuzz
if [ "$HOST_ONLY" = 0 ]; then
    install_cargo_tool cargo-ndk cargo-ndk
fi

if [ "$HOST_ONLY" = 0 ]; then
    step "Checking the Android SDK"
    sdk=$(android_sdk) || die "Android SDK not found.
Install Android Studio or the command-line tools, then either set
ANDROID_HOME or pass --android-sdk DIR. The pieces setup needs are:
  sdkmanager \"platform-tools\" \"platforms;android-$COMPILE_SDK\" \\
             \"build-tools;$BUILD_TOOLS\" \"ndk;$NDK_VERSION\""
    ok "SDK at $sdk"
    export ANDROID_HOME="$sdk"
    export ANDROID_SDK_ROOT="$sdk"

    android_complete=1
    [ -d "$sdk/ndk/$NDK_VERSION" ] || android_complete=0
    [ -d "$sdk/platforms/android-$COMPILE_SDK" ] || android_complete=0
    [ -d "$sdk/build-tools/$BUILD_TOOLS" ] || android_complete=0
    if [ "$android_complete" = 0 ]; then
        install_android_packages "$sdk" || die "sdkmanager not found under $sdk.
Open Android Studio's SDK Manager and install the NDK $NDK_VERSION,
platform android-$COMPILE_SDK and build-tools $BUILD_TOOLS, or add the
command-line tools and re-run setup."
    fi
else
    warn "skipping the Android SDK, NDK and Rust target (--host-only)"
fi

step "Verifying"
ok "protoc $(protoc --version | awk '{ print $2 }')"
ok "libavcodec $(pkg-config --modversion libavcodec), libavformat $(pkg-config --modversion libavformat), libavutil $(pkg-config --modversion libavutil)"
ok "libpipewire-0.3 $(pkg-config --modversion libpipewire-0.3)"
# Not `java -version | head -n 1`: java would take SIGPIPE and
# pipefail would turn that into a failure.
java_line=$(java -version 2>&1) || die "java not found; install a JDK 17+"
ok "${java_line%%$'\n'*}"
if [ "$HOST_ONLY" = 0 ]; then
    ok "NDK $sdk/ndk/$NDK_VERSION"
    ok "$(cargo ndk --version)"
fi
if ! have adb; then
    warn "adb not found; install android-tools to test on a tablet"
fi

step "Done"
ok "test:    scripts/test.sh"
ok "rebuild: scripts/rebuild.sh [--release]"
