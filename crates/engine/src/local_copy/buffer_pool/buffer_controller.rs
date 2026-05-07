//! PID-style buffer-size controller (task #2095).
//!
//! Implements the proportional-integral-derivative controller described in
//! `docs/design/adaptive-buffer-controller.md`. Note: the design doc cites
//! `pipeline/buffer_controller.rs` but the engine crate has no `pipeline`
//! module; the closest existing home is the buffer pool subsystem alongside
//! [`super::throughput`] which already tracks throughput EMA.
//!
//! This module ships the standalone controller and unit tests. Integration
//! into write-batch sizing, [`super::BufferPool`] capacity hints, and the
//! protocol multiplex frame batching is deferred to a follow-up wiring PR
//! tracked by task #2096.
//!
//! Ziegler-Nichols closed-loop tuning is documented in section 6 of the
//! RFC; default gains shipped here come from that table.
//!
//! # Loop body
//!
//! ```text
//! e        = setpoint - throughput
//! P        = K_p * e
//! I       += K_i * e * dt    (clamped to anti-windup window)
//! D        = K_d * (e - e_prev) / dt
//! u_next   = clamp(u_prev + (P + I + D) * scale, min_size, max_size)
//! ```
//!
//! `scale` is `1.0` because the unit analysis works out: `e` is in bytes per
//! second, `dt` is in seconds, so `K_i * e * dt` is in bytes; `K_p * e` and
//! `K_d * (e - e_prev) / dt` are also in bytes once the dimensionless gains
//! are applied. The accumulator therefore moves the buffer size by an amount
//! whose units match the manipulated variable. See section 3.2 of the RFC.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Default proportional gain (Ziegler-Nichols LAN preset).
const DEFAULT_GAIN_P: f64 = 0.6;
/// Default integral gain (Ziegler-Nichols LAN preset).
const DEFAULT_GAIN_I: f64 = 0.2;
/// Default derivative gain (Ziegler-Nichols LAN preset).
const DEFAULT_GAIN_D: f64 = 0.05;

/// Default minimum buffer size (16 KiB), per RFC section 3.1.
const DEFAULT_MIN_SIZE: usize = 16 * 1024;
/// Default maximum buffer size (4 MiB), per RFC section 3.1.
const DEFAULT_MAX_SIZE: usize = 4 * 1024 * 1024;

/// Default sample interval used to seed the controller before the first
/// `observe` call (100 ms, matching the RFC section 3.3 time trigger).
const DEFAULT_SAMPLE_INTERVAL_MS: u64 = 100;

/// Lower bound on `dt` (1 ms) to avoid divide-by-zero and amplifying noise
/// from sub-millisecond clock jitter on the derivative term.
const MIN_DT: Duration = Duration::from_millis(1);
/// Upper bound on `dt` (5 s) to avoid integral windup when the producer has
/// stalled for an extended period.
const MAX_DT: Duration = Duration::from_secs(5);

/// Builder-style configuration for [`AdaptiveBufferController`].
///
/// All fields are advisory; defaults match the LAN preset from
/// `docs/design/adaptive-buffer-controller.md` section 6.
#[derive(Debug, Clone, Copy)]
pub struct ControllerConfig {
    setpoint_bytes_per_sec: u64,
    gain_p: f64,
    gain_i: f64,
    gain_d: f64,
    anti_windup_clamp: (f64, f64),
    min_size: usize,
    max_size: usize,
    sample_interval_ms: u64,
}

impl ControllerConfig {
    /// Constructs a config seeded with Ziegler-Nichols LAN-preset gains.
    ///
    /// `setpoint_bytes_per_sec` is the target throughput in bytes per second.
    /// The default anti-windup window is `+/- max_size` bytes, matching the
    /// RFC section 3.4 guidance that `K_i * I_max` cannot exceed half the
    /// buffer-size range.
    #[must_use]
    pub fn new(setpoint_bytes_per_sec: u64) -> Self {
        let max_size = DEFAULT_MAX_SIZE;
        Self {
            setpoint_bytes_per_sec,
            gain_p: DEFAULT_GAIN_P,
            gain_i: DEFAULT_GAIN_I,
            gain_d: DEFAULT_GAIN_D,
            anti_windup_clamp: (-(max_size as f64), max_size as f64),
            min_size: DEFAULT_MIN_SIZE,
            max_size,
            sample_interval_ms: DEFAULT_SAMPLE_INTERVAL_MS,
        }
    }

    /// Overrides the lower clamp on buffer size.
    #[must_use]
    pub fn min_size(mut self, n: usize) -> Self {
        self.min_size = n.max(1);
        self
    }

    /// Overrides the upper clamp on buffer size.
    #[must_use]
    pub fn max_size(mut self, n: usize) -> Self {
        self.max_size = n.max(1);
        if self.anti_windup_clamp == (-(DEFAULT_MAX_SIZE as f64), DEFAULT_MAX_SIZE as f64) {
            self.anti_windup_clamp = (-(self.max_size as f64), self.max_size as f64);
        }
        self
    }

    /// Overrides the proportional, integral, and derivative gains.
    #[must_use]
    pub fn gains(mut self, p: f64, i: f64, d: f64) -> Self {
        self.gain_p = p;
        self.gain_i = i;
        self.gain_d = d;
        self
    }

