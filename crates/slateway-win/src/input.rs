// SPDX-License-Identifier: GPL-3.0-or-later

//! Windows touch-as-pointer injection via `SendInput` (M1,
//! user-approved): tablet contact maps to absolute mouse motion and
//! left-button presses, so the visible cursor follows the tablet.
//! `PLATFORMS.md`'s synthetic-pointer row (PT_TOUCH/PT_PEN) returns
//! in M3 with the pen, where pressure and tilt need it.
//!
//! Coordinates: `SendInput` absolute moves span the virtual desktop
//! when `MOUSEEVENTF_VIRTUALDESK` is set, so the injector maps the
//! event's 0..65535 stream coordinates onto the capture target (the
//! primary monitor, matching `WgcCapture`) inside that desktop.
//!
//! Injection needs no elevation for same-integrity targets; elevated
//! windows ignore it (UIPI), logged as an error, not a crash.

use slateway_platform::input::{InputError, PointerInjector, PointerTarget};
use slateway_proto::input::{EventKind, InputEvent, Tool};
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_MOUSE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT, SendInput,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

/// The streamed display's geometry on the host: the primary monitor,
/// which is what `WgcCapture` streams in M1.
fn primary_target() -> Result<PointerTarget, InputError> {
    // SAFETY: MonitorFromPoint and GetMonitorInfoW take plain values;
    // MONITORINFO is zero-initialized with cbSize set per contract.
    let point = POINT { x: 0, y: 0 };
    let hmonitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTOPRIMARY) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { windows::Win32::Graphics::Gdi::GetMonitorInfoW(hmonitor, &mut info) }.as_bool() {
        return Err(InputError::Setup("GetMonitorInfoW failed".into()));
    }
    Ok(PointerTarget {
        origin_x: info.rcMonitor.left,
        origin_y: info.rcMonitor.top,
        width: (info.rcMonitor.right - info.rcMonitor.left).max(0) as u32,
        height: (info.rcMonitor.bottom - info.rcMonitor.top).max(0) as u32,
    })
}

/// Mouse-backed pointer injector.
pub struct MouseInjector {
    target: PointerTarget,
    /// Virtual desktop metrics for `MOUSEEVENTF_VIRTUALDESK` math.
    vx: i32,
    vy: i32,
    vw: i32,
    vh: i32,
    pressed: bool,
}

impl MouseInjector {
    /// Reads the virtual desktop metrics and the capture target.
    pub fn new() -> Result<Self, InputError> {
        let target = primary_target()?;
        if target.width == 0 || target.height == 0 {
            return Err(InputError::Setup("primary monitor has no size".into()));
        }
        // SAFETY: GetSystemMetrics takes a plain index and cannot fail
        // for the virtual screen constants.
        let vx = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
        let vy = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
        let vw = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
        let vh = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
        if vw <= 0 || vh <= 0 {
            return Err(InputError::Setup("virtual screen has no size".into()));
        }
        Ok(MouseInjector {
            target,
            vx,
            vy,
            vw,
            vh,
            pressed: false,
        })
    }

    /// Maps the event's 0..65535 stream coordinates to absolute
    /// virtual-desktop coordinates for `SendInput`.
    fn absolute(&self, x: u16, y: u16) -> (i32, i32) {
        // Stream coordinate -> primary monitor pixel.
        let px =
            self.target.origin_x + (x as i64 * self.target.width as i64 / u16::MAX as i64) as i32;
        let py =
            self.target.origin_y + (y as i64 * self.target.height as i64 / u16::MAX as i64) as i32;
        // Pixel -> 0..65535 across the virtual desktop.
        let ax =
            ((px - self.vx) as i64 * u16::MAX as i64 / self.vw as i64).clamp(0, u16::MAX as i64);
        let ay =
            ((py - self.vy) as i64 * u16::MAX as i64 / self.vh as i64).clamp(0, u16::MAX as i64);
        (ax as i32, ay as i32)
    }

    /// Sends one mouse input (move plus optional button flags).
    fn send(&mut self, x: i32, y: i32, flags: MOUSE_EVENT_FLAGS) -> Result<(), InputError> {
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: x,
                    dy: y,
                    mouseData: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        // SAFETY: one INPUT_MOUSE record of the correct size; SendInput
        // is called from the input worker thread per its contract.
        let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
        if sent != 1 {
            return Err(InputError::Failed(format!(
                "SendInput injected {sent} of 1 events"
            )));
        }
        Ok(())
    }

    /// Absolute move to the mapped position.
    fn move_to(&mut self, x: u16, y: u16) -> Result<(), InputError> {
        let (ax, ay) = self.absolute(x, y);
        self.send(
            ax,
            ay,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        )
    }

    fn press(&mut self, x: u16, y: u16) -> Result<(), InputError> {
        let (ax, ay) = self.absolute(x, y);
        self.send(
            ax,
            ay,
            MOUSEEVENTF_MOVE
                | MOUSEEVENTF_ABSOLUTE
                | MOUSEEVENTF_VIRTUALDESK
                | MOUSEEVENTF_LEFTDOWN,
        )?;
        self.pressed = true;
        Ok(())
    }

    fn unpress(&mut self, x: u16, y: u16) -> Result<(), InputError> {
        if !self.pressed {
            return Ok(());
        }
        let (ax, ay) = self.absolute(x, y);
        self.send(
            ax,
            ay,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK | MOUSEEVENTF_LEFTUP,
        )?;
        self.pressed = false;
        Ok(())
    }
}

impl PointerInjector for MouseInjector {
    fn inject(&mut self, event: &InputEvent) -> Result<(), InputError> {
        // M1: only the finger tool drives the pointer; pen arrives in
        // M3 with the synthetic pointer device.
        if event.tool != Tool::Finger {
            return Ok(());
        }
        match event.kind {
            EventKind::Down => {
                if !self.pressed {
                    return self.press(event.x, event.y);
                }
                // A second contact (multi-touch) does not re-press.
                self.move_to(event.x, event.y)
            }
            EventKind::Move => {
                // Absolute moves work both with and without the button
                // held (drag = move while pressed).
                self.move_to(event.x, event.y)
            }
            EventKind::Up => self.unpress(event.x, event.y),
            // Hover has no mouse meaning; cancel releases the button.
            EventKind::Hover => Ok(()),
            EventKind::Cancel => {
                if self.pressed {
                    return self.unpress(event.x, event.y);
                }
                Ok(())
            }
        }
    }

    fn release(&mut self) {
        // Best-effort button release so no click can stay stuck when
        // the session ends. dx/dy are ignored without a MOVE flag.
        if self.pressed {
            let input = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: 0,
                        dwFlags: MOUSEEVENTF_LEFTUP,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            // SAFETY: same record shape as send(); a failed release
            // cannot be reported here (the session is ending).
            let _ = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
            self.pressed = false;
        }
    }
}

impl Drop for MouseInjector {
    fn drop(&mut self) {
        self.release();
    }
}
