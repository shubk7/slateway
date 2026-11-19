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
use std::time::{Duration, Instant};

use slateway_platform::capture::{CaptureConfig, CaptureFrame, CaptureSource};
use slateway_platform::encoder::{
    EncodedPacket, Encoder, EncoderConfig, EncoderError, EncoderInfo,
};
use slateway_platform::input::PointerInjector;
use slateway_proto::{Channel, FrameHeader, VIDEO_AU_TYPE, VIDEO_KEYFRAME_FLAG};
use tokio::sync::mpsc;

/// How long one capture wait runs before the pump checks the shutdown
/// and keyframe flags again.
const POLL: Duration = Duration::from_millis(200);

/// Frame-flow heartbeat cadence (docs/TESTING.md, "Frame-flow
/// heartbeats").
const HEARTBEAT: Duration = Duration::from_secs(2);

/// Opens this platform's encoder for a config (Linux and Windows pick
/// different candidate lists, AGENTS.md rule 2).
pub type EncoderOpener =
    dyn Fn(&EncoderConfig) -> Result<(Box<dyn Encoder>, EncoderInfo), EncoderError> + Send;

/// The send period for `--fps`; `None` for the damage-driven stream
/// (`--fps 0`).
fn pacing_period(fps: u32) -> Option<Duration> {
    (fps > 0).then(|| Duration::from_secs_f64(1.0 / f64::from(fps)))
}

/// The pump's pacing state for a constant-rate stream: one send per
/// period, the newest captured frame at each tick (docs/TESTING.md,
/// "Frame pacing"; ADR-015). Kept pure so the timing rules are
/// unit-testable without a capture or an encoder.
struct Pacer {
    period: Duration,
    /// The instant the next send is due.
    deadline: Instant,
}

impl Pacer {
    fn new(period: Duration, now: Instant) -> Self {
        Pacer {
            period,
            deadline: now,
        }
    }

    /// How long the pump may wait for a capture frame before it has to
    /// act on its own (send a repeat, or sample the newest frame).
    fn wait(&self, now: Instant) -> Duration {
        self.deadline.saturating_duration_since(now)
    }

    /// Whether this wake-up sends a frame.
    fn due(&self, now: Instant) -> bool {
        now >= self.deadline
    }

    /// Advances the deadline after a send (or a skipped one).
    fn advance(&mut self, now: Instant) {
        self.deadline = now + self.period;
    }

    /// Sends as soon as possible (a keyframe request re-opened the
    /// encoder: the held frame should not wait for the next tick).
    fn send_now(&mut self, now: Instant) {
        self.deadline = now;
    }
}

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
    /// Constant-rate pacing (`--fps`, ADR-015); 0 keeps the
    /// damage-driven stream.
    pub pacing_fps: u32,
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
    /// Subset of `frames_encoded` that re-sent an unchanged frame to
    /// hold the pacing floor (docs/TESTING.md, "Frame pacing").
    pub frames_repeated: AtomicU64,
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
    // Frame-flow heartbeat (docs/TESTING.md, "Frame-flow
    // heartbeats"): a window with zero encodes while the desktop
    // visibly changes puts the stall at or after the encoder; the
    // capture worker's counters disambiguate the stages before it.
    let mut heartbeat_at = std::time::Instant::now() + HEARTBEAT;
    let mut hb_encoded = 0u64;
    let mut hb_repeated = 0u64;
    let mut hb_skipped = 0u64;
    let mut hb_errors = 0u64;
    // Constant-rate pacing (docs/TESTING.md, "Frame pacing";
    // ADR-015). The capture stays damage-driven — the compositor
    // decides when a buffer exists — so the pump holds the newest
    // frame and sends exactly one frame per period, re-sending it when
    // no new damage arrived. `None` keeps the damage-driven stream
    // (`--fps 0`).
    let mut pacer = pacing_period(started.pacing_fps)
        .map(|period| Pacer::new(period, std::time::Instant::now()));
    let mut last_frame: Option<CaptureFrame> = None;
    // Which captured frame is held, and which one went out last: a
    // send is a repeat exactly when no new capture replaced the held
    // frame since the previous send (the tick may sample a frame that
    // arrived during an earlier iteration). Identity is the generation
    // counter, not the pts: a repeat is restamped in place.
    let mut generation = 0u64;
    let mut last_sent_generation = 0u64;
    loop {
        if std::time::Instant::now() >= heartbeat_at {
            heartbeat_at = std::time::Instant::now() + HEARTBEAT;
            eprintln!(
                "video: encode 2s: encoded={} repeated={} skipped={} errs={} total={}",
                hb_encoded,
                hb_repeated,
                hb_skipped,
                hb_errors,
                counters.frames_encoded.load(Ordering::Relaxed),
            );
            hb_encoded = 0;
            hb_repeated = 0;
            hb_skipped = 0;
            hb_errors = 0;
        }
        if keyframe_req.swap(false, Ordering::Relaxed) {
            // A keyframe request (decoder reset on the tablet) re-opens
            // the encoder: the fresh stream starts with SPS/PPS + IDR.
            match (started.open_encoder)(&started.config) {
                Ok((fresh, info)) => {
                    eprintln!("video: re-opened the encoder ({}) on request", info.codec);
                    encoder = fresh;
                    // The held frame must not wait for the next tick:
                    // the tablet is showing nothing right now.
                    if let Some(pacer) = pacer.as_mut() {
                        pacer.send_now(std::time::Instant::now());
                    }
                }
                Err(err) => eprintln!("video: encoder re-open failed ({err}); continuing"),
            }
        }
        // Wait for a capture frame, but no longer than the pacing
        // deadline: a static screen must not starve the stream.
        let timeout = match pacer.as_ref() {
            Some(pacer) => pacer.wait(std::time::Instant::now()).min(POLL),
            None => POLL,
        };
        let fresh = match started.source.next_frame(timeout) {
            Ok(frame) => frame,
            Err(err) => {
                eprintln!("video: capture failed ({err}); stream ends");
                return counters;
            }
        };
        let had_fresh = fresh.is_some();
        if let Some(frame) = fresh {
            // Newest-frame-wins: an unclaimed frame is replaced, never
            // queued (latency rule 1).
            last_frame = Some(frame);
            generation += 1;
        }
        let now = std::time::Instant::now();
        let send = match pacer.as_ref() {
            Some(pacer) => pacer.due(now),
            // Damage-driven: every captured frame is sent.
            None => had_fresh,
        };
        if !send {
            continue;
        }
        if let Some(pacer) = pacer.as_mut() {
            pacer.advance(now);
        }
        let Some(frame) = last_frame.as_mut() else {
            // No frame captured yet; nothing to repeat.
            continue;
        };
        // Backpressure: no room in the write queue means skip this
        // frame entirely, before encoding, so no encoded frame is
        // ever dropped (latency rule 1).
        if tx.capacity() == 0 {
            counters.frames_skipped.fetch_add(1, Ordering::Relaxed);
            hb_skipped += 1;
            continue;
        }
        if generation == last_sent_generation {
            // A repeat re-sends the same pixels but carries a fresh
            // timestamp (same clock domain as the capture's pts), so
            // pts stays monotonic and the tablet's stats see when the
            // frame was sent rather than when its content changed.
            frame.pts_us = slateway_platform::capture::monotonic_us();
            counters.frames_repeated.fetch_add(1, Ordering::Relaxed);
            hb_repeated += 1;
        } else {
            last_sent_generation = generation;
        }
        match encoder.encode(frame) {
            Ok(packets) => {
                counters.frames_encoded.fetch_add(1, Ordering::Relaxed);
                hb_encoded += 1;
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
                hb_errors += 1;
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
    /// Constant-rate pacing (`--fps`, ADR-015); 0 keeps the
    /// damage-driven stream.
    pub fps: u32,
}

impl VideoSource for PlatformVideoSource {
    fn start(&mut self) -> Result<StartedVideo, String> {
        start_platform(
            &CaptureConfig {
                // Negotiate the pacing rate so the compositor does not
                // deliver faster than the pump will send; a static screen
                // is filled by repeats regardless.
                max_fps: if self.fps > 0 { self.fps } else { 60 },
                restore_token: None,
                with_input: self.allow_input,
            },
            self.fps,
        )
    }
}

#[cfg(target_os = "linux")]
fn start_platform(config: &CaptureConfig, fps: u32) -> Result<StartedVideo, String> {
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
            fps: encoder_fps(info.fps, fps),
            bitrate_kbps: 0,
        },
        open_encoder: Box::new(slateway_linux::encoder::start),
        injector,
        pacing_fps: fps,
    })
}

