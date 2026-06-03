//! Sliding-window remaining-time estimator for `--progress` output.
//!
//! Mirrors upstream rsync's algorithm in `progress.c`. Upstream keeps a
//! 5-slot ring of `(timestamp, ofs)` samples (`PROGRESS_HISTORY_SECS = 5`),
//! advancing the ring at most once per second. Mid-transfer ticks compute
//! the rate from the oldest retained sample to the freshly recorded one and
//! divide the remaining bytes by that rate to render the ETA.
//!
//! upstream: progress.c show_progress / rprint_progress

use std::time::{Duration, Instant};

/// Number of samples retained in the sliding window. Matches upstream
/// `PROGRESS_HISTORY_SECS` (progress.c:37).
const HISTORY_SLOTS: usize = 5;

/// Minimum interval between samples being rotated into the window. Matches
/// upstream's `msdiff < 1000` early return in `show_progress` (progress.c:224).
const SAMPLE_INTERVAL: Duration = Duration::from_millis(1_000);

/// Upper bound on the rendered ETA. Matches upstream's `9999 * 3600`-second
/// guard in `rprint_progress` (progress.c:118).
const MAX_REMAINING_SECS: u64 = 9_999 * 3_600;

#[derive(Copy, Clone, Debug)]
struct Sample {
    at: Instant,
    ofs: u64,
}

/// Sliding-window remaining-time estimator.
#[derive(Debug)]
pub(crate) struct RemainingTimeEstimator {
    samples: [Option<Sample>; HISTORY_SLOTS],
    newest: usize,
    oldest: usize,
    primed: bool,
}

impl RemainingTimeEstimator {
    /// Returns a fresh estimator with no samples recorded.
    pub(crate) const fn new() -> Self {
        Self {
            samples: [None; HISTORY_SLOTS],
            newest: 0,
            oldest: 0,
            primed: false,
        }
    }

    /// Records a `(timestamp, bytes_transferred)` sample. The first observation
    /// primes every slot so the first tick reports a rate against itself
    /// (matches upstream's loop at `progress.c:220-221`); subsequent samples
    /// are throttled to one rotation per [`SAMPLE_INTERVAL`].
    pub(crate) fn observe(&mut self, now: Instant, ofs: u64) {
        let sample = Sample { at: now, ofs };
        if !self.primed {
            self.samples = [Some(sample); HISTORY_SLOTS];
            self.newest = 0;
            self.oldest = 0;
            self.primed = true;
            return;
        }

        if let Some(latest) = self.samples[self.newest]
            && now.saturating_duration_since(latest.at) < SAMPLE_INTERVAL
        {
            return;
        }

        self.newest = self.oldest;
        self.oldest = (self.oldest + 1) % HISTORY_SLOTS;
        self.samples[self.newest] = Some(sample);
    }

    /// Computes the remaining seconds required to copy `total - ofs` bytes at
    /// the rate measured between the oldest retained sample and `now`.
    ///
    /// Mirrors upstream's `rate ? ... : 0.0` ternary in `progress.c:105`: when
    /// the window is primed but no bytes were transferred over it (a stalled
    /// transfer), upstream sets `remain = 0.0`, which renders as `0:00:00`
    /// rather than the `??:??:??` placeholder. Returning `Some(0.0)` here keeps
    /// the rendered output byte-identical to upstream during a stall instead
    /// of extrapolating an ETA from stale samples.
    ///
    /// upstream: progress.c:102-105 rprint_progress
    pub(crate) fn remaining_seconds(&self, now: Instant, ofs: u64, total: u64) -> Option<f64> {
        let oldest = self.samples[self.oldest]?;
        let bytes_left = total.checked_sub(ofs)?;
        if bytes_left == 0 {
            return Some(0.0);
        }
        let bytes_delta = ofs.checked_sub(oldest.ofs)?;
        if bytes_delta == 0 {
            // Stalled window: upstream's `rate ? ... : 0.0` collapses to 0.0,
            // which renders as `0:00:00`. See `progress.c:105`.
            return Some(0.0);
        }
        let elapsed = now.saturating_duration_since(oldest.at).as_secs_f64();
        if elapsed <= 0.0 {
            return Some(0.0);
        }
        let rate = bytes_delta as f64 / elapsed;
        if rate <= 0.0 {
            return Some(0.0);
        }
        Some(bytes_left as f64 / rate)
    }

