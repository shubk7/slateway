// SPDX-License-Identifier: GPL-3.0-or-later

//! Packed input event codec (`docs/PROTOCOL.md`, "Input event").
//!
//! Each event is exactly [`EVENT_LEN`] bytes, and an input-channel
//! payload is a batch of events back to back with no extra framing, so
//! a batch payload length must be a multiple of [`EVENT_LEN`]. All
//! multi-byte fields are little-endian. Coordinates, pressure and tilt
//! are forwarded as they arrive; mapping to a target display is the
//! host's job (`docs/ARCHITECTURE.md`).

/// One input event's size in bytes.
pub const EVENT_LEN: usize = 16;

/// Pen barrel button bits in the `buttons` byte.
pub mod buttons {
    /// bit0: pen barrel button one.
    pub const BARREL_PRIMARY: u8 = 1 << 0;

    /// bit1: pen barrel button two.
    pub const BARREL_SECONDARY: u8 = 1 << 1;

    /// Documented button bits. Higher bits are not reserved by the
    /// spec; they pass through unchanged.
    pub const ALL: u8 = BARREL_PRIMARY | BARREL_SECONDARY;
}

/// What happened to the pointer: 0 down, 1 move, 2 up, 3 hover,
/// 4 cancel. Unknown kinds are rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventKind {
    /// Contact or button press started.
    Down,
    /// Position changed while in contact.
    Move,
    /// Contact or button press ended.
    Up,
    /// Pen moved above the surface without contact.
    Hover,
    /// The gesture was cancelled.
    Cancel,
}

impl EventKind {
    /// Maps a wire ID to a kind, rejecting unknown IDs.
    pub fn from_id(id: u8) -> Option<Self> {
        match id {
            0 => Some(EventKind::Down),
            1 => Some(EventKind::Move),
            2 => Some(EventKind::Up),
            3 => Some(EventKind::Hover),
            4 => Some(EventKind::Cancel),
            _ => None,
        }
    }

    /// The kind's wire ID.
    pub fn id(self) -> u8 {
        match self {
            EventKind::Down => 0,
            EventKind::Move => 1,
            EventKind::Up => 2,
            EventKind::Hover => 3,
            EventKind::Cancel => 4,
        }
    }
}

/// The tool the event came from: 0 finger, 1 pen, 2 eraser. Unknown
/// tools are rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tool {
    /// Finger touch.
    Finger,
    /// Pen contact or hover.
    Pen,
    /// The pen's eraser end.
    Eraser,
}

impl Tool {
    /// Maps a wire ID to a tool, rejecting unknown IDs.
    pub fn from_id(id: u8) -> Option<Self> {
        match id {
            0 => Some(Tool::Finger),
            1 => Some(Tool::Pen),
            2 => Some(Tool::Eraser),
            _ => None,
        }
    }

    /// The tool's wire ID.
    pub fn id(self) -> u8 {
        match self {
            Tool::Finger => 0,
            Tool::Pen => 1,
            Tool::Eraser => 2,
        }
    }
}

/// One packed input event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InputEvent {
    /// What happened to the pointer.
    pub kind: EventKind,
    /// The tool the event came from.
    pub tool: Tool,
    /// Pen barrel buttons; see [`buttons`].
    pub buttons: u8,
    /// Multi-touch pointer id.
    pub pointer_id: u8,
    /// 0 to 65535 across the streamed display or configured area.
    pub x: u16,
    /// Like [`InputEvent::x`], for the vertical axis.
    pub y: u16,
    /// 0 to 65535; 0 for fingers.
    pub pressure: u16,
    /// Degrees, signed.
    pub tilt_x: i8,
    /// Degrees, signed.
    pub tilt_y: i8,
    /// Offset from the frame's pts_us, in microseconds.
    pub dt_us: u32,
}

/// Input event parse failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InputError {
    /// The kind byte is not 0 to 4.
    #[error("unknown event kind id {id}")]
    UnknownKind {
        /// The rejected kind byte.
        id: u8,
    },
    /// The tool byte is not 0 to 2.
    #[error("unknown tool id {id}")]
    UnknownTool {
        /// The rejected tool byte.
        id: u8,
    },
    /// The batch ends with fewer than [`EVENT_LEN`] remaining bytes.
    #[error("batch ends with a partial event at byte offset {offset}")]
    Truncated {
        /// Byte offset where the partial event starts.
        offset: usize,
    },
}

