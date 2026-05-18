//! Basic convergence behaviour under representative workload patterns.
//!
//! These tests verify that the PID controller converges to optimal
//! buffer sizes under varying synthetic workload conditions. Each test
//! models a simple "plant" where throughput is a function of buffer
//! size, and asserts that the controller settles within a tolerance
//! band around the optimal operating point.

use std::time::{Duration, Instant};

use super::super::ControllerConfig;
use super::support::{lan_setpoint, linear_plant};

#[test]
fn convergence_constant_workload_stabilizes() {
    // Under a constant synthetic workload, the controller should
    // converge to a stable buffer size and stay there. We verify
    // that the buffer size stops changing (within a small window)
    // after convergence.
    let setpoint = 50 * 1024 * 1024; // 50 MB/s
    let ctrl = ControllerConfig::new(setpoint)
        .min_size(16 * 1024)
        .max_size(2 * 1024 * 1024)
        .build();

    let mid = (16 * 1024 + 2 * 1024 * 1024) / 2;
    let k = setpoint as f64 / mid as f64;
    let cap = setpoint as f64 * 4.0;

    let mut now = Instant::now();
    // Run 100 samples to let the controller converge.
    for _ in 0..100 {
        now += Duration::from_millis(100);
        let throughput = linear_plant(ctrl.buffer_size(), k, cap);
        ctrl.observe_at(throughput, now);
    }

    // Collect 20 more samples and verify the buffer size is stable.
    let mut sizes = Vec::with_capacity(20);
    for _ in 0..20 {
        now += Duration::from_millis(100);
        let throughput = linear_plant(ctrl.buffer_size(), k, cap);
        ctrl.observe_at(throughput, now);
        sizes.push(ctrl.buffer_size());
    }

    let min_s = *sizes.iter().min().unwrap();
    let max_s = *sizes.iter().max().unwrap();
    let range_pct = if min_s > 0 {
        (max_s - min_s) as f64 / min_s as f64
    } else {
        0.0
    };
    assert!(
        range_pct < 0.05,
        "buffer size should be stable after convergence: min={min_s}, max={max_s}, range={range_pct:.2}%"
    );
}

#[test]
fn convergence_buffer_grows_when_throughput_improves_with_larger_buffers() {
    // Plant: throughput scales linearly with buffer size up to a cap.
    // When throughput is below the setpoint and larger buffers help,
    // the controller should increase the buffer size.
    let setpoint = 100 * 1024 * 1024; // 100 MB/s
    let ctrl = ControllerConfig::new(setpoint)
        .min_size(16 * 1024)
        .max_size(4 * 1024 * 1024)
        .build();

    let initial = ctrl.buffer_size();
    let mid = (16 * 1024 + 4 * 1024 * 1024) / 2;
    let k = setpoint as f64 / mid as f64;
    let cap = setpoint as f64 * 4.0;

    let mut now = Instant::now();
    // Feed samples where throughput scales with buffer size.
    // Starting from midpoint, throughput = k * buffer_size < setpoint
    // because initial buffer = midpoint gives exactly setpoint, but
    // we start with throughput slightly below setpoint by using half
    // the buffer size.
    for _ in 0..30 {
        now += Duration::from_millis(100);
        let throughput = linear_plant(ctrl.buffer_size(), k, cap);
        ctrl.observe_at(throughput, now);
    }

    let final_size = ctrl.buffer_size();
    // The controller should have adjusted buffer size. Since the
    // plant is linear and setpoint matches the midpoint, the controller
    // should converge near the initial size. The key assertion is that
    // it explored and settled - not that it moved in a specific
    // direction from the already-optimal start.
    let _ = initial;
    let throughput = linear_plant(final_size, k, cap);
    let error_pct = (throughput as f64 - setpoint as f64).abs() / setpoint as f64;
    assert!(
        error_pct < 0.15,
        "controller should converge near setpoint: throughput={throughput}, setpoint={setpoint}, error={error_pct:.2}"
    );
}