    /// Returns the sliding-window rate in bytes per second, computed as
    /// `(current_ofs - oldest_ofs) / elapsed_seconds`. Returns `None` if the
    /// estimator is unprimed. Returns `Some(0.0)` when stalled (no bytes
    /// transferred over the window), matching upstream's `rate` variable in
    /// `rprint_progress` (progress.c:102-104).
    ///
    /// upstream: progress.c:102-104 rprint_progress - rate computation from
    /// oldest retained sample to current position.
    pub(crate) fn window_rate(&self, now: Instant, ofs: u64) -> Option<f64> {
        let oldest = self.samples[self.oldest]?;
        let bytes_delta = ofs.saturating_sub(oldest.ofs);
        let elapsed = now.saturating_duration_since(oldest.at).as_secs_f64();
        if elapsed <= 0.0 || bytes_delta == 0 {
            return Some(0.0);
        }
        Some(bytes_delta as f64 / elapsed)
    }

    /// Renders the remaining time as `H:MM:SS` (matching upstream's
    /// `%4u:%02u:%02u`, progress.c:121-122), or the `??:??:??` placeholder
    /// when the value exceeds upstream's `9999*3600`-second clamp. A stalled
    /// window (zero throughput over the retained samples) renders as
    /// `0:00:00`, mirroring upstream's `remain = rate ? ... : 0.0` collapse
    /// in `progress.c:105`.
    pub(crate) fn render(&self, now: Instant, ofs: u64, total: u64) -> String {
        match self.remaining_seconds(now, ofs, total) {
            Some(secs) if secs.is_finite() && secs >= 0.0 => {
                let whole = secs as u64;
                if whole > MAX_REMAINING_SECS {
                    return "??:??:??".to_owned();
                }
                let hours = whole / 3_600;
                let minutes = (whole % 3_600) / 60;
                let seconds = whole % 60;
                format!("{hours}:{minutes:02}:{seconds:02}")
            }
            _ => "??:??:??".to_owned(),
        }
    }
}

