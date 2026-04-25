//! Core token-bucket bandwidth limiter.
//!
//! The algorithm mirrors upstream rsync's `io.c:sleep_for_bwlimit()`. Each
//! call to [`BandwidthLimiter::register`] accumulates byte debt, subtracts
//! the time-based allowance elapsed since the previous call, and sleeps when
//! the outstanding debt exceeds the minimum sleep threshold
//! (`MINIMUM_SLEEP_MICROS`). An optional burst cap prevents excessive debt
//! accumulation after idle periods.

use std::num::NonZeroU64;
use std::time::{Duration, Instant};

use super::super::{
    MICROS_PER_SECOND, MINIMUM_SLEEP_MICROS, duration_from_microseconds, sleep_for,
};
use super::write_max::calculate_write_max;

/// Token-bucket style limiter that mirrors upstream rsync's pacing rules.
///
// upstream: io.c:sleep_for_bwlimit()
#[doc(alias = "--bwlimit")]
#[derive(Clone, Debug)]
pub struct BandwidthLimiter {
    limit_bytes: NonZeroU64,
    write_max: usize,
    burst_bytes: Option<NonZeroU64>,
    total_written: u128,
    last_instant: Option<Instant>,
    simulated_elapsed_us: u128,
}

impl BandwidthLimiter {
    /// Constructs a new limiter from the supplied byte-per-second rate.
    #[must_use]
    pub fn new(limit: NonZeroU64) -> Self {
        Self::with_burst(limit, None)
    }

    /// Constructs a new limiter from the supplied rate and optional burst size.
    #[must_use]
    pub fn with_burst(limit: NonZeroU64, burst: Option<NonZeroU64>) -> Self {
        let write_max = calculate_write_max(limit, burst);

        Self {
            limit_bytes: limit,
            write_max,
            burst_bytes: burst,
            total_written: 0,
            last_instant: None,
            simulated_elapsed_us: 0,
        }
    }

    /// Updates the limiter so a new byte-per-second limit takes effect.
    pub fn update_limit(&mut self, limit: NonZeroU64) {
        self.update_configuration(limit, self.burst_bytes);
    }

    /// Updates the limiter so both the rate and burst configuration take effect.
    #[doc(alias = "--bwlimit")]
    pub fn update_configuration(&mut self, limit: NonZeroU64, burst: Option<NonZeroU64>) {
        let write_max = calculate_write_max(limit, burst);

        self.limit_bytes = limit;
        self.write_max = write_max;
        self.burst_bytes = burst;
        self.total_written = 0;
        self.last_instant = None;
        self.simulated_elapsed_us = 0;
    }

    /// Resets the limiter while keeping the current configuration.
    pub const fn reset(&mut self) {
        self.total_written = 0;
        self.last_instant = None;
        self.simulated_elapsed_us = 0;
    }

    #[inline]
    fn clamp_debt_to_burst(&mut self) {
        if let Some(burst) = self.burst_bytes {
            let limit = u128::from(burst.get());
            self.total_written = self.total_written.min(limit);
        }
    }

    /// Configured bytes-per-second transfer cap.
    #[inline]
    #[must_use]
    pub const fn limit_bytes(&self) -> NonZeroU64 {
        self.limit_bytes
    }

    /// Optional burst allowance above the steady-state limit.
    #[inline]
    pub const fn burst_bytes(&self) -> Option<NonZeroU64> {
        self.burst_bytes
    }

    /// Maximum bytes permitted in a single I/O operation.
    #[inline]
    #[must_use]
    pub const fn write_max_bytes(&self) -> usize {
        self.write_max
    }

    /// Clamps `buffer_len` to `write_max` so each I/O batch stays within the pacing budget.
    #[inline]
    #[must_use]
    pub fn recommended_read_size(&self, buffer_len: usize) -> usize {
        let limit = self.write_max.max(1);
        buffer_len.min(limit)
    }

    /// Records a completed write and sleeps if the limiter accumulated debt.
    // upstream: io.c:sleep_for_bwlimit() - debt accumulation and sleep logic
    pub fn register(&mut self, bytes: usize) -> super::super::LimiterSleep {
        if bytes == 0 {
            return super::super::LimiterSleep::default();
        }

        self.total_written = self.total_written.saturating_add(bytes as u128);
        self.clamp_debt_to_burst();

        let start = Instant::now();
        let bytes_per_second = u128::from(self.limit_bytes.get());

        let mut elapsed_us = self.simulated_elapsed_us;
        if let Some(previous) = self.last_instant {
            let elapsed = start.duration_since(previous);
            let measured = elapsed.as_micros().min(u128::from(u64::MAX));
            elapsed_us = elapsed_us.saturating_add(measured);
        }
        self.simulated_elapsed_us = 0;
        if elapsed_us > 0 {
            let allowed = elapsed_us.saturating_mul(bytes_per_second) / MICROS_PER_SECOND;
            if allowed >= self.total_written {
                self.total_written = 0;
            } else {
                self.total_written -= allowed;
            }
        }

        self.clamp_debt_to_burst();

        let sleep_us = self.total_written.saturating_mul(MICROS_PER_SECOND) / bytes_per_second;

        if sleep_us < MINIMUM_SLEEP_MICROS {
            self.last_instant = Some(start);
            return super::super::LimiterSleep::default();
        }

        let requested = duration_from_microseconds(sleep_us);
        if !requested.is_zero() {
            sleep_for(requested);
        }

        let end = Instant::now();
        let elapsed_us = end
            .checked_duration_since(start)
            .map_or(0, |duration| duration.as_micros().min(u128::from(u64::MAX)));
        if sleep_us > elapsed_us {
            self.simulated_elapsed_us = sleep_us - elapsed_us;
        }
        let remaining_us = sleep_us.saturating_sub(elapsed_us);
        let leftover = remaining_us.saturating_mul(bytes_per_second) / MICROS_PER_SECOND;

        self.total_written = leftover;
        self.clamp_debt_to_burst();
        self.last_instant = Some(end);
        let actual = Duration::from_micros(elapsed_us as u64);
        super::super::LimiterSleep::new(requested, actual)
    }

    #[cfg(test)]
    pub(crate) const fn accumulated_debt_for_testing(&self) -> u128 {
        self.total_written
    }
}
