# Development

Build, test and lint commands live in `AGENTS.md`. This file covers the environment.

## Dual-boot workflow

- Develop mainly on Linux. Boot Windows for Windows backend and driver work.
- Keep a separate clone per OS and push/pull between them. Never share a working tree or `target/` directory across OSes.
- Repo has `.gitattributes` with `* text=auto eol=lf`.
- CI builds and tests both OSes plus Android on every push, so compile errors surface without rebooting.

## CachyOS (starting package list, verify names)

`rustup`, `clang`, `pkgconf`, `ffmpeg`, `pipewire`, `protobuf`, `android-tools`, a JDK, Android Studio (AUR), `v4l2loopback-dkms` and matching kernel headers. Then `rustup default stable` and `cargo install cargo-deny cargo-fuzz`.

## Windows

Rust with the MSVC toolchain, Visual Studio Build Tools (C++ workload), Windows SDK and WDK (driver work), LLVM (for bindgen), `protoc`, FFmpeg development libraries (vcpkg or a shared build), Android platform-tools, Git.

## Tablet setup

1. Enable Developer options (tap Build number 7 times), then USB debugging.
2. `adb devices` shows the tablet.
3. `adb reverse tcp:<port> tcp:<port>` (port in `PROTOCOL.md`).
4. Install the debug build with `./gradlew installDebug` from `android/`, then start `slatewayd`.

## Windows driver testing

Enable test signing only on the dev install, and only after reading `PLATFORMS.md` gotchas. Disabling Secure Boot is a system-wide change.
