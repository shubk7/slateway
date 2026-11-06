# Security and privacy

## Assets and threats

| Threat | Mitigation |
|---|---|
| Malicious app on the tablet reaching the host through `adb reverse` and injecting input | Every session is authenticated against pinned keys, even over USB |
| Other local processes connecting to the daemon | Bind to loopback or the tether interface only; authenticate before any channel opens |
| Network attackers (when Wi-Fi arrives) | Same authenticated, encrypted session; pairing code confirmation |
| Parser bugs from untrusted bytes | Length limits, fuzz targets for every parser, no `unsafe` without justification |
| Silent camera, mic or input abuse | Per-channel consent and visible indicators |
| Supply chain | Locked dependencies, `cargo deny` and `cargo audit` in CI, Gradle dependency verification, signed release artifacts |

## Session security

Noise_XX over the byte stream, with static keys per device. Always on, including over USB, so there is one code path. If no suitable Android Noise library is found in spike S3, switch to mutual TLS with pinned certificates (ADR-006).

## Pairing

1. First connection runs the handshake with unknown keys.
2. Both sides derive a 6-digit code from the handshake hash. The user confirms it matches on the host prompt and on the tablet.
3. The host stores the tablet's public key and name. The tablet stores the host's key, with its own private key wrapped by the Android Keystore.
4. Later connections require the pinned key. An unknown key restarts pairing; a changed key for a known device is a hard failure with a warning.

## Consent

Only display is enabled after pairing. Input injection, mic, camera and clipboard are granted individually in a host prompt at pairing time, stored per device, and revocable from the CLI or tray. Indicators: tray icon state for active channels, Android foreground notification, and the system mic and camera indicators.

A kill switch (`slateway disconnect`, tray menu) ends the session immediately.

## Local surfaces

- CLI IPC: Unix socket mode 0600, or a named pipe ACL'd to the current user.
- Windows driver: user-mode (UMDF), admin needed only at install, control interface restricted to the daemon.
- Linux: the daemon runs as the user. Required udev rules or groups are installed explicitly by the user, never silently.

## Privacy

No telemetry, analytics, ads or accounts. Logs stay local and never contain frame, input, audio or clipboard contents, or keys. Crash reports are local files the user may choose to share.
