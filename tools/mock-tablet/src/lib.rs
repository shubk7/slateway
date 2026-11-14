// SPDX-License-Identifier: GPL-3.0-or-later

//! Scripted tablet client for Slateway tests (docs/ARCHITECTURE.md,
//! `tools/mock-tablet`): behaves like the tablet for the daemon's M0
//! echo session, and since protocol minor 2 also for the M1 video
//! stream (`run_video_client`). Noise initiator with pairing
//! confirmation handed to the caller, Hello exchange, Pong answering,
//! and a scripted echo measurement over the experimental echo channel
//! (docs/PROTOCOL.md, "Channels": 16 and above).

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use prost::Message;
use slateway_core::crypto::{
    KEY_LEN, MAX_PLAINTEXT, SecureSession, SessionError, SessionHandshake,
};
use slateway_core::stats::{SessionStats, percentile};
use slateway_proto::Channel;
use slateway_proto::control::{
    CLOSE_TYPE, CONFIGURE_TYPE, Close, CloseReason, HELLO_ACK_TYPE, HELLO_TYPE, Hello, HelloAck,
    PAIR_NEEDED_TYPE, PING_TYPE, PONG_TYPE, Ping, Pong, REQUEST_KEYFRAME_TYPE, STATS_TYPE,
    STREAM_START_TYPE, STREAM_STOP_TYPE, Stats,
};
use slateway_proto::frame::FrameHeader;
use slateway_proto::{VIDEO_AU_TYPE, VIDEO_KEYFRAME_FLAG};
use tokio::io::{AsyncRead, AsyncWrite};

/// Echo payload layout: 8-byte big-endian sequence, 8-byte send stamp,
/// then pattern bytes.
pub const ECHO_HEADER: usize = 16;

