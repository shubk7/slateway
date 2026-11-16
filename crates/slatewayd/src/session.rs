// SPDX-License-Identifier: GPL-3.0-or-later

//! The daemon's session (docs/ROADMAP.md): Noise responder, pairing,
//! Hello/HelloAck, clock sync, the echo of experimental channel frames,
//! and since protocol minor 2 the M1 video stream
//! (capture → encode → video channel, `--mirror`).

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
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
    CAPABILITY_INPUT, CLOSE_TYPE, CONFIGURE_TYPE, Close, CloseReason, HELLO_ACK_TYPE, HELLO_TYPE,
    Hello, HelloAck, PAIR_NEEDED_TYPE, PING_TYPE, PONG_TYPE, PairNeeded, Ping, Pong,
    REQUEST_KEYFRAME_TYPE, STATS_TYPE, STREAM_START_TYPE, Stats, StreamMode, VideoCodec,
};
use slateway_proto::frame::FrameHeader;
use slateway_proto::{INPUT_BATCH_TYPE, InputEvent};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Mutex;

use crate::input::{InputCounters, InputTx, parse_input_batch, run_input_worker};
use crate::video::{PumpCounters, VideoSource};

/// Tablet registry shared across sessions: sessions run as tasks (a
/// pairing prompt must not freeze the accept loop), so the registry is
/// behind an async mutex instead of an exclusive borrow.
pub type SharedTablets = Arc<Mutex<KnownTablets>>;

/// One queued write: a frame header plus its payload, in wire order.
/// Video and control frames share this queue so the stream order
/// holds (Configure before StreamStart before the first keyframe).
type WriteCmd = (FrameHeader, Vec<u8>);

/// Capacity of the shared write queue in frames. Latency rule 1 caps
/// pipeline queues at about two frames: the pump starts skipping
/// captures while this is full, and the slots above two keep tiny
/// control frames from starving behind video AUs.
const WRITE_QUEUE: usize = 4;

/// Session behaviour knobs.
#[derive(Clone, Debug)]
pub struct SessionConfig {
    /// Echo experimental-channel frames back (the M0 exit measurement).
    pub echo: bool,
    /// Clock-sync cadence; one Ping per tick.
    pub tick: Duration,
    /// Touch-as-pointer consented via `--allow-input` (docs/SECURITY.md,
    /// "Consent"): announces [`CAPABILITY_INPUT`] in the HelloAck and
    /// runs the input worker when the platform source yields an
    /// injector. Injection itself needs the video session's injector.
    pub input: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        SessionConfig {
            echo: true,
            tick: Duration::from_secs(1),
            input: false,
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
    /// Frames read and not handled by any session stage (unknown
    /// control types, non-echo experimental frames when echo is off).
    pub discarded: u64,
    /// The session's clock-sync state (empty until the first tick).
    pub clock: ClockSync,
    /// Frame and byte counters plus the tablet's reports.
    pub stats: SessionStats,
    /// Video pump counters (zero when the session ran without video).
    pub video: PumpCounters,
    /// Input worker counters (zero when the session ran without input).
    pub input: InputCounters,
}

/// Session failures beyond [`SessionError`]: console pairing input and
/// video setup.
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
    /// The video stream could not start (capture or encoder setup).
    #[error("video stream failed: {0}")]
    Video(String),
}

