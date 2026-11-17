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
- Cable pull and replug: the tablet notification walks through
  "reconnecting (attempt N/40)" and the stream resumes on replug
  without touching the app; the host re-registers adb reverse while
  no session is live. Confirm no portal dialog spam (one consent per
  session) and no stuck pointer buttons after the pull.
- Record results with date and commit in the results log.

## Frame-flow heartbeats (freeze diagnosis)

The mirror pipeline has three places where frames can silently stop:
the compositor (damage-based delivery — a static screen legitimately
produces nothing), the capture worker (buffers skipped for
DMA-BUF/corruption/geometry), and the pump (blocked or wedged). The
heartbeats name the stage that stopped; log lines carry counters
only, never frame or input content (docs/SECURITY.md):

- Host, every 2 s — `video: capture 2s: callbacks=… published=…
  nobuf=… nodata=… badchunk=… mismatch=…` (PipeWire worker): zero
  callbacks while the desktop visibly changes means the compositor
  delivered nothing; nonzero `nobuf` means process fired but no
  buffer could be dequeued (producer pool empty); nonzero
  `nodata`/`badchunk`/`mismatch` with low `published` means the
  worker skipped frames. (2026-11-17 run: the freeze showed
  `callbacks=0` for ~40 s with the loop provably alive — the
  compositor stopped delivering buffers.)
- Host, every 2 s — `video: encode 2s: encoded=… skipped=… errs=…
  total=…` (pump): capture publishing while `encoded` stays 0 puts
  the stall at or after the encoder.
- Host, per injection — `input: Down motion=… press=… us`,
  `input: Up release=… us`, and a 1 s `input: moves=… max=… us`
  heartbeat: a notify round trip that hangs shows up as a missing or
  huge-duration line.
- Tablet, every 1 s — `video: N frames in the last 1 s (total M)`
  (watchdog thread): runs even while the reader thread is blocked in
  readFrame, so it distinguishes "host stopped sending" from
  "tablet stopped decoding".

## Hardware decode

The decoder opens with a ladder (format variants, then every AVC
decoder by name) and dumps the AVC capability inventory once on any
rejection. On Tab A7 (SM-T505, Android 14) `OMX.qcom.video.decoder.avc`
advertises 1920x1200 yet rejects configure with `CodecException
code=-1010 diag=…error_neg_1010` in every format variant, while
`c2.android.avc.decoder` (software) opens and decodes — the current
source of mirror lag/ghosting. One probe runs per decoder instance to
narrow the cause: it configures every hardware AVC decoder WITHOUT
the surface (ByteBuffer output, `COLOR_FormatYUV420Flexible`,
csd from the keyframe) at the streamed size and at two smaller
fallbacks (1920x1080, 1280x720), releasing each candidate. Opens at
smaller sizes pin the rejection on the streamed size; failures
everywhere point at a device quirk. 2026-11-17: the probe fails
without a surface too, so the surface is not the cause. Log lines:
`decoder diagnostic: '<name>' …`.

## Results log

(none yet)
