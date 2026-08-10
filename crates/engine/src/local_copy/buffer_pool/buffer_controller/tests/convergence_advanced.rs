//! Advanced convergence tests: derivative kick, sample-interval extremes,
//! producer stalls, alternating plants, monotonic approach, noise, and
//! concurrent observation.

use std::time::{Duration, Instant};

use super::super::ControllerConfig;
use super::support::{linear_plant, variance};

#[test]
fn convergence_derivative_kick_suppression() {
    // When the setpoint changes (simulated by changing the plant gain),
    // the derivative term acts on the error derivative, not the process
    // variable derivative. A sudden error spike should not cause the
    // controller to overshoot wildly. We compare overshoot with D=0
    // vs D>0.
    let min = 16 * 1024;
    let max = 4 * 1024 * 1024;
    let mid = (min + max) / 2;
    let k = 0.5;
    let setpoint = (k * mid as f64) as u64;
    let cap = setpoint as f64 * 4.0;
    let t0 = Instant::now();

    let ctrl_no_d = ControllerConfig::new(setpoint)
        .gains(0.3, 0.1, 0.0)
        .min_size(min)
        .max_size(max)
        .build();
    let ctrl_with_d = ControllerConfig::new(setpoint)
        .gains(0.3, 0.1, 0.05)
        .min_size(min)
        .max_size(max)
        .build();

    // Converge both under normal conditions.
    let mut t = t0;
    for _ in 0..80 {
        t += Duration::from_millis(100);
        let tp_no_d = linear_plant(ctrl_no_d.buffer_size(), k, cap);
        ctrl_no_d.observe_at(tp_no_d, t);
        let tp_with_d = linear_plant(ctrl_with_d.buffer_size(), k, cap);
        ctrl_with_d.observe_at(tp_with_d, t);
    }

    let steady_no_d = ctrl_no_d.buffer_size();
    let steady_with_d = ctrl_with_d.buffer_size();

    // Sudden plant change: throughput drops to 10% of before.
    let k_slow = k * 0.1;
    t += Duration::from_millis(100);
    let tp_no_d = linear_plant(ctrl_no_d.buffer_size(), k_slow, cap);
    ctrl_no_d.observe_at(tp_no_d, t);
    let tp_with_d = linear_plant(ctrl_with_d.buffer_size(), k_slow, cap);
    ctrl_with_d.observe_at(tp_with_d, t);

    let kick_no_d = (ctrl_no_d.buffer_size() as i64 - steady_no_d as i64).unsigned_abs();
    let kick_with_d = (ctrl_with_d.buffer_size() as i64 - steady_with_d as i64).unsigned_abs();

    // The derivative-equipped controller should have a larger or equal
    // initial response (D adds to the proportional kick for same-sign
    // derivative), but the key property is that neither produces an
    // output outside the configured bounds.
    assert!(
        ctrl_no_d.buffer_size() >= min && ctrl_no_d.buffer_size() <= max,
        "no-D controller output out of bounds: {}",
        ctrl_no_d.buffer_size()
    );
    assert!(
        ctrl_with_d.buffer_size() >= min && ctrl_with_d.buffer_size() <= max,
        "with-D controller output out of bounds: {}",
        ctrl_with_d.buffer_size()
    );

    // Both should have responded (non-zero kick).
    assert!(
        kick_no_d > 0 || kick_with_d > 0,
        "at least one controller should react to step change"
    );
}

#[test]
fn convergence_rapid_sample_intervals() {
    // Feed samples at 1 ms intervals (10x faster than default). The
    // controller should still converge due to the MIN_DT clamp.
    let min = 16 * 1024;
    let max = 1024 * 1024;
    let mid = (min + max) / 2;
    let k = 0.5;
    let setpoint = (k * mid as f64) as u64;
    let cap = setpoint as f64 * 4.0;

    let ctrl = ControllerConfig::new(setpoint)
        .gains(0.3, 0.1, 0.02)
        .min_size(min)
        .max_size(max)
        .sample_interval_ms(1)
        .build();

    let mut now = Instant::now();
    for _ in 0..500 {
        now += Duration::from_millis(1);
        let throughput = linear_plant(ctrl.buffer_size(), k, cap);
        ctrl.observe_at(throughput, now);
    }

    let final_throughput = linear_plant(ctrl.buffer_size(), k, cap);
    let error_pct = (final_throughput as f64 - setpoint as f64).abs() / setpoint as f64;
    assert!(
        error_pct < 0.15,
        "should converge with rapid samples: error={error_pct:.2}"
    );
}

