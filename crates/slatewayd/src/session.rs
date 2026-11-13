// SPDX-License-Identifier: GPL-3.0-or-later

//! The daemon's M0 echo session (docs/ROADMAP.md): Noise responder,
//! pairing, Hello/HelloAck, clock sync and the echo of experimental
//! channel frames. Capture and encoding arrive in M1.

use std::sync::Arc;
use std::time::Duration;

use prost::Message;
use slateway_core::clock::monotonic_us;
use slateway_core::crypto::{SecureSession, SessionError, SessionHandshake};
use slateway_core::echo;
use slateway_core::keys::{KnownTablet, KnownTablets};
use slateway_core::stats::SessionStats;
use slateway_core::{ClockSync, KEY_LEN, MAX_PLAINTEXT};
use slateway_proto::Channel;
use slateway_proto::control::{
    CLOSE_TYPE, Close, CloseReason, HELLO_ACK_TYPE, HELLO_TYPE, Hello, HelloAck, PAIR_NEEDED_TYPE,
    PING_TYPE, PONG_TYPE, PairNeeded, Ping, Pong, STATS_TYPE, Stats,
};
use slateway_proto::frame::FrameHeader;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Mutex;

/// Tablet registry shared across sessions: sessions run as tasks (a
/// pairing prompt must not freeze the accept loop), so the registry is
/// behind an async mutex instead of an exclusive borrow.
pub type SharedTablets = Arc<Mutex<KnownTablets>>;

/// Session behaviour knobs.
#[derive(Clone, Debug)]
pub struct SessionConfig {
    /// Echo experimental-channel frames back (the M0 exit measurement).
    pub echo: bool,
    /// Clock-sync cadence; one Ping per tick.
    pub tick: Duration,
}

impl Default for SessionConfig {
    fn default() -> Self {
        SessionConfig {
            echo: true,
            tick: Duration::from_secs(1),
        }
    }
}

/// What one served session observed.
#[derive(Debug, Default)]
pub struct SessionReport {
    /// True when this run paired a previously unknown tablet.
    pub paired: bool,
    /// The tablet's name (handshake payload, or the stored name).
    pub device_name: String,
    /// Echo frames returned to the tablet.
    pub echoed: u64,
    /// The session's clock-sync state (empty until the first tick).
    pub clock: ClockSync,
    /// Frame and byte counters plus the tablet's reports.
    pub stats: SessionStats,
}

