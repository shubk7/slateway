// SPDX-License-Identifier: GPL-3.0-or-later

//! Video-channel payload rules (`docs/PROTOCOL.md`, "Channels" and
//! "Media"). One frame on the video channel carries one H.264 Annex-B
//! access unit; the header's flag bit0 marks keyframes so the tablet
//! can join mid-stream and SPS/PPS travel inside the keyframe's AU.

/// Message `type` within the video channel: one access unit.
pub const VIDEO_AU_TYPE: u16 = 1;

/// The header's flag bit0 on a video frame: this access unit is a
/// keyframe (IDR; SPS/PPS precede its slice data in the payload).
pub const VIDEO_KEYFRAME_FLAG: u8 = crate::frame::flags::KEYFRAME_OR_PRIORITY;

#[cfg(test)]
mod tests {
    use super::*;

    /// Cross-checked against docs/PROTOCOL.md ("Channels": bit0
    /// keyframe on the video channel).
    #[test]
    fn keyframe_flag_matches_header_bit0() {
        assert_eq!(VIDEO_KEYFRAME_FLAG, 0b01);
        assert_eq!(VIDEO_KEYFRAME_FLAG, crate::frame::flags::ALL & 1);
    }
}
