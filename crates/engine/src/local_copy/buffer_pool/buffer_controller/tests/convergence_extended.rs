//! Extended convergence behaviour under complex multi-phase workloads.
//!
//! These tests exercise the controller's stability under workload
//! patterns, boundary conditions, and multi-phase scenarios beyond the
//! basic single-pattern coverage in [`super::convergence_basic`].

use std::time::{Duration, Instant};

use super::super::{AdaptiveBufferController, ControllerConfig};
use super::support::{linear_plant, variance};

#[test]
fn convergence_multi_phase_ramp_plateau_decay() {
    // Workload pattern: ramp up throughput linearly, hold at plateau,
    // then decay. The controller must track each phase and stabilize.
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

    // Phase 1: Ramp up from 10% to 100% of plant capacity over 50 samples.
    for i in 0..50 {
        now += Duration::from_millis(100);
        let ramp_factor = 0.1 + 0.9 * (i as f64 / 49.0);
        let throughput = linear_plant(ctrl.buffer_size(), k * ramp_factor, cap);
        ctrl.observe_at(throughput, now);
    }

    // Phase 2: Plateau at full plant capacity for 50 samples.
    for _ in 0..50 {
        now += Duration::from_millis(100);
        let throughput = linear_plant(ctrl.buffer_size(), k, cap);
        ctrl.observe_at(throughput, now);
    }

    // Verify plateau stability.
    let mut plateau_sizes = Vec::with_capacity(20);
    for _ in 0..20 {
        now += Duration::from_millis(100);
        let throughput = linear_plant(ctrl.buffer_size(), k, cap);
        ctrl.observe_at(throughput, now);
        plateau_sizes.push(ctrl.buffer_size());
    }
    let plateau_var = variance(&plateau_sizes);
    let plateau_mean = plateau_sizes.iter().sum::<usize>() as f64 / plateau_sizes.len() as f64;
    let cv = if plateau_mean > 0.0 {
        plateau_var.sqrt() / plateau_mean
    } else {
        0.0
    };
    assert!(
        cv < 0.10,
        "plateau phase should be stable: cv={cv:.4}, mean={plateau_mean:.0}"
    );

    // Phase 3: Decay plant gain by 50% and let the controller adapt.
    let k_slow = k * 0.5;
    for _ in 0..50 {
        now += Duration::from_millis(100);
        let throughput = linear_plant(ctrl.buffer_size(), k_slow, cap);
        ctrl.observe_at(throughput, now);
    }

    // After decay, throughput should still be near setpoint (controller
    // grew the buffer to compensate for reduced plant gain).
    let final_throughput = linear_plant(ctrl.buffer_size(), k_slow, cap);
    let error_pct = (final_throughput as f64 - setpoint as f64).abs() / setpoint as f64;
    assert!(
        error_pct < 0.20,
        "controller should track decay: throughput={final_throughput}, setpoint={setpoint}, error={error_pct:.2}"
    );
}

#[test]
fn convergence_zero_load_sustained_no_pathological_behavior() {
    // Feed zero throughput for many iterations. The controller should
    // saturate at max_size without panicking, producing NaN, or any
    // other pathological behavior.
    let max = 512 * 1024;
    let ctrl = ControllerConfig::new(50 * 1024 * 1024)
        .min_size(16 * 1024)
        .max_size(max)
        .build();

    let mut now = Instant::now();
    for _ in 0..500 {
        now += Duration::from_millis(100);
        let size = ctrl.observe_at(0, now);
        assert!(size >= ctrl.min_size(), "size below min: {size}");
        assert!(size <= ctrl.max_size(), "size above max: {size}");
    }

    // Should be pinned at max after sustained zero throughput.
    assert_eq!(ctrl.buffer_size(), max);

    // Verify the integrator is clamped (not infinite).
    let state = ctrl.state.lock().unwrap();
    assert!(state.integral.is_finite(), "integral must be finite");
}

