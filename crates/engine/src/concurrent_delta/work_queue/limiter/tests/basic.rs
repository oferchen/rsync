//! Acquire/release primitives, builder clamps, and integer math.

use super::super::{AimdLimiter, LimiterConfig, OverloadReason};
use super::limiter_with;

#[test]
fn isqrt_matches_floor_sqrt() {
    for n in [0u64, 1, 2, 3, 4, 9, 10, 100, 9999, 1_000_000, u64::MAX / 4] {
        let got = AimdLimiter::isqrt(n);
        assert!(got.saturating_mul(got) <= n, "isqrt({n}) = {got} too high");
        let next = got + 1;
        assert!(
            next.checked_mul(next).map(|sq| sq > n).unwrap_or(true),
            "isqrt({n}) = {got} too low"
        );
    }
}

#[test]
fn acquire_respects_target() {
    let limiter = limiter_with(2, 1, 8);
    let t1 = limiter.try_acquire().expect("first slot");
    let t2 = limiter.try_acquire().expect("second slot");
    assert!(limiter.try_acquire().is_none(), "saturated");
    t1.record_success();
    assert!(limiter.try_acquire().is_some(), "freed by t1");
    t2.record_success();
}

#[test]
fn decrease_clamped_to_min_limit() {
    let limiter = limiter_with(2, 2, 16);
    let t = limiter.try_acquire().unwrap();
    t.record_overload(OverloadReason::QueueSaturated);
    assert_eq!(limiter.target(), 2, "clamped at min_limit=2");
}

#[test]
fn config_builder_clamps_initial_target() {
    let limiter = LimiterConfig::new(100).min_limit(2).max_limit(10).build();
    assert_eq!(limiter.target(), 10, "initial clamped to max_limit");

    let limiter = LimiterConfig::new(1).min_limit(4).max_limit(16).build();
    assert_eq!(limiter.target(), 4, "initial clamped up to min_limit");
}

#[test]
fn rtt_spike_predicate() {
    let limiter = limiter_with(4, 1, 16);
    // No samples yet -> no spike.
    assert!(!limiter.is_rtt_spike(1_000_000_000));
    // Seed a stable EMA.
    for _ in 0..16 {
        limiter.update_rtt(1_000_000);
    }
    // A 100x spike is well above ema + 2*sigma.
    assert!(limiter.is_rtt_spike(100_000_000));
    // A sample at the EMA is not a spike.
    assert!(!limiter.is_rtt_spike(1_000_000));
}
