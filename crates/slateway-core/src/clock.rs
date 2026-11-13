// SPDX-License-Identifier: GPL-3.0-or-later

//! Clock sync (docs/PROTOCOL.md, "Clocks"): four-timestamp Ping/Pong
//! with the minimum-RTT sample as the offset estimate. All timestamps
//! are monotonic microseconds in each side's own timebase; the offset
//! absorbs the epoch difference (latency rule 9: monotonic only).

use std::sync::OnceLock;
use std::time::Instant;

use prost::Message;
use slateway_proto::Channel;
use slateway_proto::control::{PING_TYPE, PONG_TYPE, Ping, Pong};
use slateway_proto::frame::FrameHeader;

use crate::crypto::{SecureSession, SessionError};

/// How many recent samples the estimate draws from. One per second
/// means roughly half a minute of history.
pub const SAMPLE_WINDOW: usize = 32;

/// One completed exchange: RTT and clock offset in microseconds.
/// `offset_us` is tablet-clock minus host-clock and may be negative.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    /// Round-trip time estimate.
    pub rtt_us: i64,
    /// Tablet clock minus host clock estimate.
    pub offset_us: i64,
}

/// Clock-sync state: recent samples, estimate from the minimum-RTT
/// one (docs/PROTOCOL.md, "Clocks").
#[derive(Debug, Default)]
pub struct ClockSync {
    samples: Vec<Sample>,
}

impl ClockSync {
    /// Adds the sample of one completed exchange and returns it.
    /// Returns `None` for a physically impossible exchange - timestamps
    /// that overflow the arithmetic, the tablet replying before it
    /// received (`t2 < t1`), or a negative round trip - and does not
    /// store it, so nonsense samples cannot poison the minimum-RTT
    /// estimate. A truthful peer can never produce these; the values
    /// arrive on the wire from an authenticated but not necessarily
    /// sane peer.
    pub fn on_pong(&mut self, t0: u64, t1: u64, t2: u64, t3: u64) -> Option<Sample> {
        let (t0, t1, t2, t3) = (t0 as i64, t1 as i64, t2 as i64, t3 as i64);
        // Tablet-side processing time: send after receive.
        let tablet_time = t2.checked_sub(t1)?;
        if tablet_time < 0 {
            return None;
        }
        // RTT = (t3 - t0) - (t2 - t1); a real round trip is >= 0.
        let rtt = t3.checked_sub(t0)?.checked_sub(tablet_time)?;
        if rtt < 0 {
            return None;
        }
        // Offset = ((t1 - t0) + (t2 - t3)) / 2.
        let offset = t1
            .checked_sub(t0)?
            .checked_add(t2.checked_sub(t3)?)?
            .checked_div(2)?;
        let sample = Sample {
            rtt_us: rtt,
            offset_us: offset,
        };
        self.samples.push(sample);
        if self.samples.len() > SAMPLE_WINDOW {
            self.samples.remove(0);
        }
        Some(sample)
    }

    /// The estimate from the minimum-RTT sample in the window; none
    /// before the first exchange.
    pub fn estimate(&self) -> Option<Sample> {
        self.samples
            .iter()
            .copied()
            .min_by_key(|sample| sample.rtt_us)
    }

    /// The recent samples, oldest first.
    pub fn samples(&self) -> &[Sample] {
        &self.samples
    }
}

/// Microseconds on this process's monotonic clock; only differences
/// are meaningful (latency rule 9: monotonic clocks only).
pub fn monotonic_us() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_micros() as u64
}

