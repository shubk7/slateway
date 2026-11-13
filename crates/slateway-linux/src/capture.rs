// SPDX-License-Identifier: GPL-3.0-or-later

//! Linux monitor capture: xdg-desktop-portal ScreenCast consumed
//! through PipeWire (`docs/PLATFORMS.md`, "Capture").
//!
//! Layout: `PortalCapture::start` performs the portal dance (this is
//! where the compositor's consent dialog appears), then hands the
//! PipeWire remote fd and node id to a dedicated worker thread that
//! owns all PipeWire objects (`MainLoopBox` is `!Send`). Frames are
//! copied out of the stream buffers into
//! `slateway_platform::capture::CaptureFrame`s behind a shared mutex;
//! `next_frame` returns the newest unclaimed frame, older ones are
//! replaced in place (newest-frame semantics).

use std::{
    os::fd::OwnedFd,
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use ashpd::desktop::{
    PersistMode,
    screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType},
};
use pipewire as pw;
use pw::{
    properties::properties,
    spa::{
        buffer::ChunkFlags,
        param::{
            ParamType,
            format::{FormatProperties, MediaSubtype, MediaType},
            video::{VideoFormat, VideoInfoRaw},
        },
        pod::{self, Value, serialize::PodSerializer},
        utils::{Fraction, Rectangle, SpaTypes},
    },
};

use slateway_platform::capture::{
    CaptureConfig, CaptureError, CaptureFrame, CaptureInfo, CaptureSource, FrameFormat,
};

/// A consent prompt, negotiation or teardown must finish inside this.
const START_TIMEOUT: Duration = Duration::from_secs(20);

enum WorkerState {
    /// Worker spawned, format not yet negotiated.
    Connecting,
    Running(CaptureInfo),
    Error(String),
    Stopped,
}

/// Cheap `WorkerState` copy for the snapshot helper.
impl WorkerState {
    fn snapshot(&self) -> WorkerState {
        match self {
            WorkerState::Connecting => WorkerState::Connecting,
            WorkerState::Running(info) => WorkerState::Running(*info),
            WorkerState::Error(e) => WorkerState::Error(e.clone()),
            WorkerState::Stopped => WorkerState::Stopped,
        }
    }
}

struct State {
    worker_state: WorkerState,
    latest: Option<CaptureFrame>,
}

struct Shared {
    state: Mutex<State>,
    new_frame: Condvar,
}

impl Shared {
    fn new() -> Self {
        Self {
            state: Mutex::new(State {
                worker_state: WorkerState::Connecting,
                latest: None,
            }),
            new_frame: Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("capture state poisoned")
    }

    fn snapshot(&self) -> WorkerState {
        self.lock().worker_state.snapshot()
    }

    /// Store the newest frame, replacing any unclaimed older one.
    fn publish(&self, frame: CaptureFrame) {
        self.lock().latest = Some(frame);
        self.new_frame.notify_all();
    }

    fn set(&self, worker_state: WorkerState) {
        self.lock().worker_state = worker_state;
        self.new_frame.notify_all();
    }

    fn wait_until<'a>(
        &'a self,
        guard: std::sync::MutexGuard<'a, State>,
        deadline: Instant,
    ) -> (std::sync::MutexGuard<'a, State>, bool) {
        // std Condvar has no wait_until; wait once for the remaining
        // time and let the caller re-check its condition after every
        // notification.
        let now = Instant::now();
        if now >= deadline {
            return (guard, true);
        }
        let (guard, res) = self
            .new_frame
            .wait_timeout(guard, deadline - now)
            .expect("capture state poisoned");
        (guard, res.timed_out())
    }
}

/// Callback state for the PipeWire stream listener. Lives only on the
/// worker thread (owned by the main loop dispatch).
struct ListenerData {
    shared: Arc<Shared>,
    format: VideoInfoRaw,
    negotiated: bool,
    max_fps: u32,
}

