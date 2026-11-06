// SPDX-License-Identifier: GPL-3.0-or-later

//! Frame header codec (`docs/PROTOCOL.md`, "Frame header").
//!
//! Every frame is a [`HEADER_LEN`] byte header followed by `payload_len`
//! payload bytes; all integers are little-endian. This module converts
//! between bytes and [`FrameHeader`] values. It is I/O-free and
//! allocation-free, and it validates the length field before any
//! size-dependent work, as the spec requires.

use crate::channel::{Channel, UnknownChannelError};

/// Frame header size in bytes.
pub const HEADER_LEN: usize = 16;

/// Maximum payload length in bytes: 16 MiB. Parsers must reject
/// oversized lengths (`docs/PROTOCOL.md`, "Frame header").
pub const MAX_PAYLOAD_LEN: usize = 16 * 1024 * 1024;

/// Flag bits in the second header byte.
pub mod flags {
    /// bit0: keyframe or priority, depending on the channel.
    pub const KEYFRAME_OR_PRIORITY: u8 = 1 << 0;

    /// bit1: more-fragments, reserved for datagram transports; always
    /// zero on the ordered byte-stream transports (`docs/PROTOCOL.md`,
    /// "Transports"). The header parser does not police this bit:
    /// consumers decide what it means for their channel.
    pub const MORE_FRAGMENTS: u8 = 1 << 1;

    /// Every documented flag bit. Bits 2 to 7 are zero on the wire and
    /// rejected by [`FrameHeader::parse`] when set.
    pub const ALL: u8 = KEYFRAME_OR_PRIORITY | MORE_FRAGMENTS;
}

/// A parsed frame header.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameHeader {
    /// Channel the frame belongs to.
    pub channel: Channel,
    /// Flag bits; only bits documented in [`flags`] may be set.
    pub flags: u8,
    /// Message type within the channel.
    pub message_type: u16,
    /// Payload length in bytes, at most [`MAX_PAYLOAD_LEN`].
    pub payload_len: u32,
    /// Sender's monotonic clock in microseconds; 0 if not applicable.
    pub pts_us: u64,
}

/// Frame header parse failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// The buffer is shorter than [`HEADER_LEN`].
    #[error("frame header needs {needed} bytes, got {got}")]
    TooShort {
        /// Bytes a header requires.
        needed: usize,
        /// Bytes the buffer had.
        got: usize,
    },
    /// The payload length field exceeds [`MAX_PAYLOAD_LEN`].
    #[error("payload length {len} exceeds the {max} byte limit")]
    PayloadTooLong {
        /// The rejected length field.
        len: u32,
        /// The limit, [`MAX_PAYLOAD_LEN`] as `u32`.
        max: u32,
    },
    /// The channel byte is a reserved ID, 9 to 15.
    #[error(transparent)]
    UnknownChannel(#[from] UnknownChannelError),
    /// Undocumented flag bits (2 to 7) are set; the spec keeps them zero.
    #[error("reserved flag bits set: {flags:#04x}")]
    ReservedFlagsSet {
        /// The rejected flags byte.
        flags: u8,
    },
}

impl FrameHeader {
    /// Encodes the header into `out`, little-endian.
    pub fn encode_into(self, out: &mut [u8; HEADER_LEN]) {
        out[0] = self.channel.id();
        out[1] = self.flags;
        out[2..4].copy_from_slice(&self.message_type.to_le_bytes());
        out[4..8].copy_from_slice(&self.payload_len.to_le_bytes());
        out[8..16].copy_from_slice(&self.pts_us.to_le_bytes());
    }

    /// Encodes the header, little-endian.
    pub fn encode(self) -> [u8; HEADER_LEN] {
        let mut out = [0; HEADER_LEN];
        self.encode_into(&mut out);
        out
    }

