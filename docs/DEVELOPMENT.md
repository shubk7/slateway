# Development

Build, test and lint commands live in `AGENTS.md`. This file covers the environment.

## Dual-boot workflow

- Develop mainly on Linux. Boot Windows for Windows backend and driver work.
- Keep a separate clone per OS and push/pull between them. Never share a working tree or `target/` directory across OSes.
- Repo has `.gitattributes` with `* text=auto eol=lf`.
- CI builds and tests both OSes plus Android on every push, so compile errors surface without rebooting.

## CachyOS (starting package list, verify names)

`rustup`, `clang`, `pkgconf`, `ffmpeg`, `pipewire`, `protobuf`, `android-tools`, a JDK, Android Studio (AUR), `v4l2loopback-dkms` and matching kernel headers. Then `rustup default stable` and `cargo install cargo-deny cargo-fuzz`.

FFmpeg must be installed with its development headers (`ffmpeg` on Arch
pulls in `ffmpeg-dev` equivalents through the package); the encoder
build links the system `libavcodec`, `libavutil` and friends via
pkg-config (ADR-014). On Debian/Ubuntu that is `libavcodec-dev
libavformat-dev libavfilter-dev libavdevice-dev libswscale-dev
libswresample-dev` — the same set the CI ubuntu job installs. Any
FFmpeg major from 3.0 through 9.x builds, but a recent one is required
to run the NVENC hardware path.

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

Rust with the MSVC toolchain, Visual Studio Build Tools (C++ workload), Windows SDK and WDK (driver work), LLVM (for bindgen), `protoc`, FFmpeg development libraries (vcpkg or a shared build), Android platform-tools, Git.

FFmpeg for the encoder (ADR-014): the CI Windows job downloads the
pinned gyan.dev shared build (`ffmpeg-9.0.2-full_build-shared.zip`)
and sets `FFMPEG_DIR` plus the `bin` path. Do the same locally:
download that zip, extract it anywhere, then in PowerShell set
`FFMPEG_DIR` to the extracted folder and prepend its `bin` directory
to `PATH` so the test binaries find `avcodec-63.dll` and friends.

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
frame and re-sends it when the pacing deadline passes. `--pacing`
picks the policy (spike branch `spike/frame-pacing`; the choice is
pending a hardware A/B):

- `fill` (default) — fresh frames go out immediately; repeats hold
  the floor at `fps`. No added latency; the rate can exceed `fps` when
  the compositor bursts (~83 frames/s for a 60 Hz monitor was seen on
  Linux).
- `tick` — exactly one frame per period, the newest at the tick.
  Even cadence and a hard cap, at up to one period of frame age.

Watch `video: encode 2s: encoded=… repeated=… skipped=… errs=…` to see
how much of the stream is repeats.

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