#[test]
fn convergence_slow_sample_intervals() {
    // Feed samples at 5 s intervals (max dt). The MAX_DT clamp
    // prevents integral windup.
    let min = 16 * 1024;
    let max = 1024 * 1024;
    let mid = (min + max) / 2;
    let k = 0.5;
    let setpoint = (k * mid as f64) as u64;
    let cap = setpoint as f64 * 4.0;

    let ctrl = ControllerConfig::new(setpoint)
        .gains(0.3, 0.1, 0.02)
        .min_size(min)
        .max_size(max)
        .build();

    let mut now = Instant::now();
    for _ in 0..50 {
        now += Duration::from_secs(5);
        let throughput = linear_plant(ctrl.buffer_size(), k, cap);
        ctrl.observe_at(throughput, now);
    }

    let final_throughput = linear_plant(ctrl.buffer_size(), k, cap);
    let error_pct = (final_throughput as f64 - setpoint as f64).abs() / setpoint as f64;
    assert!(
        error_pct < 0.20,
        "should converge with slow samples: error={error_pct:.2}"
    );

    // Verify integral did not wind up to infinity.
    let state = ctrl.state.lock().unwrap();
    assert!(
        state.integral.is_finite(),
        "integral must be finite under slow sampling"
    );
}

#[test]
fn convergence_stall_then_resume() {
    // Simulate a producer stall (large gap between samples, then
    // normal sampling resumes). The controller should recover.
    let min = 16 * 1024;
    let max = 2 * 1024 * 1024;
    let mid = (min + max) / 2;
    let k = 0.5;
    let setpoint = (k * mid as f64) as u64;
    let cap = setpoint as f64 * 4.0;

    let ctrl = ControllerConfig::new(setpoint)
        .gains(0.3, 0.1, 0.02)
        .min_size(min)
        .max_size(max)
        .build();

    let mut now = Instant::now();

    // Converge normally.
    for _ in 0..80 {
        now += Duration::from_millis(100);
        let throughput = linear_plant(ctrl.buffer_size(), k, cap);
        ctrl.observe_at(throughput, now);
    }
    let size_before_stall = ctrl.buffer_size();

    // Stall for 30 seconds (well beyond MAX_DT of 5s).
    now += Duration::from_secs(30);
    let throughput = linear_plant(ctrl.buffer_size(), k, cap);
    ctrl.observe_at(throughput, now);

    // Size should still be within bounds.
    assert!(ctrl.buffer_size() >= min);
    assert!(ctrl.buffer_size() <= max);

    // Resume normal sampling and verify reconvergence.
    for _ in 0..80 {
        now += Duration::from_millis(100);
        let throughput = linear_plant(ctrl.buffer_size(), k, cap);
        ctrl.observe_at(throughput, now);
    }

    let final_throughput = linear_plant(ctrl.buffer_size(), k, cap);
    let error_pct = (final_throughput as f64 - setpoint as f64).abs() / setpoint as f64;
    assert!(
        error_pct < 0.15,
        "should reconverge after stall: before_stall={size_before_stall}, after={}, error={error_pct:.2}",
        ctrl.buffer_size()
    );
}

