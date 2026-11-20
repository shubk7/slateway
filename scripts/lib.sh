# SPDX-License-Identifier: GPL-3.0-or-later
# shellcheck shell=bash
#
# Shared helpers for the scripts/ entry points. Sourced, never run.
# Everything here is Linux-only; the Windows equivalents live in
# lib.ps1.

# shellcheck disable=SC2034 # read by the sourcing scripts
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

# rustup's default layout keeps cargo tools in ~/.cargo/bin; some distro
# packages do not put that directory on PATH, and Gradle needs
# cargo-ndk from there.
if [ -d "$HOME/.cargo/bin" ]; then
    case ":$PATH:" in
        *":$HOME/.cargo/bin:"*) ;;
        *) PATH="$HOME/.cargo/bin:$PATH" ;;
    esac
    export PATH
fi

# Keep in step with android/app/build.gradle.kts and
# docs/DEVELOPMENT.md ("Android native library").
NDK_VERSION=28.2.13676358
COMPILE_SDK=35
BUILD_TOOLS=35.0.0

if [ "$(id -u)" -eq 0 ]; then
    SUDO=""
else
    SUDO="sudo"
fi

step() { printf '\n==> %s\n' "$*"; }
ok() { printf '    %s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

have() { command -v "$1" >/dev/null 2>&1; }

# Asks before a system change. --yes skips the prompt; without a
# terminal the answer is not guessed.
confirm() {
    if [ "${ASSUME_YES:-0}" = 1 ]; then
        return 0
    fi
    if [ ! -t 0 ]; then
        die "no terminal for confirmation; re-run with --yes"
    fi
    printf '%s [y/N] ' "$1"
    local answer
    read -r answer
    case "$answer" in
        y | Y | yes | YES) return 0 ;;
        *) return 1 ;;
    esac
}

# The distro families the setup script knows how to install for.
pkg_manager() {
    local pm
    for pm in apt-get dnf pacman zypper; do
        if have "$pm"; then
            printf '%s\n' "$pm"
            return 0
        fi
    done
    return 1
}

# The JDK major version, or failure when java is missing or its
# output is not the expected `openjdk version "21.0.1"` shape.
java_major() {
    have java || return 1
    local version major
    version=$(java -version 2>&1 | awk -F'"' 'NR==1 { print $2 }')
    major=${version%%.*}
    case "$major" in
        '' | *[!0-9]*) return 1 ;;
    esac
    printf '%s\n' "$major"
}

# Gradle 9 needs Java 17 or newer.
java_ok() {
    local major
    major=$(java_major) || return 1
    [ "$major" -ge 17 ]
}

# True when every system package the workspace links against is
# already usable.
system_packages_present() {
    have cc &&
        have pkg-config &&
        have protoc &&
        have clang &&
        java_ok &&
        pkg-config --exists libavcodec libavformat libavutil &&
        pkg-config --exists libpipewire-0.3
}

install_system_packages() {
    local pm
    pm=$(pkg_manager) ||
        die "no supported package manager (apt-get, dnf, pacman, zypper); install protoc, pkg-config, clang, a JDK, libpipewire-0.3 and libavcodec/libavformat/libavutil headers by hand"
    local -a packages
    case "$pm" in
        apt-get)
            # Same set as the CI ubuntu job, plus the local JDK, clang
            # (bindgen loads libclang) and adb.
            packages=(build-essential pkg-config clang libclang-dev protobuf-compiler libpipewire-0.3-dev libavcodec-dev libavformat-dev libavutil-dev adb)
            step "Installing system packages with apt-get"
            confirm "Install with $SUDO apt-get: ${packages[*]} ?" || die "aborted"
            $SUDO apt-get update -q
            $SUDO apt-get install -y --no-install-recommends "${packages[@]}"
            install_jdk openjdk-21-jdk openjdk-17-jdk
            ;;
        dnf)
            packages=(gcc gcc-c++ make pkgconf-pkg-config clang clang-devel protobuf-compiler pipewire-devel ffmpeg-devel android-tools)
            step "Installing system packages with dnf"
            confirm "Install with $SUDO dnf: ${packages[*]} ?" || die "aborted"
            $SUDO dnf install -y "${packages[@]}" || die "dnf failed; on Fedora ffmpeg-devel comes from RPM Fusion (https://rpmfusion.org/Configuration)"
            install_jdk java-21-openjdk-devel java-17-openjdk-devel
            ;;
        pacman)
            packages=(base-devel pkgconf clang protobuf pipewire ffmpeg android-tools)
            step "Installing system packages with pacman"
            confirm "Install with $SUDO pacman: ${packages[*]} ?" || die "aborted"
            $SUDO pacman -S --needed --noconfirm "${packages[@]}"
            install_jdk jdk21-openjdk jdk17-openjdk
            ;;
        zypper)
            packages=(gcc gcc-c++ make pkg-config clang protobuf-devel pipewire-devel ffmpeg-devel android-tools)
            step "Installing system packages with zypper"
            confirm "Install with $SUDO zypper: ${packages[*]} ?" || die "aborted"
            $SUDO zypper --non-interactive install "${packages[@]}" || die "zypper failed; on openSUSE ffmpeg-devel comes from the Packman repository"
            install_jdk java-21-openjdk-devel java-17-openjdk-devel
            ;;
    esac
}

