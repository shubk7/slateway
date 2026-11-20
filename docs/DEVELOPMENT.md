# Development

Build, test and lint commands live in `AGENTS.md`. This file covers the environment.

## Dual-boot workflow

- Develop mainly on Linux. Boot Windows for Windows backend and driver work.
- Keep a separate clone per OS and push/pull between them. Never share a working tree or `target/` directory across OSes.
- Repo has `.gitattributes` with `* text=auto eol=lf`.
- CI builds and tests both OSes plus Android on every push, so compile errors surface without rebooting.

## Scripts

`scripts/` holds the same three entry points for both OSes: one-time
setup, the CI check chain, and a clean rebuild. The shell versions
detect the package manager (apt, dnf, pacman, zypper); the PowerShell
versions assume winget (or choco) for the pieces they can install.

```
scripts/setup.sh [--yes] [--host-only] [--android-sdk DIR]
scripts/test.sh
scripts/rebuild.sh [--release] [--no-clean]
```

```
powershell -ExecutionPolicy Bypass -File scripts\setup.ps1 [-Yes] [-HostOnly] [-Persist] [-AndroidSdk DIR]
powershell -ExecutionPolicy Bypass -File scripts\test.ps1
powershell -ExecutionPolicy Bypass -File scripts\rebuild.ps1 [-Release] [-NoClean]
```

- `setup` installs the system packages, Rust components, `cargo-deny`,
  `cargo-fuzz`, `cargo-ndk`, the Android Rust target and the NDK /
  platform / build-tools. It asks before every change (`--yes`/`-Yes`
  skips the prompts) and is safe to re-run. Windows setup also installs
  LLVM via winget when libclang is missing (bindgen needs `libclang.dll`
  while `ffmpeg-sys-next` builds), downloads the pinned FFmpeg shared
  build into `tools/ffmpeg` and gets `protoc` from winget, choco or the
  release zip; `-Persist` writes `FFMPEG_DIR`, `LIBCLANG_PATH`, `PATH`
  and `ANDROID_HOME` to the user environment.
- `test` runs fmt, tests, clippy and `cargo deny` for the
  workspace, then `assembleDebug testDebugUnitTest lintDebug` for the
  Android app. The Android half is required, as in CI.
- `rebuild` cleans `target/`, `fuzz/target/`, `android/native/target/` and
  the Gradle outputs, then builds the host workspace (debug or
  `--release`) and the debug APK.

The Android SDK must already be installed (Android Studio or the
command-line tools); setup installs the SDK packages but not the SDK
itself. Set `ANDROID_HOME` or pass `--android-sdk`/`-AndroidSdk`.

## CachyOS (starting package list, verify names)

`rustup`, `clang`, `pkgconf`, `ffmpeg`, `pipewire`, `protobuf`, `android-tools`, a JDK, Android Studio (AUR), `v4l2loopback-dkms` and matching kernel headers. Then `rustup default stable` and `cargo install cargo-deny cargo-fuzz`. `scripts/setup.sh` installs the non-AUR pieces on apt, dnf, pacman and zypper systems; Android Studio and `v4l2loopback` stay manual.

FFmpeg must be installed with its development headers (`ffmpeg` on Arch
pulls in `ffmpeg-dev` equivalents through the package); the encoder
build links the system `libavcodec`, `libavformat` and `libavutil` via
pkg-config (ADR-014; `slateway-ffmpeg` enables only `ffmpeg-next`'s
`codec` and `format` features, so the filter, device, swscale and
swresample headers are not needed). On Debian/Ubuntu that is
`libavcodec-dev libavformat-dev libavutil-dev` — the same set the CI
ubuntu job installs. Any FFmpeg major from 3.0 through 9.x builds, but
a recent one is required to run the NVENC hardware path.