    /// Parses the header at the start of `buf`; only the first
    /// [`HEADER_LEN`] bytes are read, so `buf` may continue with the
    /// payload. `buf` must hold at least [`HEADER_LEN`] bytes.
    ///
    /// Rejects unknown channels, undocumented flag bits and oversized
    /// lengths. The validated [`payload_len`](FrameHeader::payload_len)
    /// is safe to size buffers with; anything else in the buffer is not.
    pub fn parse(buf: &[u8]) -> Result<Self, FrameError> {
        if buf.len() < HEADER_LEN {
            return Err(FrameError::TooShort {
                needed: HEADER_LEN,
                got: buf.len(),
            });
        }
        let channel = Channel::from_id(buf[0])?;
        let flags = buf[1];
        if flags & !flags::ALL != 0 {
            return Err(FrameError::ReservedFlagsSet { flags });
        }
        let header = FrameHeader {
            channel,
            flags,
            message_type: u16::from_le_bytes([buf[2], buf[3]]),
            payload_len: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
            pts_us: u64::from_le_bytes([
                buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
            ]),
        };
        if header.payload_len > MAX_PAYLOAD_LEN as u32 {
            return Err(FrameError::PayloadTooLong {
                len: header.payload_len,
                max: MAX_PAYLOAD_LEN as u32,
            });
        }
        Ok(header)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Golden layout, cross-checked byte by byte against the "Frame
    /// header" table in docs/PROTOCOL.md (little-endian field order).
    #[test]
    fn golden_layout() {
        let header = FrameHeader {
            channel: Channel::Input,
            flags: flags::KEYFRAME_OR_PRIORITY,
            message_type: 0x1234,
            payload_len: 0xff,
            pts_us: 0x0102_0304_0506_0708,
        };
        assert_eq!(
            header.encode(),
            [
                0x02, // channel: input
                0x01, // flags: bit0
                0x34, 0x12, // message_type
                0xff, 0x00, 0x00, 0x00, // payload_len
                0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, // pts_us
            ]
        );
    }

    /// The spec pins both numbers; a change here is a protocol change.
    #[test]
    fn limits_match_spec() {
        assert_eq!(HEADER_LEN, 16);
        assert_eq!(MAX_PAYLOAD_LEN, 16 * 1024 * 1024);
    }

    #[test]
    fn too_short() {
        let bytes = [0u8; HEADER_LEN - 1];
        assert_eq!(
            FrameHeader::parse(&bytes),
            Err(FrameError::TooShort {
                needed: HEADER_LEN,
                got: HEADER_LEN - 1
            })
        );
    }

    fn header_strategy() -> impl Strategy<Value = FrameHeader> {
        (
            prop_oneof![8 => 0u8..=8, 1 => 16u8..=255],
            0u8..=flags::ALL,
            any::<u16>(),
            0u32..=MAX_PAYLOAD_LEN as u32,
            any::<u64>(),
        )
            .prop_map(
                |(channel, flags, message_type, payload_len, pts_us)| FrameHeader {
                    channel: Channel::from_id(channel).unwrap_or(Channel::Control),
                    flags,
                    message_type,
                    payload_len,
                    pts_us,
                },
            )
    }

    proptest! {
        #[test]
        fn round_trip(header in header_strategy()) {
            assert_eq!(FrameHeader::parse(&header.encode()), Ok(header));
        }

        /// parse reads only the first 16 bytes of a longer buffer.
        #[test]
        fn parse_ignores_trailing_bytes(
            header in header_strategy(),
            pad in proptest::collection::vec(any::<u8>(), 0..32),
        ) {
            let mut buf = header.encode().to_vec();
            buf.extend(pad);
            assert_eq!(FrameHeader::parse(&buf), Ok(header));
        }

        #[test]
        fn truncated_buffers_rejected(
            prefix in proptest::collection::vec(any::<u8>(), 0..HEADER_LEN),
        ) {
            assert!(matches!(FrameHeader::parse(&prefix), Err(FrameError::TooShort { .. })));
        }

        #[test]
        fn oversized_lengths_rejected(len in (MAX_PAYLOAD_LEN as u32 + 1)..=u32::MAX) {
            let mut bytes = [0u8; HEADER_LEN];
            bytes[0] = Channel::Video.id();
            bytes[4..8].copy_from_slice(&len.to_le_bytes());
            assert!(matches!(
                FrameHeader::parse(&bytes),
                Err(FrameError::PayloadTooLong { .. })
            ));
        }

        #[test]
        fn unknown_channels_rejected(id in 9u8..=15) {
            let mut bytes = [0u8; HEADER_LEN];
            bytes[0] = id;
            assert_eq!(
                FrameHeader::parse(&bytes),
                Err(FrameError::UnknownChannel(UnknownChannelError { id }))
            );
        }

        #[test]
        fn reserved_flag_bits_rejected(flag_bits in (flags::ALL + 1)..=u8::MAX) {
            let mut bytes = [0u8; HEADER_LEN];
            bytes[0] = Channel::Video.id();
            bytes[1] = flag_bits;
            assert_eq!(
                FrameHeader::parse(&bytes),
                Err(FrameError::ReservedFlagsSet { flags: flag_bits })
            );
        }
    }
}