# Gradle 9 needs Java 17+, so try the newest package name the distro
# has and fall back. Some releases carry only one of them.
install_jdk() {
    local name
    for name in "$@"; do
        if java_ok; then
            return 0
        fi
        ok "trying JDK package $name"
        case "$(pkg_manager)" in
            apt-get) $SUDO apt-get install -y --no-install-recommends "$name" || true ;;
            dnf) $SUDO dnf install -y "$name" || true ;;
            pacman) $SUDO pacman -S --needed --noconfirm "$name" || true ;;
            zypper) $SUDO zypper --non-interactive install "$name" || true ;;
        esac
    done
    if ! java_ok; then
        warn "no JDK 17+ installed; install one by hand (Gradle 9 needs it)"
    fi
}

# The first SDK directory that exists, from the environment or the
# usual install locations.
android_sdk() {
    local dir
    for dir in "${ANDROID_HOME:-}" "${ANDROID_SDK_ROOT:-}" \
        "$HOME/Android/Sdk" "$HOME/android-sdk" "/opt/android-sdk"; do
        if [ -n "$dir" ] && [ -d "$dir" ]; then
            printf '%s\n' "$dir"
            return 0
        fi
    done
    return 1
}

sdkmanager_bin() {
    local sdk="$1" candidate
    for candidate in \
        "$sdk/cmdline-tools/latest/bin/sdkmanager" \
        "$sdk/cmdline-tools/bin/sdkmanager" \
        "$sdk/tools/bin/sdkmanager"; do
        if [ -x "$candidate" ]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    if have sdkmanager; then
        command -v sdkmanager
        return 0
    fi
    return 1
}

# Installs the NDK and the platform/build-tools the app compiles
# against. Requires an sdkmanager from Android Studio or the
# command-line tools.
install_android_packages() {
    local sdk="$1" sm
    if ! sm=$(sdkmanager_bin "$sdk"); then
        return 1
    fi
    step "Installing Android SDK packages into $sdk"
    ok "platform-tools, platforms;android-$COMPILE_SDK, build-tools;$BUILD_TOOLS, ndk;$NDK_VERSION"
    confirm "Accept the Android SDK licenses and install these packages?" ||
        die "aborted; run '$sm --licenses' and re-run setup"
    # sdkmanager prints a prompt per license; yes(1) exits on SIGPIPE
    # when it is done, which pipefail would turn into a failure.
    set +o pipefail
    yes | "$sm" --licenses >/dev/null 2>&1 || true
    set -o pipefail
    "$sm" --install "platform-tools" "platforms;android-$COMPILE_SDK" \
        "build-tools;$BUILD_TOOLS" "ndk;$NDK_VERSION"
}

# Fails with the missing piece when the Android side cannot build.
# test.sh and rebuild.sh call this: the Android half is required, not
# optional (CI builds it on every push).
require_android() {
    local sdk
    sdk=$(android_sdk) ||
        die "Android SDK not found; set ANDROID_HOME or run scripts/setup.sh"
    export ANDROID_HOME="$sdk"
    export ANDROID_SDK_ROOT="$sdk"

    local major
    major=$(java_major) ||
        die "JDK not found or its version unreadable; install Java 17+ (or set JAVA_HOME)"
    [ "$major" -ge 17 ] || die "Java 17+ required (found Java $major)"

    [ -d "$sdk/ndk/$NDK_VERSION" ] ||
        die "NDK $NDK_VERSION missing in $sdk; run scripts/setup.sh"
    have cargo-ndk || die "cargo-ndk missing; cargo install cargo-ndk"
    rustup target list --installed 2>/dev/null | grep -qx aarch64-linux-android ||
        die "Android Rust target missing; rustup target add aarch64-linux-android"
}
