//! Additive-increase / multiplicative-decrease semantics, slow-start
//! transition, clamps, RTT smoothing, and debounce behaviour.

use std::io;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use super::super::{AimdLimiter, OverloadReason};
use super::limiter_with;

#[test]
fn additive_increase_after_target_successes() {
    let limiter = limiter_with(2, 1, 16);
    // Force out of slow-start by recording one fake decrease.
    // Use .max(1) to mirror production code in release_overload() which
    // guarantees last_decrease is never zero (zero means "no decrease yet"
    // and triggers the slow-start doubling path).
    limiter
        .last_decrease
        .store(AimdLimiter::now_nanos().max(1), Ordering::Release);
    // Two successes complete a window of size 2 -> target += 1.
    let t1 = limiter.try_acquire().unwrap();
    t1.record_success();
    let t2 = limiter.try_acquire().unwrap();
    t2.record_success();
    assert_eq!(limiter.target(), 3, "alpha=1 additive increase");
}

#[test]
fn multiplicative_decrease_halves_target_on_overload() {
    let limiter = limiter_with(8, 1, 16);
    let t = limiter.try_acquire().unwrap();
    t.record_overload(OverloadReason::RttSpike);
    assert_eq!(limiter.target(), 4, "beta=1/2 multiplicative decrease");
}

#[test]
fn increase_clamped_to_max_limit() {
    let limiter = limiter_with(2, 1, 3);
    // Out of slow-start so additive applies (mirror release_overload's .max(1)
    // to guarantee the value is non-zero on platforms where the first
    // now_nanos() call can return 0).
    limiter
        .last_decrease
        .store(AimdLimiter::now_nanos().max(1), Ordering::Release);
    // First window: target=2 -> 3 (clamped at max).
    let t1 = limiter.try_acquire().unwrap();
    t1.record_success();
    let t2 = limiter.try_acquire().unwrap();
    t2.record_success();
    assert_eq!(limiter.target(), 3);
    // Second window: still at 3 (clamped).
    for _ in 0..3 {
        let t = limiter.try_acquire().unwrap();
        t.record_success();
    }
    assert_eq!(limiter.target(), 3, "clamped at max_limit=3");
}

#[test]
fn debounce_suppresses_back_to_back_decreases() {
    let limiter = limiter_with(8, 1, 16);
    // Seed a non-trivial RTT EMA so the debounce window has length.
    limiter.update_rtt(10_000_000); // 10 ms
    // First overload: decreases.
    let t1 = limiter.try_acquire().unwrap();
    t1.record_overload(OverloadReason::RttSpike);
    let after_first = limiter.target();
    assert!(after_first < 8);
    // Second overload immediately after: suppressed.
    let t2 = limiter.try_acquire().unwrap();
    t2.record_overload(OverloadReason::RttSpike);
    assert_eq!(
        limiter.target(),
        after_first,
        "debounce suppresses second decrease"
    );
}

#[test]
fn rtt_ema_smoothing_converges() {
    let limiter = limiter_with(4, 1, 16);
    // Seed with a 1ms baseline, then drive 20 samples of 10ms. With
    // alpha_ema = 1/8 the geometric tail (7/8)^20 ~ 0.069, so EMA settles
    // between 9.0ms and 9.5ms after the second batch.
    for _ in 0..10 {
        limiter.update_rtt(1_000_000);
    }
    for _ in 0..20 {
        limiter.update_rtt(10_000_000);
    }
    let ema = limiter.rtt_ema_nanos();
    assert!(
        (9_000_000..=9_500_000).contains(&ema),
        "expected EMA in [9.0ms, 9.5ms], got {ema} ns",
    );
}

#[test]
fn error_kind_classification() {
    let limiter = limiter_with(8, 1, 16);
    // Transient -> overload (decreases target).
    let t = limiter.try_acquire().unwrap();
    t.record_error(io::ErrorKind::WouldBlock);
    assert_eq!(limiter.target(), 4);

    // Reset for next case (debounce would otherwise suppress).
    let limiter = limiter_with(8, 1, 16);
    let t = limiter.try_acquire().unwrap();
    t.record_error(io::ErrorKind::Interrupted);
    assert_eq!(limiter.target(), 4);

    // Deterministic -> success path; target should not decrease.
    let limiter = limiter_with(8, 1, 16);
    let t = limiter.try_acquire().unwrap();
    t.record_error(io::ErrorKind::NotFound);
    assert_eq!(limiter.target(), 8);
    let t = limiter.try_acquire().unwrap();
    t.record_error(io::ErrorKind::PermissionDenied);
    assert_eq!(limiter.target(), 8);
}

#[test]
fn slow_start_doubles_until_first_decrease() {
    let limiter = limiter_with(2, 1, 64);
    // Slow-start: complete a window of size 2 -> target doubles to 4.
    let t1 = limiter.try_acquire().unwrap();
    t1.record_success();
    let t2 = limiter.try_acquire().unwrap();
    t2.record_success();
    assert_eq!(limiter.target(), 4, "slow-start doubles 2 -> 4");

    // Complete another window of 4 -> target doubles to 8.
    for _ in 0..4 {
        let t = limiter.try_acquire().unwrap();
        t.record_success();
    }
    assert_eq!(limiter.target(), 8, "slow-start doubles 4 -> 8");

    // First overload exits slow-start and halves target.
    let t = limiter.try_acquire().unwrap();
    t.record_overload(OverloadReason::QueueSaturated);
    assert_eq!(limiter.target(), 4, "first decrease halves 8 -> 4");

    // Subsequent windows now grow additively (alpha=1) instead of doubling.
    // We must wait past the debounce window for further decreases, but
    // increases are unaffected. Need to wait long enough that the debounce
    // would have expired, but additive vs doubling is observable on the
    // success path regardless.
    thread::sleep(Duration::from_millis(5));
    // Saturate target=4 with successes: target -> 5 (additive, not 8).
    for _ in 0..4 {
        let t = limiter.try_acquire().unwrap();
        t.record_success();
    }
    assert_eq!(limiter.target(), 5, "post-decrease growth is additive");
}