/// Batch encode failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("batch needs {needed} bytes, got {got}")]
pub struct BatchEncodeError {
    /// Bytes the batch requires.
    pub needed: usize,
    /// Bytes the buffer had.
    pub got: usize,
}

/// Bytes needed to encode `count` events as one batch.
pub fn batch_len(count: usize) -> usize {
    count * EVENT_LEN
}

impl InputEvent {
    /// Encodes the event into `out`.
    pub fn encode_into(self, out: &mut [u8; EVENT_LEN]) {
        out[0] = self.kind.id();
        out[1] = self.tool.id();
        out[2] = self.buttons;
        out[3] = self.pointer_id;
        out[4..6].copy_from_slice(&self.x.to_le_bytes());
        out[6..8].copy_from_slice(&self.y.to_le_bytes());
        out[8..10].copy_from_slice(&self.pressure.to_le_bytes());
        out[10] = self.tilt_x as u8;
        out[11] = self.tilt_y as u8;
        out[12..16].copy_from_slice(&self.dt_us.to_le_bytes());
    }

    /// Encodes the event.
    pub fn encode(self) -> [u8; EVENT_LEN] {
        let mut out = [0; EVENT_LEN];
        self.encode_into(&mut out);
        out
    }

    /// Parses one event.
    pub fn parse(buf: &[u8; EVENT_LEN]) -> Result<Self, InputError> {
        let kind = EventKind::from_id(buf[0]).ok_or(InputError::UnknownKind { id: buf[0] })?;
        let tool = Tool::from_id(buf[1]).ok_or(InputError::UnknownTool { id: buf[1] })?;
        Ok(InputEvent {
            kind,
            tool,
            buttons: buf[2],
            pointer_id: buf[3],
            x: u16::from_le_bytes([buf[4], buf[5]]),
            y: u16::from_le_bytes([buf[6], buf[7]]),
            pressure: u16::from_le_bytes([buf[8], buf[9]]),
            // Single bytes, two's complement on the wire.
            tilt_x: buf[10] as i8,
            tilt_y: buf[11] as i8,
            dt_us: u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]),
        })
    }
}

/// Encodes `events` into `out`, back to back, as an input-channel
/// payload. Fails without writing anything if `out` is shorter than
/// [`batch_len`] says the batch needs.
pub fn encode_batch_into(events: &[InputEvent], out: &mut [u8]) -> Result<(), BatchEncodeError> {
    let needed = batch_len(events.len());
    if out.len() < needed {
        return Err(BatchEncodeError {
            needed,
            got: out.len(),
        });
    }
    for (i, event) in events.iter().enumerate() {
        let mut slot = [0; EVENT_LEN];
        event.encode_into(&mut slot);
        out[i * EVENT_LEN..(i + 1) * EVENT_LEN].copy_from_slice(&slot);
    }
    Ok(())
}

/// Iterates over the events in an input-channel payload without
/// allocating. Yields events in order; a payload that does not end on
/// an event boundary yields exactly one [`InputError::Truncated`] at
/// the end, and iteration stops at the first error.
pub fn parse_batch(payload: &[u8]) -> BatchIter<'_> {
    BatchIter {
        bytes: payload,
        offset: 0,
        done: false,
    }
}

/// Zero-allocation batch iterator; see [`parse_batch`].
#[derive(Clone, Debug)]
pub struct BatchIter<'a> {
    bytes: &'a [u8],
    offset: usize,
    done: bool,
}

