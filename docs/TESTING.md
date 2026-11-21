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
| Galaxy Tab S6 Lite (SM-P620, 2024) | Pen reference | S Pen, USB 2.0, 1920x1200 at 60 Hz (streamed resolution confirmed 2026-11-19), Exynos hardware AVC decoder |
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
- Host, every 2 s — `video: encode 2s: encoded=… repeated=… skipped=…
  errs=… total=…` (pump): capture publishing while `encoded` stays 0
  puts the stall at or after the encoder; `repeated` counts the AUs
  that re-sent an unchanged frame to hold the pacing floor.
- Host, per injection — `input: Down motion=… press=… us`,
  `input: Up release=… us`, and a 1 s `input: moves=… max=… us`
  heartbeat: a notify round trip that hangs shows up as a missing or
  huge-duration line.
- Tablet, every 1 s — `video: N frames in the last 1 s (total M)`
  (watchdog thread): runs even while the reader thread is blocked in
  readFrame, so it distinguishes "host stopped sending" from
  "tablet stopped decoding".

## Frame pacing

Capture is damage-driven (the compositor delivers a buffer only when the
screen changes), so the pump owns the delivery rate: it holds the newest
frame and sends exactly one frame per period (`--fps N`, default 60;
`--fps 0` = damage-driven, the pre-pacing baseline). A period with no
new damage re-sends the held frame, so a static screen keeps the
tablet's decoder ticking instead of going silent (ADR-015).

The policy is `tick`: one frame per period, the newest captured frame at
each tick. That gives an even cadence and a hard cap at `fps`, at up to
one period of frame age (16.7 ms at 60). It never catches up after a
stall — one send, then a full period of quiet — and a keyframe request
sends immediately instead of waiting for the next tick.

Repeats are re-encoded, not resent, so a static screen costs almost
nothing in bitrate but keeps the encoder and decoder ticking; each
repeat carries a fresh pts so the tablet's stats see send time, not
content age. The `video: encode 2s:` heartbeat shows
`encoded=… repeated=… skipped=… errs=…`.

The pacer advances on an absolute grid (next deadline = previous +
period), so a late wake-up shortens the following wait instead of
adding to every period; a send that misses whole periods rebases
instead of bursting. On Windows the capture also raises the timer
resolution to 1 ms for the session (the default 15.625 ms tick clamps
every shorter wait and capped `--fps 90` at ~64 fps); see
`PLATFORMS.md`, "Known gotchas". Verify with the heartbeat: at
`--fps 90` it should report ~180 encoded per 2 s window, not ~129.

### Why tick, not fill (A/B on 2026-11-19)

The spike also implemented `fill` (fresh frames never wait; repeats hold
the floor) and both shipped behind `--pacing` so one APK covered the
whole A/B. Three runs on the S6 Lite (SM-P620, Linux host, NVENC,
1920x1200, hardware `csd+low-latency` decoder), same ~2 minute routine
(scroll, drag, video, typing), host counters plus the tablet's 1 s
watchdog. The host panel is 1920x1200 at 165 Hz and the tablet is
60 Hz, so 60 is the ceiling the stream is paced to; `fill` can only
overshoot as far as the compositor's damage rate allows (165 Hz here):

| run | tablet fps (avg / median / min–max / stdev) | windows < 30 fps | bitrate | repeats |
|---|---|---|---|---|
| `--fps 0` | 24.5 / 17.5 / 0–117 / 26.5 | 93 of 132 | not measured (stopped by hand) | 0% |
| `--fps 60 --pacing fill` | 72.4 / 72.0 / 58–107 / 9.0 | 0 of 81 | 7.10 Mbit/s | 50% |
| `--fps 60 --pacing tick` | 59.9 / 60.0 / 58–62 / 0.75 | 0 of 98 | 5.28 Mbit/s | 99% |

The maintainer felt `fill` and `tick` almost the same, and both much
better than the damage-driven baseline, whose idle gaps (the watchdog
saw under 30 frames in 93 of 132 one-second windows, many of them 0)
are the stutter. `fill` did exactly
what was predicted: the compositor bursts pushed the stream to 72 fps
against a 60 fps request. `tick` won on the invariants — hard cap, even
cadence (stdev 0.75 fps), no catch-up burst, 26% less bitrate — so the
`fill` path and the `--pacing` flag were removed (ADR-015).

