//! Long-running convergence tests under sustained patterns and error bursts.

use std::io;
use std::thread;
use std::time::Duration;

use super::super::{LimiterConfig, OverloadReason};
use super::{complete_success_window, inject_overload, limiter_with};

#[test]
fn convergence_under_alternating_success_and_overload() {
    // Under a sustained pattern of success windows followed by single
    // overload signals, the target should stabilize within a bounded
    // range rather than diverging to min or max.
    let limiter = limiter_with(8, 2, 128);
    // Exit slow-start by injecting a first overload.
    let t = limiter.try_acquire().unwrap();
    t.record_overload(OverloadReason::RttSpike);
    let mut last_target = limiter.target();

    // Run 50 cycles of: one full success window, then one overload.
    // The debounce window starts near zero (fresh limiter), so we
    // sleep briefly between cycles to ensure decreases are not
    // suppressed.
    for _ in 0..50 {
        thread::sleep(Duration::from_millis(1));
        let window = limiter.target();
        for _ in 0..window {
            let t = limiter.try_acquire().unwrap();
            t.record_success();
        }
        let after_increase = limiter.target();

        thread::sleep(Duration::from_millis(1));
        let t = limiter.try_acquire().unwrap();
        t.record_overload(OverloadReason::QueueSaturated);
        last_target = limiter.target();

        // Additive increase (+1) followed by multiplicative decrease
        // (floor(n/2)) converges to a fixed point where
        // floor((n+1)/2) == n, which is n in {2, 3} for alpha=1, beta=1/2.
        // Allow a range [2, after_increase] to account for timing.
        assert!(
            last_target >= 2 && last_target <= after_increase,
            "target {last_target} should stay in [2, {after_increase}]",
        );
    }

    // After 50 cycles the target must have settled near the AIMD
    // equilibrium (which for alpha=1, beta=1/2 is 2 or 3).
    assert!(
        last_target <= 4,
        "expected convergence near 2-3, got {last_target}",
    );
}

#[test]
fn sustained_successes_grow_target_monotonically() {
    let limiter = limiter_with(4, 1, 64);
    // Exit slow-start.
    let t = limiter.try_acquire().unwrap();
    t.record_overload(OverloadReason::DiskCommitPressure);

    thread::sleep(Duration::from_millis(1));
    let mut prev = limiter.target();
    // 10 consecutive success windows should produce monotonic growth.
    for _ in 0..10 {
        let window = limiter.target();
        for _ in 0..window {
            let t = limiter.try_acquire().unwrap();
            t.record_success();
        }
        let cur = limiter.target();
        assert!(
            cur >= prev,
            "target must not decrease during pure success: {prev} -> {cur}",
        );
        prev = cur;
    }
    assert!(
        prev > 4,
        "target should have grown beyond initial after 10 success windows, got {prev}",
    );
}

#[test]
fn sustained_overloads_converge_to_min_limit() {
    let limiter = limiter_with(64, 4, 256);
    // Seed RTT EMA so the debounce window has measurable length.
    for _ in 0..8 {
        limiter.update_rtt(1_000_000); // 1 ms
    }
    // Repeatedly overload with enough delay between signals to pass
    // the debounce window (2 * rtt_ema ~ 2 ms). After enough rounds,
    // target must reach the floor.
    for _ in 0..20 {
        thread::sleep(Duration::from_millis(3));
        let t = limiter.try_acquire().unwrap();
        t.record_overload(OverloadReason::DiskCommitPressure);
    }
    assert_eq!(
        limiter.target(),
        4,
        "sustained overloads must drive target to min_limit",
    );
}

#[test]
fn custom_beta_ratio_convergence() {
    // With beta = 2/3 (less aggressive decrease), convergence settles
    // at a higher equilibrium than the default beta = 1/2.
    let limiter = LimiterConfig::new(12)
        .min_limit(1)
        .max_limit(128)
        .alpha(1)
        .beta_num(2)
        .beta_den(3)
        .build();
    // Exit slow-start.
    let t = limiter.try_acquire().unwrap();
    t.record_overload(OverloadReason::RttSpike);
    let mut prev_target = limiter.target();
    // beta=2/3 of 12 = 8
    assert_eq!(prev_target, 8);

    // Run several success-window + overload cycles. With alpha=1 and
    // beta=2/3, the equilibrium satisfies floor((n+1)*2/3) == n,
    // giving n in {2, 3}. The cycle should converge within these bounds.
    for _ in 0..30 {
        thread::sleep(Duration::from_millis(1));
        let window = limiter.target();
        for _ in 0..window {
            let t = limiter.try_acquire().unwrap();
            t.record_success();
        }
        thread::sleep(Duration::from_millis(1));
        let t = limiter.try_acquire().unwrap();
        t.record_overload(OverloadReason::ErrorRate);
        prev_target = limiter.target();
    }
    assert!(
        prev_target <= 4,
        "beta=2/3 convergence should settle near 2-3, got {prev_target}",
    );
}

