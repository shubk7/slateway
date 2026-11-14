// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared FFmpeg (libavcodec) encoder plumbing for the Slateway host
//! (`docs/PLATFORMS.md`, "Encode"; ADR-007 fixed FFmpeg as the codec
//! layer, ADR-014 the `ffmpeg-next` binding).
//!
//! The platform crates only supply the candidate list for their OS;
//! everything here is OS-independent: open the first candidate that
//! initializes, convert capture frames to yuv420p, and drain H.264
//! packets.
//!
//! M1 plumbing is deliberately conservative (latency rule 3's fallback
//! path): system-memory frames, reusable conversion planes, one copy
//! for the output packet. The RGB→yuv420p conversion is written in
//! safe Rust rather than libswscale: the swscale context is not `Send`
//! (raw pointer), and the encoder object has to travel with the
//! session's video task. GPU zero-copy paths arrive with the
//! video-channel work.

use slateway_platform::capture::{CaptureFrame, FrameFormat};
use slateway_platform::encoder::{
    EncodedPacket, Encoder, EncoderConfig, EncoderError, EncoderInfo,
};

use ffmpeg_next as ffmpeg;

use ffmpeg::codec::packet::Packet;
use ffmpeg::util::error::EAGAIN;
use ffmpeg::util::error::Error as FfError;
use ffmpeg::util::format::Pixel;
use ffmpeg::util::frame::Video as AvFrame;
use ffmpeg::util::rational::Rational;

mod convert;

/// Encode time base: microseconds, matching `CaptureFrame::pts_us`, so
/// timestamps pass through the pipeline without rescaling.
const TIME_BASE: Rational = Rational(1, 1_000_000);

/// Capture formats the converter accepts today (BGRx's ignored byte
/// rides along; RGBX/RGB would need a converter variant nobody
/// produces yet).
fn convertible(format: FrameFormat) -> bool {
    matches!(format, FrameFormat::Bgrx | FrameFormat::Rgba)
}

/// Per-candidate low-latency options (latency rule 2: B-frames are
/// already off on the context; the rest is per codec). Option names
/// verified against the system FFmpeg's `ffmpeg -h encoder=...`.
fn options_for(name: &str) -> &'static [(&'static str, &'static str)] {
    match name {
        // p1 = fastest preset, ull = ultra-low-latency tuning,
        // zerolatency = no reordering delay.
        "h264_nvenc" => &[("preset", "p1"), ("tune", "ull"), ("zerolatency", "1")],
        // ultrafast + zerolatency: no lookahead, sliced threads.
        "libx264" => &[("preset", "ultrafast"), ("tune", "zerolatency")],
        _ => &[],
    }
}

/// Rough H.264 rate for a screen mirror at the given geometry.
fn default_bitrate_kbps(config: &EncoderConfig) -> u32 {
    let pixels_per_s = config.width as u64 * config.height as u64 * config.fps.max(1) as u64;
    // ~0.06 bits per pixel per frame at 1080p60 lands at ~8.3 Mbps,
    // readable but not wasteful; the control channel re-negotiates.
    ((pixels_per_s / 16_667) as u32).clamp(1_000, 40_000)
}