/// Runs one Ping/Pong exchange over `channel` and records the sample.
/// The next frame read must be the Pong; a session loop that
/// interleaves other frames demultiplexes itself and calls
/// [`ClockSync::on_pong`] directly. `now_us` stamps this side's
/// monotonic clock.
pub async fn sync_once<S>(
    channel: &mut SecureSession<S>,
    sync: &mut ClockSync,
    now_us: impl Fn() -> u64,
) -> Result<Sample, SessionError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let t0 = now_us();
    let ping = Ping { t0 };
    let ping_bytes = ping.encode_to_vec();
    channel
        .write_frame(
            FrameHeader {
                channel: Channel::Control,
                flags: 0,
                message_type: PING_TYPE,
                payload_len: ping_bytes.len() as u32,
                pts_us: 0,
            },
            &ping_bytes,
        )
        .await?;
    // Control payloads are tiny; a fixed scratch avoids per-exchange
    // allocation.
    let mut scratch = [0u8; 4096];
    let (header, payload_len) = channel.read_frame(&mut scratch).await?;
    if header.channel != Channel::Control || header.message_type != PONG_TYPE {
        return Err(SessionError::Protocol("expected a Pong frame"));
    }
    let t3 = now_us();
    let pong = Pong::decode(&scratch[..payload_len])?;
    if pong.t0 != t0 {
        return Err(SessionError::Protocol("Pong t0 does not match the Ping"));
    }
    sync.on_pong(pong.t0, pong.t1, pong.t2, t3)
        .ok_or(SessionError::Protocol(
            "Pong timestamps are physically impossible",
        ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use slateway_proto::control::Stats;

    fn key(private: &[u8]) -> &[u8; crate::crypto::KEY_LEN] {
        private.try_into().unwrap()
    }

    #[test]
    fn perfect_clock_yields_zero_offset() {
        let mut sync = ClockSync::default();
        // Host sends at 1000, tablet receives at 1500, replies at 1600,
        // host receives at 2100: 500 us each way.
        let sample = sync.on_pong(1000, 1500, 1600, 2100).unwrap();
        assert_eq!(sample.rtt_us, 1000);
        assert_eq!(sample.offset_us, 0);
    }

    #[test]
    fn tablet_ahead_shows_positive_offset() {
        let mut sync = ClockSync::default();
        // The tablet's clock runs 100 us ahead: t1 and t2 are both
        // shifted up.
        let sample = sync.on_pong(1000, 1600, 1700, 2100).unwrap();
        assert_eq!(sample.rtt_us, 1000);
        assert_eq!(sample.offset_us, 100);
    }

    #[test]
    fn tablet_behind_shows_negative_offset() {
        let mut sync = ClockSync::default();
        let sample = sync.on_pong(1000, 1400, 1500, 2100).unwrap();
        assert_eq!(sample.rtt_us, 1000);
        assert_eq!(sample.offset_us, -100);
    }

    #[test]
    fn estimate_uses_the_minimum_rtt_sample() {
        let mut sync = ClockSync::default();
        let _ = sync.on_pong(0, 500, 600, 3000); // rtt 2500, offset -550
        let best = sync.on_pong(0, 500, 600, 1000).unwrap(); // rtt 500, offset 50
        let _ = sync.on_pong(0, 500, 600, 2000); // rtt 1500, offset -250
        assert_eq!(sync.estimate(), Some(best));
        assert_eq!(sync.samples().len(), 3);
    }

    #[test]
    fn sample_window_is_bounded() {
        let mut sync = ClockSync::default();
        for i in 0..(SAMPLE_WINDOW as u64 + 10) {
            let _ = sync.on_pong(i * 1000, i * 1000 + 400, i * 1000 + 500, i * 1000 + 1000);
        }
        assert_eq!(sync.samples().len(), SAMPLE_WINDOW);
    }

    #[test]
    fn estimate_is_none_before_the_first_exchange() {
        assert_eq!(ClockSync::default().estimate(), None);
    }

    /// Physically impossible exchanges are rejected, never stored, and
    /// must not panic on extreme timestamps (checked arithmetic): the
    /// values arrive on the wire from an authenticated but not
    /// necessarily sane peer.
    #[test]
    fn impossible_samples_are_rejected() {
        let mut sync = ClockSync::default();
        // The tablet cannot reply before it received (t2 < t1).
        assert_eq!(sync.on_pong(1000, 1500, 1400, 2100), None);
        // A round trip cannot be negative.
        assert_eq!(sync.on_pong(1000, 1500, 4000, 2100), None);
        // Extreme wire values must not panic the arithmetic (checked
        // ops reject instead), and a negative round trip at the far
        // end of the range is still caught.
        assert_eq!(sync.on_pong(u64::MAX, 0, u64::MAX, 0), None);
        assert_eq!(sync.on_pong(0, u64::MAX, u64::MAX, u64::MAX), None);
        assert_eq!(
            sync.on_pong(u64::MAX, u64::MAX - 5, u64::MAX, u64::MAX - 5),
            None
        );
        assert_eq!(
            sync.estimate(),
            None,
            "rejected samples must not enter the window"
        );
        // A valid sample after the nonsense ones still works.
        let sample = sync.on_pong(1000, 1500, 1600, 2100);
        assert!(sample.is_some());
        assert_eq!(sync.estimate(), sample);
    }

    /// Suppresses the unused-import lint for Stats in this module's
    /// test scope; Stats fields are exercised by the proto golden tests.
    #[test]
    fn stats_message_round_trips() {
        let stats = Stats {
            decode_us: 1234,
            queue_depth: 2,
        };
        let bytes = stats.encode_to_vec();
        assert_eq!(Stats::decode(bytes.as_slice()).unwrap(), stats);
    }

    /// A full Ping/Pong exchange over the encrypted session: both sides
    /// share this process's monotonic clock, so the offset must land
    /// near zero while the RTT is a real, non-negative measurement.
    #[tokio::test]
    async fn sync_over_encrypted_session_measures_offset_near_zero() {
        use crate::crypto::{SessionHandshake, generate_keypair};
        use tokio::io::duplex;

        let start = std::time::Instant::now();
        let now_us = move || start.elapsed().as_micros() as u64;

        let (initiator_priv, _) = generate_keypair().unwrap();
        let (responder_priv, _) = generate_keypair().unwrap();
        let (a, b) = duplex(4096);
        let (mut host, _tablet) = tokio::join!(
            async {
                SessionHandshake::initiator(key(&initiator_priv))
                    .unwrap()
                    .complete(a, &[])
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

        // The tablet side answers every Ping; this mirrors what the
        // service does on device.
        let mut tablet = _tablet.session;
        tokio::spawn(async move {
            let mut scratch = [0u8; 4096];
            loop {
                let (header, payload_len) = match tablet.read_frame(&mut scratch).await {
                    Ok(frame) => frame,
                    Err(_) => return,
                };
                if header.channel == Channel::Control && header.message_type == PING_TYPE {
                    let ping = match Ping::decode(&scratch[..payload_len]) {
                        Ok(ping) => ping,
                        Err(_) => return,
                    };
                    let now = now_us();
                    let pong = Pong {
                        t0: ping.t0,
                        t1: now,
                        t2: now,
                    };
                    let pong_bytes = pong.encode_to_vec();
                    if tablet
                        .write_frame(
                            FrameHeader {
                                channel: Channel::Control,
                                flags: 0,
                                message_type: PONG_TYPE,
                                payload_len: pong_bytes.len() as u32,
                                pts_us: 0,
                            },
                            &pong_bytes,
                        )
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
        });

        let mut sync = ClockSync::default();
        let sample = sync_once(&mut host.session, &mut sync, now_us)
            .await
            .unwrap();
        assert!(sample.rtt_us >= 0);
        assert!(
            sample.offset_us.abs() < 5_000,
            "same-process clocks must agree closely, got {}",
            sample.offset_us
        );
        assert_eq!(sync.estimate(), Some(sample));
    }
}
