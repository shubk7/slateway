// SPDX-License-Identifier: GPL-3.0-or-later

//! Slateway Windows backends.
//!
//! Platform-specific code lives only in this crate (AGENTS.md rule 2);
//! everything not gated on `cfg(windows)` must build on every OS.

// Explicit unsafe opt-in (AGENTS.md review gate, maintainer-approved for
// this task): Windows.Graphics.Capture and D3D11 COM interop are unsafe
// user-mode APIs. Every unsafe block in `capture.rs` carries a
// `// SAFETY:` comment; the allow is scoped to this module only.
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
pub mod capture;

#[cfg(target_os = "windows")]
pub use capture::WgcCapture;
