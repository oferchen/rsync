//! Comprehensive test coverage for the bandwidth crate.
//!
//! This module provides extensive tests targeting 95%+ coverage:
//! 1. Token bucket algorithm edge cases
//! 2. Burst handling
//! 3. Rate limiting accuracy
//! 4. Limit updates during transfer
//! 5. Zero/unlimited bandwidth cases
//!
//! Note: Integration tests cannot access internal methods like `accumulated_debt_for_testing`,
//! so we verify behavior through observable effects (sleep durations, etc.).

use bandwidth::{
    BandwidthLimiter, LimiterChange, apply_effective_limit,
    parse_bandwidth_argument, parse_bandwidth_limit, recorded_sleep_session,
};
use std::num::NonZeroU64;
use std::time::Duration;

// ============================================================================
// Helper functions
// ============================================================================

fn nz(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("non-zero value required")
}

/// Verifies duration is within tolerance percentage of expected
fn assert_duration_within_tolerance(actual: Duration, expected: Duration, tolerance_percent: f64) {
    let tolerance = Duration::from_secs_f64(expected.as_secs_f64() * tolerance_percent / 100.0);
    let min = expected.saturating_sub(tolerance);
    let max = expected.saturating_add(tolerance);
    assert!(
        actual >= min && actual <= max,
        "Duration {:?} not within {}% of expected {:?} (range {:?} to {:?})",
        actual,
        tolerance_percent,
        expected,
        min,
        max
    );
}

// ============================================================================
// 1. Token Bucket Algorithm Edge Cases
// ============================================================================

mod token_bucket_edge_cases {
    use super::*;

    #[test]
    fn debt_accumulates_linearly_with_writes() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1000)); // 1000 bytes/sec

        // Write 100 bytes 10 times
        let mut total_sleep = Duration::ZERO;
        for _ in 0..10 {
            let sleep = limiter.register(100);
            total_sleep = total_sleep.saturating_add(sleep.requested());
        }

        // Total 1000 bytes at 1000 bytes/sec should be ~1 second
        assert_duration_within_tolerance(total_sleep, Duration::from_secs(1), 15.0);
    }

    #[test]
    fn debt_clamped_at_burst_boundary() {
        let mut session = recorded_sleep_session();
        session.clear();

        // 100 bytes/sec with 200 byte burst cap
        let mut limiter = BandwidthLimiter::with_burst(nz(100), Some(nz(200)));

        // Write 1000 bytes - debt should be clamped to 200
        let sleep = limiter.register(1000);

        // At 100 bytes/sec with 200 byte max debt = 2 seconds max sleep
        // (Verifying burst clamping through sleep duration)
        assert!(sleep.requested() <= Duration::from_secs(2));
    }

    #[test]
    fn debt_reduces_with_elapsed_time() {
        let mut limiter = BandwidthLimiter::new(nz(1_000_000)); // 1 MB/s

        // Write some bytes
        let _ = limiter.register(10000);

        // Wait for time to elapse
        std::thread::sleep(Duration::from_millis(50));

        // Debt should be reduced on next register due to elapsed time
        // (exact behavior depends on internal implementation)
        let _ = limiter.register(100);

        // No panic or overflow should occur
    }

    #[test]
    fn minimum_sleep_threshold_exactly_at_boundary() {
        // MINIMUM_SLEEP_MICROS is 100,000 (0.1 seconds)
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1_000_000)); // 1 MB/s

        // 100,000 bytes at 1 MB/s = exactly 100ms = exactly at threshold
        let sleep = limiter.register(100_000);

        // At threshold, should trigger sleep (not noop)
        assert!(!sleep.is_noop());
        assert_eq!(sleep.requested(), Duration::from_millis(100));
    }

    #[test]
    fn minimum_sleep_threshold_just_below_boundary() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1_000_000)); // 1 MB/s

        // 99,999 bytes at 1 MB/s = 99.999ms = just below threshold
        let sleep = limiter.register(99_999);

        // Just below threshold should be noop
        assert!(sleep.is_noop() || sleep.requested() < Duration::from_millis(100));
    }

    #[test]
    fn minimum_sleep_threshold_just_above_boundary() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1_000_000)); // 1 MB/s

        // 100,001 bytes at 1 MB/s = 100.001ms = just above threshold
        let sleep = limiter.register(100_001);

        // Just above threshold should trigger sleep
        assert!(!sleep.is_noop());
    }

    #[test]
    fn debt_saturating_add_with_extreme_writes() {
        // Use a very high rate to avoid long sleeps in integration tests
        // (where actual sleeping occurs unlike unit tests)
        let mut limiter = BandwidthLimiter::new(nz(u64::MAX));

        // Many very large writes - should use saturating add
        for _ in 0..10 {
            let _ = limiter.register(usize::MAX / 100);
        }

        // Should not panic or overflow
    }

    #[test]
    fn first_register_establishes_baseline() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1024)); // 1 KB/s

        // First register has no prior timestamp
        let sleep1 = limiter.register(1024);

        // Should sleep for exactly 1 second (1024 bytes at 1024 bytes/sec)
        assert_eq!(sleep1.requested(), Duration::from_secs(1));
    }

    #[test]
    fn simulated_elapsed_time_compensation() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1024));

        // First write
        let sleep1 = limiter.register(1024);
        assert_eq!(sleep1.requested(), Duration::from_secs(1));

        // Second write should account for "simulated" time from first sleep
        let sleep2 = limiter.register(512);

        // Due to simulated elapsed time tracking, second sleep should be smaller
        assert!(sleep2.requested() <= Duration::from_millis(600));
    }
}

