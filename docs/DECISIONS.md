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

## Pending

- Windows virtual microphone approach (S5)
- Linux virtual monitor strategy per compositor (S1)
- Windows IDD base project (S2)
- Android library for Noise (S3)
- GUI toolkit for the tray app
- Wi-Fi transport design (N1)
- HEVC or AV1 support