impl Iterator for BatchIter<'_> {
    type Item = Result<InputEvent, InputError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        if self.bytes.len() < EVENT_LEN {
            self.done = true;
            // An empty tail is a clean end; a partial event is an error.
            if self.bytes.is_empty() {
                return None;
            }
            return Some(Err(InputError::Truncated {
                offset: self.offset,
            }));
        }
        let (chunk, rest) = self.bytes.split_at(EVENT_LEN);
        self.bytes = rest;
        // Invariant: split_at returned exactly EVENT_LEN bytes.
        let mut slot = [0; EVENT_LEN];
        slot.copy_from_slice(chunk);
        self.offset += EVENT_LEN;
        match InputEvent::parse(&slot) {
            Ok(event) => Some(Ok(event)),
            Err(err) => {
                self.done = true;
                Some(Err(err))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Golden layout, cross-checked byte by byte against the "Input
    /// event" table in docs/PROTOCOL.md (little-endian field order).
    #[test]
    fn golden_layout() {
        let event = InputEvent {
            kind: EventKind::Down,
            tool: Tool::Pen,
            buttons: buttons::BARREL_PRIMARY,
            pointer_id: 3,
            x: 0x1234,
            y: 0x5678,
            pressure: 0x9abc,
            tilt_x: -1,
            tilt_y: 45,
            dt_us: 0xdead_beef,
        };
        assert_eq!(
            event.encode(),
            [
                0x00, // kind: down
                0x01, // tool: pen
                0x01, // buttons: barrel primary
                0x03, // pointer id
                0x34, 0x12, // x
                0x78, 0x56, // y
                0xbc, 0x9a, // pressure
                0xff, // tilt_x: -1
                0x2d, // tilt_y: 45
                0xef, 0xbe, 0xad, 0xde, // dt_us
            ]
        );
    }

    /// The spec pins the event size; a change here is a protocol change.
    #[test]
    fn event_size_matches_spec() {
        assert_eq!(EVENT_LEN, 16);
    }

    fn event_strategy() -> impl Strategy<Value = InputEvent> {
        (
            0u8..=4,
            0u8..=2,
            any::<u8>(),
            any::<u8>(),
            any::<u16>(),
            any::<u16>(),
            any::<u16>(),
            any::<i8>(),
            any::<i8>(),
            any::<u32>(),
        )
            .prop_map(
                |(kind, tool, buttons, pointer_id, x, y, pressure, tilt_x, tilt_y, dt_us)| {
                    InputEvent {
                        kind: EventKind::from_id(kind).unwrap_or(EventKind::Down),
                        tool: Tool::from_id(tool).unwrap_or(Tool::Finger),
                        buttons,
                        pointer_id,
                        x,
                        y,
                        pressure,
                        tilt_x,
                        tilt_y,
                        dt_us,
                    }
                },
            )
    }

    proptest! {
        #[test]
        fn round_trip(event in event_strategy()) {
            assert_eq!(InputEvent::parse(&event.encode()), Ok(event));
        }

        #[test]
        fn unknown_kind_rejected(id in 5u8..=255) {
            let mut bytes = [0; EVENT_LEN];
            bytes[0] = id;
            assert_eq!(InputEvent::parse(&bytes), Err(InputError::UnknownKind { id }));
        }

        #[test]
        fn unknown_tool_rejected(id in 3u8..=255) {
            let mut bytes = [0; EVENT_LEN];
            bytes[1] = id;
            assert_eq!(InputEvent::parse(&bytes), Err(InputError::UnknownTool { id }));
        }

        #[test]
        fn batch_round_trip(events in proptest::collection::vec(event_strategy(), 0..=64)) {
            let mut payload = vec![0u8; batch_len(events.len())];
            encode_batch_into(&events, &mut payload).unwrap();
            let parsed: Vec<InputEvent> = parse_batch(&payload)
                .map(Result::unwrap)
                .collect();
            assert_eq!(parsed, events);
        }

        #[test]
        fn short_batch_buffer_rejected_without_writing(
            events in proptest::collection::vec(event_strategy(), 1..=8),
            shortfall in 1usize..=EVENT_LEN,
        ) {
            let mut payload = vec![0xffu8; batch_len(events.len()) - shortfall];
            assert_eq!(
                encode_batch_into(&events, &mut payload),
                Err(BatchEncodeError {
                    needed: batch_len(events.len()),
                    got: payload.len(),
                })
            );
            assert!(payload.iter().all(|&b| b == 0xff), "buffer was written");
        }

        #[test]
        fn truncated_batch_detected(
            prefix in proptest::collection::vec(event_strategy(), 0..=8),
            tail in 1usize..EVENT_LEN,
        ) {
            let mut payload = vec![0u8; batch_len(prefix.len()) + tail];
            encode_batch_into(&prefix, &mut payload).unwrap();
            let parsed: Vec<_> = parse_batch(&payload).collect();
            assert_eq!(parsed.len(), prefix.len() + 1);
            assert!(parsed[..prefix.len()].iter().all(Result::is_ok));
            assert!(matches!(
                parsed[prefix.len()],
                Err(InputError::Truncated { .. })
            ));
        }

        #[test]
        fn bad_event_inside_batch_stops_iteration(bad_kind in 5u8..=255) {
            let mut bytes = [0; EVENT_LEN];
            bytes[0] = bad_kind;
            let payload = [bytes, bytes].concat();
            let parsed: Vec<_> = parse_batch(&payload).collect();
            assert_eq!(parsed, vec![Err(InputError::UnknownKind { id: bad_kind })]);
        }
    }
}