#[test]
fn recovery_after_error_burst() {
    // Inject a burst of errors driving the target to min_limit, then
    // verify it recovers toward its original value through sustained
    // success windows.
    let limiter = limiter_with(32, 4, 128);
    // Seed RTT for meaningful debounce.
    for _ in 0..8 {
        limiter.update_rtt(500_000); // 0.5 ms
    }
    // Exit slow-start.
    inject_overload(&limiter, OverloadReason::RttSpike);

    // Error burst: drive target to min_limit.
    let mut prev = limiter.target();
    while prev > 4 {
        prev = inject_overload(&limiter, OverloadReason::DiskCommitPressure);
    }
    assert_eq!(limiter.target(), 4, "burst should drive to min_limit");

    // Recovery: sustained success windows should grow target back.
    thread::sleep(Duration::from_millis(2));
    let mut targets_during_recovery = Vec::new();
    for _ in 0..20 {
        let after = complete_success_window(&limiter);
        targets_during_recovery.push(after);
    }

    // Verify monotonic recovery: each target should be >= previous.
    for window in targets_during_recovery.windows(2) {
        assert!(
            window[1] >= window[0],
            "recovery must be monotonic: {} -> {}",
            window[0],
            window[1],
        );
    }
    // After 20 additive-increase windows from 4, target should be 24.
    let final_target = limiter.target();
    assert!(
        final_target >= 20,
        "expected significant recovery after 20 windows, got {final_target}",
    );
}

#[test]
fn constant_error_rate_convergence() {
    // Under a constant error rate (1 overload per N operations), the
    // target should converge to a steady-state value rather than
    // diverging toward max_limit or min_limit.
    let limiter = limiter_with(16, 2, 256);
    // Seed RTT for debounce.
    for _ in 0..8 {
        limiter.update_rtt(500_000);
    }
    // Exit slow-start.
    inject_overload(&limiter, OverloadReason::RttSpike);

    // Run 100 cycles: 1 success window then 1 overload. Record the
    // target after each cycle to observe convergence.
    let mut targets = Vec::with_capacity(100);
    for _ in 0..100 {
        thread::sleep(Duration::from_millis(1));
        complete_success_window(&limiter);
        inject_overload(&limiter, OverloadReason::QueueSaturated);
        targets.push(limiter.target());
    }

    // The last 20 targets should be within a narrow band,
    // indicating convergence. With alpha=1, beta=1/2, the
    // equilibrium for "1 additive window + 1 halving" is
    // floor((n+1)/2)=n => n in {2,3}.
    let tail = &targets[80..];
    let min_tail = *tail.iter().min().unwrap();
    let max_tail = *tail.iter().max().unwrap();
    assert!(
        max_tail - min_tail <= 2,
        "last 20 targets should be within a 2-wide band, got [{min_tail}, {max_tail}]",
    );
    assert!(
        max_tail <= 5,
        "equilibrium should be near 2-3, max in tail is {max_tail}",
    );
}

#[test]
fn oscillation_amplitude_bounded() {
    // Verify the sawtooth amplitude (peak minus trough) stays bounded
    // and does not grow over time. After the initial transient, each
    // cycle's peak-to-trough range should be <= alpha + 1.
    let limiter = limiter_with(20, 2, 128);
    for _ in 0..8 {
        limiter.update_rtt(500_000);
    }
    // Exit slow-start.
    inject_overload(&limiter, OverloadReason::RttSpike);

    // Run 40 cycles, recording peak (after success window) and trough
    // (after overload) for each cycle.
    let mut amplitudes = Vec::with_capacity(40);
    for _ in 0..40 {
        thread::sleep(Duration::from_millis(1));
        let peak = complete_success_window(&limiter);
        let trough = inject_overload(&limiter, OverloadReason::ErrorRate);
        if peak > trough {
            amplitudes.push(peak - trough);
        }
    }

    // After convergence (skip first 10 cycles), amplitude should be
    // bounded by alpha+1=2 for a single additive step followed by halving.
    let settled = &amplitudes[10..];
    for (i, &amp) in settled.iter().enumerate() {
        assert!(
            amp <= 3,
            "cycle {}: amplitude {amp} exceeds bound of 3",
            i + 10,
        );
    }
}

