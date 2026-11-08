// SPDX-License-Identifier: GPL-3.0-or-later

//! Session stats and the latency-overlay data path (docs/ARCHITECTURE.md,
//! latency rule 8: "Instrumentation is not optional"). M0 carries the
//! plumbing: counters, the tablet's reports and percentile math. The
//! per-stage stamps arrive with the media pipeline in M1.

use slateway_proto::control::Stats;

/// The tablet's latest report (docs/PROTOCOL.md, "Control messages").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct TabletStats {
    /// Decode duration of the last frame, microseconds.
    pub decode_us: u64,
    /// Frames queued for decode.
    pub queue_depth: u32,
}

impl From<Stats> for TabletStats {
    fn from(stats: Stats) -> Self {
        TabletStats {
            decode_us: stats.decode_us,
            queue_depth: stats.queue_depth,
        }
    }
}

/// Session aggregates for the overlay and the CLI.
#[derive(Debug, Default)]
pub struct SessionStats {
    /// Frames received from the tablet.
    pub frames_in: u64,
    /// Frames sent to the tablet.
    pub frames_out: u64,
    /// Payload bytes received from the tablet.
    pub bytes_in: u64,
    /// Payload bytes sent to the tablet.
    pub bytes_out: u64,
    /// The tablet's latest report, once it sent one.
    pub tablet: Option<TabletStats>,
}

impl SessionStats {
    /// Counts one received frame with its payload length.
    pub fn record_in(&mut self, payload_len: usize) {
        self.frames_in += 1;
        self.bytes_in += payload_len as u64;
    }

    /// Counts one sent frame with its payload length.
    pub fn record_out(&mut self, payload_len: usize) {
        self.frames_out += 1;
        self.bytes_out += payload_len as u64;
    }

    /// Replaces the tablet's report.
    pub fn record_tablet(&mut self, stats: Stats) {
        self.tablet = Some(stats.into());
    }
}

/// Nearest-rank percentile of a sample list, for the overlay's p50 and
/// p95 (`docs/TESTING.md`: "The overlay shows p50 and p95"). Empty
/// input yields none.
pub fn percentile(samples: &[u64], p: f64) -> Option<u64> {
    if samples.is_empty() {
        return None;
    }
    debug_assert!((0.0..=100.0).contains(&p), "percentile out of range");
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    let rank = rank.clamp(1, sorted.len());
    Some(sorted[rank - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_track_frames_and_bytes() {
        let mut stats = SessionStats::default();
        stats.record_in(100);
        stats.record_in(50);
        stats.record_out(10);
        stats.record_tablet(Stats {
            decode_us: 900,
            queue_depth: 1,
        });
        assert_eq!(stats.frames_in, 2);
        assert_eq!(stats.bytes_in, 150);
        assert_eq!(stats.frames_out, 1);
        assert_eq!(stats.bytes_out, 10);
        assert_eq!(
            stats.tablet,
            Some(TabletStats {
                decode_us: 900,
                queue_depth: 1
            })
        );
    }

    #[test]
    fn percentiles_match_nearest_rank() {
        let samples = [10u64, 20, 30, 40, 50, 60, 70, 80, 90, 100];
        assert_eq!(percentile(&samples, 50.0), Some(50));
        assert_eq!(percentile(&samples, 95.0), Some(100));
        assert_eq!(percentile(&samples, 0.0), Some(10));
        assert_eq!(percentile(&samples, 100.0), Some(100));
        // Unsorted input is fine.
        let unsorted = [100u64, 10, 50];
        assert_eq!(percentile(&unsorted, 50.0), Some(50));
        assert_eq!(percentile(&[], 50.0), None);
    }
}
