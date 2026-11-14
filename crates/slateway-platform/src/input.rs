// SPDX-License-Identifier: GPL-3.0-or-later

//! The [`PointerInjector`] trait for touch-as-pointer (docs/ROADMAP.md
//! M1, "Touch as pointer"): tablet touch, pen and hover events mapped
//! to the host's pointer over the streamed display's rectangle.
//!
//! M1 policy (user-approved): the primary contact drives the OS
//! pointer — Windows injects absolute mouse events (`SendInput`), so
//! the visible cursor follows; Linux routes through the RemoteDesktop
//! portal, whose session is shared with the capture so the compositor
//! maps coordinates to the streamed monitor. Platform implementations
//! live in `slateway-linux` / `slateway-win` (AGENTS.md rule 2).
//!
//! Consent (docs/SECURITY.md, "Consent"): the daemon only creates an
//! injector when the operator passed `--allow-input`; per-device
//! persisted grants arrive with the consent UI.

use slateway_proto::input::InputEvent;
use thiserror::Error;

/// Where the streamed display sits on the host's coordinate space, so
/// injectors can map the event's 0..65535 stream coordinates onto it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointerTarget {
    /// Left edge of the streamed display in host pixels.
    pub origin_x: i32,
    /// Top edge of the streamed display in host pixels.
    pub origin_y: i32,
    /// Streamed display width in host pixels.
    pub width: u32,
    /// Streamed display height in host pixels.
    pub height: u32,
}

/// Input injection failures.
#[derive(Debug, Error)]
pub enum InputError {
    /// Setup failed (portal refused, no OS permission).
    #[error("input setup failed: {0}")]
    Setup(String),
    /// The injector was running but then failed.
    #[error("input injection failed: {0}")]
    Failed(String),
}

/// Injects input events as host pointer motion and button presses.
/// All methods are blocking and called from the daemon's dedicated
/// input worker thread (like [`crate::capture::CaptureSource`], the
/// consumer owns the threading).
pub trait PointerInjector: Send {
    /// Inject one event. `event.x`/`event.y` are 0..65535 across the
    /// streamed display ([`PointerTarget`] maps them onward); the
    /// implementation decides what each [`InputEvent`] kind and tool
    /// means for its OS pointer.
    fn inject(&mut self, event: &InputEvent) -> Result<(), InputError>;

    /// Release everything (buttons held, portal session) so no contact
    /// can stay stuck when the session ends. Also runs on Drop.
    fn release(&mut self);
}
