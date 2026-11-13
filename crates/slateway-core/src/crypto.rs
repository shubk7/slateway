// SPDX-License-Identifier: GPL-3.0-or-later

//! Noise session: `Noise_XX_25519_ChaChaPoly_SHA256` over a duplex
//! byte stream (`docs/PROTOCOL.md`, "Noise records"; `docs/SECURITY.md`).
//!
//! Every byte on the stream is encrypted: handshake messages and
//! transport frames both ride in records of at most [`MAX_RECORD`]
//! bytes (`u16` little-endian length plus ciphertext). Decrypted
//! record payloads concatenate into the plain byte stream the frame
//! codec parses. The host responds in the fixed three-message XX
//! handshake; the tablet initiates.

use std::collections::VecDeque;
use std::io;

use slateway_proto::frame::{FrameHeader, HEADER_LEN, MAX_PAYLOAD_LEN};
use snow::{Builder, HandshakeState, TransportState};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The cipher suite fixed by ADR-006 and `docs/PROTOCOL.md`.
pub const CIPHER_SUITE: &str = "Noise_XX_25519_ChaChaPoly_SHA256";

/// Maximum ciphertext bytes per record: the Noise protocol limit.
pub const MAX_RECORD: usize = 65535;

/// Noise tag size.
pub const TAG_LEN: usize = 16;

/// Maximum plaintext bytes per record.
pub const MAX_PLAINTEXT: usize = MAX_RECORD - TAG_LEN;

/// Curve25519 static key length.
pub const KEY_LEN: usize = 32;

/// Maximum bytes in a frame: header plus payload.
pub const MAX_FRAME: usize = HEADER_LEN + MAX_PAYLOAD_LEN;