/// Client failures.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The socket failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The Noise session or the frame layer failed.
    #[error(transparent)]
    Session(#[from] SessionError),
    /// A control message failed to decode.
    #[error(transparent)]
    Decode(#[from] prost::DecodeError),
    /// The host violated the expected script.
    #[error("protocol violation: {0}")]
    Protocol(&'static str),
    /// The configuration is unusable.
    #[error("configuration: {0}")]
    Config(&'static str),
}

/// What one mock-tablet run should do.
#[derive(Clone, Debug)]
pub struct ClientConfig {
    /// Host address to connect to.
    pub addr: SocketAddr,
    /// Number of echo frames to measure.
    pub frames: usize,
    /// Echo payload size in bytes (at least [`ECHO_HEADER`]).
    pub size: usize,
    /// Pause between echo frames.
    pub interval: Duration,
    /// Device name reported in the handshake.
    pub device_name: String,
    /// Static private key; generated when none is given.
    pub private_key: Option<[u8; KEY_LEN]>,
    /// Send PairNeeded as the first control frame (the tablet-driven
    /// pairing case: this tablet does not trust the host yet).
    pub request_pairing: bool,
}

impl Default for ClientConfig {
    fn default() -> Self {
        ClientConfig {
            addr: SocketAddr::from(([127, 0, 0, 1], 47820)),
            frames: 100,
            size: 1024,
            interval: Duration::from_millis(10),
            device_name: "mock-tablet".into(),
            private_key: None,
            request_pairing: false,
        }
    }
}

/// What one run observed.
#[derive(Debug, Default)]
pub struct ClientSummary {
    /// Echo frames measured.
    pub frames: usize,
    /// Per-frame round trips in microseconds, in request order.
    pub rtt_us: Vec<u64>,
    /// The pairing code this run presented.
    pub pairing_code: String,
    /// The host's app version from HelloAck.
    pub host_version: String,
    /// Frame counters for the report.
    pub stats: SessionStats,
}

impl ClientSummary {
    /// p50 and p95 of the measured round trips.
    pub fn rtt_percentiles(&self) -> (Option<u64>, Option<u64>) {
        (
            percentile(&self.rtt_us, 50.0),
            percentile(&self.rtt_us, 95.0),
        )
    }
}

/// Runs the scripted tablet session over `stream`.
pub async fn run_client<S>(cfg: &ClientConfig, stream: S) -> Result<ClientSummary, ClientError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if cfg.size < ECHO_HEADER {
        return Err(ClientError::Config("echo size must fit the echo header"));
    }
    let private = match cfg.private_key {
        Some(key) => key,
        None => generate_key().await?,
    };
    let mut summary = ClientSummary::default();

    // Handshake (tablet role); msg3 carries the device name.
    let handshake = SessionHandshake::initiator(&private)?;
    let done = handshake
        .complete(stream, cfg.device_name.as_bytes())
        .await?;
    summary.pairing_code = slateway_core::pairing_code(&done.handshake_hash);
    let mut session = done.session;

    // Control frames: Hello, then HelloAck. When the host needs the
    // pairing code confirmed it answers with PairNeeded; the mock
    // simulates the confirmed tablet prompt by resending Hello
    // (docs/PROTOCOL.md, "Handshake and versioning").
    let hello = Hello {
        protocol_major: slateway_proto::control::PROTOCOL_MAJOR,
        protocol_minor: slateway_proto::control::PROTOCOL_MINOR,
        app_version: env!("CARGO_PKG_VERSION").into(),
        device_model: cfg.device_name.clone(),
        android_api: 0,
        codecs: vec![],
        decode_limits: None,
        screen: None,
        capabilities: 0,
    };
    let hello_bytes = hello.encode_to_vec();
    if cfg.request_pairing {
        // Tablet-driven pairing: this tablet does not trust the host
        // and asks for the code confirmation before its Hello.
        send_control(&mut session, PAIR_NEEDED_TYPE, &[]).await?;
    }
    send_control(&mut session, HELLO_TYPE, &hello_bytes).await?;
    let (_, ack_bytes) = loop {
        let (header, bytes) = read_control(&mut session).await?;
        match header.message_type {
            HELLO_ACK_TYPE => break (header, bytes),
            PAIR_NEEDED_TYPE => send_control(&mut session, HELLO_TYPE, &hello_bytes).await?,
            other => {
                let _ = other;
                return Err(ClientError::Protocol(
                    "expected a HelloAck frame after the pairing exchange",
                ));
            }
        }
    };
    let ack = HelloAck::decode(ack_bytes.as_slice())?;
    summary.host_version = ack.app_version;

    // One health report; zeros in M0 (no decode stages yet).
    let stats = Stats::default();
    send_control(&mut session, STATS_TYPE, &stats.encode_to_vec()).await?;

    // Scripted echo measurement, one outstanding request at a time.
    let mut scratch = vec![0u8; MAX_PLAINTEXT];
    for seq in 0..cfg.frames as u64 {
        let sent_us = slateway_core::monotonic_us();
        let payload = echo_payload(seq, sent_us, cfg.size);
        write_echo(&mut session, &payload).await?;
        let rtt = loop {
            let (header, len) = session.read_frame(&mut scratch).await?;
            if header.channel == Channel::Control && header.message_type == PING_TYPE {
                let ping = Ping::decode(&scratch[..len])?;
                let now = slateway_core::monotonic_us();
                let pong = Pong {
                    t0: ping.t0,
                    t1: now,
                    t2: now,
                };
                send_control(&mut session, PONG_TYPE, &pong.encode_to_vec()).await?;
                continue;
            }
            let is_echo = matches!(header.channel, Channel::Experimental { id } if id == slateway_core::echo::CHANNEL)
                && header.message_type == slateway_core::echo::MESSAGE_TYPE;
            if is_echo {
                if len != cfg.size {
                    return Err(ClientError::Protocol("echo length mismatch"));
                }
                let got_seq = u64::from_be_bytes(
                    scratch[..8]
                        .try_into()
                        .map_err(|_| ClientError::Protocol("short echo"))?,
                );
                if got_seq != seq {
                    return Err(ClientError::Protocol("echo sequence mismatch"));
                }
                break slateway_core::monotonic_us().saturating_sub(sent_us);
            }
            // Anything else is discarded; the daemon decides what it sends.
        };
        summary.rtt_us.push(rtt);
        summary.stats.record_in(cfg.size);
        summary.stats.record_out(cfg.size);
        tokio::time::sleep(cfg.interval).await;
    }
    summary.frames = cfg.frames;

    // Orderly shutdown: Close, then drop (docs/PROTOCOL.md, "Control
    // messages"); the daemon treats an abrupt EOF the same way.
    let close = Close {
        reason: CloseReason::NormalShutdown as i32,
    };
    send_control(&mut session, CLOSE_TYPE, &close.encode_to_vec()).await?;
    Ok(summary)
}

/// Encodes one echo request: sequence, send stamp, pattern bytes.
fn echo_payload(seq: u64, sent_us: u64, size: usize) -> Vec<u8> {
    let mut payload = Vec::with_capacity(size);
    payload.extend_from_slice(&seq.to_be_bytes());
    payload.extend_from_slice(&sent_us.to_be_bytes());
    for i in 0..size - ECHO_HEADER {
        payload.push((i % 251) as u8);
    }
    payload
}

async fn write_echo<S: AsyncRead + AsyncWrite + Unpin>(
    session: &mut SecureSession<S>,
    payload: &[u8],
) -> Result<(), ClientError> {
    session
        .write_frame(
            FrameHeader {
                channel: Channel::Experimental {
                    id: slateway_core::echo::CHANNEL,
                },
                flags: 0,
                message_type: slateway_core::echo::MESSAGE_TYPE,
                payload_len: payload.len() as u32,
                pts_us: 0,
            },
            payload,
        )
        .await?;
    Ok(())
}

async fn send_control<S: AsyncRead + AsyncWrite + Unpin>(
    session: &mut SecureSession<S>,
    message_type: u16,
    payload: &[u8],
) -> Result<(), ClientError> {
    session
        .write_frame(
            FrameHeader {
                channel: Channel::Control,
                flags: 0,
                message_type,
                payload_len: payload.len() as u32,
                pts_us: 0,
            },
            payload,
        )
        .await?;
    Ok(())
}

/// Reads one frame into a fresh vector; callers check the header.
async fn read_control<S: AsyncRead + AsyncWrite + Unpin>(
    session: &mut SecureSession<S>,
) -> Result<(FrameHeader, Vec<u8>), ClientError> {
    let mut scratch = vec![0u8; MAX_PLAINTEXT];
    let (header, len) = session.read_frame(&mut scratch).await?;
    Ok((header, scratch[..len].to_vec()))
}

async fn generate_key() -> Result<[u8; KEY_LEN], ClientError> {
    let (private, _public) = slateway_core::crypto::generate_keypair()?;
    private
        .as_slice()
        .try_into()
        .map_err(|_| ClientError::Config("generated key has the wrong length"))
}

/// What one video-mode run should do: mirror the tablet's M1 receive
/// side (docs/PROTOCOL.md, "Channels") against the daemon.
#[derive(Clone, Debug)]
pub struct VideoClientConfig {
    /// Host address to connect to.
    pub addr: SocketAddr,
    /// Device name reported in the handshake.
    pub device_name: String,
    /// Static private key; generated when none is given.
    pub private_key: Option<[u8; KEY_LEN]>,
    /// Access units to receive before the orderly shutdown.
    pub frames: usize,
    /// Send RequestKeyframe after this many AUs and require a
    /// keyframe afterwards.
    pub request_keyframe_after: Option<usize>,
    /// Health-report cadence; one Stats per tick.
    pub stats_tick: Duration,
    /// Input events to send as one batch once the first AU arrived
    /// (M1 touch-as-pointer rehearsal). Sent only after the host
    /// announced CAPABILITY_INPUT in its HelloAck.
    pub input_batch: Vec<slateway_proto::input::InputEvent>,
}

impl Default for VideoClientConfig {
    fn default() -> Self {
        VideoClientConfig {
            addr: SocketAddr::from(([127, 0, 0, 1], 47820)),
            device_name: "mock-tablet".into(),
            private_key: None,
            frames: 10,
            request_keyframe_after: None,
            stats_tick: Duration::from_millis(100),
            input_batch: Vec::new(),
        }
    }
}

/// What one video-mode run observed.
#[derive(Debug, Default)]
pub struct VideoSummary {
    /// The stream geometry from Configure (0 when none arrived).
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// AUs received.
    pub frames: usize,
    /// Keyframes received.
    pub keyframes: usize,
    /// Whether the first AU carried the keyframe flag.
    pub first_frame_was_keyframe: bool,
    /// Whether pts_us never went backwards.
    pub pts_monotonic: bool,
    /// Whether a keyframe followed the RequestKeyframe request.
    pub keyframe_after_request: bool,
    /// Largest AU payload seen (decoder budget sanity).
    pub max_payload: usize,
    /// Input events sent in the scripted batch (0 when none).
    pub inputs_sent: usize,
    /// Frame counters for the report.
    pub stats: SessionStats,
}

/// Runs the scripted video session over `stream`: handshake, expect
/// Configure + StreamStart, receive `frames` video AUs (answering
/// Pings and reporting Stats like the real tablet), optionally
/// request a keyframe mid-run, then Close. Asserts the stream
/// invariants itself and reports the numbers.
pub async fn run_video_client<S>(
    cfg: &VideoClientConfig,
    stream: S,
) -> Result<VideoSummary, ClientError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let private = match cfg.private_key {
        Some(key) => key,
        None => generate_key().await?,
    };
    let mut summary = VideoSummary::default();

    let handshake = SessionHandshake::initiator(&private)?;
    let done = handshake
        .complete(stream, cfg.device_name.as_bytes())
        .await?;
    let mut session = done.session;
    let mut scratch = vec![0u8; MAX_PLAINTEXT];

    // Hello, then HelloAck (no pairing: tests run with known tablets
    // or --auto-pair on the daemon side).
    let hello = Hello {
        protocol_major: slateway_proto::control::PROTOCOL_MAJOR,
        protocol_minor: slateway_proto::control::PROTOCOL_MINOR,
        app_version: env!("CARGO_PKG_VERSION").into(),
        device_model: cfg.device_name.clone(),
        android_api: 30,
        codecs: vec![slateway_proto::control::VideoCodec::H264High as i32],
        decode_limits: None,
        screen: None,
        capabilities: 0,
    };
    send_control(&mut session, HELLO_TYPE, &hello.encode_to_vec()).await?;
    let host_capabilities: u64 = loop {
        let (header, bytes) = read_control(&mut session).await?;
        match header.message_type {
            HELLO_ACK_TYPE => break HelloAck::decode(bytes.as_slice())?.capabilities,
            PAIR_NEEDED_TYPE => {
                send_control(&mut session, HELLO_TYPE, &hello.encode_to_vec()).await?
            }
            _ => {
                return Err(ClientError::Protocol(
                    "expected HelloAck before any streaming",
                ));
            }
        }
    };

    // Expect Configure then StreamStart (auto-stream after handshake,
    // the user-approved M1 flow); anything else is a violation.
    let mut saw_configure = false;
    let mut saw_start = false;
    while !(saw_configure && saw_start) {
        let (header, bytes) = read_control(&mut session).await?;
        match header.message_type {
            CONFIGURE_TYPE => {
                let configure = slateway_proto::control::Configure::decode(bytes.as_slice())?;
                summary.width = configure.width_px;
                summary.height = configure.height_px;
                summary.fps = configure.fps;
                saw_configure = true;
            }
            STREAM_START_TYPE => {
                if !saw_configure {
                    return Err(ClientError::Protocol("StreamStart before Configure"));
                }
                saw_start = true;
            }
            _ => {
                return Err(ClientError::Protocol(
                    "expected Configure and StreamStart after HelloAck",
                ));
            }
        }
    }

    // Receive loop: video AUs, Pings answered, periodic Stats, one
    // RequestKeyframe when scripted. Every invariant the tablet's
    // decoder would enforce is checked here.
    let mut next_stats = tokio::time::Instant::now() + cfg.stats_tick;
    let mut last_pts: Option<u64> = None;
    let mut requested_at: Option<usize> = cfg
        .request_keyframe_after
        .filter(|after| *after < cfg.frames);
    summary.pts_monotonic = true;
    while summary.frames < cfg.frames {
        let (header, payload) = tokio::select! {
            read = read_any(&mut session, &mut scratch) => read?,
            _ = tokio::time::sleep_until(next_stats) => {
                let stats = Stats::default();
                send_control(&mut session, STATS_TYPE, &stats.encode_to_vec()).await?;
                next_stats = tokio::time::Instant::now() + cfg.stats_tick;
                continue;
            }
        };
        match (header.channel, header.message_type) {
            (Channel::Control, PING_TYPE) => {
                let ping = Ping::decode(payload.as_slice())?;
                let now = slateway_core::monotonic_us();
                let pong = Pong {
                    t0: ping.t0,
                    t1: now,
                    t2: now,
                };
                send_control(&mut session, PONG_TYPE, &pong.encode_to_vec()).await?;
            }
            (Channel::Control, STREAM_STOP_TYPE) => {
                return Err(ClientError::Protocol(
                    "StreamStop before the scripted frame count",
                ));
            }
            (Channel::Video, VIDEO_AU_TYPE) => {
                if payload.is_empty() {
                    return Err(ClientError::Protocol("empty video access unit"));
                }
                let keyframe = header.flags & VIDEO_KEYFRAME_FLAG != 0;
                if header.flags & !VIDEO_KEYFRAME_FLAG != 0 {
                    return Err(ClientError::Protocol("unknown video flags set"));
                }
                if summary.frames == 0 {
                    summary.first_frame_was_keyframe = keyframe;
                    // Scripted input: one batch once the stream is
                    // visibly running and the host accepted input.
                    if !cfg.input_batch.is_empty() {
                        if host_capabilities & u64::from(slateway_proto::control::CAPABILITY_INPUT)
                            == 0
                        {
                            return Err(ClientError::Protocol(
                                "host did not announce the input capability",
                            ));
                        }
                        send_input_batch(&mut session, &cfg.input_batch).await?;
                        summary.inputs_sent = cfg.input_batch.len();
                    }
                }
                if keyframe {
                    summary.keyframes += 1;
                    // The request went out after requested_at AUs; the
                    // next keyframe satisfies it.
                    if let Some(after) = requested_at
                        && summary.frames >= after
                    {
                        summary.keyframe_after_request = true;
                        requested_at = None;
                    }
                }
                if let Some(last) = last_pts
                    && header.pts_us < last
                {
                    summary.pts_monotonic = false;
                }
                last_pts = Some(header.pts_us);
                summary.max_payload = summary.max_payload.max(payload.len());
                summary.stats.record_in(payload.len());
                summary.frames += 1;
                if Some(summary.frames) == requested_at {
                    send_control(&mut session, REQUEST_KEYFRAME_TYPE, &[]).await?;
                }
            }
            _ => {
                return Err(ClientError::Protocol("unexpected frame during video"));
            }
        }
    }
    if requested_at.is_some() {
        // The request went out but no keyframe followed: report it.
        summary.keyframe_after_request = false;
    }

    // Orderly shutdown.
    let close = Close {
        reason: CloseReason::NormalShutdown as i32,
    };
    send_control(&mut session, CLOSE_TYPE, &close.encode_to_vec()).await?;
    Ok(summary)
}

/// Reads one frame into a fresh vector; callers check the header.
async fn read_any<S: AsyncRead + AsyncWrite + Unpin>(
    session: &mut SecureSession<S>,
    scratch: &mut [u8],
) -> Result<(FrameHeader, Vec<u8>), ClientError> {
    let (header, len) = session.read_frame(scratch).await?;
    Ok((header, scratch[..len].to_vec()))
}

/// Sends one input-channel batch: events packed back to back
/// (docs/PROTOCOL.md, "Input event").
async fn send_input_batch<S: AsyncRead + AsyncWrite + Unpin>(
    session: &mut SecureSession<S>,
    events: &[slateway_proto::input::InputEvent],
) -> Result<(), ClientError> {
    let mut payload = vec![0u8; slateway_proto::input::batch_len(events.len())];
    slateway_proto::input::encode_batch_into(events, &mut payload)
        .map_err(|_| ClientError::Protocol("input batch did not fit its buffer"))?;
    session
        .write_frame(
            FrameHeader {
                channel: Channel::Input,
                flags: 0,
                message_type: slateway_proto::input::INPUT_BATCH_TYPE,
                payload_len: payload.len() as u32,
                pts_us: 0,
            },
            &payload,
        )
        .await?;
    Ok(())
}