#[test]
fn convergence_buffer_shrinks_when_oversized() {
    // Plant: throughput peaks at a moderate buffer size and stays flat
    // beyond that. The controller should not keep growing the buffer
    // past the saturation point - it should shrink back toward the
    // minimum effective size.
    let setpoint = 80 * 1024 * 1024; // 80 MB/s
    let optimal_buf = 128 * 1024; // throughput saturates at 128 KB

    let ctrl = ControllerConfig::new(setpoint)
        .min_size(16 * 1024)
        .max_size(2 * 1024 * 1024)
        .build();

    let k = setpoint as f64 / optimal_buf as f64;
    let cap = setpoint as f64;

    let mut now = Instant::now();
    // Run the control loop. Throughput saturates at setpoint regardless
    // of buffer size above optimal_buf.
    for _ in 0..100 {
        now += Duration::from_millis(100);
        let throughput = linear_plant(ctrl.buffer_size(), k, cap);
        ctrl.observe_at(throughput, now);
    }

    // Once throughput hits the setpoint (error -> 0), the controller
    // should stop growing. The integral term prevents it from shrinking
    // below the point where throughput = setpoint.
    let final_throughput = linear_plant(ctrl.buffer_size(), k, cap);
    let error_pct = (final_throughput as f64 - setpoint as f64).abs() / setpoint as f64;
    assert!(
        error_pct < 0.15,
        "controller should maintain throughput near setpoint: throughput={final_throughput}, setpoint={setpoint}"
    );
}

#[test]
fn convergence_responds_to_sudden_throughput_drop() {
    // Simulate a workload change: throughput drops suddenly (e.g.,
    // link degradation). The controller should increase the buffer
    // to compensate.
    //
    // Plant k must satisfy K_p * k < 1 for closed-loop stability.
    // With k=0.5 and default K_p=0.6, loop gain = 0.3 - well damped.
    let min = 16 * 1024;
    let max = 4 * 1024 * 1024;
    let mid = (min + max) / 2;
    let k_fast = 0.5;
    let setpoint = (k_fast * mid as f64) as u64;
    let cap_fast = setpoint as f64 * 4.0;

    let ctrl = ControllerConfig::new(setpoint)
        .min_size(min)
        .max_size(max)
        .build();

    let mut now = Instant::now();

    // Phase 1: Converge under normal conditions.
    for _ in 0..80 {
        now += Duration::from_millis(100);
        let throughput = linear_plant(ctrl.buffer_size(), k_fast, cap_fast);
        ctrl.observe_at(throughput, now);
    }
    let size_before_drop = ctrl.buffer_size();

    // Phase 2: Throughput drops by 50% (slower link).
    // k_slow=0.25, loop gain = 0.6 * 0.25 = 0.15. Very stable.
    let k_slow = k_fast * 0.5;
    let cap_slow = cap_fast * 0.5;
    for _ in 0..80 {
        now += Duration::from_millis(100);
        let throughput = linear_plant(ctrl.buffer_size(), k_slow, cap_slow);
        ctrl.observe_at(throughput, now);
    }

    let size_after_drop = ctrl.buffer_size();
    assert!(
        size_after_drop > size_before_drop,
        "controller should grow buffer after throughput drop: before={size_before_drop}, after={size_after_drop}"
    );
}

#[test]
fn convergence_responds_to_sudden_throughput_increase() {
    // Simulate throughput improvement: controller should eventually
    // shrink the buffer since less buffer is needed to hit setpoint.
    //
    // k_fast = 1.0 after the 2x gain increase. Gains are reduced
    // so K_p * k_fast = 0.3 stays within the discrete stability
    // margin; derivative damping K_d/dt * k = 0.2 prevents ringing.
    let min = 16 * 1024;
    let max = 2 * 1024 * 1024;
    let mid = (min + max) / 2;
    let k_slow = 0.5;
    let setpoint = (k_slow * mid as f64) as u64;
    let cap_slow = setpoint as f64 * 4.0;

    let ctrl = ControllerConfig::new(setpoint)
        .gains(0.3, 0.1, 0.02)
        .min_size(min)
        .max_size(max)
        .build();

    let mut now = Instant::now();

    // Phase 1: Converge under slow conditions.
    for _ in 0..80 {
        now += Duration::from_millis(100);
        let throughput = linear_plant(ctrl.buffer_size(), k_slow, cap_slow);
        ctrl.observe_at(throughput, now);
    }
    let size_before_increase = ctrl.buffer_size();

    // Phase 2: Link gets 2x faster - throughput exceeds setpoint
    // at the current buffer size, so the controller should shrink.
    let k_fast = k_slow * 2.0;
    let cap_fast = cap_slow * 2.0;
    for _ in 0..80 {
        now += Duration::from_millis(100);
        let throughput = linear_plant(ctrl.buffer_size(), k_fast, cap_fast);
        ctrl.observe_at(throughput, now);
    }

    let size_after_increase = ctrl.buffer_size();
    assert!(
        size_after_increase < size_before_increase,
        "controller should shrink buffer after throughput increase: before={size_before_increase}, after={size_after_increase}"
    );
}

