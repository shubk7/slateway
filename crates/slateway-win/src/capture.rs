// SPDX-License-Identifier: GPL-3.0-or-later

//! Windows monitor capture: Windows.Graphics.Capture with a D3D11
//! staging copy (`docs/PLATFORMS.md`, "Capture").
//!
//! Layout mirrors `slateway-linux`: `WgcCapture::start` sets up the
//! device, monitor item, frame pool and session, then a worker thread
//! parks until the consumer stops it. The FrameArrived event fires on
//! WGC-internal threads; its handler copies the frame surface into a
//! CPU-readable staging texture and publishes the newest frame behind
//! the same mutex/condvar handoff the Linux side uses.

use std::{
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use slateway_platform::capture::{
    CaptureConfig, CaptureError, CaptureFrame, CaptureInfo, CaptureSource, FrameFormat,
    monotonic_us,
};
use windows::{
    Foundation::TypedEventHandler,
    Graphics::{
        Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession},
        DirectX::Direct3D11::IDirect3DDevice,
        DirectX::DirectXPixelFormat,
        SizeInt32,
    },
    Win32::{
        Foundation::{HMODULE, POINT},
        Graphics::{
            Direct3D::{
                D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_10_1,
                D3D_FEATURE_LEVEL_11_0,
            },
            Direct3D11::{
                D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
                D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
                D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
                ID3D11Resource, ID3D11Texture2D,
            },
            Dxgi::{
                Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
                IDXGIAdapter, IDXGIDevice,
            },
            Gdi::{
                GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
            },
        },
        System::{
            Com::{COINIT_MULTITHREADED, CoInitializeEx},
            WinRT::{
                Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
                Graphics::Capture::IGraphicsCaptureItemInterop,
            },
        },
    },
    core::{Error, HRESULT, IInspectable, Interface, Result as WResult, factory},
};

/// E_FAIL, for the "call succeeded but the out parameter is empty"
/// contract violation.
const E_FAIL: HRESULT = HRESULT(0x8000_4005u32 as i32);

/// Setup must complete inside this (no consent dialog, but device
/// creation can stall on a wedged GPU stack).
const START_TIMEOUT: Duration = Duration::from_secs(10);

enum WorkerState {
    /// Worker spawned, first frame not yet delivered.
    Connecting,
    Running(CaptureInfo),
    Error(String),
    Stopped,
}

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

    /// Store the newest frame, replacing any unclaimed older one. The
    /// first delivered frame also completes `start`, carrying the real
    /// geometry (WGC has no portal-style pre-reported size).
    fn publish(&self, frame: CaptureFrame) {
        let mut s = self.lock();
        if matches!(s.worker_state, WorkerState::Connecting) {
            s.worker_state = WorkerState::Running(CaptureInfo {
                width: frame.width,
                height: frame.height,
                fps: 0,
                format: frame.format,
            });
        }
        s.latest = Some(frame);
        self.new_frame.notify_all();
    }

    fn set(&self, worker_state: WorkerState) {
        self.lock().worker_state = worker_state;
        self.new_frame.notify_all();
    }
}

/// The D3D11 objects the frame handler needs; the context and staging
/// texture are only touched under one mutex (the WGC event threads
/// serialize through it).
struct Copier {
    device: ID3D11Device,
    state: Mutex<CopierState>,
}

struct CopierState {
    context: ID3D11DeviceContext,
    /// Staging texture with its logical size, recreated when the frame
    /// size changes (display mode change).
    staging: Option<(ID3D11Texture2D, i32, i32)>,
}

#[derive(Default)]
pub struct WgcCapture {
    shared: Option<Arc<Shared>>,
    worker: Option<JoinHandle<()>>,
}

/// Primary monitor handle and geometry for the frame pool's size.
fn primary_monitor() -> WResult<(HMONITOR, u32, u32)> {
    // SAFETY: user32 monitor APIs take plain values and initialize the
    // caller-provided MONITORINFO; nothing outlives the call.
    let hmonitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        rcMonitor: Default::default(),
        rcWork: Default::default(),
        dwFlags: 0,
    };
    // SAFETY: `info` is a valid MONITORINFO with cbSize set, as the API
    // contract requires.
    if unsafe { GetMonitorInfoW(hmonitor, &mut info) }.as_bool() {
        Ok((
            hmonitor,
            (info.rcMonitor.right - info.rcMonitor.left).max(0) as u32,
            (info.rcMonitor.bottom - info.rcMonitor.top).max(0) as u32,
        ))
    } else {
        Err(Error::from_hresult(E_FAIL))
    }
}

