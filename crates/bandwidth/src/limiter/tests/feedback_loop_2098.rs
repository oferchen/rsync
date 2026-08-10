//! Feedback-loop convergence tests for `--bwlimit` (#2098).
//!
//! These tests drive the [`BandwidthLimiter`] with synthetic byte streams
//! representative of real-world load profiles and assert that the requested
//! sleep budget keeps the observed throughput within a tight tolerance band
//! of the configured target. The limiter pacing algorithm mirrors upstream
//! rsync's `io.c:sleep_for_bwlimit()`; each scenario below targets a
//! specific feedback-loop property of that algorithm.
//
// upstream: io.c sleep_for_bwlimit
use super::{BandwidthLimiter, recorded_sleep_session};
use std::num::NonZeroU64;
use std::time::Duration;

fn nz(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("non-zero rate required")
}

/// Aggregated outcome for a synthetic feed of writes through the limiter.
struct WindowSample {
    bytes: u128,
    sleep: Duration,
}

impl WindowSample {
    fn rate(&self) -> f64 {
        let seconds = self.sleep.as_secs_f64();
        if seconds <= f64::EPSILON {
            return f64::INFINITY;
        }
        self.bytes as f64 / seconds
    }
}

/// Drives the limiter with `chunks` registrations of `chunk_bytes`, returning
/// the accumulated bytes and requested sleep time. Uses the requested sleep
/// rather than wall-clock time so the test is deterministic and fast.
fn drive(limiter: &mut BandwidthLimiter, chunks: usize, chunk_bytes: usize) -> WindowSample {
    let mut bytes: u128 = 0;
    let mut sleep = Duration::ZERO;
    for _ in 0..chunks {
        let s = limiter.register(chunk_bytes);
        bytes = bytes.saturating_add(chunk_bytes as u128);
        sleep = sleep.saturating_add(s.requested());
    }
    WindowSample { bytes, sleep }
}

/// Drives the limiter until at least `min_seconds` of requested sleep have
/// accumulated, returning the full window. The chunk size is sized so that a
/// caller producing at >= 2x the target rate still respects pacing.
fn drive_until(
    limiter: &mut BandwidthLimiter,
    chunk_bytes: usize,
    min_seconds: f64,
) -> WindowSample {
    let mut bytes: u128 = 0;
    let mut sleep = Duration::ZERO;
    while sleep.as_secs_f64() < min_seconds {
        let s = limiter.register(chunk_bytes);
        bytes = bytes.saturating_add(chunk_bytes as u128);
        sleep = sleep.saturating_add(s.requested());
    }
    WindowSample { bytes, sleep }
}

/// Feeding the limiter at >= 2x the target rate must converge throughput to
/// within +-5% of the configured rate across a 1s sliding window.
#[test]
fn steady_state_oversubscribed_converges_to_target_within_one_second_window() {
    let mut session = recorded_sleep_session();
    session.clear();

    let target = 1_000_000_u64; // 1 MB/s
    // Chunk size large enough that every call accumulates >= 100 ms of debt
    // (the limiter's minimum-sleep threshold). At target / 8 each chunk
    // represents 125 ms of transfer time, guaranteeing a sleep is requested
    // on every register call so the feedback loop is exercised on every
    // sample rather than amortised across silent ticks.
    let chunk = (target / 8) as usize;
    let mut limiter = BandwidthLimiter::new(nz(target));

    // Warm up the limiter so the first-call boundary (no previous instant)
    // does not skew the measurement window.
    let _ = drive(&mut limiter, 2, chunk);

    // Measure across a >= 1s sliding window. Driving with chunk = target/16
    // for at least 1s requires at least 16 chunks; the loop continues until
    // the requested sleep crosses the 1s boundary.
    let window = drive_until(&mut limiter, chunk, 1.0);
    let observed = window.rate();
    let deviation = (observed - target as f64).abs() / target as f64 * 100.0;
    assert!(
        deviation <= 5.0,
        "oversubscribed steady-state: observed {observed:.2} B/s deviates {deviation:.3}% from {target} over {:?}",
        window.sleep,
    );
}

