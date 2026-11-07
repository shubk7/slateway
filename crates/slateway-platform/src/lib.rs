// SPDX-License-Identifier: GPL-3.0-or-later

//! Slateway platform traits and shared types only.
//!
//! The [`transport::Transport`] trait produces the duplex byte stream
//! to the tablet (`docs/ARCHITECTURE.md`, "Platform traits"). The
//! transports that need no platform-specific code (adb reverse, the
//! loopback test transport) live here too. The remaining traits
//! (DisplayBackend, CaptureSource, Encoder, InputInjector, the audio
//! traits) arrive with their milestones; their platform implementations
//! live only in `slateway-win` and `slateway-linux` (AGENTS.md rule 2).

pub mod transport;

pub use transport::{
    AdbReverseTransport, AsyncReadWrite, BoxStream, DEFAULT_PORT, LoopbackTransport, Transport,
    TransportError,
};