/// Try each candidate in order and return the first that opens.
/// Candidates are libavcodec encoder names ("h264_nvenc", "libx264");
/// the chosen one is reported in the [`EncoderInfo`].
///
/// When `config.bitrate_kbps` is 0, a geometry-based default applies.
/// Width and height must be even (4:2:0 chroma); odd geometries fail
/// setup.
pub fn start(
    config: &EncoderConfig,
    candidates: &[&str],
) -> Result<(Box<dyn Encoder>, EncoderInfo), EncoderError> {
    if !config.width.is_multiple_of(2) || !config.height.is_multiple_of(2) {
        return Err(EncoderError::Setup(format!(
            "geometry {}x{} is odd; 4:2:0 encoding needs even dimensions",
            config.width, config.height
        )));
    }
    let bitrate_kbps = if config.bitrate_kbps == 0 {
        default_bitrate_kbps(config)
    } else {
        config.bitrate_kbps
    };
    let config = EncoderConfig {
        bitrate_kbps,
        ..*config
    };

    ffmpeg::init().map_err(|e| EncoderError::Setup(format!("ffmpeg init: {e}")))?;

    let mut failures = Vec::new();
    for name in candidates {
        match open_encoder(config, name) {
            Ok(enc) => {
                let info = EncoderInfo {
                    codec: name.to_string(),
                    // H.264 packets at screen-mirror rates are KBs, not
                    // MBs; a one-frame budget with headroom for the
                    // consumer's buffer sizing.
                    max_packet_bytes_hint: bitrate_kbps as usize * 1000 / 8,
                };
                return Ok((Box::new(enc) as Box<dyn Encoder>, info));
            }
            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }
    Err(EncoderError::Setup(format!(
        "no candidate encoder opened: {}",
        failures.join("; ")
    )))
}

fn open_encoder(config: EncoderConfig, name: &str) -> Result<FfmpegEncoder, EncoderError> {
    let codec = ffmpeg::codec::encoder::find_by_name(name)
        .ok_or_else(|| EncoderError::Setup(format!("encoder {name} not found")))?;
    let mut ctx = ffmpeg::codec::context::Context::new_with_codec(codec)
        .encoder()
        .video()
        .map_err(|e| EncoderError::Setup(format!("{name}: not a video encoder: {e}")))?;

    ctx.set_width(config.width);
    ctx.set_height(config.height);
    ctx.set_format(Pixel::YUV420P);
    ctx.set_time_base(TIME_BASE);
    ctx.set_frame_rate(Some(Rational(config.fps.max(1) as i32, 1)));
    // Keyframe distance: 2 seconds of GOP for join recovery and error
    // containment; on-demand IDRs arrive with the control channel.
    ctx.set_gop((config.fps * 2).max(1));
    // Latency rule 2: no B-frames.
    ctx.set_max_b_frames(0);
    ctx.set_bit_rate(config.bitrate_kbps as usize * 1000);

    let mut options = ffmpeg::Dictionary::new();
    for (key, value) in options_for(name) {
        options.set(key, value);
    }
    let opened = ctx
        .open_with(options)
        .map_err(|e| EncoderError::Setup(format!("{name}: {e}")))?;

    Ok(FfmpegEncoder {
        enc: opened,
        // The encoder input frame does not depend on the capture
        // format, so it is allocated here, once.
        output: Some(AvFrame::new(Pixel::YUV420P, config.width, config.height)),
        format: None,
        config,
    })
}

struct FfmpegEncoder {
    /// The opened libavcodec encoder (`video::Encoder` derefs through
    /// to the send/receive methods).
    enc: ffmpeg::codec::encoder::video::Encoder,
    /// Reusable encoder input (yuv420p, config geometry).
    output: Option<AvFrame>,
    /// The frame format the converter was built for. Fixed after the
    /// first frame; a later frame with another format errors out.
    format: Option<FrameFormat>,
    config: EncoderConfig,
}

impl Encoder for FfmpegEncoder {
    fn encode(&mut self, frame: &CaptureFrame) -> Result<Vec<EncodedPacket>, EncoderError> {
        let mut out = Vec::new();
        let format = self.prepare(frame)?;
        // Destructure for disjoint borrows: the converter writes the
        // frame planes, the encoder context consumes them.
        let Self { enc, output, .. } = self;
        let output = output
            .as_mut()
            .ok_or_else(|| EncoderError::Failed("converter missing".into()))?;
        convert::rgb_to_yuv420(frame, format, output)
            .map_err(|e| EncoderError::Failed(format!("color conversion: {e}")))?;
        output.set_pts(Some(i64::try_from(frame.pts_us).unwrap_or(i64::MAX)));
        enc.send_frame(output)
            .map_err(|e| EncoderError::Failed(format!("send frame: {e}")))?;
        drain(enc, &mut out)?;
        Ok(out)
    }

    fn flush(&mut self) -> Result<Vec<EncodedPacket>, EncoderError> {
        let mut out = Vec::new();
        let Self { enc, .. } = self;
        enc.send_eof()
            .map_err(|e| EncoderError::Failed(format!("encoder eof: {e}")))?;
        drain(enc, &mut out)?;
        Ok(out)
    }
}

impl FfmpegEncoder {
    /// Validate the frame against the configuration and pin the
    /// converter's input format on the first frame (see `prepare`).
    fn prepare(&mut self, frame: &CaptureFrame) -> Result<FrameFormat, EncoderError> {
        prepare(self, frame)
    }
}

/// Validate the frame against the configuration and pin the converter's
/// input format on the first frame.
fn prepare(this: &mut FfmpegEncoder, frame: &CaptureFrame) -> Result<FrameFormat, EncoderError> {
    if !frame.is_consistent() {
        return Err(EncoderError::FrameMismatch(format!(
            "inconsistent frame {}x{} stride {} len {}",
            frame.width,
            frame.height,
            frame.stride,
            frame.data.len()
        )));
    }
    if frame.width != this.config.width || frame.height != this.config.height {
        return Err(EncoderError::FrameMismatch(format!(
            "frame is {}x{}, encoder was opened for {}x{} (re-open on resolution change)",
            frame.width, frame.height, this.config.width, this.config.height
        )));
    }
    match this.format {
        None if convertible(frame.format) => {
            this.format = Some(frame.format);
            Ok(frame.format)
        }
        None => Err(EncoderError::Failed(format!(
            "format {:?} is not encodable",
            frame.format
        ))),
        Some(f) if f == frame.format => Ok(f),
        Some(f) => Err(EncoderError::Failed(format!(
            "frame format changed from {f:?} to {:?}; re-open the encoder",
            frame.format
        ))),
    }
}

/// Drain packets the encoder produced into `out`. A fresh Packet
/// per receive: the wrapper unrefs on drop, which is what
/// `avcodec_receive_packet` reuse requires (its internal move-ref
/// would otherwise pile references onto a reused packet).
fn drain(
    enc: &mut ffmpeg::codec::encoder::video::Encoder,
    out: &mut Vec<EncodedPacket>,
) -> Result<(), EncoderError> {
    loop {
        let mut packet = Packet::empty();
        match enc.receive_packet(&mut packet) {
            Ok(()) => {
                if let Some(data) = packet.data() {
                    let pts_us = packet.pts().unwrap_or(0).max(0) as u64;
                    out.push(EncodedPacket {
                        pts_us,
                        data: data.to_vec(),
                        keyframe: packet.is_key(),
                    });
                }
            }
            Err(FfError::Other { errno: EAGAIN }) | Err(FfError::Eof) => return Ok(()),
            Err(e) => return Err(EncoderError::Failed(format!("receive packet: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::start;
    use slateway_platform::capture::{CaptureFrame, FrameFormat};
    use slateway_platform::encoder::{EncoderConfig, EncoderError};

    #[test]
    fn bitrate_default_scales_with_geometry() {
        let small = EncoderConfig {
            width: 640,
            height: 480,
            fps: 30,
            bitrate_kbps: 0,
        };
        let big = EncoderConfig {
            width: 1920,
            height: 1200,
            fps: 60,
            bitrate_kbps: 0,
        };
        assert!(super::default_bitrate_kbps(&small) < super::default_bitrate_kbps(&big));
        assert!(super::default_bitrate_kbps(&big) >= 1000);
    }

    #[test]
    fn options_cover_known_candidates() {
        assert!(!super::options_for("h264_nvenc").is_empty());
        assert!(!super::options_for("libx264").is_empty());
        assert!(super::options_for("h264_foo").is_empty());
    }

    #[test]
    fn unknown_candidate_fails_with_setup() {
        let config = EncoderConfig {
            width: 64,
            height: 48,
            fps: 30,
            bitrate_kbps: 100,
        };
        let err = start(&config, &["h264_slateway_nonexistent"])
            .err()
            .expect("test");
        assert!(matches!(err, EncoderError::Setup(_)));
    }

    #[test]
    fn odd_geometry_fails_setup() {
        let config = EncoderConfig {
            width: 65,
            height: 48,
            fps: 30,
            bitrate_kbps: 100,
        };
        let err = match start(&config, &["libx264"]) {
            Err(e) => e,
            Ok(_) => panic!("test: odd geometry accepted"),
        };
        assert!(matches!(err, EncoderError::Setup(_)));
    }

    /// The full path on a software encoder available in every CI
    /// environment: synthetic moving frames in, keyframes out,
    /// timestamps preserved.
    #[test]
    fn libx264_encodes_synthetic_frames() {
        let config = EncoderConfig {
            width: 320,
            height: 180,
            fps: 30,
            bitrate_kbps: 2_000,
        };
        let (mut enc, info) = start(&config, &["libx264"]).expect("test");
        assert_eq!(info.codec, "libx264");

        let mut packets = 0;
        let mut keyframes = 0;
        for n in 0..6i64 {
            let mut data = vec![0u8; 320 * 180 * 4];
            // Move a block so every frame differs from the previous one.
            for y in 0..180 {
                let x = (n as usize * 4 + y) % 300;
                let row = &mut data[y * 1280..][..1280];
                row[x..x + 20].fill(200);
            }
            let frame = CaptureFrame {
                width: 320,
                height: 180,
                stride: 320 * 4,
                format: FrameFormat::Bgrx,
                data,
                pts_us: n as u64 * 33_333,
            };
            let out = enc.encode(&frame).expect("test");
            for p in out {
                if p.keyframe {
                    keyframes += 1;
                }
                assert_eq!(p.pts_us, n as u64 * 33_333);
                assert!(!p.data.is_empty());
                packets += 1;
            }
        }
        assert!(packets > 0, "no packets produced");
        assert!(keyframes > 0, "no keyframes produced");

        let flushed = enc.flush().expect("test");
        assert!(!flushed.is_empty() || packets > 0);
    }

    #[test]
    fn geometry_mismatch_errors() {
        let config = EncoderConfig {
            width: 320,
            height: 180,
            fps: 30,
            bitrate_kbps: 2_000,
        };
        let (mut enc, _) = start(&config, &["libx264"]).expect("test");
        let frame = CaptureFrame {
            width: 640,
            height: 360,
            stride: 640 * 4,
            format: FrameFormat::Bgrx,
            data: vec![0u8; 640 * 360 * 4],
            pts_us: 0,
        };
        let err = enc.encode(&frame).expect_err("test");
        assert!(matches!(err, EncoderError::FrameMismatch(_)));
    }
}
