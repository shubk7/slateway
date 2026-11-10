// SPDX-License-Identifier: GPL-3.0-or-later

//! Loopback transport: the host listens on loopback and accepts the
//! tablet's connection (`docs/PROTOCOL.md`, "Transports": adb reverse
//! has the tablet connect to host loopback).
//!
//! This is both the test transport (ephemeral port, no adb) and the
//! accept half of the adb reverse transport.

use std::net::{Ipv4Addr, SocketAddr};

use async_trait::async_trait;
use tokio::net::TcpListener;

use super::{BoxStream, Transport, TransportError};

/// Listens on host loopback and accepts the tablet's connection.
pub struct LoopbackTransport {
    port: u16,
    listener: Option<TcpListener>,
}

impl LoopbackTransport {
    /// New transport that listens on an OS-assigned loopback port at
    /// prepare time.
    pub fn new() -> Self {
        Self {
            port: 0,
            listener: None,
        }
    }

    /// New transport that listens on a fixed loopback port at prepare
    /// time; port 0 behaves like [`LoopbackTransport::new`].
    pub fn with_port(port: u16) -> Self {
        Self {
            port,
            listener: None,
        }
    }
}

impl Default for LoopbackTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Transport for LoopbackTransport {
    async fn prepare(&mut self) -> Result<Option<SocketAddr>, TransportError> {
        // Loopback only, never all interfaces (docs/PROTOCOL.md,
        // "Transports"). Idempotent: the daemon re-prepares between
        // sessions (adb reverse re-registration, cable replug) while
        // the listener stays bound, so a tablet reconnecting during
        // pairing never dials a dead port.
        if let Some(listener) = &self.listener {
            return Ok(Some(listener.local_addr()?));
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, self.port)).await?;
        let addr = listener.local_addr()?;
        self.listener = Some(listener);
        Ok(Some(addr))
    }

    async fn stream(&mut self) -> Result<BoxStream, TransportError> {
        // The listener stays with the transport (not taken): one
        // accept per call, accepts continue while a session runs.
        let listener = self.listener.as_ref().ok_or(TransportError::NotReady)?;
        let (stream, _peer) = listener.accept().await?;
        // Latency rule 5: frames are sent immediately, on both ends of
        // the link. Without this the daemon's replies wait on Nagle
        // against the peer's delayed ACK (~40 ms on Linux).
        let _ = stream.set_nodelay(true);
        Ok(Box::new(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    #[tokio::test]
    async fn stream_before_prepare_fails() {
        let mut transport = LoopbackTransport::new();
        assert!(matches!(
            transport.stream().await,
            Err(TransportError::NotReady)
        ));
    }

    #[tokio::test]
    async fn binds_loopback_only() {
        let mut transport = LoopbackTransport::new();
        let addr = transport.prepare().await.unwrap().unwrap();
        assert!(addr.is_ipv4());
        assert!(addr.ip().is_loopback());
    }

    #[tokio::test]
    async fn exchanges_bytes_with_the_tablet_side() {
        let mut transport = LoopbackTransport::new();
        let addr = transport.prepare().await.unwrap().unwrap();
        let mut tablet = TcpStream::connect(addr).await.unwrap();
        let mut host = transport.stream().await.unwrap();

        tablet.write_all(b"hello").await.unwrap();
        let mut buf = [0u8; 5];
        host.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");

        host.write_all(b"ack").await.unwrap();
        let mut buf = [0u8; 3];
        tablet.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ack");
    }

    #[tokio::test]
    async fn accepts_two_connections_and_survives_reprepare() {
        // The daemon re-prepares between sessions and accepts the next
        // tablet while a pairing prompt is open: the listener must
        // stay bound across accepts (the retry EOF regression).
        let mut transport = LoopbackTransport::new();
        let addr = transport.prepare().await.unwrap().unwrap();
        let _tablet1 = TcpStream::connect(addr).await.unwrap();
        let _host1 = transport.stream().await.unwrap();

        let addr2 = transport.prepare().await.unwrap().unwrap();
        assert_eq!(addr, addr2, "re-prepare reuses the bound listener");
        let _tablet2 = TcpStream::connect(addr2).await.unwrap();
        let _host2 = transport.stream().await.unwrap();
    }
}
