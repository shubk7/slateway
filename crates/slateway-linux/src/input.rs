// SPDX-License-Identifier: GPL-3.0-or-later

//! Linux pointer injection through the RemoteDesktop portal
//! (docs/PLATFORMS.md, "Pointer and touch"): absolute pointer motion
//! and button presses on the portal session shared with the capture,
//! so the compositor maps coordinates to the streamed monitor
//! automatically.
//!
//! The portal session is created by `PortalCapture::start` (one
//! consent dialog covers capture and input); this type only issues
//! the notify calls, blocking on the portal dance runtime the capture
//! hands over (zbus binds the session's connection to it). The calls
//! come from the daemon's dedicated input worker thread. Each notify
//! is a short D-Bus round trip (sub-millisecond on the session bus);
//! the input worker has nothing else to do, so it waits for them.
//! Every call's duration is logged (docs/TESTING.md, "Injection
//! timing"): a notify that hangs or stalls is the prime suspect when
//! the mirror freezes around input events.

use ashpd::desktop::Session;
use ashpd::desktop::remote_desktop::{KeyState, RemoteDesktop};
use slateway_platform::input::{InputError, PointerInjector, PointerTarget};
use slateway_proto::input::{EventKind, InputEvent, Tool};
use std::time::{Duration, Instant};

/// evdev button codes the portal expects (linux/input-event-codes.h).
const BTN_LEFT: i32 = 0x110;

/// Window for the aggregated Move-injection heartbeat.
const MOVE_WINDOW: Duration = Duration::from_secs(1);

/// What `PortalCapture::start` hands over for input: clones of the
/// portal proxy and session (both `Clone` + `Send`; the zbus
/// connection underneath is an `Arc`), plus the stream and target the
/// coordinates map to. The runtime the portal session was created on
/// rides along: zbus binds the connection to the runtime it was built
/// on, so it must outlive the proxies.
pub struct PortalHandle {
    pub proxy: RemoteDesktop,
    pub session: Session<RemoteDesktop>,
    pub stream: u32,
    pub target: PointerTarget,
    /// The portal dance runtime; `None` until `PortalCapture::start`
    /// fills it in after the dance completes.
    pub runtime: Option<tokio::runtime::Runtime>,
}

/// Maps an ashpd failure onto the injector error.
fn map_error(err: ashpd::Error) -> InputError {
    InputError::Failed(err.to_string())
}

/// Portal-backed pointer injector. Every method may block on a D-Bus
/// round trip; the trait contract keeps them on the input worker
/// thread.
pub struct PortalPointer {
    /// The portal handle; owns the runtime the session lives on.
    handle: Option<PortalHandle>,
    /// Per-window move counters for the injection-timing heartbeat.
    /// The input worker is the only caller, so plain fields are safe.
    moves_window: Instant,
    moves_count: u32,
    moves_max_us: u32,
}

impl PortalPointer {
    /// Wraps a portal handle obtained from the capture's start. The
    /// dance runtime must be present: the portal session's zbus
    /// connection only runs on it.
    pub fn new(handle: PortalHandle) -> Result<Self, InputError> {
        if handle.runtime.is_none() {
            return Err(InputError::Setup(
                "portal handle carries no runtime (the capture dance did not attach one)".into(),
            ));
        }
        Ok(Self {
            handle: Some(handle),
            moves_window: Instant::now(),
            moves_count: 0,
            moves_max_us: 0,
        })
    }
}

