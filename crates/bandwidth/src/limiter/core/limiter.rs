//! Core token-bucket bandwidth limiter.
//!
//! The algorithm mirrors upstream rsync's `io.c:sleep_for_bwlimit()`. Each
//! call to [`BandwidthLimiter::register`] accumulates byte debt, subtracts
//! the time-based allowance elapsed since the previous call, and sleeps when
//! the outstanding debt exceeds the minimum sleep threshold
//! (`MINIMUM_SLEEP_MICROS`).

use std::num::NonZeroU64;
use std::time::{Duration, Instant};

use super::super::{
    MICROS_PER_SECOND, MINIMUM_SLEEP_MICROS, duration_from_microseconds, sleep_for,
};
use super::write_max::calculate_write_max;

/// Token-bucket style bandwidth limiter that mirrors upstream rsync's pacing rules.
///
/// Each call to [`register`](Self::register) accumulates byte debt. The limiter
/// subtracts a time-based allowance (proportional to the configured rate) for
/// the wall-clock time elapsed since the previous call. When outstanding debt
/// exceeds a minimum threshold, the limiter sleeps to keep the transfer at or
/// below the target byte-per-second rate.
///
/// The chunk size returned by [`write_max_bytes`](Self::write_max_bytes) scales
/// linearly with the rate so that pacing sleeps remain short and responsive,
/// matching the `bwlimit_writemax = bwlimit * 128` formula in upstream rsync.
///
// upstream: io.c:sleep_for_bwlimit()
#[doc(alias = "--bwlimit")]
#[derive(Clone, Debug)]
pub struct BandwidthLimiter {
    limit_bytes: NonZeroU64,
    write_max: usize,
    total_written: u128,
    last_instant: Option<Instant>,
    simulated_elapsed_us: u128,
}

impl BandwidthLimiter {
    /// Constructs a new limiter from the supplied byte-per-second rate.
    // upstream: io.c:sleep_for_bwlimit() - initial setup
    #[must_use]
    pub fn new(limit: NonZeroU64) -> Self {
        let write_max = calculate_write_max(limit);

        Self {
            limit_bytes: limit,
            write_max,
            total_written: 0,
            last_instant: None,
            simulated_elapsed_us: 0,
        }
    }

    /// Updates the limiter so a new byte-per-second limit takes effect.
    ///
    /// Recalculates the write-max chunk size for the new rate and resets all
    /// internal accounting (debt, timing, simulated elapsed time) so the new
    /// rate applies immediately without carryover from the previous period.
    /// This is also used when a daemon module overrides the client-supplied
    /// `--bwlimit`.
    // upstream: options.c:2392 - daemon_bwlimit override reconfiguration
    #[doc(alias = "--bwlimit")]
    pub fn update_limit(&mut self, limit: NonZeroU64) {
        let write_max = calculate_write_max(limit);

        self.limit_bytes = limit;
        self.write_max = write_max;
        self.total_written = 0;
        self.last_instant = None;
        self.simulated_elapsed_us = 0;
    }

    /// Resets the limiter while keeping the current configuration.
    ///
    /// Clears accumulated debt and timing state so the next
    /// [`register`](Self::register) call starts a fresh accounting period.
    /// The rate setting is preserved.
    pub const fn reset(&mut self) {
        self.total_written = 0;
        self.last_instant = None;
        self.simulated_elapsed_us = 0;
    }

    /// Configured bytes-per-second transfer cap.
    #[inline]
    #[must_use]
    pub const fn limit_bytes(&self) -> NonZeroU64 {
        self.limit_bytes
    }

    /// Maximum bytes permitted in a single I/O operation.
    ///
    /// The value scales linearly with the configured rate so that pacing
    /// sleeps remain short and responsive. Callers should split writes
    /// larger than this threshold into chunks.
    // upstream: options.c:2395 - bwlimit_writemax = bwlimit * 128
    #[inline]
    #[must_use]
    pub const fn write_max_bytes(&self) -> usize {
        self.write_max
    }

    /// Clamps `buffer_len` to [`write_max_bytes`](Self::write_max_bytes) so each
    /// I/O batch stays within the pacing budget.
    ///
    /// Returns the smaller of `buffer_len` and the configured write-max,
    /// guaranteeing at least one byte to prevent zero-length reads.
    #[inline]
    #[must_use]
    pub fn recommended_read_size(&self, buffer_len: usize) -> usize {
        let limit = self.write_max.max(1);
        buffer_len.min(limit)
    }

    /// Records a completed write and sleeps if the accumulated debt exceeds
    /// the minimum threshold.
    ///
    /// The method adds `bytes` to the outstanding debt, subtracts the
    /// time-based allowance for the wall-clock time since the previous call,
    /// and sleeps if the resulting debt represents at least 100 ms of transfer
    /// time. Returns a
    /// [`LimiterSleep`](super::super::LimiterSleep) describing the requested
    /// and actual sleep durations.
    ///
    /// A zero-byte call is a no-op and returns immediately.
    // upstream: io.c:sleep_for_bwlimit() - debt accumulation and sleep logic
    pub fn register(&mut self, bytes: usize) -> super::super::LimiterSleep {
        if bytes == 0 {
            return super::super::LimiterSleep::default();
        }

        self.total_written = self.total_written.saturating_add(bytes as u128);

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
        // A short sleep (the backend woke earlier than requested) leaves debt
        // unpaid. Carry the un-slept remainder forward as simulated elapsed
        // time so the next register() credits it and pacing stays on target.
        if sleep_us > elapsed_us {
            self.simulated_elapsed_us = sleep_us - elapsed_us;
        }
        let remaining_us = sleep_us.saturating_sub(elapsed_us);
        let leftover = remaining_us.saturating_mul(bytes_per_second) / MICROS_PER_SECOND;

        self.total_written = leftover;
        self.last_instant = Some(end);
        let actual = Duration::from_micros(elapsed_us as u64);
        super::super::LimiterSleep::new(requested, actual)
    }

    #[cfg(test)]
    pub(crate) const fn accumulated_debt_for_testing(&self) -> u128 {
        self.total_written
    }
}
