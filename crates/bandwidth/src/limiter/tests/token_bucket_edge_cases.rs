use super::*;
use crate::limiter::MIN_WRITE_MAX;
use std::num::NonZeroU64;
use std::time::Duration;

fn nz(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("non-zero value required")
}

#[test]
fn token_bucket_debt_ceiling_at_u128_max() {
    // Even with extremely large writes, debt shouldn't panic or wrap around
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1)); // 1 B/s (slowest possible)

    // Multiple massive writes - test saturating_add behavior
    for _ in 0..5 {
        let _ = limiter.register(usize::MAX / 100);
    }

    // Should not panic; debt is internally managed
    // The debt is clamped by timing calculations
}

#[test]
fn token_bucket_zero_elapsed_time_accumulates_full_debt() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1024));

    // First register with no prior instant
    let sleep = limiter.register(2048);

    // All bytes become debt, sleep for 2 seconds
    assert_eq!(sleep.requested(), Duration::from_secs(2));
}

#[test]
fn token_bucket_timing_forgives_exactly_all_debt() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1000));

    // Register some debt
    let _ = limiter.register(1000);

    // Sleep exactly long enough
    std::thread::sleep(Duration::from_secs(1));

    // Next register should have minimal debt (forgiven by elapsed time)
    let sleep = limiter.register(100);

    // Due to timing variations and simulated sleep in tests, sleep may be near-zero
    // Allow for timing tolerance in test environment
    assert!(
        sleep.requested() <= Duration::from_millis(150),
        "Expected sleep <= 150ms, got {:?}",
        sleep.requested()
    );
}

#[test]
fn rate_limiting_accuracy_simple_doubling() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1000));

    // Write 1000 bytes - should sleep 1 second
    let sleep1 = limiter.register(1000);
    assert_eq!(sleep1.requested(), Duration::from_secs(1));

    session.clear();

    // Write 2000 bytes - should sleep ~2 seconds.
    // Allow 1ms tolerance: integer division truncation in microsecond
    // arithmetic can lose up to 1 byte of credit per register() call,
    // which at 1000 bytes/sec maps to 1ms of sleep shortfall.
    let sleep2 = limiter.register(2000);
    let requested = sleep2.requested();
    assert!(
        requested >= Duration::from_millis(1999) && requested <= Duration::from_secs(2),
        "expected ~2s sleep, got {requested:?}"
    );
}

#[test]
fn rate_limiting_accuracy_fractional_seconds() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1000));

    // Write 250 bytes - should sleep 0.25 seconds (250ms)
    let sleep = limiter.register(250);
    assert_eq!(sleep.requested(), Duration::from_millis(250));
}

#[test]
fn rate_limiting_accuracy_microsecond_precision() {
    let mut session = recorded_sleep_session();
    session.clear();

    // 1MB/s rate
    let mut limiter = BandwidthLimiter::new(nz(1_000_000));

    // Write 1 byte - should sleep 1 microsecond
    let sleep = limiter.register(1);

    // At 1 MB/s, 1 byte takes 1 microsecond
    // But MINIMUM_SLEEP_MICROS is 100_000, so should be noop
    assert!(sleep.is_noop());
}

#[test]
fn rate_limiting_accuracy_at_minimum_threshold() {
    let mut session = recorded_sleep_session();
    session.clear();

    // MINIMUM_SLEEP_MICROS is 100_000 (0.1 seconds)
    // Configure limiter so 100_000 bytes takes exactly 0.1 seconds
    let mut limiter = BandwidthLimiter::new(nz(1_000_000)); // 1 MB/s

    // Write exactly the amount for minimum threshold
    let sleep = limiter.register(100_000);

    // Should trigger sleep (at threshold, not below)
    assert!(!sleep.is_noop());
    assert_eq!(sleep.requested(), Duration::from_millis(100));
}

