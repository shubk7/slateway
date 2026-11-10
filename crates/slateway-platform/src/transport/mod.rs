// SPDX-License-Identifier: GPL-3.0-or-later

//! The [`Transport`] trait and the transports that share no
//! platform-specific code (`docs/ARCHITECTURE.md`, "Platform traits").
//!
//! The transport decides who connects: listen transports (adb reverse,
//! the loopback test transport) wait for the tablet to dial, dial
//! transports (USB tethering, AOA, later) connect to the tablet.
//! Handshake roles are fixed regardless: the host responds
//! (`docs/PROTOCOL.md`, "Transports").

mod adb;
mod loopback;

pub use adb::AdbReverseTransport;
pub use loopback::LoopbackTransport;

use std::{io, net::SocketAddr};

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncWrite};

/// Default TCP port for the transports that listen (`docs/PROTOCOL.md`,
/// "Transports"). Configurable by the caller.
pub const DEFAULT_PORT: u16 = 47820;

/// The stream capabilities a session needs from the tablet link.
pub trait AsyncReadWrite: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncReadWrite for T {}

/// The duplex byte stream to the tablet, boxed so each transport can
/// return its own stream type (TCP today, other carriers later).
pub type BoxStream = Box<dyn AsyncReadWrite>;

/// Transport failures.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// Binding, accepting or dialing failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// An adb command failed. The exit code is carried for programmatic
    /// use, plus the last line of adb's stderr as context: the daemon
    /// retries the tunnel registration periodically, so its own error
    /// line must say why without adb scribbling on the terminal.
    #[error(
        "adb {step} failed{detail}",
        detail = .detail
            .as_ref()
            .map(|text| format!(": {text}"))
            .unwrap_or_default()
    )]
    Adb {
        /// Which adb invocation failed ("reverse", "reverse --remove").
        step: &'static str,
        /// Exit code, if the process reported one.
        code: Option<i32>,
        /// Last non-empty line of adb's stderr, if it printed one.
        detail: Option<String>,
    },
    /// [`Transport::stream`] was called before [`Transport::prepare`].
    #[error("transport is not ready: prepare must run before stream")]
    NotReady,
}

/// Produces the duplex byte stream to the tablet
/// (`docs/ARCHITECTURE.md`, "Platform traits").
#[async_trait]
pub trait Transport: Send {
    /// Prepares the link: binds the listener, registers the adb reverse
    /// tunnel. Returns the address the tablet side dials, for the
    /// listen transports; `None` will be the answer of the dial
    /// transports (USB tethering, AOA) that arrive later.
    ///
    /// Idempotent: a bound listener stays bound and the tunnel is
    /// (re-)registered on every call, so the daemon can refresh the
    /// registration while idle - the adb reverse rule dies with the
    /// USB connection.
    async fn prepare(&mut self) -> Result<Option<SocketAddr>, TransportError>;

    /// Produces the duplex byte stream to the tablet, waiting for the
    /// connection (accept or dial) as the transport requires. Call
    /// after [`Transport::prepare`]; repeatable - each call accepts
    /// one connection while the listener keeps serving.
    async fn stream(&mut self) -> Result<BoxStream, TransportError>;
}
