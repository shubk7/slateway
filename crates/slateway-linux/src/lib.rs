// SPDX-License-Identifier: GPL-3.0-or-later

//! Slateway Linux backends.
//!
//! Platform-specific code lives only in this crate (AGENTS.md rule 2);
//! everything not gated on `cfg(linux)` must build on every OS.

#[cfg(target_os = "linux")]
pub mod capture;

#[cfg(target_os = "linux")]
pub mod encoder;

#[cfg(target_os = "linux")]
pub use capture::PortalCapture;