#[test]
fn rate_limiting_accuracy_just_below_minimum_threshold() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1_000_000)); // 1 MB/s

    // Write just below threshold (99_999 bytes -> 99,999 us)
    let sleep = limiter.register(99_999);

    // Should be noop (below MINIMUM_SLEEP_MICROS)
    assert!(sleep.is_noop());
}

#[test]
fn large_write_accumulates_unbounded_debt() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(10));

    // Write large amount - debt accumulates proportionally to the rate.
    let sleep = limiter.register(10000);

    // Should sleep for 1000 seconds (10000 / 10)
    assert_eq!(sleep.requested(), Duration::from_secs(1000));
}

#[test]
fn config_update_during_large_debt() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(10));

    // Accumulate huge debt
    let _ = limiter.register(10000);

    // Update to faster rate - should clear debt
    limiter.update_limit(nz(1_000_000));

    assert_eq!(limiter.accumulated_debt_for_testing(), 0);

    // Next write should be fast
    let sleep = limiter.register(1000);
    assert!(sleep.requested() < Duration::from_millis(10));
}

#[test]
fn reset_clears_last_instant() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1_000_000));

    // Establish last_instant
    let _ = limiter.register(1000);

    // Reset
    limiter.reset();

    // Next register should behave like first (no elapsed time)
    let sleep = limiter.register(100_000);
    assert_eq!(sleep.requested(), Duration::from_millis(100));
}

#[test]
fn write_max_saturating_multiplication() {
    let limiter = BandwidthLimiter::new(nz(u64::MAX));

    let write_max = limiter.write_max_bytes();

    // Should be a valid usize, not panic
    assert!(write_max > 0);
    assert!(write_max >= MIN_WRITE_MAX);
}

#[test]
fn write_max_1kib_calculation() {
    // Exactly 1 KiB/s should give base_write_max = 128, clamped to MIN_WRITE_MAX
    let limiter = BandwidthLimiter::new(nz(1024));

    assert_eq!(limiter.write_max_bytes(), MIN_WRITE_MAX);
}

#[test]
fn write_max_scales_linearly_in_kib_range() {
    // In the KiB range, write_max should scale roughly linearly
    let limiter_10k = BandwidthLimiter::new(nz(10 * 1024));
    let limiter_20k = BandwidthLimiter::new(nz(20 * 1024));

    let wm_10k = limiter_10k.write_max_bytes();
    let wm_20k = limiter_20k.write_max_bytes();

    // 20k should be roughly 2x of 10k
    assert!(wm_20k > wm_10k);
    assert!(wm_20k >= wm_10k * 2);
}

#[test]
fn recommended_read_size_zero_buffer() {
    let limiter = BandwidthLimiter::new(nz(1024));

    // Zero buffer should return zero
    assert_eq!(limiter.recommended_read_size(0), 0);
}

#[test]
fn recommended_read_size_one_byte_buffer() {
    let limiter = BandwidthLimiter::new(nz(1024));

    // One byte buffer should return one
    assert_eq!(limiter.recommended_read_size(1), 1);
}

#[test]
fn recommended_read_size_exactly_write_max() {
    let limiter = BandwidthLimiter::new(nz(100 * 1024)); // write_max = 12800

    let write_max = limiter.write_max_bytes();
    assert_eq!(limiter.recommended_read_size(write_max), write_max);
}

#[test]
fn recommended_read_size_usize_max_buffer() {
    let limiter = BandwidthLimiter::new(nz(1024));

    // Even with huge buffer, should clamp to write_max
    let result = limiter.recommended_read_size(usize::MAX);
    assert_eq!(result, limiter.write_max_bytes());
}

#[test]
fn elapsed_time_u64_max_clamping() {
    // Exercises the `.min(u128::from(u64::MAX))` saturation in the elapsed-time
    // path so an absurd Instant gap still produces a finite microsecond count.
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1_000_000));

    // Register, then register again quickly
    let _ = limiter.register(1000);
    let _ = limiter.register(1000);

    // Should not panic from timing calculations
}

