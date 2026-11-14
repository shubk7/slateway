// SPDX-License-Identifier: GPL-3.0-or-later

//! The M1 mirror rehearsal (docs/ROADMAP.md, "Video channel"):
//! the daemon's session with a synthetic capture and a real software
//! encoder (libx264) against `mock-tablet`'s video mode, in-process.
//! Asserts the wire order (Configure, StreamStart, keyframe-first
//! AUs), pts monotonicity, and the RequestKeyframe → keyframe path.

use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use slateway_core::KEY_LEN;
use slateway_core::crypto::generate_keypair;
use slateway_core::keys::KnownTablets;
use slateway_platform::capture::{
    CaptureConfig, CaptureError, CaptureFrame, CaptureInfo, CaptureSource, FrameFormat,
    monotonic_us,
};
use slateway_platform::encoder::EncoderConfig;
use slateway_platform::input::{InputError, PointerInjector};
use slateway_proto::input::{EventKind, InputEvent, Tool};
use slatewayd::session::{SessionConfig, SharedTablets, serve_once};
use slatewayd::video::{EncoderOpener, StartedVideo, VideoSource};
use tempfile::TempDir;
use tokio::sync::Mutex;

/// A synthetic monitor: 64x48 BGRx frames with a moving pattern, one
/// frame per call at ~30 fps. Enough for libx264 to produce real,
/// decodable access units without stressing CI.
struct SyntheticCapture {
    counter: u64,
}

impl CaptureSource for SyntheticCapture {
    fn start(&mut self, _config: &CaptureConfig) -> Result<CaptureInfo, CaptureError> {
        Ok(CaptureInfo {
            width: 64,
            height: 48,
            fps: 30,
            format: FrameFormat::Bgrx,
        })
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<Option<CaptureFrame>, CaptureError> {
        // 30 fps pacing; the timeout guard keeps the pump's poll loop
        // responsive to shutdown.
        std::thread::sleep(timeout.min(Duration::from_millis(16)));
        let width = 64usize;
        let height = 48usize;
        let stride = width * 4;
        let mut data = vec![0u8; height * stride];
        for y in 0..height {
            for x in 0..width {
                let phase = (x as u64 + y as u64 + self.counter) % 64;
                let off = y * stride + x * 4;
                data[off] = (phase * 4) as u8;
                data[off + 1] = (self.counter % 256) as u8;
                data[off + 2] = 128;
            }
        }
        self.counter += 1;
        Ok(Some(CaptureFrame {
            width: width as u32,
            height: height as u32,
            stride,
            format: FrameFormat::Bgrx,
            data,
            pts_us: monotonic_us(),
        }))
    }

    fn stop(&mut self) {}
}

/// A recording injector: the test's stand-in for the platform
/// injectors (AGENTS.md rule 2 keeps real OS injection out of CI).
/// Collects every injected event so assertions run on the
/// post-policy stream. The log is shared with the test through an
/// `Arc` because the session moves the injector into its worker.
#[derive(Clone, Default)]
struct RecorderInjector {
    events: Arc<StdMutex<Vec<InputEvent>>>,
}

impl RecorderInjector {
    fn injected(&self) -> Vec<InputEvent> {
        self.events.lock().expect("recorder poisoned").clone()
    }
}

impl PointerInjector for RecorderInjector {
    fn inject(&mut self, event: &InputEvent) -> Result<(), InputError> {
        self.events.lock().expect("recorder poisoned").push(*event);
        Ok(())
    }

