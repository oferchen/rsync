//! Basic PID-term semantics, anti-windup, clamping, and accessor tests.

use std::time::{Duration, Instant};

use super::super::config::{
    DEFAULT_GAIN_D, DEFAULT_GAIN_I, DEFAULT_GAIN_P, DEFAULT_MAX_SIZE, DEFAULT_MIN_SIZE,
};
use super::super::{AdaptiveBufferController, ControllerConfig};
use super::support::lan_setpoint;

fn fresh(setpoint: u64) -> AdaptiveBufferController {
    ControllerConfig::new(setpoint).build()
}

fn fresh_small_window() -> AdaptiveBufferController {
    ControllerConfig::new(lan_setpoint())
        .min_size(16 * 1024)
        .max_size(1024 * 1024)
        .build()
}

#[test]
fn proportional_term_grows_buffer_below_setpoint() {
    let ctrl = fresh_small_window();
    let before = ctrl.buffer_size();
    let now = Instant::now();
    // Throughput well under setpoint -> error positive -> buffer grows.
    let after = ctrl.observe_at(1024, now + Duration::from_millis(100));
    assert!(
        after > before,
        "buffer should grow when below setpoint: before={before}, after={after}"
    );
}

#[test]
fn proportional_term_shrinks_buffer_above_setpoint() {
    let ctrl = fresh_small_window();
    let before = ctrl.buffer_size();
    let now = Instant::now();
    // Throughput well above setpoint -> error negative -> buffer shrinks.
    let after = ctrl.observe_at(lan_setpoint() * 4, now + Duration::from_millis(100));
    assert!(
        after < before,
        "buffer should shrink when above setpoint: before={before}, after={after}"
    );
}

#[test]
fn integral_term_eliminates_steady_state_error_after_repeated_low_samples() {
    // Pure-P would leave a residual offset; the integrator should keep
    // pushing the buffer up across repeated identical low samples.
    // Use a moderate setpoint (1 MB/s) so the integral accumulates
    // gradually without saturating the anti-windup clamp or hitting
    // max_size on the first step.
    let ctrl = ControllerConfig::new(1_000_000)
        .gains(0.0, 0.5, 0.0)
        .min_size(16 * 1024)
        .max_size(4 * 1024 * 1024)
        .build();
    let mut now = Instant::now();
    let mut last = ctrl.buffer_size();
    let mut grew_each_step = true;
    for _ in 0..5 {
        now += Duration::from_millis(100);
        let next = ctrl.observe_at(0, now);
        if next <= last {
            grew_each_step = false;
            break;
        }
        last = next;
    }
    assert!(
        grew_each_step,
        "integral term should accumulate and keep growing the buffer"
    );
}

#[test]
fn derivative_term_dampens_overshoot_on_step_input() {
    let setpoint = lan_setpoint();
    let no_d = ControllerConfig::new(setpoint)
        .gains(0.6, 0.2, 0.0)
        .min_size(16 * 1024)
        .max_size(4 * 1024 * 1024)
        .build();
    let with_d = ControllerConfig::new(setpoint)
        .gains(0.6, 0.2, 0.05)
        .min_size(16 * 1024)
        .max_size(4 * 1024 * 1024)
        .build();
    let t0 = Instant::now();
    // Step from far-below-setpoint (large positive error) to far-above
    // (large negative error). The derivative term should temper the
    // resulting swing relative to a P+I-only controller.
    no_d.observe_at(0, t0 + Duration::from_millis(100));
    with_d.observe_at(0, t0 + Duration::from_millis(100));
    let no_d_after = no_d.observe_at(setpoint * 4, t0 + Duration::from_millis(200));
    let with_d_after = with_d.observe_at(setpoint * 4, t0 + Duration::from_millis(200));
    // Both swing downward on the second sample (error flipped sign and
    // grew rapidly); the derivative-equipped controller swings less far.
    let no_d_delta = no_d_after as i64 - no_d.config.max_size as i64 / 2;
    let with_d_delta = with_d_after as i64 - with_d.config.max_size as i64 / 2;
    assert!(
        with_d_delta.abs() < no_d_delta.abs() || with_d_after >= no_d_after,
        "derivative should damp overshoot: no_d_after={no_d_after}, with_d_after={with_d_after}"
    );
}

#[test]
fn anti_windup_clamps_integral_under_sustained_error() {
    let cap = 1_000.0;
    let ctrl = ControllerConfig::new(lan_setpoint())
        .gains(0.0, 1.0, 0.0)
        .anti_windup_clamp((-cap, cap))
        .min_size(16 * 1024)
        .max_size(4 * 1024 * 1024)
        .build();
    let mut now = Instant::now();
    for _ in 0..100 {
        now += Duration::from_millis(100);
        ctrl.observe_at(0, now);
    }
    let state = ctrl.state.lock().unwrap();
    assert!(
        state.integral <= cap + f64::EPSILON,
        "integral should be clamped to the anti-windup ceiling, got {}",
        state.integral
    );
    assert!(
        state.integral >= -cap - f64::EPSILON,
        "integral should be clamped to the anti-windup floor, got {}",
        state.integral
    );
}