/// The encoder's nominal rate: the pacing rate when pacing is on, the
/// negotiated capture rate otherwise. It also rides `Configure.fps` to
/// the tablet.
fn encoder_fps(capture_fps: u32, pacing_fps: u32) -> u32 {
    if pacing_fps > 0 {
        pacing_fps
    } else {
        capture_fps
    }
}

#[cfg(target_os = "windows")]
fn start_platform(config: &CaptureConfig, fps: u32) -> Result<StartedVideo, String> {
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
            fps: encoder_fps(info.fps, fps),
            bitrate_kbps: 0,
        },
        open_encoder: Box::new(slateway_win::encoder::start),
        injector,
        pacing_fps: fps,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn start_platform(_config: &CaptureConfig, _fps: u32) -> Result<StartedVideo, String> {
    Err("this platform has no video capture backend".into())
}

#[cfg(test)]
mod tests {
    use super::{Pacer, pacing_period};
    use std::time::{Duration, Instant};

    const PERIOD: Duration = Duration::from_millis(16);

    #[test]
    fn damage_driven_has_no_period() {
        assert_eq!(pacing_period(0), None);
        assert_eq!(pacing_period(60), Some(Duration::from_secs_f64(1.0 / 60.0)));
        assert_eq!(pacing_period(30), Some(Duration::from_secs_f64(1.0 / 30.0)));
    }

    #[test]
    fn tick_holds_a_fresh_frame_to_the_deadline() {
        let t0 = Instant::now();
        let mut pacer = Pacer::new(PERIOD, t0);
        assert!(pacer.due(t0));
        pacer.advance(t0);
        let early = t0 + Duration::from_millis(4);
        assert!(!pacer.due(early), "a fresh frame waits for the tick");
        assert_eq!(pacer.wait(early), Duration::from_millis(12));
        assert!(!pacer.due(t0 + Duration::from_millis(15)));
        assert!(pacer.due(t0 + PERIOD));
    }

    #[test]
    fn stall_sends_once_and_rebases() {
        let t0 = Instant::now();
        let mut pacer = Pacer::new(PERIOD, t0);
        pacer.advance(t0);
        // Blocked for five periods: one send, then a full period of
        // quiet — never a catch-up burst.
        let late = t0 + PERIOD * 5;
        assert!(pacer.due(late));
        pacer.advance(late);
        assert!(!pacer.due(late));
        assert_eq!(pacer.wait(late), PERIOD);
        assert!(pacer.due(late + PERIOD));
    }

    #[test]
    fn keyframe_request_bypasses_the_deadline() {
        let t0 = Instant::now();
        let mut pacer = Pacer::new(PERIOD, t0);
        pacer.advance(t0);
        let now = t0 + Duration::from_millis(1);
        assert!(!pacer.due(now));
        pacer.send_now(now);
        assert!(pacer.due(now));
    }
}