fn create_d3d_device() -> WResult<(ID3D11Device, ID3D11DeviceContext)> {
    let levels: [D3D_FEATURE_LEVEL; 2] = [D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1];
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    // SAFETY: the out pointers point at locals that live through the
    // call; D3D11CreateDevice writes its results into them and the
    // adapter param is None (default adapter, driver type below).
    unsafe {
        D3D11CreateDevice(
            None::<&IDXGIAdapter>,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
    }
    let Some(device) = device else {
        return Err(Error::from_hresult(E_FAIL));
    };
    let Some(context) = context else {
        return Err(Error::from_hresult(E_FAIL));
    };
    Ok((device, context))
}

/// Worker thread body: COM apartment, D3D device, monitor item, frame
/// pool, session and the FrameArrived handler all live and die here.
fn run_worker(shared: Arc<Shared>) {
    // Best effort: WGC's free-threaded creation path does not need a
    // specific apartment; if COM is already initialized (possibly in
    // another mode), continue and let the next call surface a real
    // error instead of failing setup here.
    // SAFETY: standard COM apartment initialization on this thread.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };

    match setup_capture(&shared) {
        Ok((pool, session)) => {
            // Park until stop or failure; the handler does the real
            // work. Pool and session live until the worker exits;
            // closing them here releases the capture on this thread.
            while matches!(
                shared.snapshot(),
                WorkerState::Connecting | WorkerState::Running(_)
            ) {
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = session.Close();
            let _ = pool.Close();
            shared.set(WorkerState::Stopped);
        }
        Err(e) => shared.set(WorkerState::Error(format!("capture setup: {e}"))),
    }
}

fn setup_capture(
    shared: &Arc<Shared>,
) -> WResult<(Direct3D11CaptureFramePool, GraphicsCaptureSession)> {
    let (device, context) = create_d3d_device()?;
    let dxgi_device: IDXGIDevice = device.cast()?;
    // SAFETY: linkage-only wrapper around d3d11.dll; the returned
    // IInspectable owns a reference to the wrapper device.
    let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device)? };
    let d3d_device: IDirect3DDevice = inspectable.cast()?;

    let (hmonitor, width, height) = primary_monitor()?;

    // SAFETY: CreateForMonitor hands the returned GraphicsCaptureItem
    // its own reference; nothing aliases.
    let item: GraphicsCaptureItem = unsafe {
        factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?
            .CreateForMonitor::<GraphicsCaptureItem>(hmonitor)?
    };

    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &d3d_device,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        2,
        SizeInt32 {
            Width: width as i32,
            Height: height as i32,
        },
    )?;

    let copier = Arc::new(Copier {
        device,
        state: Mutex::new(CopierState {
            context,
            staging: None,
        }),
    });
    let handler_shared = Arc::clone(shared);
    let handler_copier = Arc::clone(&copier);
    let handler = TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(
        move |pool, _args| -> WResult<()> {
            handle_frame(pool.unwrap(), &handler_shared, &handler_copier);
            Ok(())
        },
    );
    pool.FrameArrived(&handler)?;
    let session = pool.CreateCaptureSession(&item)?;
    session.StartCapture()?;
    Ok((pool, session))
}

/// Copy one arrived frame into a staging texture and publish it. Errors
/// become a shared failure state; event handlers cannot propagate them
/// usefully.
fn handle_frame(pool: &Direct3D11CaptureFramePool, shared: &Arc<Shared>, copier: &Arc<Copier>) {
    match copy_frame(pool, copier) {
        Ok(Some(frame)) => shared.publish(frame),
        Ok(None) => {}
        Err(e) => shared.set(WorkerState::Error(format!("frame copy: {e}"))),
    }
}