#[test]
fn convergence_max_throughput_sustained_no_pathological_behavior() {
    // Feed extremely high throughput (u64::MAX / 2) for many iterations.
    // The controller should saturate at min_size without overflow.
    let min = 16 * 1024;
    let ctrl = ControllerConfig::new(1_000_000)
        .min_size(min)
        .max_size(1024 * 1024)
        .build();

    let mut now = Instant::now();
    for _ in 0..500 {
        now += Duration::from_millis(100);
        let size = ctrl.observe_at(u64::MAX / 2, now);
        assert!(size >= min, "size below min: {size}");
        assert!(size <= ctrl.max_size(), "size above max: {size}");
    }

    assert_eq!(ctrl.buffer_size(), min);

    let state = ctrl.state.lock().unwrap();
    assert!(state.integral.is_finite(), "integral must be finite");
    assert!(state.prev_error.is_finite(), "prev_error must be finite");
}

#[test]
fn convergence_sawtooth_load_bounded_oscillation() {
    // Feed a repeating sawtooth pattern: throughput linearly ramps up
    // then drops to zero, repeating. The controller's oscillation
    // amplitude should remain bounded and not grow over time.
    let setpoint = 50 * 1024 * 1024u64;
    let ctrl = ControllerConfig::new(setpoint)
        .min_size(16 * 1024)
        .max_size(2 * 1024 * 1024)
        .build();

    let mut now = Instant::now();

    // Run 5 sawtooth cycles, 20 samples each.
    let mut cycle_ranges = Vec::new();
    for _cycle in 0..5 {
        let mut sizes = Vec::with_capacity(20);
        for i in 0..20 {
            now += Duration::from_millis(100);
            // Ramp from 0 to 2x setpoint linearly.
            let fraction = i as f64 / 19.0;
            let throughput = (setpoint as f64 * 2.0 * fraction) as u64;
            ctrl.observe_at(throughput, now);
            sizes.push(ctrl.buffer_size());
        }
        let cycle_min = *sizes.iter().min().unwrap();
        let cycle_max = *sizes.iter().max().unwrap();
        cycle_ranges.push(cycle_max - cycle_min);
    }

    // The oscillation range should not grow over successive cycles.
    // Compare the last cycle's range to the first cycle's range.
    let first_range = cycle_ranges[0];
    let last_range = *cycle_ranges.last().unwrap();
    assert!(
        last_range <= first_range + first_range / 10,
        "oscillation should not grow: first_range={first_range}, last_range={last_range}"
    );
}

#[test]
fn convergence_reset_and_reconverge() {
    // Converge the controller, then reset the PID accumulators and
    // verify it re-converges to the same operating point.
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

    // Converge initially.
    for _ in 0..100 {
        now += Duration::from_millis(100);
        let throughput = linear_plant(ctrl.buffer_size(), k, cap);
        ctrl.observe_at(throughput, now);
    }
    let converged_size = ctrl.buffer_size();
    let converged_throughput = linear_plant(converged_size, k, cap);
    let error_before = (converged_throughput as f64 - setpoint as f64).abs() / setpoint as f64;

    // Reset the PID state.
    ctrl.reset();

    // Re-converge.
    for _ in 0..100 {
        now += Duration::from_millis(100);
        let throughput = linear_plant(ctrl.buffer_size(), k, cap);
        ctrl.observe_at(throughput, now);
    }
    let reconverged_size = ctrl.buffer_size();
    let reconverged_throughput = linear_plant(reconverged_size, k, cap);
    let error_after = (reconverged_throughput as f64 - setpoint as f64).abs() / setpoint as f64;

    // Both convergences should reach similar accuracy.
    assert!(
        error_before < 0.15,
        "initial convergence failed: error={error_before:.2}"
    );
    assert!(
        error_after < 0.15,
        "post-reset reconvergence failed: error={error_after:.2}"
    );
}

#[test]
fn convergence_narrow_min_max_window() {
    // When min_size and max_size are very close together, the controller
    // should still converge without oscillating between the two bounds.
    let min = 100 * 1024;
    let max = 120 * 1024;
    let setpoint = 50 * 1024 * 1024u64;

    let ctrl = ControllerConfig::new(setpoint)
        .min_size(min)
        .max_size(max)
        .build();

    let mut now = Instant::now();
    for _ in 0..100 {
        now += Duration::from_millis(100);
        ctrl.observe_at(setpoint, now);
    }

    // With throughput exactly at setpoint, the buffer should stabilize.
    let mut sizes = Vec::with_capacity(20);
    for _ in 0..20 {
        now += Duration::from_millis(100);
        ctrl.observe_at(setpoint, now);
        sizes.push(ctrl.buffer_size());
    }

    // All sizes should be the same (steady state with zero error).
    let unique: std::collections::HashSet<_> = sizes.iter().collect();
    assert!(
        unique.len() <= 2,
        "narrow window should produce stable output, got {} distinct sizes: {:?}",
        unique.len(),
        sizes
    );
}