/// Session and pairing failures.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// The underlying stream failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The Noise state machine rejected a message or a call.
    #[error("noise: {0}")]
    Noise(#[from] snow::Error),
    /// A record or frame exceeded its limit.
    #[error("{what} of {len} bytes exceeds the {max} byte limit")]
    TooLarge {
        /// What exceeded the limit ("record", "frame", ...).
        what: &'static str,
        /// Observed length.
        len: usize,
        /// The limit.
        max: usize,
    },
    /// The peer violated the protocol.
    #[error("protocol violation: {0}")]
    Protocol(&'static str),
    /// A decrypted frame failed to parse.
    #[error(transparent)]
    Frame(#[from] slateway_proto::frame::FrameError),
    /// A decrypted control message failed to decode.
    #[error(transparent)]
    Decode(#[from] prost::DecodeError),
}

/// Derives the 6-digit pairing code from the handshake hash
/// (`docs/PROTOCOL.md`, "Noise records"): first 4 bytes big-endian mod
/// 1,000,000, zero-padded. Takes exactly the SHA-256 length
/// (`HandshakeDone::handshake_hash`), so it cannot index out of
/// bounds.
pub fn pairing_code(handshake_hash: &[u8; 32]) -> String {
    let value = u32::from_be_bytes([
        handshake_hash[0],
        handshake_hash[1],
        handshake_hash[2],
        handshake_hash[3],
    ]) % 1_000_000;
    format!("{value:06}")
}

/// Generates a Curve25519 static keypair with the session cipher suite.
pub fn generate_keypair() -> Result<(Vec<u8>, Vec<u8>), SessionError> {
    let params = CIPHER_SUITE.parse()?;
    let keypair = Builder::new(params).generate_keypair()?;
    Ok((keypair.private, keypair.public))
}

/// One side of a Noise XX handshake over a duplex stream.
pub struct SessionHandshake {
    state: HandshakeState,
    /// Ciphertext output buffer, reused for every record.
    record: Box<[u8; MAX_RECORD]>,
}

/// What a completed handshake yields before transport mode.
pub struct HandshakeDone<S> {
    /// The remote party's static public key (32 bytes).
    pub remote_static: [u8; KEY_LEN],
    /// The handshake hash; the pairing code derives from it.
    pub handshake_hash: [u8; 32],
    /// The final handshake message's payload: the tablet sends its
    /// device name, the host may answer with its own.
    pub peer_payload: Vec<u8>,
    /// The encrypted session, ready for frames. It owns the stream.
    pub session: SecureSession<S>,
}

impl SessionHandshake {
    /// New initiator (the tablet's role).
    pub fn initiator(local_private_key: &[u8; KEY_LEN]) -> Result<Self, SessionError> {
        Ok(Self {
            state: Self::build(local_private_key, true)?,
            record: Box::new([0u8; MAX_RECORD]),
        })
    }

    /// New responder (the host's role).
    pub fn responder(local_private_key: &[u8; KEY_LEN]) -> Result<Self, SessionError> {
        Ok(Self {
            state: Self::build(local_private_key, false)?,
            record: Box::new([0u8; MAX_RECORD]),
        })
    }

    fn build(
        local_private_key: &[u8; KEY_LEN],
        initiator: bool,
    ) -> Result<HandshakeState, SessionError> {
        let params = CIPHER_SUITE.parse()?;
        // snow 0.10's builder calls are fallible.
        let builder = Builder::new(params).local_private_key(local_private_key)?;
        Ok(if initiator {
            builder.build_initiator()?
        } else {
            builder.build_responder()?
        })
    }

    /// Runs the whole handshake over `stream`, which the returned
    /// session then owns: three XX messages, `final_payload` rides in
    /// the last message this side sends (empty for the responder).
    /// Returns the pairing inputs and the session.
    pub async fn complete<S>(
        mut self,
        mut stream: S,
        final_payload: &[u8],
    ) -> Result<HandshakeDone<S>, SessionError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let initiator = self.state.is_initiator();
        let mut peer_payload = Vec::new();
        for round in 0..3 {
            // The initiator sends rounds 0 and 2, the responder round 1.
            let my_turn = (round % 2 == 0) == initiator;
            if my_turn {
                let payload: &[u8] = if round == 2 { final_payload } else { &[] };
                self.send(&mut stream, payload).await?;
            } else {
                peer_payload = self.receive(&mut stream).await?;
            }
        }
        if !self.state.is_handshake_finished() {
            return Err(SessionError::Protocol("handshake did not finish"));
        }
        let remote_static: [u8; KEY_LEN] = self
            .state
            .get_remote_static()
            .and_then(|key| key.try_into().ok())
            .ok_or(SessionError::Protocol("remote static key unknown after XX"))?;
        let handshake_hash: [u8; 32] = self
            .state
            .get_handshake_hash()
            .try_into()
            .map_err(|_| SessionError::Protocol("unexpected handshake hash length"))?;
        let noise = self.state.into_transport_mode()?;
        Ok(HandshakeDone {
            remote_static,
            handshake_hash,
            peer_payload,
            session: SecureSession {
                stream,
                noise,
                record: self.record,
                staging: Box::new([0u8; MAX_RECORD]),
                pending: Vec::new(),
                inflight: Vec::new(),
                outbox: VecDeque::new(),
                outbox_off: 0,
            },
        })
    }

    /// Writes and sends one handshake message carrying `payload`.
    async fn send<S>(&mut self, stream: &mut S, payload: &[u8]) -> Result<(), SessionError>
    where
        S: AsyncWrite + Unpin,
    {
        if payload.len() > MAX_PLAINTEXT {
            return Err(SessionError::TooLarge {
                what: "handshake payload",
                len: payload.len(),
                max: MAX_PLAINTEXT,
            });
        }
        let len = self.state.write_message(payload, self.record.as_mut())?;
        write_record(stream, &self.record[..len]).await
    }

    /// Reads and processes one handshake message; returns its payload.
    async fn receive<S>(&mut self, stream: &mut S) -> Result<Vec<u8>, SessionError>
    where
        S: AsyncRead + Unpin,
    {
        let len = read_record(stream, self.record.as_mut()).await?;
        // Handshake payloads are tiny (a device name); a plain Vec is
        // fine here, unlike the per-frame transport path.
        let mut payload = vec![0u8; len];
        let n = self.state.read_message(&self.record[..len], &mut payload)?;
        payload.truncate(n);
        Ok(payload)
    }
}

/// An established, encrypted session carrying the frame protocol.
///
/// Per-frame work allocates nothing: records decrypt into one reused
/// buffer and frames are handed out of one reused pending buffer
/// (docs/ARCHITECTURE.md, latency rule 4; the media path will move to
/// pooled buffers in M1).
pub struct SecureSession<S> {
    stream: S,
    noise: TransportState,
    /// Ciphertext in, ciphertext out: reused for every record.
    record: Box<[u8; MAX_RECORD]>,
    /// Plaintext staging, reused for every outgoing record. Separate
    /// from `record` because snow's write takes disjoint slices.
    staging: Box<[u8; MAX_RECORD]>,
    /// Decrypted bytes not yet consumed by the frame parser.
    pending: Vec<u8>,
    /// Wire bytes consumed from the stream but not yet decrypted as a
    /// complete record (length prefix included). Real bytes only, so a
    /// cancelled read resumes exactly where it stopped and no stream
    /// byte is ever lost.
    inflight: Vec<u8>,
    /// Records already encrypted (their nonces are burned) and queued
    /// for the wire, length prefix included, in order. A cancelled
    /// write flushes them on the next write, so the stream stays
    /// record-aligned and abandoned frames are delivered whole.
    outbox: VecDeque<Vec<u8>>,
    /// Bytes of `outbox` front already on the wire.
    outbox_off: usize,
}

impl<S: AsyncRead + AsyncWrite + Unpin> SecureSession<S> {
    /// Reads one complete frame: its header and, into `payload`, its
    /// bytes. Returns the header and the payload length. The buffer
    /// must be at least as large as the frame's payload.
    pub async fn read_frame(
        &mut self,
        payload: &mut [u8],
    ) -> Result<(FrameHeader, usize), SessionError> {
        let mut header = [0u8; HEADER_LEN];
        loop {
            if self.pending.len() >= HEADER_LEN {
                header.copy_from_slice(&self.pending[..HEADER_LEN]);
                let parsed = FrameHeader::parse(&header)?;
                let total = HEADER_LEN + parsed.payload_len as usize;
                if payload.len() < parsed.payload_len as usize {
                    return Err(SessionError::TooLarge {
                        what: "frame for the caller buffer",
                        len: parsed.payload_len as usize,
                        max: payload.len(),
                    });
                }
                while self.pending.len() < total {
                    self.pull_record().await?;
                }
                payload[..parsed.payload_len as usize]
                    .copy_from_slice(&self.pending[HEADER_LEN..total]);
                self.pending.drain(..total);
                return Ok((parsed, parsed.payload_len as usize));
            }
            self.pull_record().await?;
        }
    }

    /// Encrypts and writes one frame. Frames larger than one record
    /// are split; the peer's frame parser reassembles them from the
    /// decrypted stream.
    ///
    /// Cancel-safe: encrypted records queue in the outbox and flush
    /// here and on every later write, so a cancelled write still
    /// delivers its records whole and the stream stays record-aligned
    /// (encrypting burns nonces in order; the wire must consume them
    /// in the same order).
    pub async fn write_frame(
        &mut self,
        header: FrameHeader,
        payload: &[u8],
    ) -> Result<(), SessionError> {
        // Deliver anything a previously cancelled write left queued
        // before this frame's records join the queue.
        self.flush_outbox().await?;
        let frame_len = HEADER_LEN + payload.len();
        if frame_len > MAX_FRAME {
            return Err(SessionError::TooLarge {
                what: "frame",
                len: frame_len,
                max: MAX_FRAME,
            });
        }
        let header_bytes = header.encode();
        let mut header_left = HEADER_LEN;
        let mut offset = 0usize;
        loop {
            // Each record carries some of the header (first record
            // only) and then as much payload as fits.
            let room = MAX_PLAINTEXT - header_left;
            let chunk_len = room.min(payload.len() - offset);
            let plaintext_len = header_left + chunk_len;
            self.staging[..header_left].copy_from_slice(&header_bytes[..header_left]);
            self.staging[header_left..plaintext_len]
                .copy_from_slice(&payload[offset..offset + chunk_len]);
            offset += chunk_len;
            header_left = 0;
            let ciphertext_len = self
                .noise
                .write_message(&self.staging[..plaintext_len], self.record.as_mut())?;
            let mut wire = Vec::with_capacity(2 + ciphertext_len);
            wire.extend_from_slice(&(ciphertext_len as u16).to_le_bytes());
            wire.extend_from_slice(&self.record[..ciphertext_len]);
            self.outbox.push_back(wire);
            if offset == payload.len() {
                break;
            }
        }
        self.flush_outbox().await
    }

    /// Writes queued records to the wire. Cancel-safe: the write
    /// offset survives in the session, and a partial write resumes on
    /// the next call before anything new is queued.
    async fn flush_outbox(&mut self) -> Result<(), SessionError> {
        while let Some(front_len) = self.outbox.front().map(Vec::len) {
            while self.outbox_off < front_len {
                let front = self.outbox.front().expect("front checked above");
                let wrote = self.stream.write(&front[self.outbox_off..]).await?;
                if wrote == 0 {
                    return Err(SessionError::Io(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "stream write made no progress",
                    )));
                }
                self.outbox_off += wrote;
            }
            self.outbox.pop_front();
            self.outbox_off = 0;
        }
        Ok(())
    }

    /// Pulls one ciphertext record from the stream and decrypts it
    /// into `pending`. Incremental and cancel-safe: every wire byte
    /// lands in `inflight` as soon as it is read, so a cancelled poll
    /// (a read wrapped in a timeout, the norm under load) resumes
    /// exactly where it stopped and the Noise nonce stream never
    /// desyncs.
    async fn pull_record(&mut self) -> Result<(), SessionError> {
        loop {
            if self.inflight.len() >= 2 {
                let len = u16::from_le_bytes([self.inflight[0], self.inflight[1]]) as usize;
                if len == 0 {
                    return Err(SessionError::TooLarge {
                        what: "record",
                        len,
                        max: MAX_RECORD,
                    });
                }
                if self.inflight.len() >= 2 + len {
                    let start = self.pending.len();
                    self.pending.resize(start + len, 0);
                    match self
                        .noise
                        .read_message(&self.inflight[2..2 + len], &mut self.pending[start..])
                    {
                        Ok(n) => {
                            self.pending.truncate(start + n);
                            self.inflight.drain(..2 + len);
                            return Ok(());
                        }
                        Err(err) => {
                            // Keep the buffer invariant: no garbage tail
                            // after a failed decrypt.
                            self.pending.truncate(start);
                            return Err(err.into());
                        }
                    }
                }
            }
            self.fill_inflight().await?;
        }
    }

    /// Reads more wire bytes into `inflight`. Single `read` calls only
    /// (never `read_exact`, whose multi-read state lives in the future
    /// and dies with it); each completed read is appended to the
    /// session immediately.
    async fn fill_inflight(&mut self) -> Result<(), SessionError> {
        let want = if self.inflight.len() < 2 {
            2 - self.inflight.len()
        } else {
            let len = u16::from_le_bytes([self.inflight[0], self.inflight[1]]) as usize;
            2 + len - self.inflight.len()
        };
        let mut scratch = [0u8; 4096];
        let mut got = 0;
        while got < want {
            let take = (want - got).min(scratch.len());
            let n = self.stream.read(&mut scratch[..take]).await?;
            if n == 0 {
                return Err(SessionError::Io(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "stream closed mid-record",
                )));
            }
            self.inflight.extend_from_slice(&scratch[..n]);
            got += n;
        }
        Ok(())
    }
}

