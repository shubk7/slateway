# Architecture

```
 Android app (Kotlin)                     Host daemon "slatewayd" (Rust)
 ┌────────────────────┐                   ┌────────────────────────────┐
 │ MediaCodec decode  │◄──── video ───────│ Capture → Encoder          │
 │ Touch/pen sender   │───── input ──────►│ InputInjector              │
 │ Mic / camera       │──── audio/cam ───►│ VirtualMic / VirtualCamera │
 │ Audio playback     │◄──── audio ───────│ AudioCapture               │
 └─────────┬──────────┘                   └──────────────┬─────────────┘
           └───── one encrypted connection, typed channels ─────┘
              (wire format: PROTOCOL.md, trust: SECURITY.md)
```

The daemon is headless. A CLI (and later a tray app) controls it over local IPC (Unix socket or named pipe, current user only).

## Repo layout

```
crates/slateway-proto      wire codec and schemas, no I/O
crates/slateway-core       sessions, channels, pairing, clock sync, stats (platform-agnostic)
crates/slateway-media      FFmpeg and Opus wrappers, buffer pools
crates/slateway-platform   platform traits and shared types only
crates/slateway-win        Windows backends (cfg windows)
crates/slateway-linux      Linux backends (cfg linux)
crates/slatewayd           daemon binary
crates/slateway-cli        control CLI
android/                   Kotlin app, one module, packages per channel
drivers/windows-idd/       C++ Indirect Display Driver
tools/mock-tablet/         scripted tablet client for tests
fuzz/                      cargo-fuzz targets
```

## Platform traits (`slateway-platform`)

| Trait | Responsibility |
|---|---|
| `Transport` | Produce a duplex byte stream to the tablet (adb reverse, USB tether, later AOA/Wi-Fi). The transport decides who connects; handshake roles are fixed (host responds). |
| `DisplayBackend` | List monitors; create and destroy a virtual display; provide its frame source. |
| `CaptureSource` | Deliver frames from a monitor or virtual display, ideally as GPU textures. |
| `Encoder` | Low-latency video encode (NVENC, QSV/VAAPI, x264 fallback). |
| `InputInjector` | Pointer, touch and pen injection mapped to a target display. |
| `AudioCapture` | Capture PC output audio (loopback). |
| `VirtualMic` / `VirtualCamera` | Expose tablet audio and video as system devices. |

## Data flow

- **Video:** capture, encode, send; on the tablet decode and render the newest frame.
- **Input:** MotionEvent, packed event, send immediately, host maps to the target display and injects.
- **Audio and camera:** capture, Opus or H.264, send, host decodes into the virtual device.

## Threading

Tokio for control, network and IPC. Dedicated OS threads for capture, encode, send and for the receive side. Stages connect through bounded channels (capacity 2).

## Session lifecycle

`Disconnected → Connecting → Authenticating → Negotiating → Streaming → Reconnecting`. Cable pull moves to Reconnecting; virtual displays and devices are torn down if the session does not return.

## Latency rules (invariants)

1. Pipeline queues hold at most 2 frames. When backed up, skip new captures and lower bitrate. Never drop encoded frames mid-GOP.
2. No B-frames, no lookahead, one-frame VBV, low-latency encoder presets, on-demand IDR frames.
3. GPU zero-copy from capture to encoder where available. CPU copies only in the fallback path.
4. No allocation, locking or blocking I/O in per-frame or per-event hot paths. Use buffer pools.
5. Input is sent immediately with `TCP_NODELAY`. Batch only events that are already available.
6. The tablet renders the latest decoded frame and discards stale ones. No video jitter buffer on USB.
7. Audio has a small adaptive jitter buffer (20 to 60 ms) independent of video.
8. Every stage stamps timing into stats for the latency overlay. Instrumentation is not optional.
9. Use monotonic clocks only.

## Tech stack

Library names are starting points. Verify maintenance and license before adopting (`AGENTS.md` rule 5). Rationale: `DECISIONS.md`.

| Area | Choice |
|---|---|
| Host | Rust (stable), Cargo workspace; C++ only for the Windows driver (ADR-002) |
| Codecs | FFmpeg via maintained Rust bindings: H.264 first; Opus for audio |
| Control encoding | Protobuf (`prost`); fixed binary layouts for hot paths (ADR-004) |
| Crypto | Noise_XX via `snow`; Android library to be chosen in spike S3 (ADR-006) |
| Windows | `windows` crate; UMDF IDD driver |
| Linux | `pipewire`, `ashpd` (portals), uinput, `v4l` |
| Android | Kotlin, minSdk 29, MediaCodec, Camera2, AAudio/Oboe |
| CI | GitHub Actions: Windows, Linux, Android |