fn copy_frame(
    pool: &Direct3D11CaptureFramePool,
    copier: &Arc<Copier>,
) -> WResult<Option<CaptureFrame>> {
    let frame = pool.TryGetNextFrame()?;
    let content = frame.ContentSize()?;
    if content.Width <= 0 || content.Height <= 0 {
        return Ok(None);
    }
    let width = content.Width as u32;
    let height = content.Height as u32;
    let surface = frame.Surface()?;
    let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
    // SAFETY: GetInterface returns a reference-owned ID3D11Texture2D
    // for the frame surface; no raw pointer outlives this statement.
    let texture: ID3D11Texture2D = unsafe { access.GetInterface::<ID3D11Texture2D>()? };

    let mut state = copier.state.lock().expect("copier state poisoned");
    // Recreate the staging texture when the frame size changes.
    let staging = match state.staging.take() {
        Some((tex, w, h)) if w == content.Width && h == content.Height => tex,
        _ => {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut tex: Option<ID3D11Texture2D> = None;
            // SAFETY: desc points to a valid stack value and tex is an
            // out parameter filled by the device on success.
            unsafe {
                copier.device.CreateTexture2D(&desc, None, Some(&mut tex))?;
            }
            let Some(tex) = tex else {
                return Err(Error::from_hresult(E_FAIL));
            };
            state.staging = Some((tex.clone(), content.Width, content.Height));
            tex
        }
    };
    let staging_resource: ID3D11Resource = staging.cast()?;
    let frame_resource: ID3D11Resource = texture.cast()?;

    // SAFETY: CopyResource copies between two textures of identical
    // format and size, both owned by this device, under its context.
    unsafe {
        state
            .context
            .CopyResource(&staging_resource, &frame_resource)
    };

    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    // SAFETY: Map on a staging texture with CPU read access; the mapped
    // pointer stays valid until Unmap, and every access below happens
    // under the copier lock on the same context.
    unsafe {
        state
            .context
            .Map(&staging_resource, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
    }
    let row_bytes = width as usize * 4;
    let mut data = Vec::with_capacity(row_bytes * height as usize);
    // SAFETY: mapped.pData is RowPitch-strided; exactly the visible
    // rows of the staging texture are read before Unmap on the same
    // locked context.
    unsafe {
        let src = mapped.pData as *const u8;
        for y in 0..height as usize {
            let row = std::slice::from_raw_parts(src.add(mapped.RowPitch as usize * y), row_bytes);
            data.extend_from_slice(row);
        }
        state.context.Unmap(&staging_resource, 0);
    }
    Ok(Some(CaptureFrame {
        width,
        height,
        stride: row_bytes,
        format: FrameFormat::Bgrx,
        data,
        pts_us: monotonic_us(),
    }))
}

impl CaptureSource for WgcCapture {
    fn start(&mut self, config: &CaptureConfig) -> Result<CaptureInfo, CaptureError> {
        if self.shared.is_some() {
            return Err(CaptureError::Unsupported("already started".into()));
        }
        let shared = Arc::new(Shared::new());
        let worker_shared = Arc::clone(&shared);
        let fps_hint = config.max_fps.max(1);
        let worker = std::thread::Builder::new()
            .name("slateway-capture".into())
            .spawn(move || run_worker(worker_shared))
            .map_err(|e| CaptureError::Setup(format!("spawn worker: {e}")))?;

        // Wait for the first frame (which carries the real geometry) or
        // a failure.
        let deadline = Instant::now() + START_TIMEOUT;
        let mut guard = shared.lock();
        loop {
            match guard.worker_state {
                WorkerState::Running(info) => {
                    drop(guard);
                    self.shared = Some(shared);
                    self.worker = Some(worker);
                    return Ok(CaptureInfo {
                        fps: fps_hint,
                        ..info
                    });
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
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                drop(guard);
                shared.set(WorkerState::Stopped);
                let _ = worker.join();
                return Err(CaptureError::Setup(
                    "timed out waiting for the first frame".into(),
                ));
            }
            let next = shared
                .new_frame
                .wait_timeout(guard, remaining)
                .expect("capture state poisoned")
                .0;
            guard = next;
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
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            let (next, _timed_out) = shared
                .new_frame
                .wait_timeout(guard, remaining)
                .expect("capture state poisoned");
            guard = next;
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

impl Drop for WgcCapture {
    fn drop(&mut self) {
        self.stop();
    }
}
