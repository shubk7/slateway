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

## Results log

(none yet)