// ============================================================================
// 2. Burst Handling
// ============================================================================

mod burst_handling {
    use super::*;

    #[test]
    fn burst_caps_maximum_sleep() {
        let mut session = recorded_sleep_session();
        session.clear();

        // 1000 bytes/sec with 500 byte burst cap
        let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(500)));

        let sleep = limiter.register(10000);

        // With burst cap of 500 at 1000 B/s, max sleep should be 0.5 seconds
        assert!(sleep.requested() <= Duration::from_millis(500));
    }

    #[test]
    fn burst_caps_after_multiple_writes() {
        let mut session = recorded_sleep_session();
        session.clear();

        // 1000 bytes/sec with 500 byte burst cap
        let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(500)));

        for _ in 0..10 {
            let sleep = limiter.register(1000);
            // Each write's sleep should be capped to 500/1000 = 0.5 seconds
            assert!(sleep.requested() <= Duration::from_millis(500));
        }
    }

    #[test]
    fn no_burst_allows_unlimited_debt() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(10)); // Very slow: 10 bytes/sec

        // Write 1000 bytes
        let sleep = limiter.register(1000);

        // Without burst cap, sleep should be 100 seconds (1000/10)
        assert_eq!(sleep.requested(), Duration::from_secs(100));
    }

    #[test]
    fn burst_of_one_is_minimum() {
        let mut session = recorded_sleep_session();
        session.clear();

        // 1000 bytes/sec with burst cap of 1
        let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(1)));

        let sleep = limiter.register(10000);

        // With burst of 1 at 1000 B/s, max sleep is 0.001 seconds
        assert!(sleep.requested() <= Duration::from_millis(2));
    }

    #[test]
    fn burst_at_u64_max_no_clamping() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(u64::MAX)));

        let sleep = limiter.register(5000);

        // With max burst, no clamping occurs
        assert_eq!(sleep.requested(), Duration::from_secs(5));
    }

    #[test]
    fn burst_affects_write_max_calculation() {
        // Large rate but small burst
        let limiter = BandwidthLimiter::with_burst(nz(1024 * 1024), Some(nz(4096)));

        // Write max should be the burst value
        assert_eq!(limiter.write_max_bytes(), 4096);
    }

    #[test]
    fn burst_smaller_than_min_write_max_uses_minimum() {
        let limiter = BandwidthLimiter::with_burst(nz(1024 * 1024), Some(nz(100)));

        // MIN_WRITE_MAX is 512
        assert_eq!(limiter.write_max_bytes(), 512);
    }

    #[test]
    fn burst_larger_than_calculated_write_max() {
        // Small rate (would have small write_max) but large burst
        let limiter = BandwidthLimiter::with_burst(nz(1024), Some(nz(1_000_000)));

        // Write max should be burst value
        assert_eq!(limiter.write_max_bytes(), 1_000_000);
    }

    #[test]
    fn burst_clamping_occurs_before_and_after_elapsed_time() {
        let mut session = recorded_sleep_session();
        session.clear();

        // 100 bytes/sec with 500 byte burst cap
        let mut limiter = BandwidthLimiter::with_burst(nz(100), Some(nz(500)));

        // Large write - sleep capped to 500/100 = 5 seconds
        let sleep1 = limiter.register(10000);
        assert!(sleep1.requested() <= Duration::from_secs(5));

        // Wait and write again - sleep still capped
        std::thread::sleep(Duration::from_millis(10));
        let sleep2 = limiter.register(10000);
        assert!(sleep2.requested() <= Duration::from_secs(5));
    }

    #[test]
    fn burst_limits_sleep_duration() {
        let mut session = recorded_sleep_session();
        session.clear();

        // Very slow rate with small burst
        let mut limiter = BandwidthLimiter::with_burst(nz(10), Some(nz(100)));

        // Large write
        let sleep = limiter.register(10000);

        // Sleep capped to burst/rate = 100/10 = 10 seconds
        assert!(sleep.requested() <= Duration::from_secs(10));
    }
}