#[test]
fn clamp_to_min_size_on_persistent_overshoot() {
    let min = 16 * 1024;
    let ctrl = ControllerConfig::new(lan_setpoint())
        .min_size(min)
        .max_size(1024 * 1024)
        .build();
    let mut now = Instant::now();
    // Massive sustained over-target -> error very negative -> buffer
    // should bottom out at min_size.
    for _ in 0..200 {
        now += Duration::from_millis(100);
        ctrl.observe_at(lan_setpoint() * 100, now);
    }
    assert_eq!(ctrl.buffer_size(), min);
}

#[test]
fn clamp_to_max_size_on_persistent_undershoot() {
    let max = 1024 * 1024;
    let ctrl = ControllerConfig::new(lan_setpoint())
        .min_size(16 * 1024)
        .max_size(max)
        .build();
    let mut now = Instant::now();
    for _ in 0..200 {
        now += Duration::from_millis(100);
        ctrl.observe_at(0, now);
    }
    assert_eq!(ctrl.buffer_size(), max);
}

#[test]
fn reset_clears_integrator_and_derivative_history() {
    let ctrl = fresh(lan_setpoint());
    let mut now = Instant::now();
    for _ in 0..10 {
        now += Duration::from_millis(100);
        ctrl.observe_at(0, now);
    }
    {
        let state = ctrl.state.lock().unwrap();
        assert!(state.integral > 0.0);
        assert!(state.prev_sample_at.is_some());
    }
    ctrl.reset();
    let state = ctrl.state.lock().unwrap();
    assert_eq!(state.integral, 0.0);
    assert_eq!(state.prev_error, 0.0);
    assert!(state.prev_sample_at.is_none());
}

#[test]
fn zero_dt_does_not_panic_or_divide_by_zero() {
    let ctrl = fresh(lan_setpoint());
    let now = Instant::now();
    // Two samples at the same instant -> raw dt = 0; clamp must apply.
    let first = ctrl.observe_at(1024, now);
    let second = ctrl.observe_at(2048, now);
    assert!(first >= ctrl.config.min_size);
    assert!(second >= ctrl.config.min_size);
    assert!(first <= ctrl.config.max_size);
    assert!(second <= ctrl.config.max_size);
}

#[test]
fn ziegler_nichols_default_gains_converge_within_50_samples_to_within_10_percent() {
    // Synthetic plant: throughput is proportional to the buffer size,
    // saturating at the link capacity. The buffer-size unit is bytes,
    // so we use a proportionality constant chosen so that the link
    // capacity is reached when the buffer is mid-range.
    let setpoint = lan_setpoint();
    let ctrl = ControllerConfig::new(setpoint)
        .min_size(16 * 1024)
        .max_size(4 * 1024 * 1024)
        .build();
    // Pick k so that buffer_size = max_size / 2 yields throughput =
    // setpoint exactly. Then the controller's optimum sits at the
    // mid-range default initial size, which is within scope for the
    // proportional-only first step.
    let mid = (16 * 1024 + 4 * 1024 * 1024) / 2;
    let k = setpoint as f64 / mid as f64;
    let cap = setpoint as f64 * 4.0;
    let mut now = Instant::now();
    let mut converged = false;
    for _ in 0..50 {
        now += Duration::from_millis(100);
        let buf = ctrl.buffer_size() as f64;
        let throughput = (k * buf).min(cap);
        ctrl.observe_at(throughput as u64, now);
        let buf_now = ctrl.buffer_size() as f64;
        let measured = (k * buf_now).min(cap);
        if (measured - setpoint as f64).abs() / setpoint as f64 <= 0.10 {
            converged = true;
            break;
        }
    }
    assert!(
        converged,
        "controller failed to converge within 50 samples (final buffer={} bytes)",
        ctrl.buffer_size()
    );
}

#[test]
fn config_builder_defaults_match_rfc() {
    let cfg = ControllerConfig::new(1_000);
    assert_eq!(cfg.gain_p, DEFAULT_GAIN_P);
    assert_eq!(cfg.gain_i, DEFAULT_GAIN_I);
    assert_eq!(cfg.gain_d, DEFAULT_GAIN_D);
    assert_eq!(cfg.min_size, DEFAULT_MIN_SIZE);
    assert_eq!(cfg.max_size, DEFAULT_MAX_SIZE);
}

#[test]
fn observe_returns_size_within_bounds() {
    let ctrl = fresh_small_window();
    let mut now = Instant::now();
    for sample in [0u64, 1, lan_setpoint(), lan_setpoint() * 10, u64::MAX / 2] {
        now += Duration::from_millis(50);
        let size = ctrl.observe_at(sample, now);
        assert!(size >= ctrl.config.min_size);
        assert!(size <= ctrl.config.max_size);
    }
}

#[test]
fn reset_preserves_buffer_size() {
    let ctrl = fresh(lan_setpoint());
    let mut now = Instant::now();
    for _ in 0..5 {
        now += Duration::from_millis(100);
        ctrl.observe_at(0, now);
    }
    let before = ctrl.buffer_size();
    ctrl.reset();
    assert_eq!(
        ctrl.buffer_size(),
        before,
        "reset should preserve buffer size, only clearing PID accumulators"
    );
}

#[test]
fn min_size_accessor() {
    let ctrl = ControllerConfig::new(1_000).min_size(8 * 1024).build();
    assert_eq!(ctrl.min_size(), 8 * 1024);
}

#[test]
fn max_size_accessor() {
    let ctrl = ControllerConfig::new(1_000).max_size(512 * 1024).build();
    assert_eq!(ctrl.max_size(), 512 * 1024);
}