Known runtime limit: FFmpeg 9.0.2 (Arch, Windows CI's gyan build)
opens `libx264` fine; the Ubuntu 26.04 archive's libavcodec rejects
the same open with `Invalid argument`, so the CI Rust job is pinned
to `ubuntu-24.04` until the option handling on that build is
investigated (spike owed, see `docs/DECISIONS.md` pending items).

## Android native library (ADR-013)

The tablet's Noise facade is Rust (`android/native`) built with cargo-ndk:

```bash
sdkmanager "ndk;28.2.13676358"           # r28c; cargo-ndk finds it via ANDROID_HOME
rustup target add aarch64-linux-android
cargo install cargo-ndk
```

`./gradlew assembleDebug` runs the `buildNative` task and packs `libslateway_native.so` (arm64-v8a) into the APK. Without cargo-ndk or the NDK the build skips the facade with a warning and the app reports the missing library; CI passes `-Pslateway.nativeRequired=true` to fail instead.

## Windows

Rust with the MSVC toolchain, Visual Studio Build Tools (C++ workload), Windows SDK and WDK (driver work), LLVM (for bindgen: `ffmpeg-sys-next` runs bindgen, which loads `libclang.dll`), `protoc`, FFmpeg development libraries (vcpkg or a shared build), Android platform-tools, Git.

FFmpeg for the encoder (ADR-014): the CI Windows job downloads the
pinned gyan.dev shared build (`ffmpeg-9.0.2-full_build-shared.zip`)
and sets `FFMPEG_DIR` plus the `bin` path. `scripts\setup.ps1` does
the same locally, extracting to `tools\ffmpeg` (gitignored) and
pointing `FFMPEG_DIR`/`PATH` at it for the session (`-Persist` writes
them to the user environment).

Windows-specific notes from the M0 bring-up:

- In PowerShell, quote Gradle's property arguments (the dot gets split
  otherwise): `.\gradlew.bat assembleDebug "-Pslateway.nativeRequired=true"`.
- Run the cargo commands from the repo root; `cargo deny check` does
  not walk up from a subdirectory the way the other cargo commands do.
- The daemon's config directory is `%APPDATA%\slateway`
  (`known-tablets` and `host.key`); pairing is per host, so a tablet
  paired on Linux pairs again on Windows.

## Tablet setup

1. Enable Developer options (tap Build number 7 times), then USB debugging.
2. `adb devices` shows the tablet.
3. `adb reverse tcp:<port> tcp:<port>` (port in `PROTOCOL.md`).
4. Install the debug build with `./gradlew installDebug` from `android/`, then start `slatewayd`.

## Mirroring

`slatewayd --mirror` streams the primary monitor to the connected
tablet after the handshake (M1; the UI-driven mode switching arrives
in M2). The daemon picks the platform capture (portal on Linux, WGC
on Windows) and the encoder candidate list for its OS (NVENC, then
libx264 fallback). The tablet must be running the app build with the
video decoder; capture setup on Linux may show a screen-share consent
dialog once per session.

## Frame pacing

`--fps N` (default 60) makes the daemon send a constant frame rate
instead of only on compositor damage; `--fps 0` restores the
damage-driven stream for comparison. Capture stays damage-driven — the
compositor decides when a buffer exists — so the pump holds the newest
frame and sends one frame per period, re-sending it when nothing new
arrived (the `tick` policy, ADR-015; chosen by the 2026-11-19 A/B in
TESTING.md). A keyframe request sends at once instead of waiting for the
next tick.

Watch `video: encode 2s: encoded=… repeated=… skipped=… errs=…` to see
how much of the stream is repeats; a mostly static screen is nearly all
repeats.

## Touch as pointer

`--allow-input` (with `--mirror`) lets tablet touches drive the host
pointer (docs/SECURITY.md, "Consent": the operator's explicit opt-in
until the per-device consent UI lands). On Linux the capture session
becomes a RemoteDesktop session with pointer devices, so the same
consent dialog covers screen sharing and input; coordinates map to
the streamed monitor automatically. On Windows touches inject as
absolute mouse events over the primary monitor. Per-device grants
and a host-side prompt arrive with the M2 UI work.

## Windows driver testing

Enable test signing only on the dev install, and only after reading `PLATFORMS.md` gotchas. Disabling Secure Boot is a system-wide change.
