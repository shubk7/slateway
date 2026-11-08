// SPDX-License-Identifier: GPL-3.0-or-later

//! Control-channel schemas (`docs/PROTOCOL.md`, "Control schemas").
//!
//! Generated from `proto/control.proto` by prost; the same file
//! generates the tablet's Kotlin code. A control-channel payload is
//! one message, selected by the frame header's `type` field.

/// Frame header `type` for [`Hello`].
pub const HELLO_TYPE: u16 = 1;

/// Frame header `type` for [`HelloAck`].
pub const HELLO_ACK_TYPE: u16 = 2;

/// Frame header `type` for [`Close`].
pub const CLOSE_TYPE: u16 = 3;

/// Frame header `type` for [`Ping`] (host to tablet).
pub const PING_TYPE: u16 = 4;

/// Frame header `type` for [`Pong`] (tablet to host).
pub const PONG_TYPE: u16 = 5;

/// Frame header `type` for [`Stats`] (tablet to host).
pub const STATS_TYPE: u16 = 6;

include!(concat!(env!("OUT_DIR"), "/slateway.control.v0.rs"));

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FrameHeader, HEADER_LEN, MAX_PAYLOAD_LEN};
    use proptest::prelude::*;
    use prost::Message;

    /// Golden wire bytes, hand-computed from the proto3 wire format and
    /// cross-checked against docs/PROTOCOL.md. Pins field numbers: a
    /// change here is a protocol change.
    #[test]
    fn hello_golden_wire_bytes() {
        let hello = Hello {
            protocol_major: 0,
            protocol_minor: 0,
            app_version: "0.1.0".into(),
            device_model: "Tab".into(),
            android_api: 30,
            codecs: vec![VideoCodec::H264Baseline as i32, VideoCodec::H264High as i32],
            decode_limits: Some(DecodeLimits {
                max_width_px: 2000,
                max_height_px: 1200,
                max_fps: 60,
            }),
            screen: Some(Screen {
                width_px: 2000,
                height_px: 1200,
                refresh_millihz: 60000,
            }),
            capabilities: 0,
        };
        assert_eq!(
            hello.encode_to_vec(),
            [
                // app_version, field 3, length-delimited: "0.1.0"
                0x1a, 0x05, b'0', b'.', b'1', b'.', b'0', // device_model, field 4: "Tab"
                0x22, 0x03, b'T', b'a', b'b', // android_api, field 5, varint 30
                0x28, 0x1e,
                // codecs, field 6, packed varints [1, 3] (proto3 packs
                // repeated enums by default)
                0x32, 0x02, 0x01, 0x03,
                // decode_limits, field 7, length-delimited:
                // width 2000, height 1200, fps 60
                0x3a, 0x08, 0x08, 0xd0, 0x0f, 0x10, 0xb0, 0x09, 0x18, 0x3c,
                // screen, field 8, length-delimited:
                // width 2000, height 1200, refresh 60000 mHz
                0x42, 0x0a, 0x08, 0xd0, 0x0f, 0x10, 0xb0, 0x09, 0x18, 0xe0, 0xd4, 0x03,
            ]
        );
    }

    /// proto3 skips zero-valued fields; the major/minor and capability
    /// fields must stay absent at zero so the wire stays minimal.
    #[test]
    fn zero_fields_are_omitted() {
        let hello = Hello::default();
        assert!(hello.encode_to_vec().is_empty());
    }

    #[test]
    fn type_numbers_match_protocol_doc() {
        assert_eq!(HELLO_TYPE, 1);
        assert_eq!(HELLO_ACK_TYPE, 2);
        assert_eq!(CLOSE_TYPE, 3);
        assert_eq!(PING_TYPE, 4);
        assert_eq!(PONG_TYPE, 5);
        assert_eq!(STATS_TYPE, 6);
    }

    /// A Hello rides in a control frame: the whole frame round-trips
    /// through the header codec from the previous milestone.
    #[test]
    fn hello_fits_control_frame() {
        let hello = Hello {
            app_version: "0.1.0".into(),
            ..Hello::default()
        };
        let payload = hello.encode_to_vec();
        let header = FrameHeader {
            channel: crate::Channel::Control,
            flags: 0,
            message_type: HELLO_TYPE,
            payload_len: payload.len() as u32,
            pts_us: 0,
        };
        assert!(payload.len() <= MAX_PAYLOAD_LEN);
        let bytes = header.encode();
        assert_eq!(bytes[0], 0); // control channel
        assert_eq!(u16::from_le_bytes([bytes[2], bytes[3]]), HELLO_TYPE);
        assert_eq!(
            u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            payload.len() as u32
        );
        assert_eq!(bytes.len(), HEADER_LEN);
    }

    fn hello_strategy() -> impl Strategy<Value = Hello> {
        (
            any::<u32>(),
            any::<u32>(),
            "[a-z0-9.]{0,12}",
            "[a-zA-Z0-9 _-]{0,16}",
            any::<u32>(),
            proptest::collection::vec(0i32..=3, 0..4),
            (0u32..=7680, 0u32..=4320, 0u32..=240),
            (0u32..=7680, 0u32..=4320, 0u32..=240_000),
            any::<u64>(),
        )
            .prop_map(
                |(
                    major,
                    minor,
                    app_version,
                    device_model,
                    api,
                    codecs,
                    (w, h, fps),
                    (sw, sh, refresh),
                    capabilities,
                )| {
                    Hello {
                        protocol_major: major,
                        protocol_minor: minor,
                        app_version,
                        device_model,
                        android_api: api,
                        codecs,
                        decode_limits: Some(DecodeLimits {
                            max_width_px: w,
                            max_height_px: h,
                            max_fps: fps,
                        }),
                        screen: Some(Screen {
                            width_px: sw,
                            height_px: sh,
                            refresh_millihz: refresh,
                        }),
                        capabilities,
                    }
                },
            )
    }

    proptest! {
        /// Semantic round-trip: decode(encode(m)) == m. Note proto3
        /// re-encoding is not byte-identical (zero fields get
        /// dropped), so the invariant is on the decoded value.
        #[test]
        fn hello_semantic_round_trip(hello in hello_strategy()) {
            let bytes = hello.encode_to_vec();
            assert_eq!(Hello::decode(bytes.as_slice()).unwrap(), hello);
        }
    }
}