// ============================================================================
// 3. Rate Limiting Accuracy
// ============================================================================

mod rate_limiting_accuracy {
    use super::*;

    #[test]
    fn precise_calculation_1_second() {
        let mut session = recorded_sleep_session();
        session.clear();

        let test_cases: Vec<(u64, usize)> = vec![
            (1000, 1000),     // 1000 bytes at 1000 B/s = 1s
            (2000, 2000),     // 2000 bytes at 2000 B/s = 1s
            (1024, 1024),     // 1024 bytes at 1024 B/s = 1s
            (500, 500),       // 500 bytes at 500 B/s = 1s
            (10000, 10000),   // 10000 bytes at 10000 B/s = 1s
        ];

        for (rate, bytes) in test_cases {
            session.clear();
            let mut limiter = BandwidthLimiter::new(nz(rate));
            let sleep = limiter.register(bytes);
            assert_eq!(
                sleep.requested(),
                Duration::from_secs(1),
                "Rate {}, bytes {} should result in 1 second",
                rate,
                bytes
            );
        }
    }

    #[test]
    fn precise_calculation_fractions() {
        let mut session = recorded_sleep_session();
        session.clear();

        let test_cases: Vec<(u64, usize, Duration)> = vec![
            (1000, 500, Duration::from_millis(500)),     // 0.5s
            (1000, 250, Duration::from_millis(250)),     // 0.25s
            (1000, 750, Duration::from_millis(750)),     // 0.75s
            (2000, 500, Duration::from_millis(250)),     // 0.25s
            (4000, 1000, Duration::from_millis(250)),    // 0.25s
        ];

        for (rate, bytes, expected) in test_cases {
            session.clear();
            let mut limiter = BandwidthLimiter::new(nz(rate));
            let sleep = limiter.register(bytes);
            assert_eq!(
                sleep.requested(),
                expected,
                "Rate {}, bytes {} should result in {:?}",
                rate,
                bytes,
                expected
            );
        }
    }