#[derive(Default)]
pub struct PortalCapture {
    shared: Option<Arc<Shared>>,
    worker: Option<JoinHandle<()>>,
}

/// The portal session result: which PipeWire node to consume and the
/// remote that carries it.
struct PortalStream {
    node_id: u32,
    /// Initial monitor size reported by the portal; used as the
    /// preferred size in the format negotiation.
    width: u32,
    height: u32,
    fd: OwnedFd,
}

async fn request_portal_stream(config: &CaptureConfig) -> Result<PortalStream, CaptureError> {
    let proxy = Screencast::new()
        .await
        .map_err(|e| CaptureError::Setup(format!("screencast portal: {e}")))?;
    let session = proxy
        .create_session(Default::default())
        .await
        .map_err(|e| CaptureError::Setup(format!("create session: {e}")))?;
    proxy
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_cursor_mode(CursorMode::EMBEDDED)
                .set_sources(SourceType::MONITOR)
                .set_multiple(false)
                .set_restore_token(config.restore_token.as_deref())
                .set_persist_mode(PersistMode::DoNot),
        )
        .await
        .map_err(|e| CaptureError::Setup(format!("select sources: {e}")))?;
    let response = proxy
        .start(&session, None, Default::default())
        .await
        .map_err(|e| CaptureError::Setup(format!("start (consent refused?): {e}")))?;
    let streams = response
        .response()
        .map_err(|e| CaptureError::Setup(format!("start response: {e}")))?;
    let stream = streams
        .streams()
        .first()
        .ok_or_else(|| CaptureError::Setup("no monitor selected".into()))?
        .to_owned();
    let fd = proxy
        .open_pipe_wire_remote(&session, Default::default())
        .await
        .map_err(|e| CaptureError::Setup(format!("open pipewire remote: {e}")))?;
    // ashpd reports the monitor size as an optional (width, height) tuple.
    let (width, height) = stream.size().unwrap_or((0, 0));
    Ok(PortalStream {
        node_id: stream.pipe_wire_node_id(),
        width: width.max(0) as u32,
        height: height.max(0) as u32,
        fd,
    })
}

/// Negotiate an EnumFormat that leaves resolution and rate to the
/// compositor but pins the pixel format to what the encoder accepts.
fn enum_format_pod(preferred: Rectangle, max_fps: u32) -> Result<Vec<u8>, String> {
    let max_rate = Fraction {
        num: max_fps.max(1),
        denom: 1,
    };
    let obj = pod::object!(
        SpaTypes::ObjectParamFormat,
        ParamType::EnumFormat,
        pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        pod::property!(
            FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::BGRx,
            VideoFormat::BGRx,
            VideoFormat::RGBx,
            VideoFormat::RGBA,
        ),
        pod::property!(
            FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            preferred,
            Rectangle {
                width: 1,
                height: 1
            },
            Rectangle {
                width: 8192,
                height: 8192
            }
        ),
        pod::property!(
            FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            max_rate,
            Fraction { num: 0, denom: 1 },
            Fraction {
                num: 1000,
                denom: 1
            }
        ),
    );
    let serialized =
        PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj))
            .map_err(|e| format!("enum format serialization: {e}"))?;
    Ok(serialized.0.into_inner())
}

fn map_video_format(format: VideoFormat) -> Option<FrameFormat> {
    match format {
        VideoFormat::BGRx => Some(FrameFormat::Bgrx),
        VideoFormat::RGBA => Some(FrameFormat::Rgba),
        VideoFormat::RGBx => Some(FrameFormat::Rgbx),
        _ => None,
    }
}

