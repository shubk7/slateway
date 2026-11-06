# Protocol (draft v0)

One ordered, reliable byte stream per session, carrying typed channels. Encryption and authentication wrap the whole stream (`SECURITY.md`). All integers are little-endian.

## Transports

| Transport | Who connects | Notes |
|---|---|---|
| `adb reverse` | Tablet connects to host loopback | Development default. Needs USB debugging. |
| USB tethering | Host connects to tablet's tether address | No debugging needed. |
| AOA | Host (USB accessory mode) | Later. |
| Wi-Fi | Either | Later (N1). UDP or QUIC for media. |

Default port 47820, configurable. Listeners bind to loopback or the specific tether interface, never all interfaces.

## Frame header (16 bytes)

| Offset | Size | Field | Notes |
|---|---|---|---|
| 0 | 1 | channel | See below |
| 1 | 1 | flags | bit0 keyframe or priority; bit1 more-fragments (reserved for datagram transports); rest zero |
| 2 | 2 | type | Message type within the channel |
| 4 | 4 | length | Payload bytes, at most 16 MiB |
| 8 | 8 | pts_us | Sender's monotonic clock in microseconds; 0 if not applicable |

Parsers must reject oversized lengths and unknown channels, and never allocate based on length before validating it.

## Channels

| ID | Name | Direction | Payload |
|---|---|---|---|
| 0 | control | both | Protobuf |
| 1 | video | PC to tablet | H.264 Annex-B access units |
| 2 | input | tablet to PC | Packed input events |
| 3 | audio-out | PC to tablet | Opus |
| 4 | audio-in | tablet to PC | Opus |
| 5 | camera | tablet to PC | H.264 Annex-B |
| 6 | clipboard | both | Protobuf |
| 7 | tablet-screen | tablet to PC | H.264 (X2) |
| 8 | file | both | Reserved (X3) |
| 9-15 | reserved | | |
| 16+ | experimental | | Not stable |

## Handshake and versioning

1. After the encrypted channel is up: `Hello` (protocol major.minor, app version, device model, Android API, supported codecs and decode limits, screen size and refresh rate, capability bitmask), answered by `HelloAck`.
2. Pairing or key verification (`SECURITY.md`).
3. `Configure`: mode (mirror, extend, drawing-tablet), target display, resolution, fps, bitrate, codec, rotation.
4. `StreamStart`. A resolution or rotation change sends a new `Configure` followed by an IDR frame.

Major mismatch is refused with an error. Minor changes are additive only. Protobuf tags are never reused. Unknown control messages are ignored if marked optional, otherwise they are an error.

## Control messages

| Message | Purpose |
|---|---|
| `Hello` / `HelloAck` | Capability exchange |
| `PairRequest` / `PairConfirm` | Pairing, consent grants |
| `Configure` / `StreamStart` / `StreamStop` | Session setup |
| `RequestKeyframe` | Tablet asks for an IDR after loss or decoder reset |
| `Ping` / `Pong` | Four-timestamp clock sync, once per second |
| `Stats` | Tablet reports decode time and queue depth |
| `DeviceEvent` | Rotation, power and lifecycle changes |
| `Close` | Orderly shutdown with a reason code |

## Clocks

The host monotonic clock is the session reference. The host estimates the tablet offset from `Ping`/`Pong` using the minimum-RTT samples. Tablet-originated media is converted to host time on arrival. pts is relative to session start.

## Input event (16 bytes, packed in batches)

| Offset | Size | Field | Notes |
|---|---|---|---|
| 0 | 1 | kind | 0 down, 1 move, 2 up, 3 hover, 4 cancel |
| 1 | 1 | tool | 0 finger, 1 pen, 2 eraser |
| 2 | 1 | buttons | bit0 and bit1 pen barrel buttons |
| 3 | 1 | pointer_id | Multi-touch id |
| 4 | 2 | x | 0 to 65535 across the streamed display or configured area |
| 6 | 2 | y | Same |
| 8 | 2 | pressure | 0 to 65535, 0 for fingers |
| 10 | 1 | tilt_x | Degrees, signed |
| 11 | 1 | tilt_y | Degrees, signed |
| 12 | 4 | dt_us | Offset from the frame's pts_us |

The tablet handles its own rotation; coordinates are always in the stream's frame.

## Media

- **Video:** H.264 High profile, Annex-B, SPS/PPS before every IDR, no B-frames.
- **Audio:** Opus, 48 kHz. 10 ms frames PC to tablet, 20 ms frames for the mic.
- **Camera:** H.264 from the tablet encoder. `Configure` carries rotation and mirror flags.
- **Clipboard:** UTF-8 text, at most 1 MiB, origin id and sequence number to prevent echo loops.

## Backpressure

| Channel | Policy |
|---|---|
| video | Skip new captures when more than 2 frames are queued, adapt bitrate |
| input | Never dropped; coalesce move events if the queue grows |
| audio | Drop oldest, conceal the gap |
| camera | Skip until the next keyframe |