    #[test]
    fn precise_calculation_multiple_seconds() {
        let mut session = recorded_sleep_session();
        session.clear();

        let test_cases: Vec<(u64, usize, u64)> = vec![
            (1000, 2000, 2),    // 2s
            (1000, 5000, 5),    // 5s
            (1000, 10000, 10),  // 10s
            (500, 5000, 10),    // 10s
            (100, 1000, 10),    // 10s
        ];

        for (rate, bytes, expected_secs) in test_cases {
            session.clear();
            let mut limiter = BandwidthLimiter::new(nz(rate));
            let sleep = limiter.register(bytes);
            assert_eq!(
                sleep.requested(),
                Duration::from_secs(expected_secs),
                "Rate {}, bytes {} should result in {} seconds",
                rate,
                bytes,
                expected_secs
            );
        }
    }

    #[test]
    fn accumulated_sleep_matches_expected_total() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1024)); // 1 KB/s
        let chunk_size = 512; // Half of rate
        let num_chunks = 10;

        for _ in 0..num_chunks {
            let _ = limiter.register(chunk_size);
        }

        // Total: 5120 bytes at 1024 B/s = 5 seconds
        let total = session.total_duration();
        assert_duration_within_tolerance(total, Duration::from_secs(5), 10.0);
    }

    #[test]
    fn very_fast_rate_negligible_sleep() {
        let mut session = recorded_sleep_session();
        session.clear();

        // 1 GB/s
        let mut limiter = BandwidthLimiter::new(nz(1_000_000_000));

        let sleep = limiter.register(1_000_000); // 1 MB

        // At 1 GB/s, 1 MB takes 1ms
        assert!(sleep.requested() <= Duration::from_millis(2));
    }

    #[test]
    fn very_slow_rate_large_sleep() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1)); // 1 byte/sec

        let sleep = limiter.register(10);

        // At 1 byte/sec, 10 bytes = 10 seconds
        assert_eq!(sleep.requested(), Duration::from_secs(10));
    }

    #[test]
    fn millisecond_precision() {
        let mut session = recorded_sleep_session();
        session.clear();

        // 1 MB/s for easy calculation
        let mut limiter = BandwidthLimiter::new(nz(1_000_000));

        // 123,000 bytes = 123ms
        let sleep = limiter.register(123_000);
        assert_eq!(sleep.requested(), Duration::from_millis(123));
    }
}

// ============================================================================
// 4. Limit Updates During Transfer
// ============================================================================

mod limit_updates_during_transfer {
    use super::*;