#[test]
fn slow_start_doubles_multiple_windows() {
    let limiter = limiter_with(1, 1, 256);
    let mut expected = 1;
    // During slow-start, each completed window doubles the target.
    for _ in 0..7 {
        let window = limiter.target();
        assert_eq!(window, expected);
        for _ in 0..window {
            let t = limiter.try_acquire().unwrap();
            t.record_success();
        }
        expected = expected.saturating_mul(2).min(256);
    }
    assert_eq!(
        limiter.target(),
        128,
        "slow-start should reach 128 after 7 doublings"
    );
}

#[test]
fn overload_during_slow_start_transitions_to_additive() {
    let limiter = limiter_with(4, 1, 128);
    // One slow-start window: 4 -> 8.
    for _ in 0..4 {
        let t = limiter.try_acquire().unwrap();
        t.record_success();
    }
    assert_eq!(limiter.target(), 8);

    // Overload exits slow-start.
    let t = limiter.try_acquire().unwrap();
    t.record_overload(OverloadReason::RttSpike);
    assert_eq!(limiter.target(), 4);

    thread::sleep(Duration::from_millis(1));

    // Next window: additive, not doubling.
    for _ in 0..4 {
        let t = limiter.try_acquire().unwrap();
        t.record_success();
    }
    assert_eq!(
        limiter.target(),
        5,
        "post-overload growth must be additive (+1, not x2)"
    );

    for _ in 0..5 {
        let t = limiter.try_acquire().unwrap();
        t.record_success();
    }
    assert_eq!(limiter.target(), 6, "second additive window: 5 -> 6");
}

#[test]
fn max_limit_reached_during_slow_start() {
    // Slow-start doubles but must clamp at max_limit.
    let limiter = limiter_with(4, 1, 10);
    // Window of 4 -> 8.
    for _ in 0..4 {
        let t = limiter.try_acquire().unwrap();
        t.record_success();
    }
    assert_eq!(limiter.target(), 8);
    // Window of 8 -> 16, but clamped to max_limit=10.
    for _ in 0..8 {
        let t = limiter.try_acquire().unwrap();
        t.record_success();
    }
    assert_eq!(
        limiter.target(),
        10,
        "slow-start doubling must clamp at max_limit",
    );
}

#[test]
fn debounce_expires_after_twice_rtt_ema() {
    let limiter = limiter_with(16, 1, 64);
    // Seed RTT EMA to 5ms so debounce window = 10ms.
    for _ in 0..16 {
        limiter.update_rtt(5_000_000);
    }
    let t = limiter.try_acquire().unwrap();
    t.record_overload(OverloadReason::ErrorRate);
    let after_first = limiter.target();
    assert_eq!(after_first, 8);

    // Immediate second overload: debounce suppresses.
    let t = limiter.try_acquire().unwrap();
    t.record_overload(OverloadReason::ErrorRate);
    assert_eq!(limiter.target(), 8, "suppressed within debounce window");

    // Wait past 2 * rtt_ema (~10ms), then overload again.
    thread::sleep(Duration::from_millis(15));
    let t = limiter.try_acquire().unwrap();
    t.record_overload(OverloadReason::ErrorRate);
    assert_eq!(
        limiter.target(),
        4,
        "decrease should apply after debounce expires",
    );
}

#[test]
fn mixed_transient_and_deterministic_errors() {
    // Deterministic errors (NotFound, PermissionDenied) should not
    // decrease the target. Only transient errors should.
    let limiter = limiter_with(8, 1, 16);

    // A batch of deterministic errors: target stays at 8.
    for kind in [
        io::ErrorKind::NotFound,
        io::ErrorKind::PermissionDenied,
        io::ErrorKind::AlreadyExists,
    ] {
        let t = limiter.try_acquire().unwrap();
        t.record_error(kind);
    }
    assert_eq!(
        limiter.target(),
        8,
        "deterministic errors must not reduce target",
    );

    // A transient error triggers overload.
    let t = limiter.try_acquire().unwrap();
    t.record_error(io::ErrorKind::TimedOut);
    assert_eq!(limiter.target(), 4, "transient TimedOut must halve target",);
}

#[test]
fn debounce_prevents_cascading_collapse() {
    // Without debounce, N back-to-back overloads would collapse
    // target to min_limit. With debounce, only the first should
    // take effect within the window.
    let limiter = limiter_with(64, 4, 256);
    // Seed a 5ms RTT so debounce window is ~10ms.
    for _ in 0..16 {
        limiter.update_rtt(5_000_000);
    }

    // First overload: target halves.
    let t = limiter.try_acquire().unwrap();
    t.record_overload(OverloadReason::RttSpike);
    let after_first = limiter.target();
    assert_eq!(after_first, 32);

    // Rapid burst of 10 overloads within the debounce window (~10ms).
    // None should decrease further.
    for _ in 0..10 {
        let t = limiter.try_acquire().unwrap();
        t.record_overload(OverloadReason::QueueSaturated);
    }
    assert_eq!(
        limiter.target(),
        32,
        "debounce must prevent cascading collapse from rapid burst",
    );

    // After debounce expires, next overload should take effect.
    thread::sleep(Duration::from_millis(15));
    let t = limiter.try_acquire().unwrap();
    t.record_overload(OverloadReason::DiskCommitPressure);
    assert_eq!(
        limiter.target(),
        16,
        "overload after debounce expires should halve target",
    );
}