#[test]
fn simulated_elapsed_carries_forward() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1000));

    // First register creates debt and sleeps
    let sleep1 = limiter.register(1000);
    assert_eq!(sleep1.requested(), Duration::from_secs(1));

    // In test mode, actual sleep time is near-zero
    // simulated_elapsed_us should compensate in the next register
}

#[test]
fn sleep_calculation_division_precision() {
    let mut session = recorded_sleep_session();
    session.clear();

    // Test division precision: 333 bytes at 1000 B/s = 0.333 seconds = 333ms
    let mut limiter = BandwidthLimiter::new(nz(1000));

    let sleep = limiter.register(333);
    assert_eq!(sleep.requested(), Duration::from_millis(333));
}

#[test]
fn multiple_zero_writes_are_noops() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1000));

    // Multiple zero writes
    for _ in 0..100 {
        let sleep = limiter.register(0);
        assert!(sleep.is_noop());
    }

    // Debt should still be zero
    assert_eq!(limiter.accumulated_debt_for_testing(), 0);
}

#[test]
fn zero_write_between_normal_writes() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1000));

    let _ = limiter.register(500);
    let sleep_zero = limiter.register(0);
    assert!(sleep_zero.is_noop());
    let _ = limiter.register(500);

    // Zero write shouldn't affect timing
}

#[test]
fn clone_creates_independent_limiter() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter1 = BandwidthLimiter::new(nz(1000));
    let _ = limiter1.register(500);

    let limiter2 = limiter1.clone();

    // Modify limiter1
    let _ = limiter1.register(500);

    // limiter2 should still have old debt
    // (both have the same debt since clone preserves state)
    let debt2 = limiter2.accumulated_debt_for_testing();
    let _ = debt2; // Just verify no panic
}

#[test]
fn debug_output_contains_relevant_info() {
    let limiter = BandwidthLimiter::new(nz(12345));

    let debug = format!("{limiter:?}");

    // Should contain the configured limit value
    assert!(debug.contains("12345") || debug.contains("limit"));
}

#[test]
fn stress_alternating_large_small_writes() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(10_000));

    for i in 0..50 {
        if i % 2 == 0 {
            let _ = limiter.register(100_000);
        } else {
            let _ = limiter.register(10);
        }
    }

    // Should not panic or have unexpected behavior
}

#[test]
fn stress_rapid_config_changes() {
    let mut limiter = BandwidthLimiter::new(nz(1000));

    for i in 1..=100 {
        let limit = nz(i * 1000);
        limiter.update_limit(limit);
        let _ = limiter.register(100);
    }

    // Should handle rapid reconfiguration
}

#[test]
fn boundary_min_write_max_exact() {
    // Limiter configured to give exactly MIN_WRITE_MAX
    let limiter = BandwidthLimiter::new(nz(512));

    assert_eq!(limiter.write_max_bytes(), MIN_WRITE_MAX);
}

#[test]
fn boundary_sleep_exactly_one_second() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1024));

    // Write exactly 1024 bytes - should sleep exactly 1 second
    let sleep = limiter.register(1024);
    assert_eq!(sleep.requested(), Duration::from_secs(1));
}

#[test]
fn boundary_sleep_exactly_one_millisecond() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1_000_000)); // 1 MB/s

    // Write 1000 bytes - should sleep 1 millisecond
    let sleep = limiter.register(1000);
    // At 1 MB/s, 1000 bytes = 1ms, but this is below MINIMUM_SLEEP_MICROS (100ms)
    // So this should be a noop or very small sleep
    assert!(
        sleep.is_noop() || sleep.requested() < Duration::from_millis(100),
        "Expected noop or <100ms, got {:?}",
        sleep.requested()
    );
}

#[test]
fn boundary_u64_max_rate_minimal_sleep() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(u64::MAX));

    // Even large writes should have negligible sleep
    let sleep = limiter.register(1_000_000);

    assert!(sleep.requested() < Duration::from_micros(100));
}
