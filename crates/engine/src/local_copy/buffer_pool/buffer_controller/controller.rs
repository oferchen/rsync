//! Lock-free `AdaptiveBufferController` implementation.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use super::config::ControllerConfig;
use super::math::{clamp_f64, compute_dt};
use super::state::ControllerState;

/// PID-style buffer-size controller.
///
/// The controller is `Send + Sync`. The current recommended buffer size is
/// stored in an [`AtomicUsize`] for lock-free reads from the hot path; the
/// PID state is updated under a [`Mutex`] only on the (relatively rare)
/// sample interval.
#[derive(Debug)]
pub struct AdaptiveBufferController {
    pub(super) config: ControllerConfig,
    pub(super) buffer_size: AtomicUsize,
    pub(super) state: Mutex<ControllerState>,
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

    /// Returns the configured minimum buffer size.
    #[must_use]
    pub fn min_size(&self) -> usize {
        self.config.min_size
    }

    /// Returns the configured maximum buffer size.
    #[must_use]
    pub fn max_size(&self) -> usize {
        self.config.max_size
    }
}