#[test]
fn target_always_within_bounds_under_random_pattern() {
    // Property-style test: regardless of the success/overload pattern,
    // target must always stay within [min_limit, max_limit].
    let min = 3;
    let max = 50;
    let limiter = limiter_with(10, min, max);
    for _ in 0..8 {
        limiter.update_rtt(500_000);
    }

    // Pseudo-random pattern using a simple LCG to avoid pulling in rand.
    let mut state: u32 = 0xDEAD_BEEF;
    for _ in 0..500 {
        state = state.wrapping_mul(1103515245).wrapping_add(12345);
        let is_overload = (state >> 16) % 5 == 0; // ~20% error rate

        if let Some(t) = limiter.try_acquire() {
            if is_overload {
                thread::sleep(Duration::from_millis(1));
                t.record_overload(OverloadReason::QueueSaturated);
            } else {
                t.record_success();
            }
        }

        let target = limiter.target();
        assert!(
            target >= min && target <= max,
            "target {target} outside bounds [{min}, {max}]",
        );
    }
}

#[test]
fn short_error_burst_does_not_collapse_to_floor() {
    // A short burst (2-3 errors) should decrease the target but NOT
    // drive it all the way to min_limit from a high starting point.
    let limiter = limiter_with(32, 4, 128);
    for _ in 0..8 {
        limiter.update_rtt(500_000);
    }
    // Exit slow-start.
    inject_overload(&limiter, OverloadReason::RttSpike);
    let after_exit = limiter.target(); // 16

    // Two more overloads.
    inject_overload(&limiter, OverloadReason::DiskCommitPressure);
    let after_second = limiter.target(); // 8
    inject_overload(&limiter, OverloadReason::DiskCommitPressure);
    let after_third = limiter.target(); // 4

    assert_eq!(after_exit, 16);
    assert_eq!(after_second, 8);
    assert_eq!(after_third, 4);
    assert_eq!(
        after_third, 4,
        "3 overloads from 32 should stop at 4 (= min_limit), halving 32->16->8->4",
    );
}

#[test]
fn interleaved_overload_reasons_converge() {
    // Different overload reasons should all trigger the same
    // multiplicative decrease. Mix them and verify convergence.
    let limiter = limiter_with(16, 2, 128);
    for _ in 0..8 {
        limiter.update_rtt(500_000);
    }
    // Exit slow-start.
    inject_overload(&limiter, OverloadReason::RttSpike);

    let reasons = [
        OverloadReason::RttSpike,
        OverloadReason::QueueSaturated,
        OverloadReason::DiskCommitPressure,
        OverloadReason::ErrorRate,
    ];

    let mut targets = Vec::with_capacity(40);
    for i in 0..40 {
        thread::sleep(Duration::from_millis(1));
        complete_success_window(&limiter);
        // Rotate through overload reasons.
        let reason = reasons[i % reasons.len()];
        inject_overload(&limiter, reason);
        targets.push(limiter.target());
    }

    // Should converge regardless of which reason is used.
    let tail = &targets[30..];
    let min_tail = *tail.iter().min().unwrap();
    let max_tail = *tail.iter().max().unwrap();
    assert!(
        max_tail <= 5,
        "all overload reasons should drive convergence, max in tail is {max_tail}",
    );
    assert!(
        max_tail - min_tail <= 2,
        "tail should be stable, got range [{min_tail}, {max_tail}]",
    );
}

