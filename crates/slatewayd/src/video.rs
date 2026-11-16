// SPDX-License-Identifier: GPL-3.0-or-later

//! The video stream (docs/ROADMAP.md M1 "Video channel"): platform
//! capture feeding the FFmpeg encoder on a dedicated OS thread, whose
//! packets go to the session's writer. Threading per
//! `docs/ARCHITECTURE.md`: capture and encode run on their own
//! thread; the network stage stays on the async session task.
//!
//! Backpressure (latency rule 1): the pump only sends when the
//! session's write queue has room, and skips captures (before
//! encoding) while the queue is full. Encoded frames are never
//! dropped: skipping happens at the capture stage, and a frame's
//! packets are all delivered or none.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use slateway_platform::capture::{CaptureConfig, CaptureSource};
use slateway_platform::encoder::{
    EncodedPacket, Encoder, EncoderConfig, EncoderError, EncoderInfo,
};
use slateway_platform::input::PointerInjector;
use slateway_proto::{Channel, FrameHeader, VIDEO_AU_TYPE, VIDEO_KEYFRAME_FLAG};
use tokio::sync::mpsc;

/// How long one capture wait runs before the pump checks the shutdown
/// and keyframe flags again.
const POLL: Duration = Duration::from_millis(200);

/// Opens this platform's encoder for a config (Linux and Windows pick
/// different candidate lists, AGENTS.md rule 2).
pub type EncoderOpener =
    dyn Fn(&EncoderConfig) -> Result<(Box<dyn Encoder>, EncoderInfo), EncoderError> + Send;

/// What one started video source hands the pump: the running capture,
/// the stream geometry, and this platform's encoder opener (Linux and
/// Windows pick different candidate lists, AGENTS.md rule 2). With
/// input enabled the source also yields the platform's pointer
/// injector (Linux: the portal session shared with the capture;
/// Windows: SendInput over the captured monitor).
pub struct StartedVideo {
    pub source: Box<dyn CaptureSource>,
    pub config: EncoderConfig,
    pub open_encoder: Box<EncoderOpener>,
    pub injector: Option<Box<dyn PointerInjector>>,
}

/// Produces the platform video source for a session. `start` blocks
/// while the capture initializes (on Linux that includes the portal
/// consent dialog).
pub trait VideoSource: Send {
    fn start(&mut self) -> Result<StartedVideo, String>;
}

/// Counters the pump keeps for the session report.
#[derive(Debug, Default)]
pub struct PumpCounters {
    pub frames_encoded: AtomicU64,
    pub frames_skipped: AtomicU64,
    pub encode_errors: AtomicU64,
}