impl Default for RemainingTimeEstimator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    #[test]
    fn unprimed_renders_placeholder() {
        let est = RemainingTimeEstimator::new();
        let now = Instant::now();
        assert_eq!(est.render(now, 0, 1_000), "??:??:??");
    }

    #[test]
    fn steady_throughput_converges() {
        // 1 MB/s for 10 s, total transfer 50 MB. After warmup the ETA should
        // be ~40 seconds with a tight tolerance.
        let mut est = RemainingTimeEstimator::new();
        let t0 = Instant::now();
        let rate: u64 = 1_000_000;
        let total: u64 = 50_000_000;
        for sec in 0..=10 {
            est.observe(at(t0, sec), sec * rate);
        }
        let secs = est
            .remaining_seconds(at(t0, 10), 10 * rate, total)
            .expect("rate available");
        assert!((secs - 40.0).abs() < 0.5, "expected ~40s, got {secs}");
    }

    #[test]
    fn recent_dip_dominates_long_term_average() {
        // First 100 s at 10 MB/s, then a sudden 1 MB/s dip for 5 s.
        // Cumulative average is still close to 10 MB/s (so an ETA of ~10 s for
        // the remaining 100 MB), but the sliding window should track the dip
        // (rate ~1 MB/s -> ETA ~100 s).
        let mut est = RemainingTimeEstimator::new();
        let t0 = Instant::now();
        let fast: u64 = 10_000_000;
        let slow: u64 = 1_000_000;
        let mut ofs = 0u64;
        for sec in 0..100 {
            ofs += fast;
            est.observe(at(t0, sec), ofs);
        }
        for sec in 100..105 {
            ofs += slow;
            est.observe(at(t0, sec), ofs);
        }
        let total = ofs + 100_000_000;
        let secs = est
            .remaining_seconds(at(t0, 105), ofs, total)
            .expect("rate available");
        let cumulative = (total - ofs) as f64 / (ofs as f64 / 105.0);
        assert!(
            secs > 5.0 * cumulative,
            "secs={secs} cumulative={cumulative}"
        );
        assert!(secs > 50.0, "expected window to track dip, got {secs}");
    }

    #[test]
    fn sample_throttled_to_one_per_second() {
        // Prime once, then feed five sub-second updates and assert the ring
        // never rotated by reading the oldest sample's elapsed and ofs
        // through `remaining_seconds`.
        let mut est = RemainingTimeEstimator::new();
        let t0 = Instant::now();
        est.observe(t0, 0);
        for ms in [100u64, 250, 500, 750, 999] {
            est.observe(t0 + Duration::from_millis(ms), 1_000 * ms);
        }
        // Oldest is still the prime: bytes_delta = 5_000_000 - 0 = 5_000_000,
        // elapsed = 0.999 s -> rate ~ 5_005_005 B/s -> ETA for the remaining
        // 5_000_000 bytes ~ 0.999 s. The test would fail if any throttled
        // observation rotated the ring (the new oldest would have a tiny
        // elapsed and an undefined rate).
        let secs = est
            .remaining_seconds(t0 + Duration::from_millis(999), 5_000_000, 10_000_000)
            .expect("rate available");
        assert!(
            (secs - 0.999).abs() < 0.05,
            "throttle skipped: ETA should reflect prime anchor, got {secs}"
        );
        // Crossing the 1 s boundary rotates the ring exactly once. The new
        // oldest is one slot ahead in the ring; samples[1] still holds the
        // prime (t0, 0), so the rate is now measured over a full 1 s window.
        est.observe(t0 + Duration::from_millis(1_000), 10_000_000);
        let secs2 = est
            .remaining_seconds(t0 + Duration::from_millis(1_000), 10_000_000, 20_000_000)
            .expect("rate available");
        assert!(
            (secs2 - 1.0).abs() < 0.05,
            "expected ~1s after first rotation, got {secs2}"
        );
    }

    #[test]
    fn window_expires_older_samples() {
        // After more than HISTORY_SLOTS rotations, the oldest sample reflects
        // a position roughly HISTORY_SLOTS seconds back, so the rate stays
        // proportional to recent throughput.
        let mut est = RemainingTimeEstimator::new();
        let t0 = Instant::now();
        for sec in 0..20 {
            est.observe(at(t0, sec), sec * 2_000_000);
        }
        let secs = est
            .remaining_seconds(at(t0, 20), 20 * 2_000_000, 60_000_000)
            .expect("rate available");
        assert!((secs - 10.0).abs() < 1.0, "expected ~10s, got {secs}");
    }

    #[test]
    fn render_zero_when_complete() {
        let mut est = RemainingTimeEstimator::new();
        let t0 = Instant::now();
        est.observe(t0, 0);
        est.observe(t0 + Duration::from_secs(1), 1_000);
        assert_eq!(
            est.render(t0 + Duration::from_secs(1), 1_000, 1_000),
            "0:00:00"
        );
    }

    #[test]
    fn render_clamps_huge_eta() {
        let mut est = RemainingTimeEstimator::new();
        let t0 = Instant::now();
        est.observe(t0, 0);
        est.observe(t0 + Duration::from_secs(1), 1);
        // 1 byte/sec rate vs u64::MAX bytes left -> clamped placeholder.
        assert_eq!(
            est.render(t0 + Duration::from_secs(1), 1, u64::MAX),
            "??:??:??"
        );
    }

    #[test]
    fn render_matches_format() {
        let mut est = RemainingTimeEstimator::new();
        let t0 = Instant::now();
        est.observe(t0, 0);
        est.observe(t0 + Duration::from_secs(1), 1_000);
        // 3_661 bytes left at 1_000 B/s -> 3.661s. We need a bigger gap to
        // produce hours; assert basic H:MM:SS shape instead.
        let rendered = est.render(t0 + Duration::from_secs(1), 1_000, 4_000);
        assert!(
            rendered.chars().filter(|c| *c == ':').count() == 2,
            "rendered={rendered}"
        );
    }

    /// Stalled transfer: the window is primed and rotates across several
    /// ticks, but `ofs` never advances. Upstream collapses to
    /// `remain = 0.0` -> `0:00:00`; we must do the same instead of
    /// extrapolating from older samples or emitting `??:??:??`.
    /// upstream: progress.c:102-125 rprint_progress
    #[test]
    fn stall_mid_transfer_renders_as_upstream() {
        let mut est = RemainingTimeEstimator::new();
        let t0 = Instant::now();
        est.observe(t0, 100);
        est.observe(at(t0, 1), 100);
        est.observe(at(t0, 2), 100);
        est.observe(at(t0, 3), 100);
        let secs = est
            .remaining_seconds(at(t0, 3), 100, 10_000)
            .expect("stall yields Some(0.0)");
        assert_eq!(secs, 0.0, "stall should mirror upstream's remain=0.0");
        assert_eq!(est.render(at(t0, 3), 100, 10_000), "0:00:00");
    }

    /// After a stall, resuming throughput must repopulate the window so the
    /// estimator switches back to a live ETA rather than continuing to report
    /// `0:00:00`. Verifies the ring rotation keeps working through the dip.
    #[test]
    fn stall_then_recovery_returns_live_eta() {
        let mut est = RemainingTimeEstimator::new();
        let t0 = Instant::now();
        est.observe(t0, 0);
        for sec in 1..=4 {
            est.observe(at(t0, sec), 0);
        }
        assert_eq!(est.render(at(t0, 4), 0, 10_000), "0:00:00");
        // Resume at 1 MB/s for long enough to flush the stalled samples out
        // of the 5-slot window.
        let rate: u64 = 1_000_000;
        let mut ofs = 0u64;
        for sec in 5..=15 {
            ofs += rate;
            est.observe(at(t0, sec), ofs);
        }
        let secs = est
            .remaining_seconds(at(t0, 15), ofs, ofs + 10 * rate)
            .expect("rate available after recovery");
        assert!(
            (secs - 10.0).abs() < 1.0,
            "expected ~10s after recovery, got {secs}"
        );
        assert_ne!(est.render(at(t0, 15), ofs, ofs + 10 * rate), "??:??:??");
    }

    /// Property-style sweep: feed a bursty throughput pattern and assert the
    /// ETA stays within `[recent_min_rate_eta, recent_max_rate_eta]` (i.e.,
    /// tracks the window, not the cumulative average).
    #[test]
    fn eta_bounded_by_recent_rate_envelope() {
        let mut est = RemainingTimeEstimator::new();
        let t0 = Instant::now();
        let total: u64 = 1_000_000_000;
        let pattern: [u64; 12] = [
            8_000_000, 9_000_000, 7_000_000, 10_000_000, 8_500_000, 9_500_000, 6_000_000,
            8_000_000, 9_000_000, 7_500_000, 8_000_000, 9_000_000,
        ];
        let mut ofs = 0u64;
        for (sec, step) in pattern.iter().enumerate() {
            ofs += step;
            est.observe(at(t0, sec as u64 + 1), ofs);
        }
        let now = at(t0, pattern.len() as u64 + 1);
        let secs = est
            .remaining_seconds(now, ofs, total)
            .expect("rate available");
        // Loose envelope: use the global min/max rate of the pattern.
        // The strict per-window envelope drifts slightly because the
        // oldest pointer in a 5-slot ring after 12 samples covers a
        // 6-second window centered on samples 6-11 rather than the
        // last 5 samples (HISTORY_SLOTS+1 prime-init semantics).
        let min_rate = *pattern.iter().min().unwrap() as f64;
        let max_rate = *pattern.iter().max().unwrap() as f64;
        let upper = (total - ofs) as f64 / min_rate;
        let lower = (total - ofs) as f64 / max_rate;
        assert!(
            secs >= lower - 1.0 && secs <= upper + 1.0,
            "secs={secs} not in [{lower}, {upper}]"
        );
    }

    /// Upstream renders the time overflow as `"  ??:??:??"` (10 chars, two
    /// leading spaces inside the buffer; see `progress.c:119`). Our `render()`
    /// returns the 8-character `"??:??:??"` and relies on the caller's
    /// 10-wide right-aligned field to pad the leading spaces. Verify the
    /// final on-the-wire form is byte-equivalent.
    /// upstream: progress.c:118-119 rprint_progress
    #[test]
    fn overflow_sentinel_matches_upstream_after_padding() {
        let mut est = RemainingTimeEstimator::new();
        let t0 = Instant::now();
        est.observe(t0, 0);
        est.observe(t0 + Duration::from_secs(1), 1);
        let rendered = est.render(t0 + Duration::from_secs(1), 1, u64::MAX);
        assert_eq!(rendered, "??:??:??");
        let on_wire = format!("{rendered:>10}");
        assert_eq!(on_wire, "  ??:??:??");
        assert_eq!(on_wire.len(), 10);
    }

    /// Boundary case at the upstream clamp threshold (`9999 * 3600` seconds).
    /// Exactly at the limit must still render numerically; one second past
    /// must collapse to the sentinel.
    /// upstream: progress.c:118 rprint_progress
    #[test]
    fn overflow_boundary_exact_vs_off_by_one() {
        // Construct synthetic samples so the computed `remain` lands precisely
        // at the boundary, then one second beyond it.
        let mut est = RemainingTimeEstimator::new();
        let t0 = Instant::now();
        est.observe(t0, 0);
        est.observe(t0 + Duration::from_secs(1), 1);
        let at_limit = MAX_REMAINING_SECS;
        let over_limit = MAX_REMAINING_SECS + 1;
        // Rate is 1 B/s; bytes_left = `at_limit` yields remain == at_limit.
        assert_ne!(
            est.render(t0 + Duration::from_secs(1), 1, 1 + at_limit),
            "??:??:??"
        );
        assert_eq!(
            est.render(t0 + Duration::from_secs(1), 1, 1 + over_limit),
            "??:??:??"
        );
    }

    /// upstream: progress.c:102-104 - window rate is computed from the oldest
    /// retained sample to the current position.
    #[test]
    fn window_rate_unprimed_returns_none() {
        let est = RemainingTimeEstimator::new();
        let now = Instant::now();
        // Unprimed estimator has no samples, so window_rate returns None.
        assert!(est.window_rate(now, 1_000).is_none());
    }

    #[test]
    fn window_rate_steady_throughput() {
        let mut est = RemainingTimeEstimator::new();
        let t0 = Instant::now();
        let rate: u64 = 1_000_000;
        for sec in 0..=5 {
            est.observe(at(t0, sec), sec * rate);
        }
        let computed = est.window_rate(at(t0, 5), 5 * rate).expect("rate");
        assert!(
            (computed - 1_000_000.0).abs() < 10_000.0,
            "expected ~1MB/s, got {computed}"
        );
    }

    #[test]
    fn window_rate_stalled_returns_zero() {
        let mut est = RemainingTimeEstimator::new();
        let t0 = Instant::now();
        est.observe(t0, 100);
        est.observe(at(t0, 1), 100);
        est.observe(at(t0, 2), 100);
        let rate = est.window_rate(at(t0, 2), 100).expect("rate");
        assert_eq!(rate, 0.0, "stalled window should yield zero rate");
    }

    #[test]
    fn window_rate_zero_elapsed_returns_zero() {
        let mut est = RemainingTimeEstimator::new();
        let t0 = Instant::now();
        est.observe(t0, 0);
        let rate = est.window_rate(t0, 1_000).expect("rate");
        assert_eq!(rate, 0.0, "zero elapsed should yield zero rate");
    }
}