/// A single instantaneous burst at 4x the per-second target must be dampened
/// and the limiter must return to the configured rate within ~2 seconds of
/// requested sleep budget. The post-burst window starts only after the
/// limiter has had at least a one-second tail to absorb the burst penalty.
#[test]
fn burst_recovers_to_target_within_two_seconds() {
    let mut session = recorded_sleep_session();
    session.clear();

    let target = 100_000_u64; // 100 KB/s
    let chunk = (target / 8) as usize; // 12.5 ms per chunk under steady state
    let mut limiter = BandwidthLimiter::new(nz(target));

    // Emit a 4x burst in a single call. The limiter clamps debt and returns
    // a single large sleep request rather than oscillating; the requested
    // sleep equals burst_bytes / target.
    let burst_bytes = (target * 4) as usize;
    let burst_sleep = limiter.register(burst_bytes).requested();
    // The burst alone must not push the requested sleep beyond a small
    // multiple of the per-second budget. 4x bytes at the target rate must
    // request ~4 seconds.
    assert!(
        burst_sleep <= Duration::from_secs(5),
        "burst request {burst_sleep:?} should be bounded by burst/rate"
    );

    // Absorb the burst over a short tail of steady-state writes.
    let tail = drive(&mut limiter, 4, chunk);

    // Measure the recovery window: at least 1s of steady-state pacing post
    // burst. The combined burst sleep + tail sleep should be at most ~2s
    // beyond the steady-state expectation for the bytes delivered.
    let recovery = drive_until(&mut limiter, chunk, 1.0);
    let observed = recovery.rate();
    let deviation = (observed - target as f64).abs() / target as f64 * 100.0;
    assert!(
        deviation <= 5.0,
        "post-burst recovery: {observed:.2} B/s deviates {deviation:.3}% from {target}"
    );

    // Time-bound assertion: the burst is fully absorbed within ~2s once the
    // tail has stabilised, i.e. the tail itself does not request more than
    // the bytes-delivered expectation plus a small grace.
    let tail_expected = tail.bytes as f64 / target as f64;
    let tail_grace = 2.0;
    assert!(
        tail.sleep.as_secs_f64() <= tail_expected + tail_grace,
        "tail sleep {:?} exceeds expected {tail_expected:.3}s + {tail_grace:.1}s grace",
        tail.sleep,
    );
}

/// Increasing the target mid-test (0.5 MB/s -> 2 MB/s) must ramp to the new
/// rate without overshoot. The limiter resets debt on `update_limit`, so the
/// post-change window should converge to the new target without first
/// exceeding it. The maximum per-window rate observed across several short
/// windows must remain within +5% of the new target.
#[test]
fn target_change_ramps_without_overshoot() {
    let mut session = recorded_sleep_session();
    session.clear();

    let slow = 500_000_u64; // 0.5 MB/s
    let fast = 2_000_000_u64; // 2 MB/s
    let slow_chunk = (slow / 8) as usize;
    let fast_chunk = (fast / 8) as usize;
    let mut limiter = BandwidthLimiter::new(nz(slow));

    // Stabilise at the slow rate.
    let _ = drive(&mut limiter, 2, slow_chunk);
    let pre = drive(&mut limiter, 8, slow_chunk);
    let pre_rate = pre.rate();
    let pre_dev = (pre_rate - slow as f64).abs() / slow as f64 * 100.0;
    assert!(
        pre_dev <= 5.0,
        "pre-change: {pre_rate:.2} deviates {pre_dev:.3}% from {slow}"
    );

    // Switch to the fast rate; `update_limit` clears accumulated debt so the
    // new rate takes effect on the very next register call.
    limiter.update_limit(nz(fast));

    // Drive at the fast rate and assert no window exceeds the new target by
    // more than 5%. Several short windows expose any overshoot during the
    // ramp; a no-overshoot limiter caps each window at <= target * 1.05.
    let _ = drive(&mut limiter, 2, fast_chunk); // post-change warm-up
    for window_idx in 0..4 {
        let window = drive(&mut limiter, 4, fast_chunk);
        let observed = window.rate();
        assert!(
            observed <= fast as f64 * 1.05,
            "ramp window {window_idx}: observed {observed:.2} exceeds {fast} by more than 5% (overshoot)"
        );
        let deviation = (observed - fast as f64).abs() / fast as f64 * 100.0;
        assert!(
            deviation <= 5.0,
            "ramp window {window_idx}: observed {observed:.2} deviates {deviation:.3}% from {fast}"
        );
    }
}

/// Convergence must hold for very small targets where each chunk represents
/// hundreds of milliseconds of debt. At 1 KB/s, a 256 B chunk requests
/// exactly 250 ms; ten such chunks delivered must request ~2.5s of sleep
/// total, matching the configured rate within +-5%.
#[test]
fn tiny_target_one_kilobyte_per_second_converges() {
    let mut session = recorded_sleep_session();
    session.clear();

    let target = 1_024_u64; // 1 KB/s
    let chunk = (target / 4) as usize; // 256 B -> 250 ms per chunk
    let mut limiter = BandwidthLimiter::new(nz(target));

    // Warm-up to amortise the first-call boundary.
    let _ = drive(&mut limiter, 2, chunk);

    // Drive 10 chunks (~2.5s requested sleep) and assert convergence.
    let window = drive(&mut limiter, 10, chunk);
    let observed = window.rate();
    let deviation = (observed - target as f64).abs() / target as f64 * 100.0;
    assert!(
        deviation <= 5.0,
        "tiny target 1 KB/s: observed {observed:.2} B/s deviates {deviation:.3}%"
    );

    // The cumulative requested sleep over 10 chunks of 256 B at 1024 B/s is
    // 10 * 256 / 1024 = 2.5s. Allow +-5% wiggle.
    let expected = Duration::from_millis(2_500);
    let diff = window.sleep.abs_diff(expected);
    assert!(
        diff <= Duration::from_millis(125),
        "tiny target cumulative sleep {:?} differs from {:?} by {:?}",
        window.sleep,
        expected,
        diff,
    );
}