    fn release(&mut self) {}
}

/// The test video source: synthetic capture, libx264 directly (no
/// platform candidate lists; AGENTS.md rule 2 keeps those in the
/// platform crates), and the recording injector standing in for the
/// platform pointer path.
struct TestVideoSource {
    recorder: RecorderInjector,
}

impl VideoSource for TestVideoSource {
    fn start(&mut self) -> Result<StartedVideo, String> {
        let mut source = Box::new(SyntheticCapture { counter: 0 }) as Box<dyn CaptureSource>;
        let info = source
            .start(&CaptureConfig::default())
            .map_err(|e| e.to_string())?;
        let open: Box<EncoderOpener> =
            Box::new(|config: &EncoderConfig| slateway_ffmpeg::start(config, &["libx264"]));
        Ok(StartedVideo {
            source,
            config: EncoderConfig {
                width: info.width,
                height: info.height,
                fps: info.fps,
                bitrate_kbps: 0,
            },
            open_encoder: open,
            injector: Some(Box::new(self.recorder.clone())),
        })
    }
}

fn known_registry() -> (TempDir, SharedTablets) {
    let dir = TempDir::new().unwrap();
    let known: SharedTablets = Arc::new(Mutex::new(
        KnownTablets::load(&dir.path().join("known-tablets")).unwrap(),
    ));
    (dir, known)
}

fn host_key() -> [u8; KEY_LEN] {
    let (priv_, _) = generate_keypair().unwrap();
    priv_.as_slice().try_into().unwrap()
}

/// Full mirror rehearsal: auto-stream after handshake, keyframe first,
/// monotonic pts, keyframe re-open on request.
#[tokio::test(flavor = "multi_thread")]
async fn mirror_session_streams_decodable_keyframes() {
    let (_dir, known) = known_registry();
    let host_priv = host_key();
    let recorder = RecorderInjector::default();

    // The scripted touch stroke: down, a move run, up (finger,
    // contact 0, normalized coordinates across the streamed display).
    let stroke = vec![
        InputEvent {
            kind: EventKind::Down,
            tool: Tool::Finger,
            buttons: 0,
            pointer_id: 0,
            x: 1000,
            y: 2000,
            pressure: 0,
            tilt_x: 0,
            tilt_y: 0,
            dt_us: 0,
        },
        InputEvent {
            kind: EventKind::Move,
            tool: Tool::Finger,
            buttons: 0,
            pointer_id: 0,
            x: 2000,
            y: 3000,
            pressure: 0,
            tilt_x: 0,
            tilt_y: 0,
            dt_us: 8000,
        },
        InputEvent {
            kind: EventKind::Down,
            tool: Tool::Finger,
            buttons: 0,
            pointer_id: 1,
            x: 500,
            y: 500,
            pressure: 0,
            tilt_x: 0,
            tilt_y: 0,
            dt_us: 8000,
        },
        InputEvent {
            kind: EventKind::Up,
            tool: Tool::Finger,
            buttons: 0,
            pointer_id: 0,
            x: 2000,
            y: 3000,
            pressure: 0,
            tilt_x: 0,
            tilt_y: 0,
            dt_us: 16000,
        },
        // The second contact lifts after the primary (two-finger
        // touch): the M1 policy ignored it entirely, so its up is
        // ignored too.
        InputEvent {
            kind: EventKind::Up,
            tool: Tool::Finger,
            buttons: 0,
            pointer_id: 1,
            x: 500,
            y: 500,
            pressure: 0,
            tilt_x: 0,
            tilt_y: 0,
            dt_us: 24000,
        },
    ];

    let (a, b) = tokio::io::duplex(64 * 1024);
    let recorder_for_server = recorder.clone();
    let server = tokio::spawn(async move {
        let cfg = SessionConfig {
            echo: false,
            tick: Duration::from_millis(20),
            input: true,
        };
        let confirm = |_code: &str, _device: &str| true;
        serve_once(
            a,
            &host_priv,
            &known,
            &cfg,
            &confirm,
            Some(Box::new(TestVideoSource {
                recorder: recorder_for_server,
            })),
        )
        .await
    });

    let client_cfg = mock_tablet::VideoClientConfig {
        frames: 12,
        request_keyframe_after: Some(5),
        stats_tick: Duration::from_millis(50),
        input_batch: stroke,
        ..Default::default()
    };
    let client = mock_tablet::run_video_client(&client_cfg, b);
    let (client_res, server_res) = tokio::join!(client, server);
    let summary = client_res.unwrap_or_else(|err| panic!("client failed: {err}"));
    let report = server_res
        .unwrap_or_else(|err| panic!("server task failed: {err:?}"))
        .unwrap_or_else(|err| panic!("server failed: {err}"));

    // Configure carried the capture's geometry.
    assert_eq!(summary.width, 64);
    assert_eq!(summary.height, 48);
    assert_eq!(summary.fps, 30);
    // The stream opens with a keyframe (fresh SPS/PPS + IDR).
    assert!(
        summary.first_frame_was_keyframe,
        "the first access unit must be a keyframe"
    );
    assert_eq!(summary.frames, 12, "every scripted AU arrived");
    assert!(summary.pts_monotonic, "pts_us never goes backwards");
    assert!(summary.max_payload > 0, "AUs carry real H.264 payloads");
    // RequestKeyframe re-opened the encoder: a second keyframe followed.
    assert!(
        summary.keyframe_after_request,
        "a keyframe must follow the RequestKeyframe"
    );
    assert!(summary.keyframes >= 2);
    // The pump did real work; nothing was skipped in this quiet test.
    assert!(
        report
            .video
            .frames_encoded
            .load(std::sync::atomic::Ordering::Relaxed)
            > 0
    );
    assert_eq!(
        report
            .video
            .encode_errors
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );

    // Touch-as-pointer: the scripted stroke reached the injector as
    // down → move → up for the primary contact; the second contact
    // (pointer_id 1) was ignored by the M1 policy.
    assert_eq!(summary.inputs_sent, 5, "the scripted batch was sent");
    let injected = recorder.injected();
    assert_eq!(
        injected.len(),
        3,
        "down, move and up inject; the second contact is ignored: {injected:?}"
    );
    assert_eq!(injected[0].kind, EventKind::Down);
    assert_eq!(injected[0].x, 1000);
    assert_eq!(injected[1].kind, EventKind::Move);
    assert_eq!(injected[1].pointer_id, 0);
    assert_eq!(injected[2].kind, EventKind::Up);
    // Worker counters agree with what the injector saw.
    assert_eq!(
        report
            .input
            .events_in
            .load(std::sync::atomic::Ordering::Relaxed),
        5
    );
    assert_eq!(
        report
            .input
            .events_injected
            .load(std::sync::atomic::Ordering::Relaxed),
        3
    );
    assert_eq!(
        report
            .input
            .events_coalesced
            .load(std::sync::atomic::Ordering::Relaxed),
        2
    );
    assert_eq!(
        report
            .input
            .injection_errors
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}
