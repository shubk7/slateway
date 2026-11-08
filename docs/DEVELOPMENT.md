# Development

Build, test and lint commands live in `AGENTS.md`. This file covers the environment.

## Dual-boot workflow

- Develop mainly on Linux. Boot Windows for Windows backend and driver work.
- Keep a separate clone per OS and push/pull between them. Never share a working tree or `target/` directory across OSes.
- Repo has `.gitattributes` with `* text=auto eol=lf`.
- CI builds and tests both OSes plus Android on every push, so compile errors surface without rebooting.

## CachyOS (starting package list, verify names)

`rustup`, `clang`, `pkgconf`, `ffmpeg`, `pipewire`, `protobuf`, `android-tools`, a JDK, Android Studio (AUR), `v4l2loopback-dkms` and matching kernel headers. Then `rustup default stable` and `cargo install cargo-deny cargo-fuzz`.

## Android native library (ADR-013)

The tablet's Noise facade is Rust (`android/native`) built with cargo-ndk:

```bash
sdkmanager "ndk;28.2.13676358"           # r28c; cargo-ndk finds it via ANDROID_HOME
rustup target add aarch64-linux-android
cargo install cargo-ndk
```

`./gradlew assembleDebug` runs the `buildNative` task and packs `libslateway_native.so` (arm64-v8a) into the APK. Without cargo-ndk or the NDK the build skips the facade with a warning and the app reports the missing library; CI passes `-Pslateway.nativeRequired=true` to fail instead.

## Windows

Rust with the MSVC toolchain, Visual Studio Build Tools (C++ workload), Windows SDK and WDK (driver work), LLVM (for bindgen), `protoc`, FFmpeg development libraries (vcpkg or a shared build), Android platform-tools, Git.

## Tablet setup

1. Enable Developer options (tap Build number 7 times), then USB debugging.
2. `adb devices` shows the tablet.
3. `adb reverse tcp:<port> tcp:<port>` (port in `PROTOCOL.md`).
4. Install the debug build with `./gradlew installDebug` from `android/`, then start `slatewayd`.

## Windows driver testing

Enable test signing only on the dev install, and only after reading `PLATFORMS.md` gotchas. Disabling Secure Boot is a system-wide change.