/// Serves one tablet connection until it closes or sends Close.
///
/// `confirm` is consulted for unknown tablets with the 6-digit code;
/// returning true pairs and stores the tablet. The M0 exit criteria
/// (echo with RTT and clock offset) read from the returned report.
/// When `video` is given, the session auto-streams after the
/// handshake (user-approved for M1): Configure, StreamStart, then the
/// capture → encode pump.
///
/// Stage diagnostics go to stderr (`session: …` / `video: …`); stdout
/// stays for the operator-facing prompts and reports. Frames and
/// payloads are never logged (docs/SECURITY.md).
pub async fn serve_once<S>(
    stream: S,
    host_private: &[u8; KEY_LEN],
    known: &SharedTablets,
    cfg: &SessionConfig,
    confirm: &(dyn Fn(&str, &str) -> bool + Send + Sync),
    video: Option<Box<dyn VideoSource>>,
) -> Result<SessionReport, SessionFailure>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let handshake = SessionHandshake::responder(host_private)?;
    let done = handshake.complete(stream, &[]).await?;
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
    // names the device so the operator knows which tablet is asking;
    // it blocks this task only, the accept loop stays live.
    let display_name = known_name.clone().unwrap_or_else(|| device_name.clone());
    if tablet_wants_pairing || daemon_wants_pairing {
        let code = slateway_core::pairing_code(&done.handshake_hash);
        if !confirm(&code, &display_name) {
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
    // HelloAck capability bitmask (docs/PROTOCOL.md): bit0 announces
    // input acceptance — the operator's --allow-input decision, sent
    // before the capture's consent dialog could delay the ack. When
    // the injector later fails to come up, tablet input frames are
    // discarded (counted), never injected.
    let capabilities: u64 = u64::from(if cfg.input { CAPABILITY_INPUT } else { 0 });
    let ack = HelloAck {
        protocol_major: slateway_proto::control::PROTOCOL_MAJOR,
        protocol_minor: slateway_proto::control::PROTOCOL_MINOR,
        app_version: env!("CARGO_PKG_VERSION").into(),
        capabilities,
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

    // From here the session splits: the broker task owns the
    // encrypted session and multiplexes the queued writes against the
    // read loop (a cancelled read resumes exactly where it stopped:
    // the frame layer never loses stream bytes). The session logic
    // below talks to it through two channels; the video pump writes
    // through the same queue, so frame order holds: Configure and
    // StreamStart are queued before the pump starts.
    let (write_tx, write_rx) = tokio::sync::mpsc::channel::<WriteCmd>(WRITE_QUEUE);
    let (read_tx, mut read_rx) = tokio::sync::mpsc::channel::<(FrameHeader, Vec<u8>)>(64);

    // Video setup blocks (on Linux that includes the portal consent
    // dialog) and queues its control frames before the broker and the
    // pump start, so the tablet sees Configure, StreamStart, then the
    // first access unit, in order.
    let mut video_join: Option<std::thread::JoinHandle<PumpCounters>> = None;
    let mut keyframe_req: Option<Arc<AtomicBool>> = None;
    // Signals the input worker to stop (session teardown).
    let mut input_stop: Option<Arc<AtomicBool>> = None;
    // Input: the worker owns the injector on a dedicated thread; the
    // session loop forwards parsed events through this sender. The
    // receiver moves into the worker below; with no injector it drops
    // here and session-loop sends fail harmlessly (counted).
    let (input_tx, input_rx) = std::sync::mpsc::channel::<InputEvent>();
    let mut input_join: Option<std::thread::JoinHandle<InputCounters>> = None;
    if let Some(mut source) = video {
        // The portal capture blocks inside its own one-shot runtime
        // (consent dialog, D-Bus); the trait contract demands a
        // dedicated OS thread, and block_on panics on an async worker
        // (tokio: "Cannot start a runtime from within a runtime").
        // spawn_blocking is that thread.
        let start_result = match tokio::task::spawn_blocking(move || source.start()).await {
            Ok(result) => result,
            Err(join_err) => {
                let _ = send_control(
                    &mut session,
                    CLOSE_TYPE,
                    &Close {
                        reason: CloseReason::Error as i32,
                    }
                    .encode_to_vec(),
                )
                .await;
                return Err(SessionFailure::Video(format!(
                    "video start task: {join_err}"
                )));
            }
        };
        let mut started = match start_result {
            Ok(started) => started,
            Err(err) => {
                // The tablet is waiting; give it the orderly shutdown.
                let _ = send_control(
                    &mut session,
                    CLOSE_TYPE,
                    &Close {
                        reason: CloseReason::Error as i32,
                    }
                    .encode_to_vec(),
                )
                .await;
                return Err(SessionFailure::Video(err));
            }
        };
        let encoder_config = started.config;
        // Input injection rides the mirror session (one consent
        // story): the source's injector runs on the input worker.
        if let Some(injector) = started.injector.take() {
            eprintln!("session: input injection enabled");
            let stop = Arc::new(AtomicBool::new(false));
            let worker_stop = std::sync::Arc::clone(&stop);
            input_join = Some(
                std::thread::Builder::new()
                    .name("slateway-input".into())
                    .spawn(move || run_input_worker(injector, input_rx, worker_stop))
                    .map_err(|e| SessionFailure::Video(format!("input worker: {e}")))?,
            );
            input_stop = Some(stop);
        } else if cfg.input {
            eprintln!("session: input announced but the platform yielded no injector");
        }
        let configure = slateway_proto::control::Configure {
            mode: StreamMode::Mirror as i32,
            width_px: encoder_config.width,
            height_px: encoder_config.height,
            fps: encoder_config.fps,
            // 0 means the encoder's geometry default; renegotiation
            // arrives with the control channel's mode switching.
            bitrate_kbps: encoder_config.bitrate_kbps,
            codec: VideoCodec::H264High as i32,
        };
        queue_frame(
            &write_tx,
            FrameHeader {
                channel: Channel::Control,
                flags: 0,
                message_type: CONFIGURE_TYPE,
                payload_len: configure.encoded_len() as u32,
                pts_us: 0,
            },
            &configure.encode_to_vec(),
        )
        .await?;
        queue_frame(
            &write_tx,
            FrameHeader {
                channel: Channel::Control,
                flags: 0,
                message_type: STREAM_START_TYPE,
                payload_len: 0,
                pts_us: 0,
            },
            &[],
        )
        .await?;
        eprintln!(
            "session: streaming {}x{} @{} (mirror)",
            encoder_config.width, encoder_config.height, encoder_config.fps
        );
        let req = Arc::new(AtomicBool::new(false));
        keyframe_req = Some(Arc::clone(&req));
        let pump_tx = write_tx.clone();
        video_join = Some(std::thread::spawn(move || {
            crate::video::pump(started, pump_tx, req)
        }));
    }

    let broker = tokio::spawn(run_broker(session, write_rx, read_tx));

    // Session loop: frames from the tablet (relayed by the broker),
    // one Ping per tick. The deadline is absolute, so frequent frames
    // cannot starve the clock sync.
    let mut clock = ClockSync::default();
    let loop_result = run_session_loop(
        &mut read_rx,
        &write_tx,
        cfg,
        &mut clock,
        &mut report,
        keyframe_req.as_ref(),
        &input_tx,
    )
    .await;
    report.clock = clock;
    drop(input_tx);
    if let Err(err) = &loop_result {
        let disconnect = matches!(err, SessionFailure::Session(e) if is_disconnect(e));
        if !disconnect {
            // The tablet did not go away: give it the reason. Best
            // effort, the socket may already be half gone.
            eprintln!("session: sending Close ({err})");
            let close = Close {
                reason: CloseReason::Error as i32,
            };
            let _ = queue_frame(&write_tx, close_header(), &close.encode_to_vec()).await;
        }
    }

    // Teardown: dropping the logic's sender lets the broker finish on
    // a drained queue or at EOF; the pump's pending send unblocks
    // when the broker's receiver drops. Joining keeps the counters
    // exact; a detached pump would lose its tail.
    drop(write_tx);
    let (broker_result, broker_stats) = broker.await.unwrap_or_else(|_join_err| {
        (
            Err(SessionError::Protocol("broker panicked")),
            SessionStats::default(),
        )
    });
    if let Err(join_err) = broker_result
        && !matches!(loop_result, Err(SessionFailure::Session(_)))
    {
        return Err(SessionFailure::Session(join_err));
    }
    if let Some(join) = video_join {
        report.video = join.join().unwrap_or_default();
    }
    // Input teardown: signal the worker (it releases the injector on
    // the stop flag) and join for exact counters. The stop flag is
    // belt-and-braces: dropping input_tx above already ends the
    // worker's recv loop.
    if let Some(stop) = input_stop {
        stop.store(true, std::sync::atomic::Ordering::Release);
    }
    if let Some(join) = input_join {
        report.input = join.join().unwrap_or_default();
    }
    report.stats.frames_in += broker_stats.frames_in;
    report.stats.bytes_in += broker_stats.bytes_in;
    report.stats.frames_out += broker_stats.frames_out;
    report.stats.bytes_out += broker_stats.bytes_out;
    loop_result.map(|()| report)
}

/// The broker owns the encrypted session for the rest of the session:
/// it drains the shared write queue and relays every read to the
/// session logic. Wire counters are exact because every read and
/// write passes here exactly once.
async fn run_broker<S>(
    mut session: SecureSession<S>,
    mut write_rx: tokio::sync::mpsc::Receiver<WriteCmd>,
    read_tx: tokio::sync::mpsc::Sender<(FrameHeader, Vec<u8>)>,
) -> (Result<(), SessionError>, SessionStats)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut scratch = vec![0u8; MAX_PLAINTEXT];
    let mut stats = SessionStats::default();
    let mut result = Ok(());
    loop {
        tokio::select! {
            cmd = write_rx.recv() => match cmd {
                Some((header, payload)) => {
                    match session.write_frame(header, &payload).await {
                        Ok(()) => stats.record_out(payload.len()),
                        Err(err) => {
                            // A write failing because the peer went
                            // away (Close then drop races the queue
                            // drain; Windows reports it on the first
                            // write, Linux on a later one) is a
                            // disconnect, not a protocol failure.
                            if !is_disconnect(&err) {
                                result = Err(err);
                            }
                            break;
                        }
                    }
                }
                // Every sender is gone: the session is over.
                None => break,
            },
            read = session.read_frame(&mut scratch) => match read {
                Ok((header, len)) => {
                    stats.record_in(len);
                    let payload = scratch[..len].to_vec();
                    if read_tx.send((header, payload)).await.is_err() {
                        // The session logic is gone; the session is over.
                        break;
                    }
                }
                Err(err) => {
                    if !is_disconnect(&err) {
                        result = Err(err);
                    }
                    break;
                }
            }
        }
    }
    (result, stats)
}

/// The post-handshake loop: one Ping per tick, echo of experimental
/// frames, clock and stats bookkeeping, and keyframe requests routed
/// to the video pump's flag.
#[allow(clippy::too_many_arguments)]
async fn run_session_loop(
    read_rx: &mut tokio::sync::mpsc::Receiver<(FrameHeader, Vec<u8>)>,
    write_tx: &tokio::sync::mpsc::Sender<WriteCmd>,
    cfg: &SessionConfig,
    clock: &mut ClockSync,
    report: &mut SessionReport,
    keyframe_req: Option<&Arc<AtomicBool>>,
    input_tx: &InputTx,
) -> Result<(), SessionFailure> {
    let mut next_ping = tokio::time::Instant::now() + cfg.tick;
    let mut clock_announced = false;
    let mut discarded_logged = false;
    loop {
        tokio::select! {
            read = read_rx.recv() => match read {
                Some((header, payload)) => {
                    let handled = handle_frame(
                        header,
                        &payload,
                        cfg,
                        clock,
                        report,
                        keyframe_req,
                        write_tx,
                        input_tx,
                    )
                    .await;
                    if handled.unhandled {
                        report.discarded += 1;
                        if !discarded_logged {
                            discarded_logged = true;
                            eprintln!(
                                "session: discarding unhandled frames (first: channel {:?} type {})",
                                header.channel, header.message_type
                            );
                        }
                    }
                    if handled.stop {
                        break;
                    }
                }
                // The broker is gone; the session is over.
                None => break,
            },
            _ = tokio::time::sleep_until(next_ping) => {
                let ping = Ping { t0: monotonic_us() };
                queue_control(write_tx, PING_TYPE, &ping.encode_to_vec()).await?;
                next_ping = tokio::time::Instant::now() + cfg.tick;
            }
        }
        if !clock_announced && let Some(estimate) = clock.estimate() {
            clock_announced = true;
            eprintln!("session: clock sync live (rtt {} us)", estimate.rtt_us);
        }
    }
    Ok(())
}

/// What handling one tablet frame did.
struct Handled {
    stop: bool,
    unhandled: bool,
}

/// Handles one frame from the tablet. Malformed messages are
/// discarded, not fatal: they are authenticated data, so a malformed
/// frame is a bug, not an attack, and the next tick resyncs.
#[allow(clippy::too_many_arguments)]
async fn handle_frame(
    header: FrameHeader,
    payload: &[u8],
    cfg: &SessionConfig,
    clock: &mut ClockSync,
    report: &mut SessionReport,
    keyframe_req: Option<&Arc<AtomicBool>>,
    write_tx: &tokio::sync::mpsc::Sender<WriteCmd>,
    input_tx: &InputTx,
) -> Handled {
    match (header.channel, header.message_type) {
        (Channel::Control, PONG_TYPE) => match Pong::decode(payload) {
            Ok(pong) => {
                if clock
                    .on_pong(pong.t0, pong.t1, pong.t2, monotonic_us())
                    .is_none()
                {
                    eprintln!("session: rejected a physically impossible clock sample");
                }
            }
            Err(_) => eprintln!("session: discarded a malformed Pong"),
        },
        (Channel::Control, STATS_TYPE) => {
            if let Ok(stats) = Stats::decode(payload) {
                report.stats.record_tablet(stats);
            }
        }
        (Channel::Control, CLOSE_TYPE) => {
            eprintln!("session: tablet sent Close");
            return Handled {
                stop: true,
                unhandled: false,
            };
        }
        (Channel::Control, REQUEST_KEYFRAME_TYPE) => {
            if let Some(req) = keyframe_req {
                req.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
        (Channel::Input, INPUT_BATCH_TYPE) => {
            // Tablet input (docs/PROTOCOL.md, "Input event"): parse
            // the batch and hand every event to the injector worker.
            // Frames without an accepted capability (or a failed
            // injector) are discarded with the rest of the unknowns.
            match parse_input_batch(payload) {
                Ok(events) => {
                    for event in events {
                        // A failed send means the worker is gone
                        // (injection never came up or the session is
                        // ending); the event is dropped, not fatal.
                        let _ = input_tx.send(event);
                    }
                }
                Err(err) => eprintln!("session: discarded a malformed input batch ({err})"),
            }
        }
        (Channel::Control, HELLO_TYPE) => {
            eprintln!("session: unexpected duplicate Hello");
            return Handled {
                stop: true,
                unhandled: false,
            };
        }
        (Channel::Experimental { id }, message_type)
            if id == echo::CHANNEL && message_type == echo::MESSAGE_TYPE && cfg.echo =>
        {
            let echo_header = FrameHeader {
                channel: Channel::Experimental { id: echo::CHANNEL },
                flags: 0,
                message_type: echo::MESSAGE_TYPE,
                payload_len: payload.len() as u32,
                pts_us: 0,
            };
            if write_tx
                .send((echo_header, payload.to_vec()))
                .await
                .is_err()
            {
                return Handled {
                    stop: true,
                    unhandled: false,
                };
            }
            report.echoed += 1;
        }
        _ => {
            // Unknown control types and non-echo experimental frames
            // are discarded by design (docs/PROTOCOL.md); they stay
            // visible in the report instead of vanishing.
            return Handled {
                stop: false,
                unhandled: true,
            };
        }
    }
    Handled {
        stop: false,
        unhandled: false,
    }
}

/// Queues one control frame with an empty-ish header.
async fn queue_control(
    write_tx: &tokio::sync::mpsc::Sender<WriteCmd>,
    message_type: u16,
    payload: &[u8],
) -> Result<(), SessionError> {
    let header = FrameHeader {
        channel: Channel::Control,
        flags: 0,
        message_type,
        payload_len: payload.len() as u32,
        pts_us: 0,
    };
    queue_frame(write_tx, header, payload).await
}

/// Queues one frame with its payload.
async fn queue_frame(
    write_tx: &tokio::sync::mpsc::Sender<WriteCmd>,
    header: FrameHeader,
    payload: &[u8],
) -> Result<(), SessionError> {
    write_tx
        .send((header, payload.to_vec()))
        .await
        .map_err(|_| SessionError::Protocol("writer closed"))
}

/// A Close frame header with the Error reason.
fn close_header() -> FrameHeader {
    let close = Close {
        reason: CloseReason::Error as i32,
    };
    FrameHeader {
        channel: Channel::Control,
        flags: 0,
        message_type: CLOSE_TYPE,
        payload_len: close.encoded_len() as u32,
        pts_us: 0,
    }
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
                input: false,
            };
            let confirm = |_code: &str, _device: &str| true;
            serve_once(a, &host_priv, &known, &cfg, &confirm, None).await
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
                input: false,
            };
            let confirm = |_code: &str, _device: &str| true;
            serve_once(a, &host_priv, &known, &cfg, &confirm, None).await
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
            input: false,
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
        let confirm = |_code: &str, _device: &str| true;
        let server = serve_once(a, &host_priv, &known, &cfg, &confirm, None);
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
        let confirm = move |_code: &str, _device: &str| {
            prompt_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            true
        };
        let server = serve_once(a, &host_priv, &known, &cfg, &confirm, None);
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
            input: false,
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
        let confirm = |_code: &str, _device: &str| true;
        let server = serve_once(a, &host_priv, &known, &cfg, &confirm, None);
        let (summary, report) = tokio::join!(client, server);
        assert_eq!(summary.unwrap().frames, 1);
        assert!(report.unwrap().paired);

        // Second run with the same key: no prompt, no new pairing.
        let (a, b) = tokio::io::duplex(4096);
        let client = run_client(&client_cfg, a);
        let confirm = |_code: &str, _device: &str| panic!("must not prompt for a known tablet");
        let server = serve_once(b, &host_priv, &known, &cfg, &confirm, None);
        let (summary, report) = tokio::join!(client, server);
        let report = report.unwrap();
        assert!(!report.paired, "a known tablet must not pair again");
        assert_eq!(report.device_name, "Tab");
        assert_eq!(summary.unwrap().frames, 1);
    }
}