Note for the next run: the damage-driven baseline ended with the portal
delivering 1920x1160 buffers while the encoder was opened for 1920x1200,
which the encoder rejects ("re-open on resolution change"); the stream
stayed at zero encodes until the daemon was stopped. Geometry changes
need the planned encoder re-open; this is not a pacing regression.

### Pacing checks (other devices and rates)

1. Run `--fps 0`, `--fps 30` and `--fps 60` through the same routine
   (one variable at a time; the APK never changes, only the daemon).
2. Record per run: the `video: encode 2s:` lines (encoded/repeated/
   skipped/errs), the tablet's 1 s watchdog (must stay near the pacing
   rate, not oscillate), felt smoothness and latency, and glass-to-glass
   p95 if measured (M1 exit criterion <= 80 ms).
3. `--fps 30` is the fallback for devices that struggle to decode 60.

## Hardware decode

The decoder opens with a ladder through every AVC decoder on the
device by name, in the platform's REGULAR_CODECS order (the platform
default is the first entry), and dumps the AVC capability inventory
once on any rejection. The format variants are ordered by the
decoder's hardware acceleration: hardware decoders try
`csd+low-latency` first (the desirable mode, spike S4); software
decoders try plain `csd` first — the A/B test for the Tab A7 freeze,
whose software `c2.android.avc.decoder` opened with `KEY_LOW_LATENCY`
and is suspected to hold the last access unit unrendered until the
next one arrives ("frozen until I touch"). Collect on the next A7
run: whether the freeze changes with the reordered ladder, and
whether the tablet watchdog reports >0 frames that never appear on
screen (decoder-held frame) vs 0 (host-side).

On Tab A7 (SM-T505, Android 14) `OMX.qcom.video.decoder.avc`
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
`decoder diagnostic: '<name>' …`. The A7 freeze investigation is
paused (2026-11-19, results log); the reordered ladder stays because
it is the safer default for software decoders.

## Results log

- 2026-11-16 · commit 402dffc · Tab A7 (SM-T505) · Linux host: fullscreen
  toggle verified working by the maintainer; touch-as-pointer verified
  (clicks land where touched). Open issues: mirror freezes on the tablet
  after each click until the next touch; lag and ghosting (tablet runs the
  software `c2.android.avc.decoder`; `OMX.qcom.video.decoder.avc` rejects
  configure with `error_neg_1010` in every format variant).
- 2026-11-17 · commits 72a61d3/ea0fe3e · Tab A7 (SM-T505) · Linux host:
  instrumented run. Pipeline clean end to end: capture `callbacks ==
  published == encoded`, no skips, no errors, no backpressure; the
  tablet's 1 s watchdog tracked the host's encode counts exactly. Freeze
  windows showed the compositor delivering 0 buffers for ~40 s with the
  capture loop provably alive (heartbeats kept printing); each click
  produced only ~2–3 frames (the cursor-jump repaint). Injection healthy
  (0.7–1.5 ms per notify; no hung portal calls). Decoder diagnostic: qcom
  -1010 fails WITHOUT the surface too — the surface is not the cause.
- 2026-11-17 · Tab S6 Lite (SM-P620) · Linux host: maintainer report —
  no stutters, no lag, no ghosting, touch input works well.
- 2026-11-19 · A7 investigation paused: the Tab A7 (SM-T505) freezes
  and ghosts while the Tab S6 runs the same build clean, so the
  maintainer reads it as device/ROM-specific (the A7 runs a custom ROM)
  rather than a pipeline defect. The A7 stays the low-end baseline for
  decode load, but no further freeze work until it matters for the
  milestone. S6 is the primary day-to-day test device by maintainer
  preference, not a project-wide priority choice.
- 2026-11-19 · Tab S6 Lite (SM-P620) · Linux host: frame-pacing A/B
  (`--fps 0`, `--fps 60 --pacing fill`, `--fps 60 --pacing tick`), same
  routine each run. Damage-driven: 24.5 fps average, 0 frames in most
  idle seconds, 3 encode errors when the portal switched to 1920x1160
  buffers at the end. `fill`: 72.4 fps (compositor bursts over the
  60 fps request), 7.10 Mbit/s, 50% repeats, no skips or errors.
  `tick`: 59.9 fps (median 60, stdev 0.75, 58–62), 5.28 Mbit/s, 99%
  repeats, no skips or errors. Hardware `csd+low-latency` decoder on
  every run; the tablet's watchdog tracked the host's encodes with no
  stall windows. Maintainer felt `fill` and `tick` almost identical,
  both clearly better than `--fps 0`. Chose `tick`; `fill` and
  `--pacing` removed, ADR-015.
