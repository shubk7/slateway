// SPDX-License-Identifier: GPL-3.0-or-later

//! Scripted tablet client for Slateway tests (docs/ARCHITECTURE.md,
//! `tools/mock-tablet`): behaves like the tablet for the daemon's M0
//! echo session. Noise initiator with pairing confirmation handed to
//! the caller, Hello exchange, Pong answering, and a scripted echo
//! measurement over the experimental echo channel
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
    CLOSE_TYPE, Close, CloseReason, HELLO_ACK_TYPE, HELLO_TYPE, Hello, HelloAck, PING_TYPE,
    PONG_TYPE, Ping, Pong, STATS_TYPE, Stats,
};
use slateway_proto::frame::FrameHeader;
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

    // Control frames: Hello, then HelloAck.
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
    send_control(&mut session, HELLO_TYPE, &hello.encode_to_vec()).await?;
    let (ack_header, ack_bytes) = read_control(&mut session).await?;
    if ack_header.message_type != HELLO_ACK_TYPE {
        return Err(ClientError::Protocol("expected a HelloAck frame"));
    }
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