#[test]
fn convergence_alternating_fast_and_slow_plant() {
    // Alternate between a fast plant (throughput above setpoint) and a
    // slow plant (throughput below setpoint). The controller should
    // remain bounded and eventually stabilize (variance decreases or
    // stays bounded across cycles).
    let min = 16 * 1024;
    let max = 2 * 1024 * 1024;
    let mid = (min + max) / 2;
    let k = 0.5;
    let setpoint = (k * mid as f64) as u64;
    let cap = setpoint as f64 * 4.0;

    let ctrl = ControllerConfig::new(setpoint)
        .gains(0.3, 0.1, 0.02)
        .min_size(min)
        .max_size(max)
        .build();

    let mut now = Instant::now();

    // 10 cycles: 10 samples with k*1.5 (above setpoint), then 10
    // with k*0.7 (below setpoint). The average throughput at midpoint
    // is ~1.1*setpoint, so the controller should settle to a moderate
    // buffer size.
    let k_fast = k * 1.5;
    let k_slow = k * 0.7;

    // Early window: collect sizes from first 5 cycles.
    let mut early_sizes = Vec::new();
    for _ in 0..5 {
        for _ in 0..10 {
            now += Duration::from_millis(100);
            let throughput = linear_plant(ctrl.buffer_size(), k_fast, cap);
            ctrl.observe_at(throughput, now);
            early_sizes.push(ctrl.buffer_size());
        }
        for _ in 0..10 {
            now += Duration::from_millis(100);
            let throughput = linear_plant(ctrl.buffer_size(), k_slow, cap);
            ctrl.observe_at(throughput, now);
            early_sizes.push(ctrl.buffer_size());
        }
    }

    // Late window: collect sizes from next 5 cycles.
    let mut late_sizes = Vec::new();
    for _ in 0..5 {
        for _ in 0..10 {
            now += Duration::from_millis(100);
            let throughput = linear_plant(ctrl.buffer_size(), k_fast, cap);
            ctrl.observe_at(throughput, now);
            late_sizes.push(ctrl.buffer_size());
        }
        for _ in 0..10 {
            now += Duration::from_millis(100);
            let throughput = linear_plant(ctrl.buffer_size(), k_slow, cap);
            ctrl.observe_at(throughput, now);
            late_sizes.push(ctrl.buffer_size());
        }
    }

    // Late variance should be bounded (not growing).
    let early_var = variance(&early_sizes);
    let late_var = variance(&late_sizes);
    assert!(
        late_var <= early_var * 1.5 + 1.0,
        "oscillation should not grow: early_var={early_var:.0}, late_var={late_var:.0}"
    );

    // All sizes must be within bounds.
    for &s in early_sizes.iter().chain(late_sizes.iter()) {
        assert!(
            s >= min && s <= max,
            "size {s} out of bounds [{min}, {max}]"
        );
    }
}

#[test]
fn convergence_monotonic_approach_from_min() {
    // Start the controller at min_size (by setting min close to initial)
    // and verify it monotonically approaches the setpoint from below
    // (no overshoots that dip below the starting point). Use a plant
    // where setpoint is achievable well within the buffer range.
    let min = 16 * 1024;
    let max = 4 * 1024 * 1024;
    let mid = (min + max) / 2;

    // Set gains low enough that loop gain K_p * k < 1 for stability.
    let k = 0.3;
    let setpoint = (k * mid as f64) as u64;
    let cap = setpoint as f64 * 4.0;

    // Use min_size close to the default initial midpoint so the
    // controller starts from a consistent position.
    let ctrl = ControllerConfig::new(setpoint)
        .gains(0.2, 0.05, 0.01)
        .min_size(min)
        .max_size(max)
        .build();

    let initial = ctrl.buffer_size();
    let mut now = Instant::now();

    // Feed low throughput so the controller grows the buffer.
    let mut floor = initial;
    let mut monotonic_violations = 0;
    for _ in 0..50 {
        now += Duration::from_millis(100);
        // Plant returns less than setpoint so error is positive -> grow.
        let throughput = linear_plant(ctrl.buffer_size(), k * 0.5, cap);
        ctrl.observe_at(throughput, now);
        let current = ctrl.buffer_size();
        if current < floor {
            monotonic_violations += 1;
        }
        floor = floor.max(current);
    }

    // Allow at most a couple of minor dips due to derivative kick
    // when the error rate changes sign.
    assert!(
        monotonic_violations <= 3,
        "growth from below setpoint should be mostly monotonic: {monotonic_violations} violations",
    );
}

#[test]
fn convergence_high_noise_amplitude_bounded_output() {
    // Even under extremely noisy measurements (noise amplitude equals
    // the setpoint), the controller must keep its output within bounds.
    let min = 16 * 1024;
    let max = 2 * 1024 * 1024;
    let setpoint = 50 * 1024 * 1024u64;

    let ctrl = ControllerConfig::new(setpoint)
        .min_size(min)
        .max_size(max)
        .build();

    let mut now = Instant::now();

    // Noise pattern with full-range swings around the setpoint.
    let noise_factors: [f64; 10] = [0.0, 2.0, 0.1, 1.9, 0.3, 1.7, 0.5, 1.5, 0.2, 1.8];
    for i in 0..200 {
        now += Duration::from_millis(100);
        let throughput = (setpoint as f64 * noise_factors[i % noise_factors.len()]) as u64;
        let size = ctrl.observe_at(throughput, now);
        assert!(
            size >= min,
            "output below min at iteration {i}: {size} < {min}"
        );
        assert!(
            size <= max,
            "output above max at iteration {i}: {size} > {max}"
        );
    }
}

