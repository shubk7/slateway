// SPDX-License-Identifier: GPL-3.0-or-later

//! The [`CaptureSource`] trait and shared frame types.
//!
//! Per `docs/ARCHITECTURE.md`, capture runs on a dedicated OS thread that
//! calls `start`, then pulls frames with `next_frame` in a loop; the
//! platform implementations live in `slateway-linux` and `slateway-win`
//! (AGENTS.md rule 2). The trait is deliberately blocking: the daemon
//! owns the threading, the implementation owns its device handles.

use std::time::{Duration, Instant};

use thiserror::Error;

/// Pixel formats a [`CaptureSource`] may deliver.
///
/// Only formats the encoders accept today are listed; anything else the
/// source negotiates is its own problem to reject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameFormat {
    /// 32 bits per pixel, memory order B, G, R, A/pad (FFmpeg `bgra`,
    /// DRM fourcc `BGRX`).
    Bgrx,
    /// 32 bits per pixel, memory order R, G, B, A/pad (FFmpeg `rgba`).
    Rgba,
    /// 32 bits per pixel, memory order R, G, B, pad (FFmpeg `rgb0`).
    Rgbx,
    /// 24 bits per pixel, memory order R, G, B (FFmpeg `rgb24`).
    Rgb,
}

impl FrameFormat {
    /// Bytes per pixel; `stride` may still be larger (row padding).
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            FrameFormat::Bgrx | FrameFormat::Rgba | FrameFormat::Rgbx => 4,
            FrameFormat::Rgb => 3,
        }
    }
}

/// One captured frame. The pixel data is an owned copy; zero-copy GPU
/// paths arrive with the encoder work (ARCHITECTURE.md latency rules).
#[derive(Debug, Clone)]
pub struct CaptureFrame {
    pub width: u32,
    pub height: u32,
    /// Bytes per row; may exceed `width * bytes_per_pixel` (padding).
    pub stride: usize,
    pub format: FrameFormat,
    pub data: Vec<u8>,
    /// Monotonic microseconds at the moment the source handed the frame
    /// over (process-local origin). Clock-offset math against the tablet
    /// happens in the stats layer, not here.
    pub pts_us: u64,
}

impl CaptureFrame {
    /// Sanity check used by sources after copying out of a device buffer.
    pub fn is_consistent(&self) -> bool {
        let bpp = self.format.bytes_per_pixel();
        self.stride >= self.width as usize * bpp
            && self.data.len()
                >= (self.height as usize - 1) * self.stride + self.width as usize * bpp
    }
}

/// Monotonic microseconds from a process-local origin, for frame
/// timestamps (`CaptureFrame::pts_us`). Cross-machine clock math
/// happens in the stats layer.
pub fn monotonic_us() -> u64 {
    static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    ORIGIN.get_or_init(Instant::now).elapsed().as_micros() as u64
}

/// Static description of a running capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureInfo {
    pub width: u32,
    pub height: u32,
    /// Negotiated capture rate hint; compositors may still deliver
    /// damage-driven frames more slowly on a static screen.
    pub fps: u32,
    pub format: FrameFormat,
}

/// Configuration for a capture start.
#[derive(Debug, Clone, Default)]
pub struct CaptureConfig {
    /// Requested capture rate. Sources are free to clamp or ignore.
    pub max_fps: u32,
    /// Linux portals: a restore token from a previously approved
    /// screencast lets the compositor skip the consent dialog. Stored
    /// and refreshed by the daemon config, not by the source.
    pub restore_token: Option<String>,
}

#[derive(Debug, Error)]
pub enum CaptureError {
    /// Setup failed (no portal, permission refused, no devices).
    #[error("capture setup failed: {0}")]
    Setup(String),
    /// The stream was set up but then failed or was revoked.
    #[error("capture failed while running: {0}")]
    Failed(String),
    /// Called outside the threading contract (for example `start` from
    /// an async runtime thread, which cannot block on the portal).
    #[error("capture called from an unsupported context: {0}")]
    Unsupported(String),
}

/// Delivers monitor frames. All methods are blocking; `start` must run
/// on a dedicated OS thread and not inside a tokio runtime.
pub trait CaptureSource: Send {
    /// Set up the capture and begin delivering frames. Blocks until the
    /// first format negotiation completes (this is where an interactive
    /// consent prompt may appear), or a few seconds at most.
    fn start(&mut self, config: &CaptureConfig) -> Result<CaptureInfo, CaptureError>;

    /// Yield the newest frame not yet returned, waiting at most
    /// `timeout`. Newer frames replace older ones (newest-frame
    /// semantics); `Ok(None)` means "nothing new within the timeout",
    /// not "capture ended".
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<CaptureFrame>, CaptureError>;

    /// Tear the capture down and release all device handles. Also runs
    /// on Drop; must not panic.
    fn stop(&mut self);
}

#[cfg(test)]
mod tests {
    use super::{CaptureConfig, CaptureFrame, CaptureInfo, CaptureSource, FrameFormat};

    #[test]
    fn bytes_per_pixel_matches_format() {
        assert_eq!(FrameFormat::Bgrx.bytes_per_pixel(), 4);
        assert_eq!(FrameFormat::Rgba.bytes_per_pixel(), 4);
        assert_eq!(FrameFormat::Rgbx.bytes_per_pixel(), 4);
        assert_eq!(FrameFormat::Rgb.bytes_per_pixel(), 3);
    }

    #[test]
    fn frame_consistency_rejects_short_buffers() {
        let mut frame = CaptureFrame {
            width: 4,
            height: 2,
            stride: 16,
            format: FrameFormat::Bgrx,
            data: vec![0; 32],
            pts_us: 0,
        };
        assert!(frame.is_consistent());
        // Truncated last row must be rejected.
        frame.data.truncate(24);
        assert!(!frame.is_consistent());
        // Stride smaller than a row must be rejected.
        frame.stride = 8;
        frame.data = vec![0; 32];
        assert!(!frame.is_consistent());
    }

    // The trait needs at least one compile-time user on every OS to
    // keep its shape honest; a never-starting stub suffices.
    struct Stub;
    impl CaptureSource for Stub {
        fn start(&mut self, _config: &CaptureConfig) -> Result<CaptureInfo, super::CaptureError> {
            Err(super::CaptureError::Unsupported("stub".into()))
        }
        fn next_frame(
            &mut self,
            _timeout: std::time::Duration,
        ) -> Result<Option<CaptureFrame>, super::CaptureError> {
            Ok(None)
        }
        fn stop(&mut self) {}
    }

    fn assert_send<T: Send>() {}
    #[test]
    fn capture_source_is_send() {
        assert_send::<Stub>();
    }
}
