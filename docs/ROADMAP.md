# Roadmap

Feature IDs are defined in `SCOPE.md`. Latency targets are hypotheses: measure first, then revise them here (`TESTING.md`). Current milestone: **M0**.

## Spikes (time-boxed; record outcomes in `DECISIONS.md`)

| ID | Question | Before |
|---|---|---|
| S1 | Does the portal virtual-monitor source work on KDE Plasma and GNOME? Else X11 or EVDI fallback | M2 |
| S2 | Can an existing open-source IDD be built, test-signed and installed, and what is its license? | M2 |
| S3 | Which Noise library works on Android, or do we fall back to mutual TLS? | M0 crypto |
| S4 | MediaCodec low-latency decode behaviour on Tab A7 and Tab S6 Lite | M1 |
| S5 | Windows virtual microphone approach | M4 |
| S6 | Which MotionEvent axes does the S6 Lite pen report (tilt, hover, eraser)? | M3 |

## M0 Foundation

- [x] Repo skeleton: workspace, `rust-toolchain.toml`, `.gitattributes`, `deny.toml`, `LICENSE`, SPDX headers
- [x] CI for Windows, Linux and Android with fmt, clippy, deny
- [x] `slateway-proto`: frame codec, schemas, property tests, fuzz target
- [x] `Transport` trait, adb-reverse transport, loopback transport
- [x] Android shell: connect, foreground service, handshake
- [x] Noise handshake, pairing code, key storage
- [x] Clock sync, stats, latency overlay plumbing
- [x] `tools/mock-tablet`

**Exit:** authenticated, encrypted echo with RTT and clock offset on both tablets and both OSes; CI green.

## M1 Mirror (D1, T1)

- [ ] Capture: Linux portal and PipeWire, Windows capture API
- [ ] Encoders: NVENC, QSV or VAAPI, x264 fallback, low-latency settings
- [ ] Video channel, MediaCodec decode, newest-frame rendering
- [ ] Touch as pointer on both OSes
- [ ] Reconnect after cable pull

**Exit:** 1080p60 for 10 minutes without sustained drops; glass-to-glass p95 at most 80 ms on both tablets (stretch 60 ms on S6 Lite); touch lands within 2 px.

## M2 Extend (D2)

- [ ] Linux virtual monitor per S1; Windows IDD per S2
- [ ] Mirror/extend mode switching, resolution, refresh and DPI negotiation
- [ ] Hot-plug handling; virtual display removed on disconnect or crash

**Exit:** independent tablet screen with correct touch mapping, surviving cable pull and daemon crash.

## M3 Pen and drawing-tablet mode (T2, T3)

- [ ] Pressure, tilt, hover, eraser end to end
- [ ] Windows pen injection; Linux uinput tablet device
- [ ] Drawing-tablet mode with a configurable target area

**Exit:** pressure-sensitive strokes work in Krita on both OSes.

## M4 Audio and clipboard (A1, A2, X1)

- [ ] PC audio to tablet (Opus, small jitter buffer)
- [ ] Mic to PC: Linux PipeWire source first, Windows per S5
- [ ] Clipboard sync with echo prevention

**Exit:** audio stays within 40 ms of video; mic selectable in a browser and OBS.

## M5 Camera (C1)

- [ ] Camera2 capture, H.264 encode on tablet, host decode
- [ ] `v4l2loopback` device on Linux; Media Foundation virtual camera on Windows 11

**Exit:** tablet camera appears in a browser and OBS on both OSes.

## M6 Extras (X2, X3, N1)

- [ ] Tablet screen view on PC via MediaProjection; optional scrcpy module for control
- [ ] File transfer
- [ ] Wi-Fi transport design and implementation

## M7 Packaging and first release

- [ ] Windows installer and driver signing plan
- [ ] Linux packages (AUR first), APK via GitHub releases and F-Droid
- [ ] `CODE_OF_CONDUCT.md`, issue templates, user docs