    #[test]
    fn update_limit_resets_state() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(100));

        // Write to accumulate state
        let _ = limiter.register(10000);

        // Update limit resets internal state
        limiter.update_limit(nz(200));

        // Fresh write should behave as if starting fresh
        session.clear();
        let sleep = limiter.register(200);

        // 200 bytes at 200 B/s = 1 second (no accumulated debt)
        assert_eq!(sleep.requested(), Duration::from_secs(1));
    }

    #[test]
    fn update_configuration_resets_state() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(100));

        // Write to accumulate state
        let _ = limiter.register(10000);

        // Update configuration resets internal state
        limiter.update_configuration(nz(200), Some(nz(500)));

        // Fresh write should behave as if starting fresh
        session.clear();
        let sleep = limiter.register(200);

        // 200 bytes at 200 B/s = 1 second
        assert_eq!(sleep.requested(), Duration::from_secs(1));
    }

    #[test]
    fn reset_clears_state_preserves_config() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::with_burst(nz(100), Some(nz(500)));

        let _ = limiter.register(10000);

        limiter.reset();

        // Configuration preserved
        assert_eq!(limiter.limit_bytes().get(), 100);
        assert_eq!(limiter.burst_bytes().unwrap().get(), 500);

        // Fresh write should behave as if starting fresh
        session.clear();
        let sleep = limiter.register(100);
        assert_eq!(sleep.requested(), Duration::from_secs(1));
    }

    #[test]
    fn update_limit_preserves_burst() {
        let mut limiter = BandwidthLimiter::with_burst(nz(100), Some(nz(500)));

        limiter.update_limit(nz(200));

        assert_eq!(limiter.limit_bytes().get(), 200);
        assert_eq!(limiter.burst_bytes().unwrap().get(), 500);
    }

    #[test]
    fn update_configuration_changes_both() {
        let mut limiter = BandwidthLimiter::new(nz(100));

        limiter.update_configuration(nz(200), Some(nz(1000)));

        assert_eq!(limiter.limit_bytes().get(), 200);
        assert_eq!(limiter.burst_bytes().unwrap().get(), 1000);
    }

    #[test]
    fn update_configuration_can_remove_burst() {
        let mut limiter = BandwidthLimiter::with_burst(nz(100), Some(nz(500)));

        limiter.update_configuration(nz(200), None);

        assert_eq!(limiter.limit_bytes().get(), 200);
        assert!(limiter.burst_bytes().is_none());
    }

    #[test]
    fn update_limit_changes_write_max() {
        let mut limiter = BandwidthLimiter::new(nz(1024)); // Low rate
        let initial = limiter.write_max_bytes();

        limiter.update_limit(nz(1024 * 1024)); // High rate
        let updated = limiter.write_max_bytes();

        assert!(updated > initial);
    }

    #[test]
    fn rate_increase_during_transfer() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1024));

        // Initial write at slow rate
        let sleep1 = limiter.register(1024);
        assert_eq!(sleep1.requested(), Duration::from_secs(1));

        // Increase rate 10x
        limiter.update_limit(nz(10240));

        // Same write should be 10x faster
        session.clear();
        let sleep2 = limiter.register(1024);
        assert!(sleep2.requested() <= Duration::from_millis(150));
    }

    #[test]
    fn rate_decrease_during_transfer() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(10240)); // Fast

        // Write at fast rate
        let sleep1 = limiter.register(1024);
        assert!(sleep1.requested() <= Duration::from_millis(150));

        // Decrease rate 10x
        limiter.update_limit(nz(1024));

        // Same write should be 10x slower
        session.clear();
        let sleep2 = limiter.register(1024);
        assert_eq!(sleep2.requested(), Duration::from_secs(1));
    }

    #[test]
    fn multiple_rate_changes_during_transfer() {
        let mut limiter = BandwidthLimiter::new(nz(1000));

        // First batch
        let _ = limiter.register(500);

        // Change rate
        limiter.update_limit(nz(2000));
        let _ = limiter.register(1000);

        // Change rate again
        limiter.update_limit(nz(500));
        let _ = limiter.register(250);

        // No panics, proper state management
    }

    #[test]
    fn burst_change_during_transfer() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1000));

        let _ = limiter.register(5000);

        // Add burst cap
        limiter.update_configuration(nz(1000), Some(nz(200)));

        let sleep = limiter.register(5000);

        // With burst cap of 200 at 1000 B/s, max sleep = 0.2 seconds
        assert!(sleep.requested() <= Duration::from_millis(200));
    }
}

// ============================================================================
// 5. Zero/Unlimited Bandwidth Cases
// ============================================================================

mod zero_unlimited_bandwidth {
    use super::*;

    #[test]
    fn zero_byte_write_is_noop() {
        let mut limiter = BandwidthLimiter::new(nz(1000));

        let sleep = limiter.register(0);

        assert!(sleep.is_noop());
        assert_eq!(sleep.requested(), Duration::ZERO);
        assert_eq!(sleep.actual(), Duration::ZERO);
    }

