# Testing

## Layers

| Layer | Tooling |
|---|---|
| Unit and property tests | `cargo test`, `proptest` for the codec |
| Fuzzing | `cargo fuzz` targets for the frame parser and every control message decoder |
| Integration | Daemon against `tools/mock-tablet` over a loopback transport, software encoder only (CI has no GPU) |
| Android | JVM unit tests plus instrumented tests where possible |
| Hardware in the loop | Manual checklist on the reference devices, per milestone |

## Latency measurement

Targets are in `ROADMAP.md` exit criteria; this is how to measure them.

1. **Stage timing:** every stage stamps pts and wall-clock deltas into stats. The overlay shows p50 and p95 for capture, encode, send, receive, decode and present.
2. **Glass-to-glass:** film the PC screen and the tablet together in 240 fps slow motion with a running millisecond counter on screen. Count frames between the counter change on each.
3. **Input latency:** same method with a pen or finger tracing on the tablet while the PC cursor is visible.

Record every measured result, with date and git commit, in this file's results log below.

## Reference devices

| Device | Role | Notes |
|---|---|---|
| Laptop on Windows | Host | RTX 4050 (NVENC), Intel UHD (QSV), i7-14650HX |
| Laptop on CachyOS | Host | Same hardware |
| Galaxy Tab S6 Lite | Pen reference | S Pen, USB 2.0, 2000x1200 at 60 Hz (verify) |
| Galaxy Tab A7 | Low-end baseline | No pen, weaker decoder, USB 2.0 |

Android and One UI versions: record here once checked.

## Manual checklist (run every milestone)

Cable pull and replug, tablet screen off and on, rotation, host sleep and wake, 10-minute soak without dropped frames, daemon crash leaves no virtual display or device behind.

## M1 video verification (owed on hardware)

The CI rehearsal covers the wire and encoder logic against
`mock-tablet`'s video mode with libx264 (slatewayd's
`mirror_video` integration test, including the scripted touch stroke
against a recording injector). These stay owed on the reference
tablets (spike S4, M1 exit criteria):

- Real `--mirror` session over adb reverse: picture visible on the
  tablet, no drift after 10 minutes.
- MediaCodec low-latency decode behaviour on Tab A7 and Tab S6 Lite:
  `KEY_LOW_LATENCY` support per device, actual latency, decoder
  reconfig on resolution change.
- Rotation and resolution change: new `Configure`, new keyframe,
  decoder keeps up.
- Touch as pointer (`--allow-input`): taps land where touched on the
  mirrored monitor (±2 px M1 exit criterion), drags track, multi-
  finger touch causes no pointer jumps; verify on both tablets, both
  OSes (Linux needs the portal RemoteDesktop grant in the consent
  dialog).
- Record results with date and commit in the results log.

## Results log

(none yet)