impl PointerInjector for PortalPointer {
    fn inject(&mut self, event: &InputEvent) -> Result<(), InputError> {
        // M1: only the finger tool drives the pointer; pen arrives in
        // M3 with its own device (uinput, PLATFORMS.md).
        if event.tool != Tool::Finger {
            return Ok(());
        }
        // Take the handle out for the duration: the timing log and
        // the move window live in self, and taking it keeps the
        // borrow checker happy (self is untouched until the end).
        let handle = self
            .handle
            .take()
            .ok_or_else(|| InputError::Failed("injector already released".into()))?;
        // Single owner, single block_on: the input worker is the only
        // caller (tokio panics on concurrent block_on of one runtime).
        let runtime = handle
            .runtime
            .as_ref()
            .ok_or_else(|| InputError::Failed("injector already released".into()))?;
        let (proxy, session, stream) = (&handle.proxy, &handle.session, handle.stream);
        let mut moves_count = self.moves_count;
        let mut moves_max_us = self.moves_max_us;
        let mut moves_window = self.moves_window;
        let result = match event.kind {
            EventKind::Down => {
                // Motion first, then the press: the click lands where
                // the finger touched. The portal expects coordinates
                // in the stream's logical coordinate space
                // (NotifyPointerMotionAbsolute, "see the logical_size
                // stream property in ScreenCast" — the xdg RemoteDesktop
                // spec), not normalized 0..1.
                let x = event.x as f64 / u16::MAX as f64 * handle.target.width as f64;
                let y = event.y as f64 / u16::MAX as f64 * handle.target.height as f64;
                let motion_at = Instant::now();
                let motion = runtime.block_on(async {
                    proxy
                        .notify_pointer_motion_absolute(session, stream, x, y, Default::default())
                        .await
                });
                let motion_us = motion_at.elapsed();
                if let Err(err) = motion {
                    eprintln!(
                        "input: Down motion FAILED after {} us ({err})",
                        motion_us.as_micros()
                    );
                    self.moves_count = moves_count;
                    self.moves_max_us = moves_max_us;
                    self.moves_window = moves_window;
                    self.handle = Some(handle);
                    return Err(map_error(err));
                }
                let press_at = Instant::now();
                let press = runtime.block_on(async {
                    proxy
                        .notify_pointer_button(
                            session,
                            BTN_LEFT,
                            KeyState::Pressed,
                            Default::default(),
                        )
                        .await
                });
                let press_us = press_at.elapsed();
                eprintln!(
                    "input: Down motion={}us press={}us{}",
                    motion_us.as_micros(),
                    press_us.as_micros(),
                    if press.is_err() { " FAILED" } else { "" },
                );
                press.map_err(map_error)
            }
            EventKind::Move => {
                let x = event.x as f64 / u16::MAX as f64 * handle.target.width as f64;
                let y = event.y as f64 / u16::MAX as f64 * handle.target.height as f64;
                let at = Instant::now();
                let res = runtime.block_on(async {
                    proxy
                        .notify_pointer_motion_absolute(session, stream, x, y, Default::default())
                        .await
                });
                let us = at.elapsed().as_micros().min(u128::from(u32::MAX)) as u32;
                moves_count += 1;
                if us > moves_max_us {
                    moves_max_us = us;
                }
                if moves_window.elapsed() >= MOVE_WINDOW {
                    eprintln!(
                        "input: moves={} max={} us in the last 1 s",
                        moves_count, moves_max_us
                    );
                    moves_window = Instant::now();
                    moves_count = 0;
                    moves_max_us = 0;
                }
                res.map_err(map_error)
            }
            EventKind::Up => {
                let at = Instant::now();
                let res = runtime.block_on(async {
                    proxy
                        .notify_pointer_button(
                            session,
                            BTN_LEFT,
                            KeyState::Released,
                            Default::default(),
                        )
                        .await
                });
                eprintln!(
                    "input: Up release={} us{}",
                    at.elapsed().as_micros(),
                    if res.is_err() { " FAILED" } else { "" },
                );
                res.map_err(map_error)
            }
            // Hover and cancel carry no pointer meaning for a finger.
            EventKind::Hover | EventKind::Cancel => Ok(()),
        };
        self.moves_count = moves_count;
        self.moves_max_us = moves_max_us;
        self.moves_window = moves_window;
        self.handle = Some(handle);
        result
    }

    fn release(&mut self) {
        if let Some(handle) = self.handle.take() {
            // Best-effort button release so a contact cannot stay
            // stuck when the session ends. The runtime moves out with
            // the handle; block_on it while it still exists.
            let mut handle = handle;
            let runtime = handle.runtime.take();
            let _ = runtime.and_then(|rt| {
                rt.block_on(async {
                    handle
                        .proxy
                        .notify_pointer_button(
                            &handle.session,
                            BTN_LEFT,
                            KeyState::Released,
                            Default::default(),
                        )
                        .await
                })
                .ok()
            });
        }
    }
}

impl Drop for PortalPointer {
    fn drop(&mut self) {
        self.release();
    }
}