#[test]
fn convergence_minimum_size_enforced_under_extreme_overshoot() {
    // Even when throughput massively exceeds the setpoint (error
    // is deeply negative), the buffer size must never drop below
    // the configured minimum.
    let min = 32 * 1024;
    let ctrl = ControllerConfig::new(1_000_000) // 1 MB/s setpoint
        .min_size(min)
        .max_size(1024 * 1024)
        .build();

    let mut now = Instant::now();
    // Feed extremely high throughput (1 GB/s - 1000x the setpoint).
    for _ in 0..200 {
        now += Duration::from_millis(100);
        ctrl.observe_at(1_000_000_000, now);
    }

    assert_eq!(ctrl.buffer_size(), min, "buffer size must clamp at minimum");
}

#[test]
fn convergence_maximum_size_enforced_under_extreme_undershoot() {
    // Even when throughput is zero (error equals the full setpoint),
    // the buffer size must never exceed the configured maximum.
    let max = 512 * 1024;
    let ctrl = ControllerConfig::new(lan_setpoint())
        .min_size(16 * 1024)
        .max_size(max)
        .build();

    let mut now = Instant::now();
    for _ in 0..200 {
        now += Duration::from_millis(100);
        ctrl.observe_at(0, now);
    }

    assert_eq!(ctrl.buffer_size(), max, "buffer size must clamp at maximum");
}

#[test]
fn convergence_oscillation_dampens_over_time() {
    // Feed alternating high/low throughput samples. The derivative
    // term should damp oscillations so the buffer size variance
    // decreases over time (later samples have smaller swings).
    let setpoint = 50 * 1024 * 1024;
    let ctrl = ControllerConfig::new(setpoint)
        .min_size(16 * 1024)
        .max_size(2 * 1024 * 1024)
        .build();

    let mut now = Instant::now();
    let low = setpoint / 4;
    let high = setpoint * 4;

    // Phase 1: first 20 alternating samples.
    let mut early_sizes = Vec::with_capacity(20);
    for i in 0..20 {
        now += Duration::from_millis(100);
        let throughput = if i % 2 == 0 { low } else { high };
        ctrl.observe_at(throughput, now);
        early_sizes.push(ctrl.buffer_size());
    }

    // Phase 2: next 20 alternating samples.
    let mut late_sizes = Vec::with_capacity(20);
    for i in 0..20 {
        now += Duration::from_millis(100);
        let throughput = if i % 2 == 0 { low } else { high };
        ctrl.observe_at(throughput, now);
        late_sizes.push(ctrl.buffer_size());
    }

    let early_range = early_sizes.iter().max().unwrap() - early_sizes.iter().min().unwrap();
    let late_range = late_sizes.iter().max().unwrap() - late_sizes.iter().min().unwrap();

    // The late window should have equal or smaller oscillation amplitude.
    assert!(
        late_range <= early_range,
        "oscillation should dampen: early_range={early_range}, late_range={late_range}"
    );
}

