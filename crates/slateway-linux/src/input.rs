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

use ashpd::desktop::Session;
use ashpd::desktop::remote_desktop::{KeyState, RemoteDesktop};
use slateway_platform::input::{InputError, PointerInjector, PointerTarget};
use slateway_proto::input::{EventKind, InputEvent, Tool};

/// evdev button codes the portal expects (linux/input-event-codes.h).
const BTN_LEFT: i32 = 0x110;

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
        let Some(handle) = self.handle.as_ref() else {
            return Err(InputError::Failed("injector already released".into()));
        };
        // Single owner, single block_on: the input worker is the only
        // caller (tokio panics on concurrent block_on of one runtime).
        let runtime = handle
            .runtime
            .as_ref()
            .ok_or_else(|| InputError::Failed("injector already released".into()))?;
        let (proxy, session, stream) = (&handle.proxy, &handle.session, handle.stream);
        match event.kind {
            EventKind::Down => {
                // Motion first, then the press: the click lands where
                // the finger touched. The portal expects coordinates in
                // the stream's logical coordinate space
                // (NotifyPointerMotionAbsolute, "see the logical_size
                // stream property in ScreenCast" — the xdg RemoteDesktop
                // spec), not normalized 0..1.
                let x = event.x as f64 / u16::MAX as f64 * handle.target.width as f64;
                let y = event.y as f64 / u16::MAX as f64 * handle.target.height as f64;
                runtime.block_on(async move {
                    proxy
                        .notify_pointer_motion_absolute(session, stream, x, y, Default::default())
                        .await
                        .map_err(map_error)?;
                    proxy
                        .notify_pointer_button(
                            session,
                            BTN_LEFT,
                            KeyState::Pressed,
                            Default::default(),
                        )
                        .await
                        .map_err(map_error)
                })
            }
            EventKind::Move => {
                let x = event.x as f64 / u16::MAX as f64 * handle.target.width as f64;
                let y = event.y as f64 / u16::MAX as f64 * handle.target.height as f64;
                runtime.block_on(async move {
                    proxy
                        .notify_pointer_motion_absolute(session, stream, x, y, Default::default())
                        .await
                        .map_err(map_error)
                })
            }
            EventKind::Up => runtime.block_on(async move {
                proxy
                    .notify_pointer_button(
                        session,
                        BTN_LEFT,
                        KeyState::Released,
                        Default::default(),
                    )
                    .await
                    .map_err(map_error)
            }),
            // Hover and cancel carry no pointer meaning for a finger.
            EventKind::Hover | EventKind::Cancel => Ok(()),
        }
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