/// Session failures beyond [`SessionError`]: console pairing input.
#[derive(Debug, thiserror::Error)]
pub enum SessionFailure {
    /// The Noise session or the frame layer failed.
    #[error(transparent)]
    Session(#[from] SessionError),
    /// The host refused or could not complete pairing.
    #[error("pairing failed: {0}")]
    Pairing(&'static str),
    /// A control message failed to decode.
    #[error(transparent)]
    Decode(#[from] prost::DecodeError),
}

/// Serves one tablet connection until it closes or sends Close.
///
/// `confirm` is consulted for unknown tablets with the 6-digit code;
/// returning true pairs and stores the tablet. The M0 exit criteria
/// (echo with RTT and clock offset) read from the returned report.
///
/// Stage diagnostics go to stderr (`session: …`); stdout stays for the
/// operator-facing prompts and reports. Frames and payloads are never
/// logged (docs/SECURITY.md).
pub async fn serve_once<S>(
    mut stream: S,
    host_private: &[u8; KEY_LEN],
    known: &SharedTablets,
    cfg: &SessionConfig,
    confirm: &(dyn Fn(&str) -> bool + Send + Sync),
) -> Result<SessionReport, SessionFailure>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let handshake = SessionHandshake::responder(host_private)?;
    let done = handshake.complete(&mut stream, &[]).await?;
    let remote_static = done.remote_static;
    let device_name = String::from_utf8_lossy(&done.peer_payload)
        .trim()
        .to_owned();
    let device_name = if device_name.is_empty() {
        "tablet".to_owned()
    } else {
        device_name
    };
    eprintln!("session: noise handshake complete, device '{device_name}'");

    let mut report = SessionReport::default();
    let mut session = done.session;

    // Pairing gate (docs/PROTOCOL.md, "Handshake and versioning";
    // docs/SECURITY.md, "Pairing"): the tablet's first control frame
    // says whether IT needs the code confirmed, the registry says
    // whether the daemon does. The prompt runs when either side needs
    // pairing, so the code is confirmed on both screens whenever
    // either one requires it.
    let known_name = known
        .lock()
        .await
        .find(&remote_static)
        .map(|tablet| tablet.name.clone());
    let daemon_wants_pairing = known_name.is_none();
    match &known_name {
        Some(name) => eprintln!("session: tablet is known ('{name}')"),
        None => eprintln!("session: unknown tablet, pairing required"),
    }

    let mut scratch = vec![0u8; MAX_PLAINTEXT];

    // The tablet's first control frame: PairNeeded or Hello.
    let (header, len) = session.read_frame(&mut scratch).await?;
    if header.channel != Channel::Control {
        return Err(SessionError::Protocol("expected a control frame").into());
    }
    let tablet_wants_pairing = match header.message_type {
        PAIR_NEEDED_TYPE => {
            eprintln!("session: tablet requests pairing confirmation");
            true
        }
        HELLO_TYPE => false,
        _ => return Err(SessionError::Protocol("expected Hello or PairNeeded").into()),
    };

    // One operator prompt, when either side needs pairing. The prompt
    // blocks this task only; the accept loop stays live.
    if tablet_wants_pairing || daemon_wants_pairing {
        let code = slateway_core::pairing_code(&done.handshake_hash);
        if !confirm(&code) {
            eprintln!("session: pairing refused");
            // Best effort: the tablet may still be waiting for an
            // answer; give it the orderly shutdown.
            let _ = send_control(
                &mut session,
                CLOSE_TYPE,
                &Close {
                    reason: CloseReason::Error as i32,
                }
                .encode_to_vec(),
            )
            .await;
            return Err(SessionFailure::Pairing(
                "the operator did not confirm the code",
            ));
        }
        eprintln!("session: pairing confirmed by the operator");
    }

    // Settle the Hello: after the tablet's own prompt it sends Hello;
    // if it sent Hello directly but the daemon needs pairing, it
    // resends Hello after the daemon's PairNeeded.
    let (_header, len) = if tablet_wants_pairing {
        let (header, len) = session.read_frame(&mut scratch).await?;
        if header.channel != Channel::Control || header.message_type != HELLO_TYPE {
            return Err(SessionError::Protocol(
                "expected Hello after the tablet's pairing confirmation",
            )
            .into());
        }
        (header, len)
    } else if daemon_wants_pairing {
        send_control(
            &mut session,
            PAIR_NEEDED_TYPE,
            &PairNeeded {}.encode_to_vec(),
        )
        .await?;
        eprintln!("session: asked the tablet to confirm the pairing code");
        let (header, len) = session.read_frame(&mut scratch).await?;
        if header.channel != Channel::Control || header.message_type != HELLO_TYPE {
            return Err(SessionError::Protocol(
                "expected Hello re-sent after the tablet's pairing confirmation",
            )
            .into());
        }
        (header, len)
    } else {
        (header, len)
    };

    let hello = Hello::decode(&scratch[..len])?;
    if hello.protocol_major != slateway_proto::control::PROTOCOL_MAJOR {
        send_control(
            &mut session,
            CLOSE_TYPE,
            &Close {
                reason: CloseReason::MajorMismatch as i32,
            }
            .encode_to_vec(),
        )
        .await?;
        return Err(SessionError::Protocol("protocol major mismatch").into());
    }
    eprintln!(
        "session: hello from '{}' (app '{}', android api {})",
        hello.device_model, hello.app_version, hello.android_api
    );
    let ack = HelloAck {
        protocol_major: slateway_proto::control::PROTOCOL_MAJOR,
        protocol_minor: slateway_proto::control::PROTOCOL_MINOR,
        app_version: env!("CARGO_PKG_VERSION").into(),
        capabilities: 0,
    };
    send_control(&mut session, HELLO_ACK_TYPE, &ack.encode_to_vec()).await?;

    // The exchange completed with the operator's confirmation: store
    // the tablet so later sessions pair silently.
    if daemon_wants_pairing {
        known
            .lock()
            .await
            .insert(KnownTablet {
                public: remote_static,
                name: device_name.clone(),
            })
            .map_err(|_err| SessionFailure::Pairing("could not store the paired tablet"))?;
        eprintln!("session: tablet '{device_name}' paired and stored");
        report.paired = true;
    }
    report.device_name = known_name.unwrap_or_else(|| device_name.clone());

    // Session loop: frames from the tablet, and one Ping per tick. A
    // deadline-bounded read keeps the channel free for the tick's
    // write; the deadline is absolute so frequent frames cannot starve
    // the clock sync.
    let mut clock = ClockSync::default();
    let mut stats = SessionStats::default();
    let mut next_ping = tokio::time::Instant::now() + cfg.tick;
    let mut clock_announced = false;
    loop {
        let read = tokio::time::timeout_at(next_ping, session.read_frame(&mut scratch)).await;
        match read {
            Err(_deadline_elapsed) => {
                send_ping(&mut session).await?;
                stats.record_out(PING_WIRE_LEN);
                next_ping = tokio::time::Instant::now() + cfg.tick;
            }
            Ok(Err(err)) => {
                if is_disconnect(&err) {
                    // The tablet went away (clean EOF or reset): the
                    // session ends and the report still counts.
                    eprintln!("session: tablet closed the connection");
                    break;
                }
                return Err(err.into());
            }
            Ok(Ok((header, len))) => {
                stats.record_in(len);
                let payload = &scratch[..len];
                match (header.channel, header.message_type) {
                    (Channel::Control, PONG_TYPE) => {
                        let pong = Pong::decode(payload)?;
                        match clock.on_pong(pong.t0, pong.t1, pong.t2, monotonic_us()) {
                            None => {
                                eprintln!("session: rejected a physically impossible clock sample")
                            }
                            Some(_sample) => {
                                if let (false, Some(estimate)) = (clock_announced, clock.estimate())
                                {
                                    clock_announced = true;
                                    eprintln!(
                                        "session: clock sync live (rtt {} us)",
                                        estimate.rtt_us
                                    );
                                }
                            }
                        }
                    }
                    (Channel::Control, STATS_TYPE) => {
                        if let Ok(stats_msg) = Stats::decode(payload) {
                            stats.record_tablet(stats_msg);
                        }
                    }
                    (Channel::Control, CLOSE_TYPE) => {
                        eprintln!("session: tablet sent Close");
                        break;
                    }
                    (Channel::Control, HELLO_TYPE) => {
                        return Err(SessionError::Protocol("unexpected duplicate Hello").into());
                    }
                    (Channel::Experimental { id }, message_type)
                        if id == echo::CHANNEL
                            && message_type == echo::MESSAGE_TYPE
                            && cfg.echo =>
                    {
                        session
                            .write_frame(
                                FrameHeader {
                                    channel: Channel::Experimental { id: echo::CHANNEL },
                                    flags: 0,
                                    message_type: echo::MESSAGE_TYPE,
                                    payload_len: len as u32,
                                    pts_us: 0,
                                },
                                payload,
                            )
                            .await?;
                        report.echoed += 1;
                        stats.record_out(len);
                    }
                    _ => {}
                }
                if tokio::time::Instant::now() >= next_ping {
                    send_ping(&mut session).await?;
                    next_ping = tokio::time::Instant::now() + cfg.tick;
                }
            }
        }
    }
    report.clock = clock;
    report.stats = stats;
    Ok(report)
}

/// Wire length of a Ping frame payload: one varint field, 0 at t0=0.
const PING_WIRE_LEN: usize = 2;

async fn send_ping<S: AsyncRead + AsyncWrite + Unpin>(
    session: &mut SecureSession<S>,
) -> Result<(), SessionError> {
    let ping = Ping { t0: monotonic_us() };
    let bytes = ping.encode_to_vec();
    session
        .write_frame(
            FrameHeader {
                channel: Channel::Control,
                flags: 0,
                message_type: PING_TYPE,
                payload_len: bytes.len() as u32,
                pts_us: 0,
            },
            &bytes,
        )
        .await
}

/// Whether a session error means the tablet disconnected (as opposed
/// to a protocol failure worth reporting).
fn is_disconnect(err: &SessionError) -> bool {
    match err {
        SessionError::Io(io) => matches!(
            io.kind(),
            std::io::ErrorKind::UnexpectedEof
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::BrokenPipe
        ),
        _ => false,
    }
}

async fn send_control<S: AsyncRead + AsyncWrite + Unpin>(
    session: &mut SecureSession<S>,
    message_type: u16,
    payload: &[u8],
) -> Result<(), SessionError> {
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
        .await
}
#[cfg(test)]
mod tests {
    use super::*;
    use mock_tablet::{ClientConfig, run_client};
    use slateway_core::crypto::generate_keypair;
    use tempfile::TempDir;

    /// The M0 exit rehearsal: the daemon's session versus the scripted
    /// mock tablet over one encrypted session - pairing, Hello exchange,
    /// clock sync and the echo measurement, in-process.
    #[tokio::test]
    async fn echo_session_with_the_mock_tablet() {
        let dir = TempDir::new().unwrap();
        let known: SharedTablets = Arc::new(Mutex::new(
            KnownTablets::load(&dir.path().join("known-tablets")).unwrap(),
        ));
        let (host_priv, _) = generate_keypair().unwrap();
        let host_priv: [u8; KEY_LEN] = host_priv.as_slice().try_into().unwrap();

        let (a, b) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            let cfg = SessionConfig {
                echo: true,
                tick: Duration::from_millis(20),
            };
            let confirm = |_code: &str| true;
            serve_once(a, &host_priv, &known, &cfg, &confirm).await
        });

        let client_cfg = ClientConfig {
            frames: 20,
            size: 1000,
            interval: Duration::from_millis(5),
            device_name: "mock-tablet".into(),
            ..Default::default()
        };
        let client = run_client(&client_cfg, b);
        let (client_res, server_res) = tokio::join!(client, server);
        let summary = client_res.unwrap_or_else(|err| panic!("client failed: {err}"));
        let report = server_res
            .unwrap_or_else(|err| panic!("server task failed: {err:?}"))
            .unwrap_or_else(|err| panic!("server failed: {err}"));
        assert_eq!(summary.frames, 20);
        assert_eq!(summary.rtt_us.len(), 20);
        assert!(summary.rtt_us.iter().all(|rtt| *rtt > 0));
        assert!(!summary.pairing_code.is_empty());
        assert!(!summary.host_version.is_empty());
        assert!(report.paired, "the first run pairs an unknown tablet");
        assert_eq!(report.device_name, "mock-tablet");
        assert_eq!(report.echoed, 20);
        assert!(
            report.clock.estimate().is_some(),
            "clock pings flowed within the test window"
        );
        assert!(report.stats.tablet.is_some(), "the mock reports Stats");
        // 20 echo frames + Hello + Stats, plus one Pong per clock tick.
        assert!(
            report.stats.frames_in >= 22,
            "echo frames, Hello, Stats and Pongs must arrive"
        );
    }