/// The worker thread body: own all PipeWire objects, dispatch until the
/// consumer stops us or the stream fails, then drop them on this thread.
fn run_worker(portal: PortalStream, shared: Arc<Shared>, max_fps: u32) {
    let main_loop = match pw::main_loop::MainLoopBox::new(None) {
        Ok(ml) => ml,
        Err(e) => return shared.set(WorkerState::Error(format!("pipewire main loop: {e}"))),
    };
    let context = match pw::context::ContextBox::new(main_loop.loop_(), None) {
        Ok(c) => c,
        Err(e) => return shared.set(WorkerState::Error(format!("pipewire context: {e}"))),
    };
    // connect_fd consumes the portal's PipeWire remote; everything
    // downstream talks to the compositor's screencast node through it.
    let core = match context.connect_fd(portal.fd, None) {
        Ok(c) => c,
        Err(e) => return shared.set(WorkerState::Error(format!("pipewire connect: {e}"))),
    };
    let stream = match pw::stream::StreamBox::new(
        &core,
        "slateway-capture",
        properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    ) {
        Ok(s) => s,
        Err(e) => return shared.set(WorkerState::Error(format!("pipewire stream: {e}"))),
    };

    let data = ListenerData {
        shared: Arc::clone(&shared),
        format: VideoInfoRaw::default(),
        negotiated: false,
        max_fps,
    };

    let values = match enum_format_pod(
        Rectangle {
            width: portal.width.max(1),
            height: portal.height.max(1),
        },
        max_fps,
    ) {
        Ok(bytes) => bytes,
        Err(e) => return shared.set(WorkerState::Error(e)),
    };
    // The pod borrows `values`, which must outlive it.
    let Some(negotiation_pod) = pw::spa::pod::Pod::from_bytes(&values) else {
        return shared.set(WorkerState::Error("negotiation pod was malformed".into()));
    };

    let _listener = match stream
        .add_local_listener_with_user_data(data)
        .param_changed(|_, data, id, param| {
            let Some(param) = param else {
                return;
            };
            if id != pw::spa::param::ParamType::Format.as_raw() {
                return;
            }
            let parsed = pw::spa::param::format_utils::parse_format(param);
            let (media_type, media_subtype) = match parsed {
                Ok(v) => v,
                Err(_) => return,
            };
            if media_type != MediaType::Video || media_subtype != MediaSubtype::Raw {
                return;
            }
            if data.format.parse(param).is_err() {
                return;
            }
            let Some(pixel_format) = map_video_format(data.format.format()) else {
                data.shared.set(WorkerState::Error(format!(
                    "compositor negotiated unencodable format {:?}",
                    data.format.format().as_raw()
                )));
                return;
            };
            if data.negotiated {
                return;
            }
            data.negotiated = true;
            let size = data.format.size();
            let rate = data.format.framerate();
            let info = CaptureInfo {
                width: size.width,
                height: size.height,
                fps: if rate.num == 0 {
                    data.max_fps
                } else {
                    rate.num
                },
                format: pixel_format,
            };
            data.shared.set(WorkerState::Running(info));
        })
        .process(|stream, data| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let plane = &mut datas[0];
            let (chunk_size, chunk_stride) = {
                let chunk = plane.chunk();
                if chunk.size() == 0 || chunk.flags().contains(ChunkFlags::CORRUPTED) {
                    return;
                }
                (
                    chunk.size() as usize,
                    chunk.stride().unsigned_abs() as usize,
                )
            };
            // `data()` maps the buffer contents; None is normal for
            // DMA-BUF-only buffers, which the zero-copy encoder path
            // will consume directly. We skip those frames for now.
            let Some(pixels) = plane.data() else {
                return;
            };
            let relevant = pixels.len().min(chunk_size);
            let info = match data.shared.snapshot() {
                WorkerState::Running(info) => info,
                _ => return,
            };
            let frame = CaptureFrame {
                width: info.width,
                height: info.height,
                stride: chunk_stride,
                format: info.format,
                data: pixels[..relevant].to_vec(),
                pts_us: frame_pts_us(),
            };
            if frame.is_consistent() {
                data.shared.publish(frame);
            }
        })
        .register()
    {
        Ok(l) => l,
        Err(e) => return shared.set(WorkerState::Error(format!("pipewire listener: {e}"))),
    };

    if matches!(shared.snapshot(), WorkerState::Error(_)) {
        return;
    }
    if let Err(e) = stream.connect(
        pw::spa::utils::Direction::Input,
        Some(portal.node_id),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut [negotiation_pod],
    ) {
        return shared.set(WorkerState::Error(format!("pipewire connect: {e}")));
    }

    // Dispatch until stop or failure; `iterate` gives us the idle-time
    // exit condition that `run()` cannot (a static screen produces no
    // callbacks to quit from).
    while matches!(
        shared.snapshot(),
        WorkerState::Connecting | WorkerState::Running(_)
    ) {
        main_loop
            .loop_()
            .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(200)));
    }
    shared.set(WorkerState::Stopped);
    // Drop order: stream, core, context, main loop.
}

