// SPDX-License-Identifier: GPL-3.0-or-later

//! The [`Encoder`] trait and shared packet types.
//!
//! Per `docs/ARCHITECTURE.md`, the encoder is a synchronous object the
//! session's video pump owns: it converts capture frames and hands back
//! H.264 packets. The FFmpeg plumbing lives in `slateway-ffmpeg`; the
//! platform crates (`slateway-linux`, `slateway-win`) only pick the
//! encoder candidate list for their OS (AGENTS.md rule 2).

use thiserror::Error;

use crate::capture::CaptureFrame;

/// Configuration for an encoder start. Width, height and fps must match
/// the capture the frames come from; an encoder cannot change geometry
/// mid-stream, so the consumer re-opens one on a resolution change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Target bit rate in kilobits per second.
    pub bitrate_kbps: u32,
}

/// Static description of a running encoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderInfo {
    /// The libavcodec encoder actually in use (for example
    /// `h264_nvenc`), so logs and stats can name it.
    pub codec: String,
    /// Default size of one output packet buffer, as a hint for the
    /// consumer's buffer sizing.
    pub max_packet_bytes_hint: usize,
}

/// One encoded H.264 packet (Annex-B, decodable standalone by
/// MediaCodec; SPS/PPS repeat with keyframes).
#[derive(Debug, Clone)]
pub struct EncodedPacket {
    /// Microsecond timestamp, copied from the input frame's `pts_us`.
    pub pts_us: u64,
    pub data: Vec<u8>,
    pub keyframe: bool,
}

#[derive(Debug, Error)]
pub enum EncoderError {
    /// No candidate encoder could be opened for this configuration.
    #[error("encoder setup failed: {0}")]
    Setup(String),
    /// The encoder was running but then failed.
    #[error("encode failed: {0}")]
    Failed(String),
    /// The frame does not match the configuration (geometry or format
    /// the encoder cannot accept).
    #[error("frame does not match encoder configuration: {0}")]
    FrameMismatch(String),
}

/// Encodes capture frames into H.264 packets. All methods are blocking;
/// the consumer owns the threading (like [`crate::capture::CaptureSource`]).
pub trait Encoder: Send {
    /// Encode one frame and drain the packets produced. Returns an
    /// empty Vec when the encoder still buffers the frame (hardware
    /// pipelines may hold a frame or two); the consumer sends whatever
    /// comes out, immediately, in order.
    ///
    /// Frame timestamps pass through: every returned packet carries the
    /// input frame's `pts_us`.
    fn encode(&mut self, frame: &CaptureFrame) -> Result<Vec<EncodedPacket>, EncoderError>;

    /// Signal end of input and drain everything the encoder still
    /// holds. The encoder cannot be used afterwards.
    fn flush(&mut self) -> Result<Vec<EncodedPacket>, EncoderError>;
}

#[cfg(test)]
mod tests {
    use super::{EncodedPacket, EncoderConfig, EncoderError};

    #[test]
    fn config_is_copy_and_comparable() {
        let a = EncoderConfig {
            width: 1920,
            height: 1200,
            fps: 60,
            bitrate_kbps: 8000,
        };
        assert_eq!(a, a);
        assert_eq!(a.bitrate_kbps, 8000);
    }

    #[test]
    fn error_display_is_nonempty() {
        let e = EncoderError::Setup("no encoder".into());
        assert!(!e.to_string().is_empty());
    }

    #[test]
    fn packet_fields_round_trip() {
        let p = EncodedPacket {
            pts_us: 12345,
            data: vec![0, 1, 2],
            keyframe: true,
        };
        assert_eq!(p.pts_us, 12345);
        assert!(p.keyframe);
        assert_eq!(p.data.len(), 3);
    }
}
