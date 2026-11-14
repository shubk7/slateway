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

## Noise records

Everything on the stream is encrypted with `Noise_XX_25519_ChaChaPoly_SHA256`
(`SECURITY.md`), carried in length-prefixed records:

| Offset | Size | Field |
|---|---|---|
| 0 | 2 | record length, little-endian, at most 65535 |
| 2 | len | Noise ciphertext |

65535 bytes is the Noise protocol's maximum message size. Decrypted
record payloads concatenate into a plain byte stream: handshake
messages while the three-message XX handshake is in progress (-> e,
<- e, ee, s, es, -> s, se; the tablet initiates and the host responds,
roles fixed), then 16-byte-header frames (see below) once both sides
are in transport mode. Both sides derive the 6-digit pairing code from
the handshake hash: first 4 bytes, big-endian, mod 1,000,000,
zero-padded.

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

1. After the encrypted channel is up, a side that needs the pairing
   code confirmed sends `PairNeeded` (type 7, empty) as its FIRST
   control frame; the receiver then runs its own code prompt, so the
   code - derived from the handshake hash on both sides, never on the
   wire - is visible and confirmed on both screens whenever either
   side requires pairing (`SECURITY.md`, "Pairing"). Cases: the
   tablet does not trust the host (tablet sends `PairNeeded`); the
   host does not know the tablet (host answers the tablet's `Hello`
   with `PairNeeded`, and the tablet resends `Hello` after its
   prompt); both at once (the host prompts on receiving
   `PairNeeded`). Neither side stores a key without its own
   confirmed prompt.
2. After pairing is settled: `Hello` (protocol major.minor, app version, device model, Android API, supported codecs and decode limits, screen size and refresh rate, capability bitmask), answered by `HelloAck`.
3. `Configure`: mode (mirror, extend, drawing-tablet), target display, resolution, fps, bitrate, codec, rotation.
4. `StreamStart`. A resolution or rotation change sends a new `Configure` followed by an IDR frame.

Major mismatch is refused with an error. Minor changes are additive
only (minor 1 adds `PairNeeded`, minor 2 adds the video session
messages `Configure`, `StreamStart`, `StreamStop`, `RequestKeyframe`).
Protobuf tags are never reused. Unknown control messages are ignored
if marked optional, otherwise they are an error.

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

The host monotonic clock is the session reference. The host sends Ping
(type 4) once per second with its send time t0; the tablet answers
Pong (type 5) with t0 echoed, its receive time t1 and its send time
t2; the host stamps t3 on arrival. The host estimates the tablet
offset as ((t1 - t0) + (t2 - t3)) / 2 and the RTT as
(t3 - t0) - (t2 - t1), keeping the minimum-RTT sample. All timestamps
are monotonic microseconds in each side's own timebase; the offset
absorbs the epoch difference. Tablet-originated media is converted to
host time on arrival. pts is relative to session start.

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

## Input session flow (M1, touch as pointer)

1. The tablet sends input batches on channel 2 (type 1) only after
   the HelloAck announced capability bit0 — without it the host is
   discarding input, so the tablet keeps its touches local.
2. The tablet extracts every MotionEvent into packed events (kind,
   tool, pointer id, 0..65535 coordinates across the streamed view,
   pressure for pen tools) and coalesces move runs per pointer before
   writing a batch.
3. The host's input worker applies the contact policy: the first
   contact to go down is primary and drives the OS pointer (Windows
   `SendInput` absolute mouse; Linux RemoteDesktop portal pointer
   notifications) until it lifts. Additional simultaneous contacts
   are ignored in M1 — no pointer jumps, no stuck buttons. Pen
   pressure and tilt pass through the wire but only the pen work in
   M3 consumes them.
4. On session end the host releases held buttons so no contact stays
   stuck.

## Control schemas

Control payloads are protobuf (proto3, package `slateway.control.v0`). The
normative schema is `crates/slateway-proto/proto/control.proto`; host and
tablet both generate code from that one file. The frame header's `type`
field selects the message:

| type | Message | Direction |
|---|---|---|
| 1 | Hello | tablet to host |
| 2 | HelloAck | host to tablet |
| 3 | Close | both |
| 4 | Ping | host to tablet |
| 5 | Pong | tablet to host |
| 6 | Stats | tablet to host |
| 7 | PairNeeded | both |
| 8 | Configure | host to tablet |
| 9 | StreamStart | host to tablet |
| 10 | StreamStop | host to tablet |
| 11 | RequestKeyframe | tablet to host |
| 12+ | unassigned | |

Per-channel message types (one value per channel so far):

| Channel | type | Payload |
|---|---|---|
| video (1) | 1 | One H.264 Annex-B access unit |
| input (2) | 1 | One batch of packed 16-byte events |

Type numbers are never reused. Unknown control types are an error in
draft v0 (both sides run the same version). Capability bitmasks have all
bits reserved in M0; bits are documented here as they get assigned:

| Bit | Announced in | Meaning |
|---|---|---|
| 0 | HelloAck (host) | The host accepts input-channel batches — the operator passed `--allow-input` (`SECURITY.md`, "Consent"). The tablet sends touch only while this bit is set. |

## Media

- **Video:** H.264 High profile, Annex-B, SPS/PPS before every IDR, no B-frames. One video-channel frame carries exactly one access unit (type 1); header flag bit0 marks IDR keyframes. `pts_us` is the capture timestamp relative to session start. The first AU of a stream (and of every new `Configure`) is a keyframe, so the tablet can join or re-join without history.
- **Audio:** Opus, 48 kHz. 10 ms frames PC to tablet, 20 ms frames for the mic.
- **Camera:** H.264 from the tablet encoder. `Configure` carries rotation and mirror flags.
- **Clipboard:** UTF-8 text, at most 1 MiB, origin id and sequence number to prevent echo loops.

## Video session flow (minor 2)

1. After `HelloAck`, in M1 the host auto-streams when mirroring is
   enabled (`slatewayd --mirror`): `Configure` (mode `Mirror`,
   streamed geometry, fps, codec; `bitrate_kbps` 0 means the encoder's
   geometry default), then `StreamStart`, then video AUs. Both control
   frames queue before the first AU, so wire order holds.
2. A resolution or rotation change sends a new `Configure` followed by
   an IDR frame (a fresh encoder open; SPS/PPS ride in the keyframe).
3. After a decoder reset the tablet sends `RequestKeyframe`; the host
   re-opens its encoder, which restarts the stream at SPS/PPS + IDR.
4. `StreamStop` ends the stream; the tablet releases its decoder.

## Backpressure

| Channel | Policy |
|---|---|
| video | Skip new captures when more than 2 frames are queued, adapt bitrate |
| input | Never dropped; coalesce move events if the queue grows |
| audio | Drop oldest, conceal the gap |
| camera | Skip until the next keyframe |