fn frame_pts_us() -> u64 {
    // Process-local monotonic origin; the stats layer does the
    // cross-machine math.
    static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    ORIGIN.get_or_init(Instant::now).elapsed().as_micros() as u64
}

impl CaptureSource for PortalCapture {
    fn start(&mut self, config: &CaptureConfig) -> Result<CaptureInfo, CaptureError> {
        if self.shared.is_some() {
            return Err(CaptureError::Unsupported("already started".into()));
        }
        // `tokio::runtime::Builder::block_on` panics if called from
        // inside a runtime; the trait contract keeps this off the async
        // threads (ARCHITECTURE.md: dedicated OS thread for capture).
        let portal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| CaptureError::Unsupported(format!("no tokio runtime: {e}")))?
            .block_on(request_portal_stream(config))?;

        let shared = Arc::new(Shared::new());
        let worker_shared = Arc::clone(&shared);
        let max_fps = config.max_fps.max(1);
        let worker = std::thread::Builder::new()
            .name("slateway-capture".into())
            .spawn(move || run_worker(portal, worker_shared, max_fps))
            .map_err(|e| CaptureError::Setup(format!("spawn worker: {e}")))?;

        // Wait for the first format negotiation or a failure.
        let deadline = Instant::now() + START_TIMEOUT;
        let mut guard = shared.lock();
        loop {
            match guard.worker_state {
                WorkerState::Running(info) => {
                    drop(guard);
                    self.shared = Some(shared);
                    self.worker = Some(worker);
                    return Ok(info);
                }
                WorkerState::Error(ref e) => {
                    let msg = e.clone();
                    drop(guard);
                    shared.set(WorkerState::Stopped);
                    let _ = worker.join();
                    return Err(CaptureError::Setup(msg));
                }
                WorkerState::Connecting | WorkerState::Stopped => {}
            }
            let (next, timed_out) = shared.wait_until(guard, deadline);
            guard = next;
            if timed_out {
                drop(guard);
                shared.set(WorkerState::Stopped);
                let _ = worker.join();
                return Err(CaptureError::Setup(
                    "timed out waiting for format negotiation".into(),
                ));
            }
        }
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<Option<CaptureFrame>, CaptureError> {
        let Some(shared) = self.shared.as_ref() else {
            return Err(CaptureError::Failed("capture not started".into()));
        };
        let deadline = Instant::now() + timeout;
        let mut guard = shared.lock();
        loop {
            match guard.worker_state {
                WorkerState::Error(ref e) => return Err(CaptureError::Failed(e.clone())),
                WorkerState::Stopped => return Ok(None),
                WorkerState::Connecting | WorkerState::Running(_) => {}
            }
            if let Some(frame) = guard.latest.take() {
                return Ok(Some(frame));
            }
            let (next, timed_out) = shared.wait_until(guard, deadline);
            guard = next;
            if timed_out {
                return Ok(None);
            }
        }
    }

    fn stop(&mut self) {
        if let Some(shared) = self.shared.take() {
            shared.set(WorkerState::Stopped);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for PortalCapture {
    fn drop(&mut self) {
        self.stop();
    }
}