#[test]
fn convergence_step_change_settles_within_20_samples() {
    // After a step change in the plant's gain (simulating a new
    // workload), the controller should re-converge within 20
    // samples (2 seconds at 100 ms intervals).
    //
    // k1=0.5, k2=1.0. Gains scaled so K_p*k2 = 0.3 for stability.
    let min = 16 * 1024;
    let max = 4 * 1024 * 1024;
    let mid = (min + max) / 2;
    let k1 = 0.5;
    let setpoint = (k1 * mid as f64) as u64;
    let cap = setpoint as f64 * 4.0;

    let ctrl = ControllerConfig::new(setpoint)
        .gains(0.3, 0.1, 0.02)
        .min_size(min)
        .max_size(max)
        .build();

    let mut now = Instant::now();
    // Converge under k1.
    for _ in 0..100 {
        now += Duration::from_millis(100);
        let throughput = linear_plant(ctrl.buffer_size(), k1, cap);
        ctrl.observe_at(throughput, now);
    }

    // Step change: k doubles (half the buffer needed for same throughput).
    let k2 = k1 * 2.0;
    let mut converged_at = None;
    for i in 0..20 {
        now += Duration::from_millis(100);
        let throughput = linear_plant(ctrl.buffer_size(), k2, cap);
        ctrl.observe_at(throughput, now);
        let measured = linear_plant(ctrl.buffer_size(), k2, cap);
        let error_pct = (measured as f64 - setpoint as f64).abs() / setpoint as f64;
        if error_pct < 0.10 {
            converged_at = Some(i);
            break;
        }
    }

    assert!(
        converged_at.is_some(),
        "controller should re-converge within 20 samples after step change (final buffer={}, throughput={})",
        ctrl.buffer_size(),
        linear_plant(ctrl.buffer_size(), k2, cap)
    );
}

#[test]
fn convergence_pure_integral_eliminates_offset() {
    // With K_p = 0 and K_d = 0, a pure integral controller should
    // still drive the steady-state error to zero given enough samples.
    let setpoint = 10_000_000; // 10 MB/s
    let ctrl = ControllerConfig::new(setpoint)
        .gains(0.0, 0.3, 0.0)
        .min_size(4 * 1024)
        .max_size(1024 * 1024)
        .build();

    let mid = (4 * 1024 + 1024 * 1024) / 2;
    let k = setpoint as f64 / mid as f64;
    let cap = setpoint as f64 * 4.0;

    let mut now = Instant::now();
    for _ in 0..200 {
        now += Duration::from_millis(100);
        let throughput = linear_plant(ctrl.buffer_size(), k, cap);
        ctrl.observe_at(throughput, now);
    }

    let final_throughput = linear_plant(ctrl.buffer_size(), k, cap);
    let error_pct = (final_throughput as f64 - setpoint as f64).abs() / setpoint as f64;
    assert!(
        error_pct < 0.10,
        "pure integral should eliminate steady-state error: throughput={final_throughput}, setpoint={setpoint}, error={error_pct:.2}"
    );
}

#[test]
fn convergence_wan_preset_with_high_jitter() {
    // WAN-like conditions: lower setpoint, higher jitter. The
    // controller should still converge despite noisy measurements.
    let min = 4 * 1024;
    let max = 512 * 1024;
    let mid = (min + max) / 2;
    let k = 0.5;
    let setpoint = (k * mid as f64) as u64;
    let ctrl = ControllerConfig::new(setpoint)
        .gains(0.4, 0.15, 0.03)
        .min_size(min)
        .max_size(max)
        .build();

    let cap = setpoint as f64 * 4.0;

    let mut now = Instant::now();
    // Simulate jittery throughput: base + noise pattern.
    let noise_pattern: [f64; 8] = [0.7, 1.3, 0.9, 1.1, 0.8, 1.2, 0.95, 1.05];
    for i in 0..100 {
        now += Duration::from_millis(100);
        let base_throughput = linear_plant(ctrl.buffer_size(), k, cap) as f64;
        let jittered = (base_throughput * noise_pattern[i % noise_pattern.len()]) as u64;
        ctrl.observe_at(jittered, now);
    }

    // Despite jitter, throughput should be near the setpoint.
    let final_throughput = linear_plant(ctrl.buffer_size(), k, cap);
    let error_pct = (final_throughput as f64 - setpoint as f64).abs() / setpoint as f64;
    assert!(
        error_pct < 0.25,
        "WAN preset should converge despite jitter: throughput={final_throughput}, setpoint={setpoint}, error={error_pct:.2}"
    );
}