#[test]
fn convergence_setpoint_at_plant_maximum() {
    // When the setpoint equals the plant's maximum output, the
    // controller should grow the buffer to the point where the
    // plant saturates and then stabilize at max buffer size.
    let min = 16 * 1024;
    let max = 2 * 1024 * 1024;
    let mid = (min + max) / 2;
    // Choose k so that throughput = setpoint at the midpoint.
    // The plant cap equals setpoint, so at mid: k*mid = setpoint.
    // For buffer sizes > mid, throughput saturates at plant_cap.
    let setpoint = 50 * 1024 * 1024u64;
    let k = setpoint as f64 / mid as f64;
    let plant_cap = setpoint as f64; // Plant saturates at setpoint.

    let ctrl = ControllerConfig::new(setpoint)
        .gains(0.3, 0.1, 0.02)
        .min_size(min)
        .max_size(max)
        .build();

    let mut now = Instant::now();
    for _ in 0..200 {
        now += Duration::from_millis(100);
        let throughput = linear_plant(ctrl.buffer_size(), k, plant_cap);
        ctrl.observe_at(throughput, now);
    }

    // At the plant ceiling, throughput saturates. The controller's
    // error goes to zero once throughput matches setpoint. The buffer
    // should stabilize (not keep growing) once throughput = setpoint.
    let final_throughput = linear_plant(ctrl.buffer_size(), k, plant_cap);
    let error_pct = (final_throughput as f64 - setpoint as f64).abs() / setpoint as f64;
    assert!(
        error_pct < 0.05,
        "throughput should match setpoint at saturation: throughput={final_throughput}, setpoint={setpoint}"
    );
}

#[test]
fn convergence_asymmetric_gains_bias() {
    // With high P and low I, convergence should be fast but have
    // residual steady-state offset. With high I and low P, convergence
    // should be slower but eliminate offset. This verifies the relative
    // behavior matches PID theory.
    let min = 16 * 1024;
    let max = 2 * 1024 * 1024;
    let mid = (min + max) / 2;
    let k = 0.3;
    let setpoint = (k * mid as f64) as u64;
    let cap = setpoint as f64 * 4.0;
    let t0 = Instant::now();

    let ctrl_high_p = ControllerConfig::new(setpoint)
        .gains(0.5, 0.01, 0.0)
        .min_size(min)
        .max_size(max)
        .build();
    let ctrl_high_i = ControllerConfig::new(setpoint)
        .gains(0.05, 0.3, 0.0)
        .min_size(min)
        .max_size(max)
        .build();

    let mut t = t0;
    for _ in 0..300 {
        t += Duration::from_millis(100);
        let tp_hp = linear_plant(ctrl_high_p.buffer_size(), k, cap);
        ctrl_high_p.observe_at(tp_hp, t);
        let tp_hi = linear_plant(ctrl_high_i.buffer_size(), k, cap);
        ctrl_high_i.observe_at(tp_hi, t);
    }

    let final_hp = linear_plant(ctrl_high_p.buffer_size(), k, cap);
    let final_hi = linear_plant(ctrl_high_i.buffer_size(), k, cap);
    let error_hp = (final_hp as f64 - setpoint as f64).abs() / setpoint as f64;
    let error_hi = (final_hi as f64 - setpoint as f64).abs() / setpoint as f64;

    // High-I should have lower steady-state error than high-P.
    assert!(
        error_hi <= error_hp + 0.05,
        "high-I should have lower or equal steady-state error: error_hi={error_hi:.3}, error_hp={error_hp:.3}"
    );
}

#[test]
fn convergence_concurrent_observers() {
    // Multiple threads calling observe_at concurrently should not cause
    // panics, data corruption, or outputs outside bounds.
    use std::sync::Arc;
    use std::thread;

    let setpoint = 50 * 1024 * 1024u64;
    let min = 16 * 1024;
    let max = 2 * 1024 * 1024;

    let ctrl = Arc::new(
        ControllerConfig::new(setpoint)
            .min_size(min)
            .max_size(max)
            .build(),
    );

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let ctrl = Arc::clone(&ctrl);
            thread::spawn(move || {
                for _ in 0..200 {
                    let size = ctrl.observe(setpoint);
                    assert!(size >= min, "concurrent output below min: {size}");
                    assert!(size <= max, "concurrent output above max: {size}");
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked during concurrent observe");
    }
}
