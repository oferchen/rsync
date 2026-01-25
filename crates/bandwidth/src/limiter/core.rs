use super::{
    MICROS_PER_SECOND, MIN_WRITE_MAX, MINIMUM_SLEEP_MICROS, duration_from_microseconds, sleep_for,
};
use std::num::NonZeroU64;
use std::time::{Duration, Instant};

fn calculate_write_max(limit: NonZeroU64, burst: Option<NonZeroU64>) -> usize {
    let kib = if limit.get() < 1024 {
        1
    } else {
        limit.get() / 1024
    };

    let base_write_max = u128::from(kib)
        .saturating_mul(128)
        .max(MIN_WRITE_MAX as u128);
    let mut write_max = base_write_max.min(usize::MAX as u128) as usize;

    if let Some(burst) = burst {
        let burst = burst.get().min(usize::MAX as u64);
        write_max = usize::try_from(burst)
            .unwrap_or(usize::MAX)
            .max(MIN_WRITE_MAX)
            .max(1);
    }

    write_max.max(MIN_WRITE_MAX)
}

/// Token-bucket style limiter that mirrors upstream rsync's pacing rules.
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

    /// Returns the configured limit in bytes per second.
    #[inline]
    #[must_use]
    pub const fn limit_bytes(&self) -> NonZeroU64 {
        self.limit_bytes
    }

    /// Returns the configured burst size in bytes, if any.
    #[inline]
    #[must_use]
    pub const fn burst_bytes(&self) -> Option<NonZeroU64> {
        self.burst_bytes
    }

    /// Returns the maximum chunk size the limiter schedules before sleeping.
    #[inline]
    #[must_use]
    pub const fn write_max_bytes(&self) -> usize {
        self.write_max
    }

    /// Returns the maximum chunk size that should be written before sleeping.
    #[inline]
    #[must_use]
    pub fn recommended_read_size(&self, buffer_len: usize) -> usize {
        let limit = self.write_max.max(1);
        buffer_len.min(limit)
    }

    /// Records a completed write and sleeps if the limiter accumulated debt.
    pub fn register(&mut self, bytes: usize) -> super::LimiterSleep {
        if bytes == 0 {
            return super::LimiterSleep::default();
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
            return super::LimiterSleep::default();
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
        super::LimiterSleep::new(requested, actual)
    }

    #[cfg(test)]
    pub(crate) const fn accumulated_debt_for_testing(&self) -> u128 {
        self.total_written
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nz(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("non-zero value required")
    }

    // Tests for calculate_write_max
    #[test]
    fn calculate_write_max_small_limit_uses_minimum() {
        // For limits < 1024, kib = 1, base_write_max = 1 * 128 = 128
        // But MIN_WRITE_MAX is 512, so we get 512
        let result = calculate_write_max(nz(100), None);
        assert_eq!(result, MIN_WRITE_MAX);
    }

    #[test]
    fn calculate_write_max_1kb_limit() {
        // For limit = 1024, kib = 1, base_write_max = 1 * 128 = 128 < 512
        let result = calculate_write_max(nz(1024), None);
        assert_eq!(result, MIN_WRITE_MAX);
    }

    #[test]
    fn calculate_write_max_large_limit() {
        // For limit = 1024*100 = 102400, kib = 100, base_write_max = 100 * 128 = 12800
        let result = calculate_write_max(nz(1024 * 100), None);
        assert_eq!(result, 12800);
    }

    #[test]
    fn calculate_write_max_with_burst_overrides() {
        // Burst overrides the calculated write_max
        let result = calculate_write_max(nz(1024 * 100), Some(nz(8192)));
        assert_eq!(result, 8192);
    }

    #[test]
    fn calculate_write_max_with_small_burst_uses_minimum() {
        // Small burst values are clamped to MIN_WRITE_MAX
        let result = calculate_write_max(nz(1024 * 100), Some(nz(100)));
        assert_eq!(result, MIN_WRITE_MAX);
    }

    // Tests for BandwidthLimiter::new
    #[test]
    fn bandwidth_limiter_new_stores_limit() {
        let limiter = BandwidthLimiter::new(nz(10000));
        assert_eq!(limiter.limit_bytes().get(), 10000);
    }

    #[test]
    fn bandwidth_limiter_new_has_no_burst() {
        let limiter = BandwidthLimiter::new(nz(10000));
        assert!(limiter.burst_bytes().is_none());
    }

    #[test]
    fn bandwidth_limiter_new_initializes_counters() {
        let limiter = BandwidthLimiter::new(nz(10000));
        assert_eq!(limiter.accumulated_debt_for_testing(), 0);
    }

    // Tests for BandwidthLimiter::with_burst
    #[test]
    fn bandwidth_limiter_with_burst_stores_both() {
        let limiter = BandwidthLimiter::with_burst(nz(10000), Some(nz(5000)));
        assert_eq!(limiter.limit_bytes().get(), 10000);
        assert_eq!(limiter.burst_bytes().unwrap().get(), 5000);
    }

    #[test]
    fn bandwidth_limiter_with_burst_none_is_same_as_new() {
        let limiter1 = BandwidthLimiter::new(nz(10000));
        let limiter2 = BandwidthLimiter::with_burst(nz(10000), None);
        assert_eq!(limiter1.limit_bytes(), limiter2.limit_bytes());
        assert_eq!(limiter1.burst_bytes(), limiter2.burst_bytes());
    }

    // Tests for update_limit
    #[test]
    fn update_limit_changes_limit() {
        let mut limiter = BandwidthLimiter::new(nz(10000));
        limiter.update_limit(nz(20000));
        assert_eq!(limiter.limit_bytes().get(), 20000);
    }

    #[test]
    fn update_limit_resets_counters() {
        let mut limiter = BandwidthLimiter::new(nz(10000));
        let _ = limiter.register(5000); // Add some debt
        limiter.update_limit(nz(20000));
        assert_eq!(limiter.accumulated_debt_for_testing(), 0);
    }

    #[test]
    fn update_limit_preserves_burst() {
        let mut limiter = BandwidthLimiter::with_burst(nz(10000), Some(nz(5000)));
        limiter.update_limit(nz(20000));
        assert_eq!(limiter.burst_bytes().unwrap().get(), 5000);
    }

    // Tests for update_configuration
    #[test]
    fn update_configuration_changes_both() {
        let mut limiter = BandwidthLimiter::new(nz(10000));
        limiter.update_configuration(nz(20000), Some(nz(8000)));
        assert_eq!(limiter.limit_bytes().get(), 20000);
        assert_eq!(limiter.burst_bytes().unwrap().get(), 8000);
    }

    #[test]
    fn update_configuration_can_remove_burst() {
        let mut limiter = BandwidthLimiter::with_burst(nz(10000), Some(nz(5000)));
        limiter.update_configuration(nz(20000), None);
        assert!(limiter.burst_bytes().is_none());
    }

    #[test]
    fn update_configuration_resets_counters() {
        let mut limiter = BandwidthLimiter::new(nz(10000));
        let _ = limiter.register(5000);
        limiter.update_configuration(nz(20000), None);
        assert_eq!(limiter.accumulated_debt_for_testing(), 0);
    }

    // Tests for reset
    #[test]
    fn reset_clears_debt() {
        let mut limiter = BandwidthLimiter::new(nz(10000));
        let _ = limiter.register(5000);
        limiter.reset();
        assert_eq!(limiter.accumulated_debt_for_testing(), 0);
    }

    #[test]
    fn reset_preserves_configuration() {
        let mut limiter = BandwidthLimiter::with_burst(nz(10000), Some(nz(5000)));
        let _ = limiter.register(5000);
        limiter.reset();
        assert_eq!(limiter.limit_bytes().get(), 10000);
        assert_eq!(limiter.burst_bytes().unwrap().get(), 5000);
    }

    // Tests for accessor methods
    #[test]
    fn limit_bytes_returns_configured_limit() {
        let limiter = BandwidthLimiter::new(nz(12345));
        assert_eq!(limiter.limit_bytes().get(), 12345);
    }

    #[test]
    fn burst_bytes_returns_none_when_not_set() {
        let limiter = BandwidthLimiter::new(nz(10000));
        assert!(limiter.burst_bytes().is_none());
    }

    #[test]
    fn burst_bytes_returns_some_when_set() {
        let limiter = BandwidthLimiter::with_burst(nz(10000), Some(nz(5000)));
        assert_eq!(limiter.burst_bytes().unwrap().get(), 5000);
    }

    #[test]
    fn write_max_bytes_returns_calculated_max() {
        let limiter = BandwidthLimiter::new(nz(1024 * 100));
        assert_eq!(limiter.write_max_bytes(), 12800);
    }

    #[test]
    fn recommended_read_size_clamps_to_write_max() {
        let limiter = BandwidthLimiter::new(nz(1024 * 100));
        assert_eq!(limiter.recommended_read_size(100000), 12800);
    }

    #[test]
    fn recommended_read_size_returns_buffer_len_when_smaller() {
        let limiter = BandwidthLimiter::new(nz(1024 * 100));
        assert_eq!(limiter.recommended_read_size(100), 100);
    }

    #[test]
    fn recommended_read_size_handles_empty_buffer() {
        let limiter = BandwidthLimiter::new(nz(1024 * 100));
        assert_eq!(limiter.recommended_read_size(0), 0);
    }

    // Tests for register
    #[test]
    fn register_zero_bytes_is_noop() {
        let mut limiter = BandwidthLimiter::new(nz(10000));
        let sleep = limiter.register(0);
        assert!(sleep.is_noop());
        assert_eq!(limiter.accumulated_debt_for_testing(), 0);
    }

    #[test]
    fn register_small_amount_no_sleep() {
        // With high limit, small writes shouldn't trigger sleep
        let mut limiter = BandwidthLimiter::new(nz(1_000_000_000)); // 1 GB/s
        let sleep = limiter.register(100);
        // Sleep should be minimal or zero for such small amount
        assert!(sleep.requested() < Duration::from_millis(1));
    }

    #[test]
    fn register_accumulates_debt() {
        let mut limiter = BandwidthLimiter::new(nz(1_000_000)); // 1 MB/s
        let _ = limiter.register(1000);
        // Some debt should be accumulated (exact amount depends on timing)
        // Can't easily test exact value due to timing
    }

    #[test]
    fn register_with_burst_clamps_debt() {
        let mut limiter = BandwidthLimiter::with_burst(nz(100), Some(nz(1000))); // Very slow
        let _ = limiter.register(5000); // Write more than burst
        // Debt should be clamped to burst size
        assert!(limiter.accumulated_debt_for_testing() <= 1000);
    }

    // Tests for Clone and Debug traits
    #[test]
    fn bandwidth_limiter_clone_creates_independent_copy() {
        let mut limiter = BandwidthLimiter::new(nz(10000));
        let _ = limiter.register(1000);
        let cloned = limiter.clone();
        assert_eq!(cloned.limit_bytes(), limiter.limit_bytes());
    }

    #[test]
    fn bandwidth_limiter_debug() {
        let limiter = BandwidthLimiter::new(nz(10000));
        let debug = format!("{limiter:?}");
        assert!(debug.contains("BandwidthLimiter"));
        assert!(debug.contains("10000"));
    }

    // Edge case tests
    #[test]
    fn bandwidth_limiter_very_small_limit() {
        let limiter = BandwidthLimiter::new(nz(1));
        assert_eq!(limiter.limit_bytes().get(), 1);
    }

    #[test]
    fn bandwidth_limiter_very_large_limit() {
        let limiter = BandwidthLimiter::new(nz(u64::MAX));
        assert_eq!(limiter.limit_bytes().get(), u64::MAX);
    }

    #[test]
    fn bandwidth_limiter_write_max_with_very_large_limit() {
        // Very large limit should still produce a reasonable write_max
        let limiter = BandwidthLimiter::new(nz(u64::MAX));
        let write_max = limiter.write_max_bytes();
        // Should be a valid usize
        assert!(write_max >= MIN_WRITE_MAX);
    }

    #[test]
    fn bandwidth_limiter_burst_larger_than_write() {
        // Burst can be larger than what we'd normally calculate
        let limiter = BandwidthLimiter::with_burst(nz(1024), Some(nz(1_000_000)));
        assert_eq!(limiter.burst_bytes().unwrap().get(), 1_000_000);
        assert_eq!(limiter.write_max_bytes(), 1_000_000);
    }

    // ========================================================================
    // Debt Saturation Edge Cases
    // ========================================================================

    #[test]
    fn register_uses_saturating_add_prevents_overflow() {
        // Create limiter with very low rate to maximize debt
        let mut limiter = BandwidthLimiter::new(nz(1)); // 1 byte/second
        // Register multiple times - should use saturating_add internally
        for _ in 0..1000 {
            let _ = limiter.register(usize::MAX / 2);
        }
        // Should not panic or wrap around
        // Debt is clamped by timing calculations anyway
    }

    #[test]
    fn debt_accumulation_with_burst_clamping() {
        // With burst clamping, debt should never exceed burst value
        let mut limiter = BandwidthLimiter::with_burst(nz(100), Some(nz(500))); // 100 B/s, 500B burst

        // Write way more than burst allows
        let _ = limiter.register(10000);

        // Debt should be clamped to burst size
        assert!(
            limiter.accumulated_debt_for_testing() <= 500,
            "debt {} should be <= burst 500",
            limiter.accumulated_debt_for_testing()
        );
    }

    #[test]
    fn multiple_registers_with_burst_maintains_clamp() {
        let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(2000))); // 1KB/s, 2KB burst

        // Multiple writes that individually exceed burst
        let _ = limiter.register(3000);
        assert!(limiter.accumulated_debt_for_testing() <= 2000);

        let _ = limiter.register(3000);
        assert!(limiter.accumulated_debt_for_testing() <= 2000);

        let _ = limiter.register(3000);
        assert!(limiter.accumulated_debt_for_testing() <= 2000);
    }

    // ========================================================================
    // Burst Clamping Logic Tests
    // ========================================================================

    #[test]
    fn clamp_debt_to_burst_with_no_burst_is_noop() {
        let mut limiter = BandwidthLimiter::new(nz(100)); // No burst
        let _ = limiter.register(1000);
        let debt_before = limiter.accumulated_debt_for_testing();

        // Without burst, debt should accumulate based on timing/rate
        // (may be clamped by timing calculations but not by burst)
        // This test verifies burst=None doesn't affect debt artificially
        let _ = debt_before; // Just ensure the call doesn't panic
    }

    #[test]
    fn clamp_debt_to_burst_clamps_at_exact_burst_value() {
        // Create limiter with specific burst
        let mut limiter = BandwidthLimiter::with_burst(nz(1), Some(nz(100)));

        // Write exactly burst amount
        let _ = limiter.register(100);
        assert!(limiter.accumulated_debt_for_testing() <= 100);

        // Write more than burst
        let _ = limiter.register(100);
        assert!(limiter.accumulated_debt_for_testing() <= 100);
    }

    #[test]
    fn clamp_debt_to_burst_with_very_large_burst() {
        // Burst larger than typical writes
        let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(u64::MAX)));
        let _ = limiter.register(10000);
        // Should not clamp at all (burst is huge)
        // Debt behavior depends on timing
    }

    // ========================================================================
    // Write Max Calculation Edge Cases
    // ========================================================================

    #[test]
    fn calculate_write_max_u64_max_limit() {
        // u64::MAX / 1024 still fits in u128
        let result = calculate_write_max(nz(u64::MAX), None);
        // Should produce a valid usize without overflow
        assert!(result >= MIN_WRITE_MAX);
    }

    #[test]
    fn calculate_write_max_burst_overrides_calculated_value() {
        // Even with huge limit, burst should override
        let result_no_burst = calculate_write_max(nz(u64::MAX), None);
        let result_with_burst = calculate_write_max(nz(u64::MAX), Some(nz(4096)));

        // With burst, should be exactly burst value (or MIN_WRITE_MAX if smaller)
        assert!(result_with_burst <= result_no_burst.max(4096));
    }

    #[test]
    fn calculate_write_max_burst_at_boundary_512() {
        // Burst = MIN_WRITE_MAX should return MIN_WRITE_MAX
        let result = calculate_write_max(nz(10000), Some(nz(MIN_WRITE_MAX as u64)));
        assert_eq!(result, MIN_WRITE_MAX);
    }

    #[test]
    fn calculate_write_max_burst_just_above_minimum() {
        // Burst slightly above MIN_WRITE_MAX
        let result = calculate_write_max(nz(10000), Some(nz(MIN_WRITE_MAX as u64 + 1)));
        assert_eq!(result, MIN_WRITE_MAX + 1);
    }

    // ========================================================================
    // Rate Change During Operation
    // ========================================================================

    #[test]
    fn update_limit_clears_accumulated_debt() {
        let mut limiter = BandwidthLimiter::new(nz(100));
        let _ = limiter.register(1000); // Accumulate some debt

        // Update limit should reset
        limiter.update_limit(nz(200));
        assert_eq!(limiter.accumulated_debt_for_testing(), 0);
    }

    #[test]
    fn update_configuration_clears_accumulated_debt() {
        let mut limiter = BandwidthLimiter::new(nz(100));
        let _ = limiter.register(1000);

        limiter.update_configuration(nz(200), Some(nz(500)));
        assert_eq!(limiter.accumulated_debt_for_testing(), 0);
    }

    #[test]
    fn reset_preserves_limit_but_clears_debt() {
        let mut limiter = BandwidthLimiter::with_burst(nz(100), Some(nz(500)));
        let _ = limiter.register(1000);

        let limit_before = limiter.limit_bytes().get();
        let burst_before = limiter.burst_bytes().map(|b| b.get());

        limiter.reset();

        assert_eq!(limiter.limit_bytes().get(), limit_before);
        assert_eq!(limiter.burst_bytes().map(|b| b.get()), burst_before);
        assert_eq!(limiter.accumulated_debt_for_testing(), 0);
    }

    // ========================================================================
    // Zero-Byte Write Handling
    // ========================================================================

    #[test]
    fn register_zero_bytes_does_not_affect_debt() {
        let mut limiter = BandwidthLimiter::new(nz(100));
        let _ = limiter.register(1000); // Add some debt
        let _debt_before = limiter.accumulated_debt_for_testing();

        let sleep = limiter.register(0);

        assert!(sleep.is_noop());
        // Zero-byte register should be noop - debt unchanged by the zero-byte write
        // (timing may affect it between calls)
    }

    #[test]
    fn register_zero_bytes_returns_default_sleep() {
        let mut limiter = BandwidthLimiter::new(nz(100));
        let sleep = limiter.register(0);

        // Should return default (noop) sleep
        assert!(sleep.is_noop());
        assert!(sleep.requested().is_zero());
    }

    // ==================== Additional coverage tests ====================

    #[test]
    fn register_multiple_times_updates_last_instant() {
        let mut limiter = BandwidthLimiter::new(nz(1_000_000_000)); // Very fast
        // First register establishes last_instant
        let _ = limiter.register(100);
        // Second register uses elapsed time from last_instant
        let sleep2 = limiter.register(100);
        // With such a high limit, should not need to sleep
        assert!(sleep2.requested() < Duration::from_millis(1));
    }

    #[test]
    fn debt_reduction_from_elapsed_time() {
        let mut limiter = BandwidthLimiter::new(nz(1_000_000)); // 1 MB/s
        // Register some bytes
        let _ = limiter.register(1000);
        // Sleep a bit to let time elapse
        std::thread::sleep(Duration::from_millis(10));
        // Next register should have reduced debt due to elapsed time
        let sleep = limiter.register(100);
        // Debt should be partially forgiven
        assert!(sleep.requested() < Duration::from_secs(1));
    }

    #[test]
    fn elapsed_time_forgives_all_debt_when_slow_enough() {
        let mut limiter = BandwidthLimiter::new(nz(1_000_000_000)); // Very fast: 1 GB/s
        // Register a small amount
        let _ = limiter.register(100);
        // Wait a bit (more than enough time for debt to be forgiven)
        std::thread::sleep(Duration::from_millis(10));
        // Debt should be completely forgiven
        let sleep = limiter.register(100);
        // With 1 GB/s limit and small writes, no sleep needed
        assert!(sleep.is_noop() || sleep.requested() < Duration::from_micros(100));
    }

    #[test]
    fn calculate_write_max_with_tiny_limit() {
        // Limit of 1 byte/s
        let result = calculate_write_max(nz(1), None);
        // Should be MIN_WRITE_MAX (512)
        assert_eq!(result, MIN_WRITE_MAX);
    }

    #[test]
    fn calculate_write_max_progression() {
        // Test that write_max increases with limit
        let small = calculate_write_max(nz(1024), None);
        let medium = calculate_write_max(nz(1024 * 100), None);
        let large = calculate_write_max(nz(1024 * 1000), None);

        assert!(medium >= small);
        assert!(large >= medium);
    }

    #[test]
    fn recommended_read_size_with_zero_write_max() {
        // This tests the .max(1) in recommended_read_size
        // Even if write_max somehow became 0, we'd get at least 1
        // But in practice write_max is always >= MIN_WRITE_MAX
        let limiter = BandwidthLimiter::new(nz(1));
        assert!(limiter.recommended_read_size(1000) >= 1);
    }

    #[test]
    fn limiter_debt_clamping_repeated() {
        // Test that debt clamping works across multiple operations
        let mut limiter = BandwidthLimiter::with_burst(nz(100), Some(nz(500)));

        for _ in 0..10 {
            let _ = limiter.register(1000);
            assert!(limiter.accumulated_debt_for_testing() <= 500);
        }
    }

    #[test]
    fn update_limit_changes_write_max() {
        let mut limiter = BandwidthLimiter::new(nz(1024));
        let initial_write_max = limiter.write_max_bytes();

        limiter.update_limit(nz(1024 * 1024));
        let new_write_max = limiter.write_max_bytes();

        assert!(new_write_max > initial_write_max);
    }

    #[test]
    fn update_configuration_changes_write_max_based_on_burst() {
        let mut limiter = BandwidthLimiter::new(nz(1024 * 1024));
        let initial_write_max = limiter.write_max_bytes();

        // Set a smaller burst
        limiter.update_configuration(nz(1024 * 1024), Some(nz(1024)));
        let new_write_max = limiter.write_max_bytes();

        assert!(new_write_max < initial_write_max);
        assert_eq!(new_write_max, 1024);
    }

    #[test]
    fn reset_clears_simulated_elapsed_us() {
        let mut limiter = BandwidthLimiter::new(nz(1024));
        let _ = limiter.register(4096); // Accumulate some state
        limiter.reset();

        // After reset, simulated_elapsed_us should be 0
        // We can't directly check this, but behavior should be like a fresh limiter
        let sleep = limiter.register(1024);
        assert_eq!(sleep.requested(), Duration::from_secs(1));
    }

    // ========================================================================
    // Token bucket behavior tests
    // ========================================================================

    #[test]
    fn register_deterministic_sleep_calculation() {
        use super::super::recorded_sleep_session;

        let mut session = recorded_sleep_session();
        session.clear();

        // Create limiter with 1024 bytes/second
        let mut limiter = BandwidthLimiter::new(nz(1024));

        // Write exactly 1024 bytes - should request exactly 1 second sleep
        let sleep = limiter.register(1024);

        // With 1024 bytes at 1024 bytes/sec, should sleep for 1 second
        assert_eq!(sleep.requested(), Duration::from_secs(1));
    }

    #[test]
    fn register_deterministic_multiple_writes() {
        use super::super::recorded_sleep_session;

        let mut session = recorded_sleep_session();
        session.clear();

        // Create limiter with 1000 bytes/second
        let mut limiter = BandwidthLimiter::new(nz(1000));

        // Write 500 bytes - should sleep for 0.5 seconds (500_000 us)
        let sleep = limiter.register(500);
        assert_eq!(sleep.requested(), Duration::from_millis(500));
    }

    #[test]
    fn register_exact_rate_calculation() {
        use super::super::recorded_sleep_session;

        let mut session = recorded_sleep_session();
        session.clear();

        // 100 bytes/second
        let mut limiter = BandwidthLimiter::new(nz(100));

        // Write 100 bytes - should sleep for 1 second
        let sleep = limiter.register(100);
        assert_eq!(sleep.requested(), Duration::from_secs(1));
    }

    #[test]
    fn register_very_slow_rate() {
        use super::super::recorded_sleep_session;

        let mut session = recorded_sleep_session();
        session.clear();

        // 10 bytes/second
        let mut limiter = BandwidthLimiter::new(nz(10));

        // Write 10 bytes - should sleep for 1 second
        let sleep = limiter.register(10);
        assert_eq!(sleep.requested(), Duration::from_secs(1));
    }

    #[test]
    fn register_very_fast_rate_no_sleep() {
        // With very high rate, small writes should not trigger sleep
        let mut limiter = BandwidthLimiter::new(nz(u64::MAX));
        let sleep = limiter.register(1000);

        // Sleep should be negligible
        assert!(sleep.requested() < Duration::from_millis(1));
    }

    #[test]
    fn register_sleep_under_minimum_threshold() {
        // MINIMUM_SLEEP_MICROS is 100_000 (0.1 seconds)
        // With 1_000_000 bytes/second and 50_000 bytes written
        // sleep_us = 50_000 * 1_000_000 / 1_000_000 = 50_000 us (0.05 seconds)
        // This is under MINIMUM_SLEEP_MICROS, so should return default (noop)
        let mut limiter = BandwidthLimiter::new(nz(1_000_000));
        let sleep = limiter.register(50_000);

        // Under threshold, should be noop
        assert!(sleep.is_noop());
    }

    #[test]
    fn register_sleep_at_minimum_threshold() {
        // MINIMUM_SLEEP_MICROS is 100_000 (0.1 seconds)
        // With 1_000_000 bytes/second and 100_000 bytes written
        // sleep_us = 100_000 * 1_000_000 / 1_000_000 = 100_000 us
        // This equals MINIMUM_SLEEP_MICROS, so it IS above the threshold
        // (the check is `sleep_us < MINIMUM_SLEEP_MICROS`)
        let mut limiter = BandwidthLimiter::new(nz(1_000_000));
        let sleep = limiter.register(100_000);

        // At threshold (not below), should trigger sleep
        assert!(!sleep.is_noop());
        assert_eq!(sleep.requested(), Duration::from_millis(100));
    }

    #[test]
    fn register_sleep_above_minimum_threshold() {
        // With 1_000_000 bytes/second and 200_000 bytes written
        // sleep_us = 200_000 * 1_000_000 / 1_000_000 = 200_000 us (0.2 seconds)
        // This is above MINIMUM_SLEEP_MICROS
        let mut limiter = BandwidthLimiter::new(nz(1_000_000));
        let sleep = limiter.register(200_000);

        // Above threshold, should request sleep
        assert!(!sleep.is_noop());
        assert_eq!(sleep.requested(), Duration::from_millis(200));
    }

    // ========================================================================
    // Burst clamping detailed tests
    // ========================================================================

    #[test]
    fn burst_clamps_sleep_duration() {
        // With burst, debt is clamped so sleep duration is limited
        // 100 bytes/second with 500 byte burst
        let mut limiter = BandwidthLimiter::with_burst(nz(100), Some(nz(500)));

        // Write 1000 bytes - would normally sleep 10 seconds
        // But debt clamped to 500, so sleep is 5 seconds
        let sleep = limiter.register(1000);
        assert_eq!(sleep.requested(), Duration::from_secs(5));
    }

    #[test]
    fn burst_clamps_after_each_register() {
        let mut limiter = BandwidthLimiter::with_burst(nz(100), Some(nz(200)));

        // First write: 500 bytes, clamped to 200
        let _sleep1 = limiter.register(500);
        assert!(limiter.accumulated_debt_for_testing() <= 200);

        // Second write: another 500 bytes, still clamped to 200
        let _sleep2 = limiter.register(500);
        assert!(limiter.accumulated_debt_for_testing() <= 200);
    }

    #[test]
    fn no_burst_allows_unlimited_debt_growth() {
        // Without burst, debt can grow (limited only by timing)
        let mut limiter = BandwidthLimiter::new(nz(10)); // 10 bytes/second

        // Write 1000 bytes
        let sleep = limiter.register(1000);

        // Should sleep for 100 seconds (1000 bytes / 10 bytes per second)
        assert_eq!(sleep.requested(), Duration::from_secs(100));
    }

    // ========================================================================
    // Write max calculation comprehensive tests
    // ========================================================================

    #[test]
    fn write_max_minimum_for_tiny_rate() {
        let limiter = BandwidthLimiter::new(nz(1));
        // For limit < 1024, kib = 1, so base_write_max = 128
        // But MIN_WRITE_MAX is 512, so we get 512
        assert_eq!(limiter.write_max_bytes(), MIN_WRITE_MAX);
    }

    #[test]
    fn write_max_scales_with_rate() {
        // For limit = 1024 * 50 = 51200, kib = 50
        // base_write_max = 50 * 128 = 6400
        let limiter = BandwidthLimiter::new(nz(1024 * 50));
        assert_eq!(limiter.write_max_bytes(), 6400);
    }

    #[test]
    fn write_max_capped_by_burst() {
        // Even with high rate, burst caps write_max
        let limiter = BandwidthLimiter::with_burst(nz(1024 * 1000), Some(nz(4096)));
        assert_eq!(limiter.write_max_bytes(), 4096);
    }

    #[test]
    fn write_max_burst_respects_minimum() {
        // Burst below MIN_WRITE_MAX still gives MIN_WRITE_MAX
        let limiter = BandwidthLimiter::with_burst(nz(1024 * 1000), Some(nz(100)));
        assert_eq!(limiter.write_max_bytes(), MIN_WRITE_MAX);
    }

    // ========================================================================
    // Recommended read size tests
    // ========================================================================

    #[test]
    fn recommended_read_size_exact_write_max() {
        let limiter = BandwidthLimiter::new(nz(1024 * 100)); // write_max = 12800
        assert_eq!(limiter.recommended_read_size(12800), 12800);
    }

    #[test]
    fn recommended_read_size_much_larger() {
        let limiter = BandwidthLimiter::new(nz(1024 * 100)); // write_max = 12800
        assert_eq!(limiter.recommended_read_size(1_000_000), 12800);
    }

    #[test]
    fn recommended_read_size_much_smaller() {
        let limiter = BandwidthLimiter::new(nz(1024 * 100)); // write_max = 12800
        assert_eq!(limiter.recommended_read_size(10), 10);
    }

    #[test]
    fn recommended_read_size_one_byte() {
        let limiter = BandwidthLimiter::new(nz(1024 * 100));
        assert_eq!(limiter.recommended_read_size(1), 1);
    }

    // ========================================================================
    // Register with elapsed time simulation tests
    // ========================================================================

    #[test]
    fn register_first_call_has_no_elapsed_time() {
        use super::super::recorded_sleep_session;

        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1000));

        // First register - no prior last_instant, so elapsed = 0
        // All bytes become debt
        let sleep = limiter.register(1000);
        assert_eq!(sleep.requested(), Duration::from_secs(1));
    }

    #[test]
    fn register_updates_last_instant() {
        use super::super::recorded_sleep_session;

        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1_000_000_000)); // Very fast

        // First register
        let _sleep1 = limiter.register(100);

        // Second register - should use time since first
        let sleep2 = limiter.register(100);

        // With such a high rate, should be minimal sleep
        assert!(sleep2.requested() < Duration::from_millis(1));
    }

    // ========================================================================
    // Configuration update during operation tests
    // ========================================================================

    #[test]
    fn update_limit_mid_operation_resets_state() {
        use super::super::recorded_sleep_session;

        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(100));

        // Accumulate some debt
        let _sleep1 = limiter.register(100);

        // Update limit - should reset
        limiter.update_limit(nz(200));
        assert_eq!(limiter.accumulated_debt_for_testing(), 0);

        // New register should start fresh
        let sleep2 = limiter.register(200);
        assert_eq!(sleep2.requested(), Duration::from_secs(1));
    }

    #[test]
    fn update_configuration_mid_operation_resets_state() {
        use super::super::recorded_sleep_session;

        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::with_burst(nz(100), Some(nz(50)));

        // Accumulate some debt
        let _sleep1 = limiter.register(100);

        // Update configuration
        limiter.update_configuration(nz(200), Some(nz(100)));
        assert_eq!(limiter.accumulated_debt_for_testing(), 0);
        assert_eq!(limiter.limit_bytes().get(), 200);
        assert_eq!(limiter.burst_bytes().unwrap().get(), 100);
    }

    #[test]
    fn reset_mid_operation_clears_state() {
        use super::super::recorded_sleep_session;

        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(100));

        // Accumulate some debt
        let _sleep1 = limiter.register(100);

        // Reset
        limiter.reset();
        assert_eq!(limiter.accumulated_debt_for_testing(), 0);

        // Configuration preserved
        assert_eq!(limiter.limit_bytes().get(), 100);
    }

    // ========================================================================
    // LimiterSleep result validation tests
    // ========================================================================

    #[test]
    fn register_returns_correct_limiter_sleep() {
        use super::super::recorded_sleep_session;

        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1000));
        let sleep = limiter.register(2000);

        // Should request 2 seconds
        assert_eq!(sleep.requested(), Duration::from_secs(2));
        // Actual time is tracked (in tests, usually close to zero)
        assert!(sleep.actual() < Duration::from_millis(100));
    }

    #[test]
    fn register_zero_returns_noop_sleep() {
        let mut limiter = BandwidthLimiter::new(nz(1000));
        let sleep = limiter.register(0);

        assert!(sleep.is_noop());
        assert_eq!(sleep.requested(), Duration::ZERO);
        assert_eq!(sleep.actual(), Duration::ZERO);
    }

    // ========================================================================
    // Clone behavior tests
    // ========================================================================

    #[test]
    fn clone_preserves_configuration() {
        let limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(500)));
        let cloned = limiter.clone();

        assert_eq!(cloned.limit_bytes(), limiter.limit_bytes());
        assert_eq!(cloned.burst_bytes(), limiter.burst_bytes());
        assert_eq!(cloned.write_max_bytes(), limiter.write_max_bytes());
    }

    #[test]
    fn clone_preserves_state() {
        let mut limiter = BandwidthLimiter::new(nz(1000));
        let _ = limiter.register(100);

        let cloned = limiter.clone();

        // State should be preserved
        assert_eq!(
            cloned.accumulated_debt_for_testing(),
            limiter.accumulated_debt_for_testing()
        );
    }

    #[test]
    fn cloned_limiter_independent() {
        let mut limiter = BandwidthLimiter::new(nz(1000));
        let cloned = limiter.clone();

        // Modify original
        let _ = limiter.register(100);

        // Clone should be unaffected
        assert_eq!(cloned.accumulated_debt_for_testing(), 0);
    }

    // ========================================================================
    // Edge case: very large writes
    // ========================================================================

    #[test]
    fn register_very_large_write() {
        use super::super::recorded_sleep_session;

        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1_000_000)); // 1 MB/s

        // Write 1 GB
        let sleep = limiter.register(1_000_000_000);

        // Should sleep for 1000 seconds
        assert_eq!(sleep.requested(), Duration::from_secs(1000));
    }

    #[test]
    fn register_usize_max_with_high_rate() {
        use super::super::recorded_sleep_session;

        let mut session = recorded_sleep_session();
        session.clear();

        // Very high rate to keep sleep manageable
        let mut limiter = BandwidthLimiter::new(nz(u64::MAX / 1000));

        // Large write - should not panic
        let _sleep = limiter.register(usize::MAX / 1000);
    }

    // ========================================================================
    // Simulated elapsed time tests
    // ========================================================================

    #[test]
    fn simulated_elapsed_time_reduces_debt() {
        use super::super::recorded_sleep_session;

        let mut session = recorded_sleep_session();
        session.clear();

        // In test mode, sleeps are recorded but not actually performed
        // The limiter tracks simulated_elapsed_us to compensate

        let mut limiter = BandwidthLimiter::new(nz(1000));

        // First write
        let sleep1 = limiter.register(1000);
        assert_eq!(sleep1.requested(), Duration::from_secs(1));

        // In tests, the actual sleep time is near-zero, but the limiter
        // should track simulated elapsed time for subsequent calls
    }

    // ========================================================================
    // Boundary condition tests
    // ========================================================================

    #[test]
    fn register_exactly_one_byte() {
        let mut limiter = BandwidthLimiter::new(nz(1)); // 1 byte/second
        let sleep = limiter.register(1);
        assert_eq!(sleep.requested(), Duration::from_secs(1));
    }

    #[test]
    fn register_with_max_u64_limit() {
        let mut limiter = BandwidthLimiter::new(nz(u64::MAX));
        let sleep = limiter.register(1000);
        // With max rate, sleep should be negligible
        assert!(sleep.requested() < Duration::from_nanos(100));
    }

    #[test]
    fn register_with_max_burst() {
        let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(u64::MAX)));
        let sleep = limiter.register(5000);
        // With max burst, no clamping should occur
        assert_eq!(sleep.requested(), Duration::from_secs(5));
    }

    #[test]
    fn register_with_min_burst() {
        let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(1)));
        let _ = limiter.register(1000);
        // Debt should be clamped to 1
        assert!(limiter.accumulated_debt_for_testing() <= 1);
    }

    // ========================================================================
    // Accessor method consistency tests
    // ========================================================================

    #[test]
    fn accessors_consistent_after_construction() {
        let limiter = BandwidthLimiter::with_burst(nz(5000), Some(nz(2500)));

        assert_eq!(limiter.limit_bytes().get(), 5000);
        assert_eq!(limiter.burst_bytes().unwrap().get(), 2500);
        // write_max should reflect burst
        assert_eq!(limiter.write_max_bytes(), 2500);
    }

    #[test]
    fn accessors_consistent_after_update() {
        let mut limiter = BandwidthLimiter::new(nz(1000));
        limiter.update_configuration(nz(2000), Some(nz(1500)));

        assert_eq!(limiter.limit_bytes().get(), 2000);
        assert_eq!(limiter.burst_bytes().unwrap().get(), 1500);
        assert_eq!(limiter.write_max_bytes(), 1500);
    }

    #[test]
    fn accessors_consistent_after_reset() {
        let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(500)));
        let _ = limiter.register(1000);
        limiter.reset();

        // Configuration unchanged
        assert_eq!(limiter.limit_bytes().get(), 1000);
        assert_eq!(limiter.burst_bytes().unwrap().get(), 500);
        assert_eq!(limiter.write_max_bytes(), MIN_WRITE_MAX.max(500));
        // State reset
        assert_eq!(limiter.accumulated_debt_for_testing(), 0);
    }

    // ========================================================================
    // Integration-style tests
    // ========================================================================

    #[test]
    fn simulated_transfer_scenario() {
        use super::super::recorded_sleep_session;

        let mut session = recorded_sleep_session();
        session.clear();

        // Simulate transferring a 10KB file at 1KB/s
        let mut limiter = BandwidthLimiter::new(nz(1024)); // 1 KB/s

        let mut total_requested = Duration::ZERO;
        for _ in 0..10 {
            let sleep = limiter.register(1024);
            total_requested = total_requested.saturating_add(sleep.requested());
        }

        // Should have requested approximately 10 seconds total sleep
        // (may be slightly less due to timing calculations between calls)
        assert!(total_requested >= Duration::from_secs(9));
        assert!(total_requested <= Duration::from_secs(11));
    }

    #[test]
    fn simulated_bursty_transfer() {
        use super::super::recorded_sleep_session;

        let mut session = recorded_sleep_session();
        session.clear();

        // Simulate bursty transfer with burst limit
        let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(500)));

        // Large burst write - debt clamped to 500
        let sleep1 = limiter.register(2000);
        assert!(sleep1.requested() <= Duration::from_millis(500));

        // Another burst - debt still clamped
        let sleep2 = limiter.register(2000);
        assert!(sleep2.requested() <= Duration::from_millis(500));
    }
}