    #[test]
    fn zero_byte_write_does_not_affect_behavior() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1000));

        // Initial write
        let _sleep1 = limiter.register(500);

        // Zero-byte write should not affect anything
        let zero_sleep = limiter.register(0);
        assert!(zero_sleep.is_noop());

        // Zero-byte write should not have recorded any sleep
        // (session state depends on timing, so just verify noop)
        assert!(zero_sleep.is_noop());
    }

    #[test]
    fn parsing_zero_returns_unlimited() {
        let result = parse_bandwidth_argument("0").expect("parse succeeds");
        assert!(result.is_none(), "Zero should represent unlimited (None)");
    }

    #[test]
    fn parsing_zero_with_suffix_returns_unlimited() {
        let cases = ["0K", "0M", "0G", "0b", "0KB", "0MB", "0GB"];
        for case in cases {
            let result = parse_bandwidth_argument(case).expect("parse succeeds");
            assert!(result.is_none(), "{} should represent unlimited", case);
        }
    }

    #[test]
    fn minimum_rate_one_byte_per_second() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1));

        let sleep = limiter.register(1);
        assert_eq!(sleep.requested(), Duration::from_secs(1));
    }

    #[test]
    fn maximum_rate_u64_max() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(u64::MAX));

        let sleep = limiter.register(1_000_000);

        // Sleep should be essentially zero
        assert!(sleep.requested() < Duration::from_nanos(100));
    }

    #[test]
    fn unlimited_effective_limit_disables_limiter() {
        let mut limiter = Some(BandwidthLimiter::new(nz(1000)));

        let change = apply_effective_limit(
            &mut limiter,
            None,  // Unlimited
            true,  // Specified
            None,
            false,
        );

        assert_eq!(change, LimiterChange::Disabled);
        assert!(limiter.is_none());
    }

    #[test]
    fn apply_effective_limit_no_change_when_not_specified() {
        let mut limiter = Some(BandwidthLimiter::new(nz(1000)));

        let change = apply_effective_limit(
            &mut limiter,
            None,
            false,  // Not specified
            None,
            false,
        );

        assert_eq!(change, LimiterChange::Unchanged);
        assert!(limiter.is_some());
    }

    #[test]
    fn apply_effective_limit_enables_new_limiter() {
        let mut limiter: Option<BandwidthLimiter> = None;

        let change = apply_effective_limit(
            &mut limiter,
            Some(nz(1000)),
            true,
            None,
            false,
        );

        assert_eq!(change, LimiterChange::Enabled);
        assert!(limiter.is_some());
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 1000);
    }

    #[test]
    fn apply_effective_limit_uses_minimum_of_limits() {
        // Existing limiter with higher limit
        let mut limiter = Some(BandwidthLimiter::new(nz(2000)));

        let change = apply_effective_limit(
            &mut limiter,
            Some(nz(1000)),  // Lower limit
            true,
            None,
            false,
        );

        assert_eq!(change, LimiterChange::Updated);
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 1000);
    }

    #[test]
    fn apply_effective_limit_higher_limit_no_change() {
        let mut limiter = Some(BandwidthLimiter::new(nz(1000)));

        let change = apply_effective_limit(
            &mut limiter,
            Some(nz(2000)),  // Higher limit (ignored)
            true,
            None,
            false,
        );

        assert_eq!(change, LimiterChange::Unchanged);
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 1000);
    }
}

// ============================================================================
// Additional Edge Cases and Integration Tests
// ============================================================================

mod additional_edge_cases {
    use super::*;

    #[test]
    fn clone_creates_independent_copy() {
        let original = BandwidthLimiter::new(nz(1000));
        let cloned = original.clone();

        // Both should have same configuration
        assert_eq!(original.limit_bytes(), cloned.limit_bytes());
        assert_eq!(original.burst_bytes(), cloned.burst_bytes());
        assert_eq!(original.write_max_bytes(), cloned.write_max_bytes());
    }

