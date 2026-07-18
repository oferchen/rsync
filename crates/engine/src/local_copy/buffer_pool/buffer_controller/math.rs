//! Numerical helpers used by the PID controller loop.
//!
//! `compute_dt` produces a bounded sample-interval, and `clamp_f64` enforces
//! the anti-windup window without relying on `f64::clamp`, which panics
//! (in all builds) when `lo > hi` or when either bound is NaN.

use std::time::{Duration, Instant};

/// Lower bound on `dt` (1 ms) to avoid divide-by-zero and amplifying noise
/// from sub-millisecond clock jitter on the derivative term.
pub(super) const MIN_DT: Duration = Duration::from_millis(1);
/// Upper bound on `dt` (5 s) to avoid integral windup when the producer has
/// stalled for an extended period.
pub(super) const MAX_DT: Duration = Duration::from_secs(5);

/// Computes a bounded `dt` from the previous and current sample timestamps.
///
/// On the very first sample (no previous timestamp) this returns the
/// configured `sample_interval_ms`. Otherwise the elapsed duration is
/// clamped to `[MIN_DT, MAX_DT]` to avoid divide-by-zero on the derivative
/// term and integrator windup on a stalled producer.
pub(super) fn compute_dt(prev: Option<Instant>, now: Instant, default_ms: u64) -> Duration {
    let raw = match prev {
        Some(t) => now.saturating_duration_since(t),
        None => Duration::from_millis(default_ms.max(1)),
    };
    if raw < MIN_DT {
        MIN_DT
    } else if raw > MAX_DT {
        MAX_DT
    } else {
        raw
    }
}

/// Clamps an `f64` to `[lo, hi]`, treating NaN as `lo`.
///
/// Using a free function avoids `f64::clamp`, which panics (in all builds)
/// when `lo > hi` or when either bound is NaN.
pub(super) fn clamp_f64(v: f64, lo: f64, hi: f64) -> f64 {
    if v.is_nan() {
        return lo;
    }
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}
