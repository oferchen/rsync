//! Builder-style configuration for the PID buffer-size controller.
//!
//! Default gains come from the Ziegler-Nichols LAN preset documented in
//! section 6 of `docs/design/adaptive-buffer-controller.md`.

use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;

use super::controller::AdaptiveBufferController;
use super::state::ControllerState;

/// Default proportional gain (Ziegler-Nichols LAN preset).
pub(super) const DEFAULT_GAIN_P: f64 = 0.6;
/// Default integral gain (Ziegler-Nichols LAN preset).
pub(super) const DEFAULT_GAIN_I: f64 = 0.2;
/// Default derivative gain (Ziegler-Nichols LAN preset).
pub(super) const DEFAULT_GAIN_D: f64 = 0.05;

/// Default minimum buffer size (16 KiB), per RFC section 3.1.
pub(super) const DEFAULT_MIN_SIZE: usize = 16 * 1024;
/// Default maximum buffer size (4 MiB), per RFC section 3.1.
pub(super) const DEFAULT_MAX_SIZE: usize = 4 * 1024 * 1024;

/// Default sample interval used to seed the controller before the first
/// `observe` call (100 ms, matching the RFC section 3.3 time trigger).
pub(super) const DEFAULT_SAMPLE_INTERVAL_MS: u64 = 100;

/// Builder-style configuration for [`AdaptiveBufferController`].
///
/// All fields are advisory; defaults match the LAN preset from
/// `docs/design/adaptive-buffer-controller.md` section 6.
#[derive(Debug, Clone, Copy)]
pub struct ControllerConfig {
    pub(super) setpoint_bytes_per_sec: u64,
    pub(super) gain_p: f64,
    pub(super) gain_i: f64,
    pub(super) gain_d: f64,
    pub(super) anti_windup_clamp: (f64, f64),
    pub(super) min_size: usize,
    pub(super) max_size: usize,
    pub(super) sample_interval_ms: u64,
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