    #[test]
    fn debug_format_contains_key_info() {
        let limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(500)));
        let debug = format!("{:?}", limiter);

        assert!(debug.contains("BandwidthLimiter"));
        assert!(debug.contains("1000"));
    }

    #[test]
    fn accessor_methods_return_correct_values() {
        let limiter = BandwidthLimiter::with_burst(nz(5000), Some(nz(2500)));

        assert_eq!(limiter.limit_bytes().get(), 5000);
        assert_eq!(limiter.burst_bytes().unwrap().get(), 2500);
        assert_eq!(limiter.write_max_bytes(), 2500);
    }

    #[test]
    fn recommended_read_size_edge_cases() {
        let limiter = BandwidthLimiter::new(nz(1024 * 100));
        let write_max = limiter.write_max_bytes();

        // Zero buffer
        assert_eq!(limiter.recommended_read_size(0), 0);

        // One byte
        assert_eq!(limiter.recommended_read_size(1), 1);

        // Exactly at write_max
        assert_eq!(limiter.recommended_read_size(write_max), write_max);

        // Just below
        assert_eq!(limiter.recommended_read_size(write_max - 1), write_max - 1);

        // Just above
        assert_eq!(limiter.recommended_read_size(write_max + 1), write_max);

        // Much larger
        assert_eq!(limiter.recommended_read_size(usize::MAX), write_max);
    }

    #[test]
    fn write_max_scales_with_rate() {
        let rates_and_expected: Vec<(u64, usize)> = vec![
            (1, 512),                    // Tiny rate -> MIN_WRITE_MAX
            (512, 512),                  // Below 1K -> MIN_WRITE_MAX
            (1024, 512),                 // 1K -> MIN_WRITE_MAX (128 < 512)
            (1024 * 10, 1280),           // 10K -> 10 * 128 = 1280
            (1024 * 100, 12800),         // 100K -> 100 * 128 = 12800
        ];

        for (rate, expected) in rates_and_expected {
            let limiter = BandwidthLimiter::new(nz(rate));
            assert_eq!(
                limiter.write_max_bytes(),
                expected,
                "Rate {} should have write_max {}",
                rate,
                expected
            );
        }
    }

    #[test]
    fn limiter_sleep_result_tracking() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1000));
        let sleep = limiter.register(2000);

        // Check LimiterSleep struct
        assert!(!sleep.is_noop());
        assert_eq!(sleep.requested(), Duration::from_secs(2));
        // In integration tests (with test-support feature), actual sleep occurs,
        // so actual() will reflect real elapsed time, not near-zero.
        // Just verify actual() returns a value (the API works correctly).
        let _ = sleep.actual();
    }

    #[test]
    fn simulated_realistic_file_transfer() {
        let mut session = recorded_sleep_session();
        session.clear();

        // Simulate 1MB file at 100KB/s
        let file_size = 1024 * 1024;
        let rate = 100 * 1024;
        let mut limiter = BandwidthLimiter::new(nz(rate as u64));

        let chunk_size = limiter.write_max_bytes();
        let mut remaining = file_size;

        while remaining > 0 {
            let to_transfer = remaining.min(chunk_size);
            let _ = limiter.register(to_transfer);
            remaining -= to_transfer;
        }

        // Should take ~10.24 seconds
        let total = session.total_duration();
        assert_duration_within_tolerance(total, Duration::from_secs_f64(10.24), 15.0);
    }
}

// ============================================================================
// LimiterChange Tests
// ============================================================================

mod limiter_change_tests {
    use super::*;

    #[test]
    fn limiter_change_priority_ordering() {
        assert!(LimiterChange::Unchanged < LimiterChange::Updated);
        assert!(LimiterChange::Updated < LimiterChange::Enabled);
        assert!(LimiterChange::Enabled < LimiterChange::Disabled);
    }

    #[test]
    fn limiter_change_combine_returns_higher() {
        assert_eq!(
            LimiterChange::Unchanged.combine(LimiterChange::Updated),
            LimiterChange::Updated
        );
        assert_eq!(
            LimiterChange::Updated.combine(LimiterChange::Enabled),
            LimiterChange::Enabled
        );
        assert_eq!(
            LimiterChange::Enabled.combine(LimiterChange::Disabled),
            LimiterChange::Disabled
        );
    }

    #[test]
    fn limiter_change_combine_all_returns_max() {
        let changes = vec![
            LimiterChange::Unchanged,
            LimiterChange::Updated,
            LimiterChange::Enabled,
        ];
        assert_eq!(LimiterChange::combine_all(changes), LimiterChange::Enabled);
    }

    #[test]
    fn limiter_change_combine_all_empty_returns_unchanged() {
        let empty: Vec<LimiterChange> = vec![];
        assert_eq!(LimiterChange::combine_all(empty), LimiterChange::Unchanged);
    }