#[test]
fn convergence_min_equals_max_stays_fixed() {
    // When min_size == max_size, the controller has no room to adjust.
    // Buffer size must always equal that value.
    let fixed = 128 * 1024;
    let ctrl = ControllerConfig::new(50 * 1024 * 1024)
        .min_size(fixed)
        .max_size(fixed)
        .build();

    let mut now = Instant::now();
    for sample in [0u64, 1, 1_000_000, 100_000_000, u64::MAX / 2] {
        now += Duration::from_millis(100);
        let size = ctrl.observe_at(sample, now);
        assert_eq!(
            size, fixed,
            "size must equal fixed bound regardless of input"
        );
    }
}

#[test]
fn convergence_speed_proportional_only() {
    // A proportional-only controller (no I, no D) should converge
    // within a bounded number of iterations for a linear plant.
    let min = 16 * 1024;
    let max = 2 * 1024 * 1024;
    let mid = (min + max) / 2;
    let k = 0.3;
    let setpoint = (k * mid as f64) as u64;
    let cap = setpoint as f64 * 4.0;

    let ctrl = ControllerConfig::new(setpoint)
        .gains(0.5, 0.0, 0.0)
        .min_size(min)
        .max_size(max)
        .build();

    let mut now = Instant::now();
    let mut converged_at = None;
    for i in 0..100 {
        now += Duration::from_millis(100);
        let throughput = linear_plant(ctrl.buffer_size(), k, cap);
        ctrl.observe_at(throughput, now);
        let measured = linear_plant(ctrl.buffer_size(), k, cap);
        let error_pct = (measured as f64 - setpoint as f64).abs() / setpoint as f64;
        if error_pct < 0.05 && converged_at.is_none() {
            converged_at = Some(i);
        }
    }

    // P-only should converge quickly (proportional acts immediately)
    // but may have residual steady-state error. We just check it
    // reaches within 5% at some point.
    assert!(
        converged_at.is_some(),
        "P-only controller should converge within 100 samples"
    );
    assert!(
        converged_at.unwrap() < 30,
        "P-only convergence should be fast, but took {} samples",
        converged_at.unwrap()
    );
}

#[test]
fn convergence_speed_full_pid_faster_than_integral_only() {
    // Full PID should converge at least as fast as pure I controller
    // for the same plant model.
    let min = 16 * 1024;
    let max = 2 * 1024 * 1024;
    let mid = (min + max) / 2;
    let k = 0.3;
    let setpoint = (k * mid as f64) as u64;
    let cap = setpoint as f64 * 4.0;

    let t0 = Instant::now();

    let ctrl_pid = ControllerConfig::new(setpoint)
        .gains(0.3, 0.1, 0.02)
        .min_size(min)
        .max_size(max)
        .build();
    let ctrl_i_only = ControllerConfig::new(setpoint)
        .gains(0.0, 0.1, 0.0)
        .min_size(min)
        .max_size(max)
        .build();

    let tolerance = 0.10;
    let max_samples = 200;

    let find_convergence = |ctrl: &AdaptiveBufferController, start: Instant| -> Option<usize> {
        let mut t = start;
        for i in 0..max_samples {
            t += Duration::from_millis(100);
            let throughput = linear_plant(ctrl.buffer_size(), k, cap);
            ctrl.observe_at(throughput, t);
            let measured = linear_plant(ctrl.buffer_size(), k, cap);
            let error_pct = (measured as f64 - setpoint as f64).abs() / setpoint as f64;
            if error_pct < tolerance {
                return Some(i);
            }
        }
        None
    };

    let pid_samples = find_convergence(&ctrl_pid, t0);
    let i_samples = find_convergence(&ctrl_i_only, t0);

    assert!(
        pid_samples.is_some(),
        "PID controller should converge within {max_samples} samples"
    );
    assert!(
        i_samples.is_some(),
        "I-only controller should converge within {max_samples} samples"
    );
    assert!(
        pid_samples.unwrap() <= i_samples.unwrap(),
        "PID should converge at least as fast as I-only: PID={}, I-only={}",
        pid_samples.unwrap(),
        i_samples.unwrap()
    );
}