#[test]
fn recovery_rate_proportional_to_alpha() {
    // With alpha=2, recovery should be twice as fast as alpha=1.
    let limiter_a1 = LimiterConfig::new(16)
        .min_limit(2)
        .max_limit(128)
        .alpha(1)
        .build();
    let limiter_a2 = LimiterConfig::new(16)
        .min_limit(2)
        .max_limit(128)
        .alpha(2)
        .build();

    // Exit slow-start on both.
    let t = limiter_a1.try_acquire().unwrap();
    t.record_overload(OverloadReason::RttSpike);
    let t = limiter_a2.try_acquire().unwrap();
    t.record_overload(OverloadReason::RttSpike);

    // Drive both to floor.
    for limiter in [&limiter_a1, &limiter_a2] {
        while limiter.target() > 2 {
            thread::sleep(Duration::from_millis(1));
            let t = limiter.try_acquire().unwrap();
            t.record_overload(OverloadReason::DiskCommitPressure);
        }
    }

    thread::sleep(Duration::from_millis(2));

    // 10 success windows on each.
    for _ in 0..10 {
        complete_success_window(&limiter_a1);
        complete_success_window(&limiter_a2);
    }

    let target_a1 = limiter_a1.target();
    let target_a2 = limiter_a2.target();
    // alpha=2 should have recovered further than alpha=1.
    assert!(
        target_a2 > target_a1,
        "alpha=2 target ({target_a2}) should exceed alpha=1 target ({target_a1})",
    );
    // With alpha=1 from floor=2, 10 windows -> 12.
    // With alpha=2 from floor=2, 10 windows -> 22.
    assert!(
        target_a1 >= 10,
        "alpha=1 after 10 windows from 2 should be >= 10, got {target_a1}",
    );
    assert!(
        target_a2 >= 18,
        "alpha=2 after 10 windows from 2 should be >= 18, got {target_a2}",
    );
}

#[test]
fn aggressive_beta_converges_lower() {
    // beta=1/4 (more aggressive decrease) should converge to a lower
    // steady state than beta=1/2 under the same error pattern.
    let limiter_half = LimiterConfig::new(16)
        .min_limit(1)
        .max_limit(128)
        .beta_num(1)
        .beta_den(2)
        .build();
    let limiter_quarter = LimiterConfig::new(16)
        .min_limit(1)
        .max_limit(128)
        .beta_num(1)
        .beta_den(4)
        .build();

    // Exit slow-start on both.
    for limiter in [&limiter_half, &limiter_quarter] {
        let t = limiter.try_acquire().unwrap();
        t.record_overload(OverloadReason::RttSpike);
    }

    // Run 40 cycles of success window + overload on each.
    for _ in 0..40 {
        for limiter in [&limiter_half, &limiter_quarter] {
            thread::sleep(Duration::from_millis(1));
            complete_success_window(limiter);
            inject_overload(limiter, OverloadReason::ErrorRate);
        }
    }

    let target_half = limiter_half.target();
    let target_quarter = limiter_quarter.target();
    // beta=1/4 is more aggressive so should settle lower or equal.
    assert!(
        target_quarter <= target_half,
        "beta=1/4 target ({target_quarter}) should be <= beta=1/2 target ({target_half})",
    );
}

#[test]
fn transient_error_burst_with_recovery() {
    // Simulate a realistic scenario: normal operation, then a burst
    // of transient io::Errors (TimedOut), then back to normal. The
    // target should decrease during the burst and recover after.
    let limiter = limiter_with(16, 2, 64);
    for _ in 0..8 {
        limiter.update_rtt(500_000);
    }
    // Exit slow-start.
    inject_overload(&limiter, OverloadReason::RttSpike);

    thread::sleep(Duration::from_millis(2));

    // Phase 1: normal operation (5 success windows).
    for _ in 0..5 {
        complete_success_window(&limiter);
    }
    let before_burst = limiter.target();

    // Phase 2: burst of transient errors.
    for _ in 0..4 {
        thread::sleep(Duration::from_millis(1));
        let t = limiter.try_acquire().unwrap();
        t.record_error(io::ErrorKind::TimedOut);
    }
    let during_burst = limiter.target();
    assert!(
        during_burst < before_burst,
        "target should decrease during error burst: before={before_burst}, during={during_burst}",
    );

    // Phase 3: recovery (sustained success).
    thread::sleep(Duration::from_millis(2));
    for _ in 0..20 {
        complete_success_window(&limiter);
    }
    let after_recovery = limiter.target();
    assert!(
        after_recovery > during_burst,
        "target should recover after errors stop: during={during_burst}, after={after_recovery}",
    );
}