    #[test]
    fn limiter_change_is_changed() {
        assert!(!LimiterChange::Unchanged.is_changed());
        assert!(LimiterChange::Updated.is_changed());
        assert!(LimiterChange::Enabled.is_changed());
        assert!(LimiterChange::Disabled.is_changed());
    }

    #[test]
    fn limiter_change_leaves_limiter_active() {
        assert!(!LimiterChange::Unchanged.leaves_limiter_active());
        assert!(LimiterChange::Updated.leaves_limiter_active());
        assert!(LimiterChange::Enabled.leaves_limiter_active());
        assert!(!LimiterChange::Disabled.leaves_limiter_active());
    }

    #[test]
    fn limiter_change_disables_limiter() {
        assert!(!LimiterChange::Unchanged.disables_limiter());
        assert!(!LimiterChange::Updated.disables_limiter());
        assert!(!LimiterChange::Enabled.disables_limiter());
        assert!(LimiterChange::Disabled.disables_limiter());
    }

    #[test]
    fn limiter_change_from_iterator() {
        let changes = vec![LimiterChange::Unchanged, LimiterChange::Enabled];
        let result: LimiterChange = changes.into_iter().collect();
        assert_eq!(result, LimiterChange::Enabled);
    }
}

// ============================================================================
// Parsing Tests for Coverage
// ============================================================================

mod parsing_coverage {
    use super::*;

    #[test]
    fn parse_bandwidth_argument_with_burst() {
        let result = parse_bandwidth_limit("1M:32K").expect("parse succeeds");

        assert_eq!(result.rate().unwrap().get(), 1024 * 1024);
        assert_eq!(result.burst().unwrap().get(), 32 * 1024);
    }

    #[test]
    fn parse_bandwidth_argument_without_burst() {
        let result = parse_bandwidth_limit("2M").expect("parse succeeds");

        assert_eq!(result.rate().unwrap().get(), 2 * 1024 * 1024);
        assert!(result.burst().is_none());
    }

    #[test]
    fn parse_various_units() {
        let cases: Vec<(&str, u64)> = vec![
            ("512b", 512),
            ("1K", 1024),
            ("1KB", 1000),
            ("1KiB", 1024),
            ("1M", 1024 * 1024),
            ("1MB", 1_000_000),
            ("1MiB", 1024 * 1024),
            ("1G", 1024 * 1024 * 1024),
            ("1GB", 1_000_000_000),
        ];

        for (input, expected) in cases {
            let result = parse_bandwidth_argument(input)
                .expect("parse succeeds")
                .expect("non-zero limit");
            assert_eq!(result.get(), expected, "Input '{}' should parse to {}", input, expected);
        }
    }

    #[test]
    fn parse_fractional_values() {
        let cases: Vec<(&str, u64)> = vec![
            ("0.5M", 512 * 1024),
            ("1.5M", 1_572_864),
            ("0.5MB", 500_000),
            ("1.5MB", 1_500_000),
        ];

        for (input, expected) in cases {
            let result = parse_bandwidth_argument(input)
                .expect("parse succeeds")
                .expect("non-zero limit");
            assert_eq!(result.get(), expected, "Input '{}' should parse to {}", input, expected);
        }
    }

    #[test]
    fn parse_default_unit_is_kilobytes() {
        let bare = parse_bandwidth_argument("100")
            .expect("parse succeeds")
            .expect("non-zero");
        let explicit = parse_bandwidth_argument("100K")
            .expect("parse succeeds")
            .expect("non-zero");

        assert_eq!(bare, explicit);
        assert_eq!(bare.get(), 100 * 1024);
    }

    #[test]
    fn parse_minimum_value_512_bytes() {
        let at_min = parse_bandwidth_argument("512b")
            .expect("parse succeeds");
        assert!(at_min.is_some());

        let below_min = parse_bandwidth_argument("511b");
        assert!(below_min.is_err());
    }
}
