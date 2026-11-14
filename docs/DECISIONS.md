# Decisions

Format: decision, why, revisit trigger. New decisions are appended here; pending ones are not to be decided by agents (`AGENTS.md` rule 8).

| ADR | Decision | Why | Revisit if |
|---|---|---|---|
| 001 | GPLv3, SPDX `GPL-3.0-or-later` | Copyleft keeps forks open; compatible with FFmpeg GPL builds and x264. Change to `GPL-3.0-only` if the maintainer prefers | Contributors object |
| 002 | Host in Rust, Windows driver in C++, Android in Kotlin | Strict compiler gives AI agents a reliable feedback loop; simple cross-platform builds; safe parsing of untrusted bytes; bindings exist for all required APIs | Key bindings prove unmaintained |
| 003 | Single custom Android app; adb is only a USB tunnel, scrcpy is an optional module | scrcpy streams tablet to PC, the opposite of the headline feature. A custom app avoids shell-privilege tricks and one protocol covers everything | Control of the tablet becomes a core need |
| 004 | One multiplexed connection; Protobuf for control, fixed binary for input and media | Tooling and evolvability for control; no per-event allocation on hot paths | Protobuf overhead measured as significant |
| 005 | Build mirror before extend | De-risks capture, encode, decode and latency without a virtual display driver | Never |
| 006 | Always-encrypted, authenticated sessions (Noise_XX) | Defends against rogue tablet apps via `adb reverse`; one code path for USB and Wi-Fi | No good Android Noise library (spike S3): use mutual TLS with pinned certs |
| 007 | H.264 first, FFmpeg for codecs | Universal tablet hardware decode; HEVC and AV1 can be added through capability negotiation | Tablets prove to need a different baseline |
| 008 | Headless daemon plus thin CLI, tray UI later | Keeps core testable and UI-toolkit choice out of the critical path | Never |
| 009 | Android minSdk 29 | Covers the reference tablets and broad devices; low-latency decode hints improve on API 30+ | Reference devices cannot run it |
| 010 | Transport order: adb reverse, then USB tethering, then AOA, then Wi-Fi | Fastest to build and test first; removing the USB-debugging requirement later | Tethering proves unreliable on Windows |
| 011 | Android applicationId `org.slateway.app` | Matches the project name; provisional value confirmed for the M0 shell before anything ships | Play Store or F-Droid demands a different id |
| 012 | Control messages in proto3 with one shared schema (`crates/slateway-proto/proto/control.proto`), dispatched by the frame header's `type` number | ADR-004 chose protobuf; one schema file keeps host and tablet byte-identical; type numbers are never reused (PROTOCOL.md) | Wire format needs a redesign |
| 013 | Spike S3 result: snow on both sides — the Android app loads a Rust `.so` (snow 0.10.0, rev 34, Apache-2.0 OR MIT, Trail of Bits audited) through a small JNI facade, built with cargo-ndk | One maintained, audited implementation on host and tablet gives wire interop by construction. noise-java is unmaintained since 2022 (rev 30, not on Maven); mutual TLS needs cert/pinning glue and stays the fallback per ADR-006 | snow loses maintenance, or the NDK build chain proves too costly for CI |
| 014 | Host encoder: FFmpeg via `ffmpeg-next` 9.1 (WTFPL, GPLv3-compatible), candidates `h264_nvenc` then `libx264` (preset p1 + tune ull + zerolatency for nvenc, ultrafast + zerolatency for x264), BGRA/RGBA→yuv420p conversion written in safe Rust instead of libswscale (its context is `!Send` and the encoder travels with the video task); shared plumbing in `crates/slateway-ffmpeg`, platform crates only pick the candidate list | ADR-007 pinned FFmpeg; the binding was user-approved; safe-Rust conversion keeps the encoder object `Send` without new `unsafe` and stays inside the M1 latency budget | wrapper loses maintenance, or the safe conversion misses the budget at 4K/120 fps |

## Pending

- Windows virtual microphone approach (S5)
- Linux virtual monitor strategy per compositor (S1)
- Windows IDD base project (S2)
- GUI toolkit for the tray app
- Wi-Fi transport design (N1)
- HEVC or AV1 support
- `h264_vaapi` hw-frame path: needs hwcontext setup beyond what the `ffmpeg-next` wrapper exposes (ADR-014); evaluate with the video-channel task
- `h264_qsv` / `h264_amf` candidates: libavcodec's qsv wrapper wants NV12 input, amf needs its input formats verified; revisit with the video-channel task