    /// A tablet that trusts the host but is unknown to the daemon
    /// sends Hello directly; the daemon answers PairNeeded and the
    /// tablet re-sends Hello after its (simulated) prompt. This is
    /// the echo rehearsal's path: the daemon prompts once, stores the
    /// tablet, and completes the exchange.
    #[tokio::test]
    async fn unknown_tablet_sending_hello_is_asked_to_pair() {
        let dir = TempDir::new().unwrap();
        let known: SharedTablets = Arc::new(Mutex::new(
            KnownTablets::load(&dir.path().join("known-tablets")).unwrap(),
        ));
        let (host_priv, _) = generate_keypair().unwrap();
        let host_priv: [u8; KEY_LEN] = host_priv.as_slice().try_into().unwrap();

        let (a, b) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            let cfg = SessionConfig {
                echo: true,
                tick: Duration::from_millis(20),
            };
            let confirm = |_code: &str| true;
            serve_once(a, &host_priv, &known, &cfg, &confirm).await
        });

        let client_cfg = ClientConfig {
            frames: 1,
            size: 64,
            interval: Duration::from_millis(1),
            device_name: "hello-first".into(),
            ..Default::default()
        };
        let client = run_client(&client_cfg, b);
        let (client_res, server_res) = tokio::join!(client, server);
        client_res.unwrap_or_else(|err| panic!("client failed: {err}"));
        let report = server_res
            .unwrap_or_else(|err| panic!("server task failed: {err:?}"))
            .unwrap_or_else(|err| panic!("server failed: {err}"));
        assert!(report.paired, "the unknown tablet is stored after pairing");
        assert_eq!(report.device_name, "hello-first");
    }

    /// A KNOWN tablet that lost its host trust sends PairNeeded first:
    /// the daemon must prompt anyway (the code is confirmed on both
    /// screens), not store a second entry, and still complete.
    #[tokio::test]
    async fn known_tablet_requesting_pairing_gets_a_prompt() {
        let dir = TempDir::new().unwrap();
        let known: SharedTablets = Arc::new(Mutex::new(
            KnownTablets::load(&dir.path().join("known-tablets")).unwrap(),
        ));
        let (host_priv, _) = generate_keypair().unwrap();
        let host_priv: [u8; KEY_LEN] = host_priv.as_slice().try_into().unwrap();
        let (tablet_priv, _) = generate_keypair().unwrap();

        let cfg = SessionConfig {
            echo: true,
            tick: Duration::from_millis(20),
        };
        let client_cfg = ClientConfig {
            frames: 1,
            size: 64,
            interval: Duration::from_millis(1),
            device_name: "S6".into(),
            private_key: Some(tablet_priv.as_slice().try_into().unwrap()),
            request_pairing: true,
            ..Default::default()
        };

        // First run pairs and stores (the daemon's prompt auto-confirms).
        let (a, b) = tokio::io::duplex(4096);
        let client = run_client(&client_cfg, b);
        let confirm = |_code: &str| true;
        let server = serve_once(a, &host_priv, &known, &cfg, &confirm);
        let (summary, report) = tokio::join!(client, server);
        assert_eq!(summary.unwrap().frames, 1);
        assert!(report.unwrap().paired);

        // Second run: same tablet key (still known to the daemon), but
        // the tablet lost its host trust and asks for pairing. The
        // daemon prompts (counted), does NOT re-store, and completes.
        let (a, b) = tokio::io::duplex(4096);
        let client = run_client(&client_cfg, b);
        let prompted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let prompt_counter = Arc::clone(&prompted);
        let confirm = move |_code: &str| {
            prompt_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            true
        };
        let server = serve_once(a, &host_priv, &known, &cfg, &confirm);
        let (summary, report) = tokio::join!(client, server);
        assert_eq!(summary.unwrap().frames, 1);
        let report = report.unwrap();
        assert!(!report.paired, "a known tablet must not be stored twice");
        assert_eq!(report.device_name, "S6");
        assert_eq!(
            prompted.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the daemon must prompt when the tablet requests pairing"
        );
    }

    /// A second run from the same tablet must be recognized, not
    /// re-paired (docs/SECURITY.md: the pinned key is required).
    #[tokio::test]
    async fn known_tablet_is_not_paired_again() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("known-tablets");
        let known: SharedTablets = Arc::new(Mutex::new(KnownTablets::load(&path).unwrap()));
        let (host_priv, _) = generate_keypair().unwrap();
        let host_priv: [u8; KEY_LEN] = host_priv.as_slice().try_into().unwrap();
        let (tablet_priv, _) = generate_keypair().unwrap();

        let cfg = SessionConfig {
            echo: true,
            tick: Duration::from_millis(10),
        };

        // First run pairs and stores.
        let (a, b) = tokio::io::duplex(4096);
        let client_cfg = ClientConfig {
            frames: 1,
            size: 64,
            interval: Duration::from_millis(1),
            device_name: "Tab".into(),
            private_key: Some(tablet_priv.as_slice().try_into().unwrap()),
            ..Default::default()
        };
        let client = run_client(&client_cfg, b);
        let confirm = |_code: &str| true;
        let server = serve_once(a, &host_priv, &known, &cfg, &confirm);
        let (summary, report) = tokio::join!(client, server);
        assert_eq!(summary.unwrap().frames, 1);
        assert!(report.unwrap().paired);

        // Second run with the same key: no prompt, no new pairing.
        let (a, b) = tokio::io::duplex(4096);
        let client = run_client(&client_cfg, a);
        let confirm = |_code: &str| panic!("must not prompt for a known tablet");
        let server = serve_once(b, &host_priv, &known, &cfg, &confirm);
        let (summary, report) = tokio::join!(client, server);
        let report = report.unwrap();
        assert!(!report.paired, "a known tablet must not pair again");
        assert_eq!(report.device_name, "Tab");
        assert_eq!(summary.unwrap().frames, 1);
    }
}