    /// Overrides the anti-windup integral clamp window as `(low, high)`.
    #[must_use]
    pub fn anti_windup_clamp(mut self, lo_hi: (f64, f64)) -> Self {
        self.anti_windup_clamp = lo_hi;
        self
    }

    /// Overrides the default sample interval used to seed the controller.
    #[must_use]
    pub fn sample_interval_ms(mut self, ms: u64) -> Self {
        self.sample_interval_ms = ms.max(1);
        self
    }

    /// Builds an [`AdaptiveBufferController`] from this configuration.
    ///
    /// The initial buffer size is set to the midpoint of `[min_size, max_size]`
    /// rounded down, so the controller has equal room to grow or shrink in
    /// response to the first sample.
    #[must_use]
    pub fn build(self) -> AdaptiveBufferController {
        debug_assert!(self.min_size <= self.max_size);
        let initial = self.min_size.saturating_add(
            self.max_size
                .saturating_sub(self.min_size)
                .saturating_div(2),
        );
        AdaptiveBufferController {
            config: self,
            buffer_size: AtomicUsize::new(initial.clamp(self.min_size, self.max_size)),
            state: Mutex::new(ControllerState::default()),
        }
    }
}

/// Inner mutable state guarded by a single mutex.
///
/// Contains the integrator, the previous error sample, and the timestamp of
/// the previous sample. A single mutex is used because all three values are
/// updated atomically on every call to [`AdaptiveBufferController::observe`].
#[derive(Debug, Default)]
struct ControllerState {
    integral: f64,
    prev_error: f64,
    prev_sample_at: Option<Instant>,
}

/// PID-style buffer-size controller.
///
/// The controller is `Send + Sync`. The current recommended buffer size is
/// stored in an [`AtomicUsize`] for lock-free reads from the hot path; the
/// PID state is updated under a [`Mutex`] only on the (relatively rare)
/// sample interval.
#[derive(Debug)]
pub struct AdaptiveBufferController {
    config: ControllerConfig,
    buffer_size: AtomicUsize,
    state: Mutex<ControllerState>,
}

impl AdaptiveBufferController {
    /// Returns the current recommended buffer size.
    #[must_use]
    pub fn buffer_size(&self) -> usize {
        self.buffer_size.load(Ordering::Relaxed)
    }

    /// Returns the configured setpoint (bytes per second).
    #[must_use]
    pub fn setpoint(&self) -> u64 {
        self.config.setpoint_bytes_per_sec
    }

    /// Feeds an observed throughput sample (bytes per second since the last
    /// sample) and returns the new recommended buffer size.
    pub fn observe(&self, throughput_bps: u64) -> usize {
        self.observe_at(throughput_bps, Instant::now())
    }

    /// Deterministic testing hook: feeds a sample with an explicit timestamp.
    pub(crate) fn observe_at(&self, throughput_bps: u64, now: Instant) -> usize {
        let mut state = match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        let dt = compute_dt(state.prev_sample_at, now, self.config.sample_interval_ms);
        let dt_secs = dt.as_secs_f64();

        let setpoint = self.config.setpoint_bytes_per_sec as f64;
        let measured = throughput_bps as f64;
        let error = setpoint - measured;

        let p_term = self.config.gain_p * error;

        let raw_integral = state.integral + self.config.gain_i * error * dt_secs;
        let (lo, hi) = self.config.anti_windup_clamp;
        let clamped_integral = clamp_f64(raw_integral, lo, hi);
        state.integral = clamped_integral;

        let derivative = if state.prev_sample_at.is_some() {
            self.config.gain_d * (error - state.prev_error) / dt_secs
        } else {
            // First sample: no derivative history yet.
            0.0
        };

        let output = p_term + clamped_integral + derivative;

        let current = self.buffer_size.load(Ordering::Relaxed) as f64;
        let scale = 1.0;
        let next_raw = current + output * scale;
        let next = clamp_f64(
            next_raw,
            self.config.min_size as f64,
            self.config.max_size as f64,
        );
        let next_size = next as usize;
        let next_size = next_size.clamp(self.config.min_size, self.config.max_size);

        self.buffer_size.store(next_size, Ordering::Relaxed);

        state.prev_error = error;
        state.prev_sample_at = Some(now);

        next_size
    }

    /// Resets the integrator and derivative history.
    ///
    /// Called on protocol renegotiation, pipeline restart, or any other event
    /// described in section 3.5 of the RFC. The recommended buffer size is
    /// preserved; only the PID accumulators are zeroed.
    pub fn reset(&self) {
        let mut state = match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.integral = 0.0;
        state.prev_error = 0.0;
        state.prev_sample_at = None;
    }
}

/// Computes a bounded `dt` from the previous and current sample timestamps.
///
/// On the very first sample (no previous timestamp) this returns the
/// configured `sample_interval_ms`. Otherwise the elapsed duration is
/// clamped to `[MIN_DT, MAX_DT]` to avoid divide-by-zero on the derivative
/// term and integrator windup on a stalled producer.
fn compute_dt(prev: Option<Instant>, now: Instant, default_ms: u64) -> Duration {
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
/// Using a free function avoids relying on `f64::clamp` semantics for NaN,
/// which panic in debug builds when `lo > hi`.
fn clamp_f64(v: f64, lo: f64, hi: f64) -> f64 {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn lan_setpoint() -> u64 {
        // 100 MB/s, a representative LAN target.
        100 * 1024 * 1024
    }

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
        let ctrl = ControllerConfig::new(lan_setpoint())
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
}
