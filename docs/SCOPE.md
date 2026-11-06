# Scope

## Vision

One free platform for bidirectional I/O between a Windows/Linux PC and an Android tablet: screen, touch, pen, audio, mic, camera and clipboard, over a single link.

## Goals

- Free and open source (GPLv3). No ads, trackers, telemetry or accounts.
- Linux is a first-class platform, not an afterthought.
- USB-first for stability. Wireless comes later.
- Broad feature set in one app, built as independent channels over one protocol.
- Low latency, measured rather than assumed (`TESTING.md`).

## Non-goals

iOS or macOS support, cloud services, streaming over the internet, DRM or HDR content, multiple tablets at once (initially), and remote control of the tablet as a core feature (optional, X2).

## Principles

- Privacy by default; nothing leaves the local link.
- Explicit consent for input injection, mic, camera and clipboard (`SECURITY.md`).
- Reliable over fancy: reconnect gracefully, clean up virtual devices on exit or crash.
- Parity across OSes where the OS allows it; document the gaps where it does not.
- No setup friction beyond what the platform forces. Long term, no USB debugging requirement.

## Terms

- **Mirror:** the tablet shows an existing PC monitor.
- **Extend:** the tablet is an additional, virtual monitor.
- **Drawing-tablet mode:** the tablet sends pen/touch to the PC with no video.

## Supported platforms

- Host: Windows 11 (Windows 10 for everything except the virtual camera), and Linux with Wayland or X11 (KDE Plasma and GNOME first).
- Tablet: Android 10 or newer (API 29).
- Reference devices: `TESTING.md`.

## Features

| ID | Feature | Direction | Priority |
|---|---|---|---|
| D1 | Mirror | PC to tablet | Core |
| D2 | Extend (virtual display) | PC to tablet | Core |
| T1 | Touch as pointer | tablet to PC | Core |
| T2 | Pen: pressure, tilt, hover, eraser | tablet to PC | Core |
| T3 | Drawing-tablet mode | tablet to PC | Next |
| A1 | PC audio on tablet speakers | PC to tablet | Next |
| A2 | Tablet mic as PC microphone | tablet to PC | Next |
| X1 | Clipboard sync (text) | both | Next |
| C1 | Tablet camera as PC webcam | tablet to PC | Later |
| X2 | Tablet screen viewed on PC, optional control via scrcpy | tablet to PC | Later |
| X3 | File transfer | both | Later |
| N1 | Wi-Fi transport | both | Later |

Priority says what matters most; build order is in `ROADMAP.md`. Smaller features the maintainer thinks of later get an ID here first.