/// Runs the pump on the calling thread until the write queue's sender
/// half is gone (session over). Returned counters merge into the
/// session report.
pub fn pump(
    mut started: StartedVideo,
    tx: mpsc::Sender<(FrameHeader, Vec<u8>)>,
    keyframe_req: Arc<AtomicBool>,
) -> PumpCounters {
    let counters = PumpCounters::default();
    let open_started = std::time::Instant::now();
    let mut encoder = match (started.open_encoder)(&started.config) {
        Ok((encoder, info)) => {
            eprintln!(
                "video: encoding with {} (open {} ms)",
                info.codec,
                open_started.elapsed().as_millis()
            );
            encoder
        }
        Err(err) => {
            eprintln!("video: no encoder opened ({err}); video stays off");
            counters.encode_errors.fetch_add(1, Ordering::Relaxed);
            return counters;
        }
    };
    // pts relative to session start (docs/PROTOCOL.md, "Clocks").
    let base_us = slateway_core::monotonic_us();
    loop {
        if keyframe_req.swap(false, Ordering::Relaxed) {
            // A keyframe request (decoder reset on the tablet) re-opens
            // the encoder: the fresh stream starts with SPS/PPS + IDR.
            match (started.open_encoder)(&started.config) {
                Ok((fresh, info)) => {
                    eprintln!("video: re-opened the encoder ({}) on request", info.codec);
                    encoder = fresh;
                }
                Err(err) => eprintln!("video: encoder re-open failed ({err}); continuing"),
            }
        }
        let frame = match started.source.next_frame(POLL) {
            Ok(Some(frame)) => frame,
            Ok(None) => continue,
            Err(err) => {
                eprintln!("video: capture failed ({err}); stream ends");
                return counters;
            }
        };
        // Backpressure: no room in the write queue means skip this
        // capture entirely, before encoding, so no encoded frame is
        // ever dropped (latency rule 1).
        if tx.capacity() == 0 {
            counters.frames_skipped.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        match encoder.encode(&frame) {
            Ok(packets) => {
                counters.frames_encoded.fetch_add(1, Ordering::Relaxed);
                for packet in packets {
                    if send_packet(&tx, &packet, base_us).is_err() {
                        // Session is gone (all senders dropped).
                        return counters;
                    }
                }
            }
            Err(err) => {
                // Geometry or format mismatches repeat every frame:
                // one error per burst, then the capture keeps running
                // so a later frame may still fit.
                if counters.encode_errors.fetch_add(1, Ordering::Relaxed) == 0 {
                    eprintln!("video: encode failed ({err}); dropping frames until it fits");
                }
            }
        }
    }
}

/// Wraps one encoded packet in a video-channel frame and queues it.
fn send_packet(
    tx: &mpsc::Sender<(FrameHeader, Vec<u8>)>,
    packet: &EncodedPacket,
    base_us: u64,
) -> Result<(), mpsc::error::SendError<(FrameHeader, Vec<u8>)>> {
    let header = FrameHeader {
        channel: Channel::Video,
        flags: if packet.keyframe {
            VIDEO_KEYFRAME_FLAG
        } else {
            0
        },
        message_type: VIDEO_AU_TYPE,
        payload_len: packet.data.len() as u32,
        pts_us: packet.pts_us.saturating_sub(base_us),
    };
    // Blocks only while the session task drains one frame; the
    // capacity check above keeps this from stacking a backlog.
    tx.blocking_send((header, packet.data.clone()))
}

/// The platform source behind `--mirror` (slatewayd runs on a real
/// desktop; tests inject their own [`VideoSource`]).
pub struct PlatformVideoSource {
    /// Touch-as-pointer requested (the `--allow-input` flag).
    pub allow_input: bool,
}

impl VideoSource for PlatformVideoSource {
    fn start(&mut self) -> Result<StartedVideo, String> {
        start_platform(&CaptureConfig {
            max_fps: 60,
            restore_token: None,
            with_input: self.allow_input,
        })
    }
}

#[cfg(target_os = "linux")]
fn start_platform(config: &CaptureConfig) -> Result<StartedVideo, String> {
    use slateway_linux::PortalCapture;
    let mut capture = PortalCapture::default();
    let info = capture.start(config).map_err(|e| e.to_string())?;
    // With input requested, the capture's portal session carries the
    // pointer devices; take the handle before the capture moves into
    // the pump.
    let injector: Option<Box<dyn PointerInjector>> = match capture.take_input() {
        Some(handle) => {
            eprintln!("video: pointer injection granted on the portal session");
            match slateway_linux::input::PortalPointer::new(handle) {
                Ok(pointer) => Some(Box::new(pointer)),
                Err(err) => {
                    eprintln!("video: pointer injector setup failed ({err}); input stays off");
                    None
                }
            }
        }
        None => None,
    };
    let source = Box::new(capture) as Box<dyn CaptureSource>;
    Ok(StartedVideo {
        source,
        config: EncoderConfig {
            width: info.width,
            height: info.height,
            fps: info.fps,
            bitrate_kbps: 0,
        },
        open_encoder: Box::new(slateway_linux::encoder::start),
        injector,
    })
}

#[cfg(target_os = "windows")]
fn start_platform(config: &CaptureConfig) -> Result<StartedVideo, String> {
    use slateway_win::WgcCapture;
    let mut source = Box::new(WgcCapture::default()) as Box<dyn CaptureSource>;
    let info = source.start(config).map_err(|e| e.to_string())?;
    // SendInput works without a capture session; input rides the
    // mirror session for M1's consent story (one operator flag).
    let injector: Option<Box<dyn PointerInjector>> = if config.with_input {
        match slateway_win::input::MouseInjector::new() {
            Ok(mouse) => Some(Box::new(mouse)),
            Err(err) => {
                eprintln!("video: pointer injector setup failed ({err}); input stays off");
                None
            }
        }
    } else {
        None
    };
    Ok(StartedVideo {
        source,
        config: EncoderConfig {
            width: info.width,
            height: info.height,
            fps: info.fps,
            bitrate_kbps: 0,
        },
        open_encoder: Box::new(slateway_win::encoder::start),
        injector,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn start_platform(_config: &CaptureConfig) -> Result<StartedVideo, String> {
    Err("this platform has no video capture backend".into())
}
