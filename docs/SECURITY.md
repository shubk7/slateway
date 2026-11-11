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
2. Both sides derive a 6-digit code from the handshake hash. A side that needs the pairing confirmed sends `PairNeeded` as its first control frame (`PROTOCOL.md`, "Handshake and versioning"), and the receiver then runs its own prompt - so the code is always visible and confirmed on BOTH screens whenever either side requires pairing, including when only one side lost its state (tablet reinstalled, host registry wiped).
3. The host stores the tablet's public key and name; a known tablet presenting a changed key is a hard failure. The tablet stores the keys of the hosts it has paired with - an unknown host restarts pairing - with its own private key wrapped by the Android Keystore. Neither side stores a key without its own confirmed prompt.
4. Later connections require the pinned key.

## Consent

Only display is enabled after pairing. Input injection, mic, camera and clipboard are granted individually in a host prompt at pairing time, stored per device, and revocable from the CLI or tray. Indicators: tray icon state for active channels, Android foreground notification, and the system mic and camera indicators.

A kill switch (`slateway disconnect`, tray menu) ends the session immediately.

## Local surfaces

- CLI IPC: Unix socket mode 0600, or a named pipe ACL'd to the current user.
- Windows driver: user-mode (UMDF), admin needed only at install, control interface restricted to the daemon.
- Linux: the daemon runs as the user. Required udev rules or groups are installed explicitly by the user, never silently.

## Privacy

No telemetry, analytics, ads or accounts. Logs stay local and never contain frame, input, audio or clipboard contents, or keys. Crash reports are local files the user may choose to share.