/// Reads one record: `u16` little-endian length plus ciphertext.
/// Handshake-only: `read_exact` keeps its multi-read state in the
/// future, so a cancelled call loses consumed bytes - the transport
/// path uses the resumable `inflight` machinery instead. The handshake
/// is a fixed three-message exchange with no timeout wrappers.
async fn read_record<S: AsyncRead + Unpin>(
    stream: &mut S,
    buf: &mut [u8; MAX_RECORD],
) -> Result<usize, SessionError> {
    let mut len_bytes = [0u8; 2];
    stream.read_exact(&mut len_bytes).await?;
    let len = u16::from_le_bytes(len_bytes) as usize;
    if len == 0 || len > MAX_RECORD {
        return Err(SessionError::TooLarge {
            what: "record",
            len,
            max: MAX_RECORD,
        });
    }
    stream.read_exact(&mut buf[..len]).await?;
    Ok(len)
}

/// Writes one record: `u16` little-endian length plus ciphertext.
/// Handshake-only (see [`read_record`]); the transport path queues
/// records in the outbox instead.
async fn write_record<S: AsyncWrite + Unpin>(
    stream: &mut S,
    ciphertext: &[u8],
) -> Result<(), SessionError> {
    if ciphertext.is_empty() || ciphertext.len() > MAX_RECORD {
        return Err(SessionError::TooLarge {
            what: "record",
            len: ciphertext.len(),
            max: MAX_RECORD,
        });
    }
    let len = (ciphertext.len() as u16).to_le_bytes();
    stream.write_all(&len).await?;
    stream.write_all(ciphertext).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use slateway_proto::Channel;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Context, Poll};
    use tokio::io::duplex;

    /// A placeholder device name for the tablet's final handshake
    /// payload.
    const DEVICE_NAME: &[u8] = b"Tab A7";

    /// Runs a full XX handshake between two fresh sessions over a
    /// duplex pair; returns both sides' results and streams.
    async fn handshake_pair() -> (
        HandshakeDone<CorruptAtByte<tokio::io::DuplexStream>>,
        HandshakeDone<CorruptAtByte<tokio::io::DuplexStream>>,
    ) {
        let (initiator_priv, initiator_pub) = generate_keypair().unwrap();
        let (responder_priv, responder_pub) = generate_keypair().unwrap();
        let (a, b) = duplex(4096);
        let (initiator, responder) = tokio::join!(
            async {
                SessionHandshake::initiator(key(&initiator_priv))
                    .unwrap()
                    .complete(CorruptAtByte::new(a, usize::MAX), DEVICE_NAME)
                    .await
                    .unwrap()
            },
            async {
                SessionHandshake::responder(key(&responder_priv))
                    .unwrap()
                    .complete(CorruptAtByte::new(b, usize::MAX), &[])
                    .await
                    .unwrap()
            }
        );
        assert_eq!(initiator.remote_static.as_slice(), responder_pub.as_slice());
        assert_eq!(responder.remote_static.as_slice(), initiator_pub.as_slice());
        assert_eq!(
            initiator.handshake_hash, responder.handshake_hash,
            "both sides must derive the same handshake hash"
        );
        assert_eq!(responder.peer_payload, DEVICE_NAME);
        (initiator, responder)
    }

    fn key(private: &[u8]) -> &[u8; KEY_LEN] {
        private.try_into().unwrap()
    }

    /// Test sink: writes stay pending while the flag is set, so a
    /// write future can be cancelled deterministically mid-flush.
    struct StallWrites<S> {
        inner: S,
        stall: Arc<AtomicBool>,
    }

    impl<S: AsyncWrite + Unpin> AsyncWrite for StallWrites<S> {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            if self.stall.load(Ordering::SeqCst) {
                return Poll::Pending;
            }
            Pin::new(&mut self.inner).poll_write(cx, buf)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_flush(cx)
        }

        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    impl<S: AsyncRead + Unpin> AsyncRead for StallWrites<S> {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
        }
    }

    /// Reads survive cancellation mid-record: through a 1-byte pipe
    /// every record arrives in fragments and each timeout cancels the
    /// read between fragments. The frame must still arrive intact - a
    /// cancelled read must not lose consumed bytes or desync the Noise
    /// stream (the M1 landmine: reads under timeouts are the norm
    /// under load).
    #[tokio::test]
    async fn read_frame_survives_cancellation_mid_record() {
        let (initiator_priv, _) = generate_keypair().unwrap();
        let (responder_priv, _) = generate_keypair().unwrap();
        let (a, b) = duplex(1);
        let (tablet, host) = tokio::join!(
            async {
                SessionHandshake::initiator(key(&initiator_priv))
                    .unwrap()
                    .complete(a, DEVICE_NAME)
                    .await
                    .unwrap()
            },
            async {
                SessionHandshake::responder(key(&responder_priv))
                    .unwrap()
                    .complete(b, &[])
                    .await
                    .unwrap()
            }
        );
        let mut tablet = tablet.session;
        let mut host = host.session;

        // Too big to cross the 1-byte pipe inside one timeout window.
        let payload: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
        let writer_payload = payload.clone();
        let writer = tokio::spawn(async move {
            let header = FrameHeader {
                channel: Channel::Experimental { id: 200 },
                flags: 0,
                message_type: 1,
                payload_len: writer_payload.len() as u32,
                pts_us: 0,
            };
            tablet.write_frame(header, &writer_payload).await.unwrap();
            tablet.write_frame(header, &writer_payload).await.unwrap();
        });

        let mut got = vec![0u8; payload.len()];
        let mut arrived = false;
        for _ in 0..100_000 {
            match tokio::time::timeout(
                std::time::Duration::from_millis(2),
                host.read_frame(&mut got),
            )
            .await
            {
                Ok(Ok((_, len))) => {
                    assert_eq!(len, payload.len());
                    assert_eq!(got, payload, "frame content corrupted across cancellations");
                    arrived = true;
                    break;
                }
                Ok(Err(err)) => panic!("read failed under cancellation: {err}"),
                Err(_elapsed) => continue, // cancelled mid-record: resumable
            }
        }
        assert!(arrived, "the frame never completed under cancellation");
        // The stream stayed aligned: the second frame decrypts too.
        host.read_frame(&mut got).await.unwrap();
        assert_eq!(got, payload);
        writer.await.unwrap();
    }

    /// Writes survive cancellation mid-flush: a cancelled write_frame
    /// leaves its records queued (their nonces are burned), and the
    /// NEXT write delivers them before its own. The peer must see both
    /// frames whole, in order, and decryptable.
    #[tokio::test]
    async fn write_frame_survives_cancellation_mid_flush() {
        let (initiator_priv, _) = generate_keypair().unwrap();
        let (responder_priv, _) = generate_keypair().unwrap();
        let (a, b) = duplex(4096);
        let (tablet_done, host_done) = tokio::join!(
            async {
                SessionHandshake::initiator(key(&initiator_priv))
                    .unwrap()
                    .complete(a, DEVICE_NAME)
                    .await
                    .unwrap()
            },
            async {
                SessionHandshake::responder(key(&responder_priv))
                    .unwrap()
                    .complete(b, &[])
                    .await
                    .unwrap()
            }
        );
        let stall = Arc::new(AtomicBool::new(true));
        let tablet = tablet_done.session;
        let mut tablet = SecureSession {
            stream: StallWrites {
                inner: tablet.stream,
                stall: Arc::clone(&stall),
            },
            noise: tablet.noise,
            record: tablet.record,
            staging: tablet.staging,
            pending: tablet.pending,
            inflight: tablet.inflight,
            outbox: tablet.outbox,
            outbox_off: tablet.outbox_off,
        };
        let mut host = host_done.session;

        let first: Vec<u8> = vec![0xAA; 200];
        let second: Vec<u8> = vec![0xBB; 400];
        let reader = tokio::spawn(async move {
            let mut buf = vec![0u8; 400];
            let (_, len_a) = host.read_frame(&mut buf).await.unwrap();
            let frame_a = buf[..len_a].to_vec();
            let (_, len_b) = host.read_frame(&mut buf).await.unwrap();
            let frame_b = buf[..len_b].to_vec();
            (frame_a, frame_b)
        });

        let header = |payload_len: usize| FrameHeader {
            channel: Channel::Experimental { id: 200 },
            flags: 0,
            message_type: 1,
            payload_len: payload_len as u32,
            pts_us: 0,
        };
        // Cancelled mid-flush: the sink stalls on the first write.
        let cancelled = tokio::time::timeout(
            std::time::Duration::from_millis(20),
            tablet.write_frame(header(first.len()), &first),
        )
        .await;
        assert!(cancelled.is_err(), "the stalled sink must hold the write");
        // Unstall: the next write delivers the first frame's queued
        // records, then its own.
        stall.store(false, Ordering::SeqCst);
        tablet
            .write_frame(header(second.len()), &second)
            .await
            .unwrap();
        let (frame_a, frame_b) = reader.await.unwrap();
        assert_eq!(frame_a, first, "the cancelled frame must arrive whole");
        assert_eq!(frame_b, second, "the next frame must arrive after it");
    }

    #[tokio::test]
    async fn xx_handshake_agrees_on_pairing_code_and_keys() {
        let (initiator, responder) = handshake_pair().await;
        assert_eq!(
            pairing_code(&initiator.handshake_hash),
            pairing_code(&responder.handshake_hash)
        );
    }

    #[test]
    fn pairing_code_is_six_digits_and_deterministic() {
        let hash: [u8; 32] = {
            let mut hash = [0u8; 32];
            hash[..5].copy_from_slice(&[0x12u8, 0x34, 0x56, 0x78, 0x9a]);
            hash
        };
        let code = pairing_code(&hash);
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
        assert_eq!(pairing_code(&hash), code);
        // 0x12345678 mod 1,000,000 = 419896.
        assert_eq!(code, "419896");
    }

    #[tokio::test]
    async fn transport_carries_frames_both_ways() {
        let (mut session_a, mut session_b) = {
            let (i, r) = handshake_pair().await;
            (i.session, r.session)
        };

        // Small frame tablet -> host.
        let header = FrameHeader {
            channel: Channel::Control,
            flags: 0,
            message_type: 1,
            payload_len: 5,
            pts_us: 42,
        };
        session_a.write_frame(header, b"hello").await.unwrap();
        let mut payload = [0u8; 5];
        let (parsed, len) = session_b.read_frame(&mut payload).await.unwrap();
        assert_eq!(parsed, header);
        assert_eq!(len, 5);
        assert_eq!(&payload, b"hello");

        // Multi-record frame host -> tablet: 150 KiB forces chunking.
        // The writer is spawned: records must flow while the reader
        // consumes them, or the duplex buffer fills and deadlocks.
        let big = vec![0xa5u8; 150 * 1024];
        let expected_len = big.len();
        let header = FrameHeader {
            channel: Channel::Input,
            flags: 0,
            message_type: 7,
            payload_len: big.len() as u32,
            pts_us: 7,
        };
        let writer = tokio::spawn(async move {
            session_b.write_frame(header, &big).await.unwrap();
        });
        let mut payload = vec![0u8; expected_len];
        let (parsed, len) = session_a.read_frame(&mut payload).await.unwrap();
        writer.await.unwrap();
        assert_eq!(parsed, header);
        assert_eq!(len, expected_len);
        assert_eq!(payload.len(), expected_len);
        assert!(payload.iter().all(|&b| b == 0xa5));
    }

    #[tokio::test]
    async fn corrupted_ciphertext_is_rejected() {
        // The responder's XX handshake consumes exactly 100 bytes
        // (msg1 34, msg3 66: the static key and payload carry separate
        // tags). The first transport record follows at 100: two length
        // bytes, then ciphertext; byte 110 lands inside the ciphertext
        // and must break the tag. Flipping the length bytes instead
        // would hang the peer on an oversized read.
        const CORRUPT_AT: usize = 110;
        let (initiator_priv, _pub) = generate_keypair().unwrap();
        let (responder_priv, _) = generate_keypair().unwrap();
        let (a, b) = duplex(4096);
        let (mut session_a, mut session_b) = tokio::join!(
            async {
                SessionHandshake::initiator(key(&initiator_priv))
                    .unwrap()
                    .complete(CorruptAtByte::new(a, usize::MAX), &[])
                    .await
                    .unwrap()
                    .session
            },
            async {
                SessionHandshake::responder(key(&responder_priv))
                    .unwrap()
                    .complete(CorruptAtByte::new(b, CORRUPT_AT), &[])
                    .await
                    .unwrap()
                    .session
            }
        );

        session_a
            .write_frame(
                FrameHeader {
                    channel: Channel::Control,
                    flags: 0,
                    message_type: 1,
                    payload_len: 4,
                    pts_us: 0,
                },
                b"data",
            )
            .await
            .unwrap();
        let mut payload = [0u8; 4];
        assert!(
            session_b.read_frame(&mut payload).await.is_err(),
            "a bit flip anywhere in the stream must break the tag"
        );
    }

    /// Pass-through stream that flips the byte at `target` (absolute
    /// read offset) exactly once; `usize::MAX` never flips. Lets the
    /// tests place a bit error in mid-stream.
    struct CorruptAtByte<S> {
        inner: S,
        target: usize,
        seen: usize,
        flipped: bool,
    }

    impl<S> CorruptAtByte<S> {
        fn new(inner: S, target: usize) -> Self {
            Self {
                inner,
                target,
                seen: 0,
                flipped: false,
            }
        }
    }

    impl<S: AsyncRead + Unpin> AsyncRead for CorruptAtByte<S> {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            let this = self.get_mut();
            let before = buf.filled().len();
            let result = std::pin::Pin::new(&mut this.inner).poll_read(cx, buf);
            if let std::task::Poll::Ready(Ok(())) = &result {
                let filled = buf.filled_mut();
                let start = this.seen;
                let end = start + (filled.len() - before);
                if !this.flipped && this.target >= start && this.target < end {
                    filled[before + (this.target - start)] ^= 0xff;
                    this.flipped = true;
                }
                this.seen = end;
            }
            result
        }
    }

    impl<S: AsyncWrite + Unpin> AsyncWrite for CorruptAtByte<S> {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<io::Result<usize>> {
            std::pin::Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::pin::Pin::new(&mut self.get_mut().inner).poll_flush(cx)
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::pin::Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
        }
    }
}
