# Platform notes

Starting points, not guarantees. Verify each API against current documentation before relying on it (`AGENTS.md` rule 4). Open choices are in `DECISIONS.md`.

## Windows

| Area | Approach |
|---|---|
| Virtual display | Indirect Display Driver (IddCx, UMDF) built from an existing open-source IDD with a compatible license (spike S2) |
| Capture | Windows.Graphics.Capture into a D3D11 staging copy (`slateway-win::WgcCapture`); evaluate taking frames from the IDD swapchain |
| Encode | `h264_nvenc` first, `h264_qsv` and `h264_amf` next, `libx264` ultrafast zerolatency as fallback |
| Input | Synthetic pointer injection (`CreateSyntheticPointerDevice`, `InjectSyntheticPointerInput`) with touch and pen types, mapped to the target monitor rectangle |
| Virtual camera | Media Foundation virtual camera API, Windows 11 only. Windows 10 unsupported initially |
| Virtual mic | Undecided (spike S5). Needs a virtual audio driver or an existing signed one |
| Audio capture | WASAPI loopback |
| Dev driver install | Test signing, which requires Secure Boot off |

## Linux

| Area | Approach |
|---|---|
| Capture | xdg-desktop-portal ScreenCast into PipeWire (`slateway-linux::PortalCapture`), DMA-BUF where possible |
| Virtual display | Portal virtual-monitor source where the compositor supports it (check GNOME and KDE in spike S1). X11 fallback via xrandr monitors or a dummy output. EVDI is a possible fallback but is an out-of-tree kernel module |
| Pointer and touch | RemoteDesktop portal (handles output mapping automatically) |
| Pen | uinput tablet device with pressure and tilt, since portals do not carry pen data. Needs `/dev/uinput` access via a udev rule. Mapping to a specific output is compositor-specific |
| Virtual camera | `v4l2loopback` (DKMS, needs kernel headers, may need module signing under Secure Boot) |
| Virtual mic | PipeWire source node created by the daemon |
| Audio capture | PipeWire monitor of the default sink |
| Encode | `h264_nvenc`, VAAPI, `libx264` fallback |

## Android

- **Decode:** MediaCodec rendering to a SurfaceView. Use the low-latency key on API 30+ and vendor-specific hints on API 29. Release the newest output buffer and drop stale ones.
- **Input:** `requestUnbufferedDispatch`, historical MotionEvent samples, hover through generic motion events. Read tool type, pressure, tilt, orientation and distance axes. Prefer stylus over finger for palm rejection.
- **Foreground services:** declare `camera`, `microphone` and `mediaProjection` service types (Android 14+), with a persistent notification.
- **Display:** immersive mode, keep screen on, handle rotation by remapping coordinates.
- **Audio:** AAudio or Oboe for low-latency playback and capture.
- **Camera:** Camera2 into the MediaCodec encoder surface.

## Known gotchas

- **Hybrid GPUs:** capturing on the iGPU and encoding on the dGPU forces a frame copy. Display routing depends on MUX settings. Test both paths.
- **USB power:** a laptop port may not out-supply the tablet at full brightness while decoding. Expect slow discharge.
- **USB 2.0 tablets:** ample for compressed video, ruling out raw frames.
- **Secure Boot:** off for Windows test signing. It also affects out-of-tree Linux modules such as `v4l2loopback`.
- **S Pen:** confirm which axes the Tab S6 Lite pen reports, especially tilt (spike S6).
