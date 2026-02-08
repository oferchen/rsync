//! Scenario-based tests for --bwlimit bandwidth limiting.
//!
//! These tests simulate real-world transfer scenarios:
//! - File transfers with various chunk sizes
//! - Concurrent-style operations with rate changes
//! - Edge cases from rsync usage patterns
//! - Rate limiting accuracy under various conditions

use bandwidth::{
    BandwidthLimiter, LimiterChange, apply_effective_limit, parse_bandwidth_argument,
    parse_bandwidth_limit, recorded_sleep_session,
};
use std::num::NonZeroU64;
use std::time::Duration;

// ============================================================================
// Helper functions
// ============================================================================

fn nz(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("non-zero value required")
}

/// Check if actual duration is within tolerance of expected.
fn within_tolerance(actual: Duration, expected: Duration, tolerance_percent: f64) -> bool {
    if expected.is_zero() {
        return actual < Duration::from_millis(10);
    }
    let tolerance = Duration::from_secs_f64(expected.as_secs_f64() * tolerance_percent / 100.0);
    let min = expected.saturating_sub(tolerance);
    let max = expected.saturating_add(tolerance);
    actual >= min && actual <= max
}

// ============================================================================
// Simulated File Transfer Scenarios
// ============================================================================

mod file_transfer_scenarios {
    use super::*;

    #[test]
    fn transfer_1kb_file_at_10kb_per_second() {
        let mut session = recorded_sleep_session();
        session.clear();

        // Use 10x faster rate for integration tests (actual sleeps occur)
        let rate = 10 * 1024; // 10 KB/s
        let file_size = 1024; // 1 KB
        let mut limiter = BandwidthLimiter::new(nz(rate));
        let chunk_size = limiter.write_max_bytes().min(file_size);

        let mut remaining = file_size;
        while remaining > 0 {
            let to_transfer = remaining.min(chunk_size);
            let _ = limiter.register(to_transfer);
            remaining -= to_transfer;
        }

        // 1 KB at 10 KB/s = 100ms
        let total = session.total_duration();
        assert!(within_tolerance(total, Duration::from_millis(100), 20.0));
    }

    #[test]
    fn transfer_10kb_file_at_50kb_per_second() {
        let mut session = recorded_sleep_session();
        session.clear();

        // 10x faster rate for integration tests
        let rate = 50 * 1024; // 50 KB/s
        let file_size = 10 * 1024; // 10 KB
        let mut limiter = BandwidthLimiter::new(nz(rate));
        let chunk_size = limiter.write_max_bytes().min(file_size);

        let mut remaining = file_size;
        while remaining > 0 {
            let to_transfer = remaining.min(chunk_size);
            let _ = limiter.register(to_transfer);
            remaining -= to_transfer;
        }

        // 10 KB at 50 KB/s = 200ms (use larger tolerance for integration tests)
        let total = session.total_duration();
        assert!(
            within_tolerance(total, Duration::from_millis(200), 40.0),
            "Expected ~200ms, got {total:?}"
        );
    }

    #[test]
    fn transfer_1mb_file_at_10mb_per_second() {
        let mut session = recorded_sleep_session();
        session.clear();

        // 100x faster rate for integration tests
        let rate = 10 * 1024 * 1024; // 10 MB/s
        let file_size = 1024 * 1024; // 1 MB
        let mut limiter = BandwidthLimiter::new(nz(rate));
        let chunk_size = limiter.write_max_bytes();

        let mut remaining = file_size;
        while remaining > 0 {
            let to_transfer = remaining.min(chunk_size);
            let _ = limiter.register(to_transfer);
            remaining -= to_transfer;
        }

        // 1 MB at 10 MB/s = 100ms
        // Wide tolerance: many loop iterations accumulate real CPU time that
        // the limiter deducts from the sleep budget.
        let total = session.total_duration();
        assert!(within_tolerance(total, Duration::from_millis(100), 80.0));
    }

    #[test]
    fn transfer_multiple_small_files() {
        let mut session = recorded_sleep_session();
        session.clear();

        // Use slower rate so small files accumulate to trigger sleep threshold
        let rate = 10 * 1024; // 10 KB/s
        let mut limiter = BandwidthLimiter::new(nz(rate));

        // Larger files to exceed minimum sleep threshold (100ms)
        let file_sizes = [1024, 2048, 512, 1024, 2048, 512, 1024, 2048]; // ~10 KB total
        let total_bytes: usize = file_sizes.iter().sum();

        for &size in &file_sizes {
            let _ = limiter.register(size);
        }

        // Total ~10 KB at 10 KB/s = ~1 second
        let expected_secs = total_bytes as f64 / rate as f64;
        let total = session.total_duration();
        assert!(
            within_tolerance(total, Duration::from_secs_f64(expected_secs), 30.0),
            "Expected ~{expected_secs:.2}s, got {total:?}"
        );
    }

    #[test]
    fn transfer_single_large_file_in_chunks() {
        let mut session = recorded_sleep_session();
        session.clear();

        // 10x faster rate for integration tests
        let rate = 500 * 1024; // 500 KB/s
        let file_size = 500 * 1024; // 500 KB
        let mut limiter = BandwidthLimiter::new(nz(rate));
        let chunk_size = 8192; // 8 KB chunks

        let mut remaining = file_size;
        while remaining > 0 {
            let to_transfer = remaining.min(chunk_size);
            let _ = limiter.register(to_transfer);
            remaining -= to_transfer;
        }

        // 500 KB at 500 KB/s = 1 second
        // Wide tolerance: many loop iterations (61 chunks) accumulate real
        // CPU time that the limiter deducts from the sleep budget.
        let total = session.total_duration();
        assert!(within_tolerance(total, Duration::from_secs(1), 80.0));
    }

    #[test]
    fn transfer_directory_with_mixed_sizes() {
        let mut session = recorded_sleep_session();
        session.clear();

        // Use very fast rate for integration tests
        let rate = 1024 * 1024; // 1 MB/s
        let mut limiter = BandwidthLimiter::new(nz(rate));

        // Simulate directory: many tiny files, some medium, few large
        let tiny_files = [100usize; 50]; // 50 files of 100 bytes
        let small_files = [1000usize; 20]; // 20 files of 1000 bytes
        let medium_files = [10240usize; 10]; // 10 files of 10 KB
        let large_files = [102400usize; 2]; // 2 files of 100 KB

        let all_files: Vec<usize> = tiny_files
            .iter()
            .chain(small_files.iter())
            .chain(medium_files.iter())
            .chain(large_files.iter())
            .copied()
            .collect();

        let total_bytes: usize = all_files.iter().sum();

        for size in all_files {
            let _ = limiter.register(size);
        }

        // Total: ~332 KB at 1 MB/s = ~332ms
        // Wide tolerance because real wall-clock time between register() calls
        // is subtracted from the required sleep budget by the limiter.
        let expected_secs = total_bytes as f64 / rate as f64;
        let total = session.total_duration();
        assert!(
            within_tolerance(total, Duration::from_secs_f64(expected_secs), 80.0),
            "Expected ~{expected_secs:.3}s, got {total:?}"
        );
    }
}

// ============================================================================
// Rate Change Scenarios
// ============================================================================

mod rate_change_scenarios {
    use super::*;

    #[test]
    fn adaptive_rate_increase_mid_transfer() {
        let mut session = recorded_sleep_session();
        session.clear();

        // Use faster rates to avoid slow integration tests
        let mut limiter = BandwidthLimiter::new(nz(100 * 1024)); // 100 KB/s

        // Transfer first half at slow speed
        let _ = limiter.register(50 * 1024); // 50 KB = 500ms
        let mid_total = session.total_duration();
        assert!(mid_total >= Duration::from_millis(400)); // ~500ms

        // Increase rate 10x
        limiter.update_limit(nz(1000 * 1024)); // 1 MB/s

        // Transfer second half at fast speed
        let _ = limiter.register(50 * 1024); // 50 KB at 1 MB/s = 50ms
    }

    #[test]
    fn adaptive_rate_decrease_mid_transfer() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1000 * 1024)); // 1 MB/s

        // Transfer first half at fast speed
        let _ = limiter.register(500 * 1024); // 500 KB at 1 MB/s = 500ms

        // Decrease rate 10x
        limiter.update_limit(nz(500 * 1024)); // 500 KB/s

        // Transfer second half at slower speed
        let _ = limiter.register(250 * 1024); // 250 KB at 500 KB/s = 500ms
    }

    #[test]
    fn multiple_rate_changes_during_long_transfer() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(500 * 1024)); // 500 KB/s

        // Transfer with periodic rate changes (faster rates for integration tests)
        for i in 0..5 {
            let _ = limiter.register(100 * 1024); // 100 KB at 500 KB/s = 200ms

            // Alternate between fast and slower
            if i % 2 == 0 {
                limiter.update_limit(nz(1000 * 1024)); // 1 MB/s
            } else {
                limiter.update_limit(nz(500 * 1024)); // 500 KB/s
            }
        }

        // Should complete without issues
        let total = session.total_duration();
        assert!(total > Duration::ZERO);
    }

    #[test]
    fn burst_change_during_transfer() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(10_000)); // 10 KB/s

        // Transfer without burst
        let _ = limiter.register(5000); // 500ms

        // Add burst limit
        limiter.update_configuration(nz(1000), Some(nz(500)));

        // Transfer with burst limit - sleep should be capped
        let sleep = limiter.register(5000);
        assert!(sleep.requested() <= Duration::from_millis(500));
    }

    #[test]
    fn remove_burst_during_transfer() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(200)));

        // Transfer with burst - sleep capped
        let sleep1 = limiter.register(5000);
        assert!(sleep1.requested() <= Duration::from_millis(200));

        // Remove burst
        limiter.update_configuration(nz(1000), None);

        // Transfer without burst - full sleep
        let sleep2 = limiter.register(5000);
        assert_eq!(sleep2.requested(), Duration::from_secs(5));
    }
}

// ============================================================================
// Burst Behavior Scenarios
// ============================================================================

mod burst_scenarios {
    use super::*;

    #[test]
    fn burst_allows_initial_fast_transfer() {
        let mut session = recorded_sleep_session();
        session.clear();

        // Slow rate but large burst
        let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(5000)));

        // First transfer within burst
        let sleep1 = limiter.register(3000);
        // Sleep = 3000/1000 = 3 seconds (within burst, no extra clamping)
        assert_eq!(sleep1.requested(), Duration::from_secs(3));
    }

    #[test]
    fn burst_clamps_after_exceeding() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(2000)));

        // Transfer exceeds burst
        let sleep = limiter.register(10000);

        // Sleep should be clamped to burst/rate = 2000/1000 = 2s
        assert_eq!(sleep.requested(), Duration::from_secs(2));
    }

    #[test]
    fn burst_sustained_transfers() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::with_burst(nz(100), Some(nz(500)));

        // Multiple sustained transfers
        for _ in 0..10 {
            let sleep = limiter.register(1000);
            // Each sleep capped to 500/100 = 5 seconds max
            assert!(sleep.requested() <= Duration::from_secs(5));
        }
    }

    #[test]
    fn tiny_burst_limits_severely() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(10)));

        // Large transfer with tiny burst
        let sleep = limiter.register(100000);

        // Sleep capped to 10/1000 = 0.01s = 10ms
        assert!(sleep.requested() <= Duration::from_millis(20));
    }

    #[test]
    fn burst_from_parsing() {
        let components = parse_bandwidth_limit("100K:32K").expect("parse succeeds");

        let rate = components.rate().unwrap();
        let burst = components.burst().unwrap();

        let limiter = BandwidthLimiter::with_burst(rate, Some(burst));

        // Verify configuration
        assert_eq!(limiter.limit_bytes().get(), 100 * 1024);
        assert_eq!(limiter.burst_bytes().unwrap().get(), 32 * 1024);
    }
}

// ============================================================================
// Edge Cases from Real Usage
// ============================================================================

mod real_usage_edge_cases {
    use super::*;

    #[test]
    fn rsync_typical_bwlimit_100() {
        // Typical rsync usage: rsync --bwlimit=100 ...
        let limit = parse_bandwidth_argument("100")
            .expect("parse succeeds")
            .expect("limit available");

        assert_eq!(limit.get(), 100 * 1024); // 100K
    }

    #[test]
    fn rsync_typical_bwlimit_1m() {
        // Typical rsync usage: rsync --bwlimit=1m ...
        let limit = parse_bandwidth_argument("1m")
            .expect("parse succeeds")
            .expect("limit available");

        assert_eq!(limit.get(), 1024 * 1024); // 1M
    }

    #[test]
    fn rsync_unlimited_transfer() {
        // rsync --bwlimit=0 means unlimited
        let limit = parse_bandwidth_argument("0").expect("parse succeeds");
        assert!(limit.is_none());
    }

    #[test]
    fn rsync_burst_syntax() {
        // rsync --bwlimit=1m:256k
        let components = parse_bandwidth_limit("1m:256k").expect("parse succeeds");

        assert_eq!(components.rate().unwrap().get(), 1024 * 1024);
        assert_eq!(components.burst().unwrap().get(), 256 * 1024);
    }

    #[test]
    fn empty_file_transfer() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1024));

        // Zero-byte transfer
        let sleep = limiter.register(0);
        assert!(sleep.is_noop());
    }

    #[test]
    fn very_small_file_transfer() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1_000_000)); // 1 MB/s

        // 1 byte transfer
        let sleep = limiter.register(1);
        // Very small transfer might be below threshold
        assert!(sleep.is_noop() || sleep.requested() < Duration::from_millis(100));
    }

    #[test]
    fn network_blip_recovery() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(10 * 1024));

        // Normal transfer
        let _ = limiter.register(5 * 1024);

        // Simulate network blip by resetting
        limiter.reset();

        // Resume transfer
        let _ = limiter.register(5 * 1024);

        // Should still work correctly
        let total = session.total_duration();
        assert!(total > Duration::ZERO);
    }

    #[test]
    fn rate_limit_during_checksum_phase() {
        // During rsync's checksum/matching phase, many small reads occur
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(100 * 1024)); // 100 KB/s

        // Simulate many small checksum block reads
        for _ in 0..1000 {
            let _ = limiter.register(128); // 128-byte blocks
        }

        // Total: 128 KB at 100 KB/s = 1.28s
        // Wide tolerance: 1000 loop iterations accumulate real CPU time that
        // the limiter deducts from the sleep budget.
        let total = session.total_duration();
        assert!(within_tolerance(total, Duration::from_secs_f64(1.28), 80.0));
    }
}

// ============================================================================
// Timing Accuracy Tests
// ============================================================================

mod timing_accuracy {
    use super::*;

    #[test]
    fn accuracy_1_byte_at_1000_bps() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1000));

        let sleep = limiter.register(1);
        // 1 byte at 1000 B/s = 0.001 seconds = 1ms
        // But this is below minimum threshold, so might be noop
        assert!(sleep.is_noop() || sleep.requested() <= Duration::from_millis(2));
    }

    #[test]
    fn accuracy_100_bytes_at_1000_bps() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1000));

        let sleep = limiter.register(100);
        // 100 bytes at 1000 B/s = 0.1 seconds = 100ms
        assert_eq!(sleep.requested(), Duration::from_millis(100));
    }

    #[test]
    fn accuracy_333_bytes_at_1000_bps() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1000));

        let sleep = limiter.register(333);
        // 333 bytes at 1000 B/s = 0.333 seconds = 333ms
        assert_eq!(sleep.requested(), Duration::from_millis(333));
    }

    #[test]
    fn accuracy_microsecond_level() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1_000_000)); // 1 MB/s

        let sleep = limiter.register(123456);
        // 123456 bytes at 1 MB/s = 0.123456 seconds = 123.456ms
        // Should round to 123ms
        assert_eq!(sleep.requested(), Duration::from_micros(123456));
    }

    #[test]
    fn accuracy_accumulated_matches_single() {
        // Use a single session to avoid blocking on mutex
        let mut session = recorded_sleep_session();
        session.clear();

        // Test single large transfer: 500,000 bytes at 1 MB/s = 500ms
        let mut limiter1 = BandwidthLimiter::new(nz(1_000_000)); // 1 MB/s
        let _ = limiter1.register(500_000); // 500ms
        let single_total = session.total_duration();

        // Verify the single transfer took ~500ms
        assert!(
            within_tolerance(single_total, Duration::from_millis(500), 20.0),
            "Single transfer: expected ~500ms, got {single_total:?}"
        );

        // Clear and test accumulated small transfers: 50 x 10,000 bytes = 500,000 bytes
        session.clear();
        let mut limiter2 = BandwidthLimiter::new(nz(1_000_000)); // 1 MB/s
        for _ in 0..50 {
            let _ = limiter2.register(10_000);
        }
        let accumulated_total = session.total_duration();

        // Should also be ~500ms (wide tolerance for loop CPU time).
        assert!(
            within_tolerance(accumulated_total, Duration::from_millis(500), 60.0),
            "Accumulated transfer: expected ~500ms, got {accumulated_total:?}"
        );
    }

    #[test]
    fn timing_consistency_across_rates() {
        // Different rates, same byte/rate ratio should give same time
        let test_cases: Vec<(u64, usize)> = vec![
            (1000, 1000),     // 1s
            (2000, 2000),     // 1s
            (5000, 5000),     // 1s
            (10000, 10000),   // 1s
            (100000, 100000), // 1s
        ];

        for (rate, bytes) in test_cases {
            let mut session = recorded_sleep_session();
            session.clear();

            let mut limiter = BandwidthLimiter::new(nz(rate));
            let sleep = limiter.register(bytes);

            assert_eq!(
                sleep.requested(),
                Duration::from_secs(1),
                "Rate {rate}, bytes {bytes}"
            );
        }
    }
}

// ============================================================================
// Apply Effective Limit Scenarios
// ============================================================================

mod apply_effective_limit_scenarios {
    use super::*;

    #[test]
    fn enable_new_limiter() {
        let mut limiter: Option<BandwidthLimiter> = None;

        let change = apply_effective_limit(&mut limiter, Some(nz(1000)), true, None, false);

        assert_eq!(change, LimiterChange::Enabled);
        assert!(limiter.is_some());
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 1000);
    }

    #[test]
    fn enable_with_burst() {
        let mut limiter: Option<BandwidthLimiter> = None;

        let change = apply_effective_limit(&mut limiter, Some(nz(1000)), true, Some(nz(500)), true);

        assert_eq!(change, LimiterChange::Enabled);
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 1000);
        assert_eq!(limiter.as_ref().unwrap().burst_bytes().unwrap().get(), 500);
    }

    #[test]
    fn disable_existing_limiter() {
        let mut limiter = Some(BandwidthLimiter::new(nz(1000)));

        let change = apply_effective_limit(&mut limiter, None, true, None, false);

        assert_eq!(change, LimiterChange::Disabled);
        assert!(limiter.is_none());
    }

    #[test]
    fn update_to_lower_limit() {
        let mut limiter = Some(BandwidthLimiter::new(nz(2000)));

        let change = apply_effective_limit(&mut limiter, Some(nz(1000)), true, None, false);

        assert_eq!(change, LimiterChange::Updated);
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 1000);
    }

    #[test]
    fn unchanged_higher_limit() {
        let mut limiter = Some(BandwidthLimiter::new(nz(1000)));

        let change = apply_effective_limit(&mut limiter, Some(nz(2000)), true, None, false);

        assert_eq!(change, LimiterChange::Unchanged);
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 1000);
    }

    #[test]
    fn update_burst_only() {
        let mut limiter = Some(BandwidthLimiter::new(nz(1000)));

        let change = apply_effective_limit(&mut limiter, None, false, Some(nz(500)), true);

        assert_eq!(change, LimiterChange::Updated);
        assert_eq!(limiter.as_ref().unwrap().burst_bytes().unwrap().get(), 500);
    }

    #[test]
    fn unchanged_when_nothing_specified() {
        let mut limiter = Some(BandwidthLimiter::new(nz(1000)));

        let change = apply_effective_limit(&mut limiter, None, false, None, false);

        assert_eq!(change, LimiterChange::Unchanged);
    }

    #[test]
    fn chained_operations() {
        let mut limiter: Option<BandwidthLimiter> = None;

        // Enable
        let c1 = apply_effective_limit(&mut limiter, Some(nz(2000)), true, None, false);
        assert_eq!(c1, LimiterChange::Enabled);

        // Update lower
        let c2 = apply_effective_limit(&mut limiter, Some(nz(1000)), true, None, false);
        assert_eq!(c2, LimiterChange::Updated);

        // Add burst
        let c3 = apply_effective_limit(&mut limiter, None, false, Some(nz(500)), true);
        assert_eq!(c3, LimiterChange::Updated);

        // Disable
        let c4 = apply_effective_limit(&mut limiter, None, true, None, false);
        assert_eq!(c4, LimiterChange::Disabled);
    }
}

// ============================================================================
// Write Max and Recommended Read Size Tests
// ============================================================================

mod write_max_scenarios {
    use super::*;

    #[test]
    fn write_max_for_slow_rate() {
        let limiter = BandwidthLimiter::new(nz(512)); // Very slow
        let write_max = limiter.write_max_bytes();
        // Should be at least MIN_WRITE_MAX (512)
        assert!(write_max >= 512);
    }

    #[test]
    fn write_max_for_fast_rate() {
        let limiter = BandwidthLimiter::new(nz(100 * 1024 * 1024)); // 100 MB/s
        let write_max = limiter.write_max_bytes();
        // Should be larger than slow rate
        assert!(write_max > 512);
    }

    #[test]
    fn write_max_scales_with_rate() {
        let slow = BandwidthLimiter::new(nz(1024));
        let medium = BandwidthLimiter::new(nz(100 * 1024));
        let fast = BandwidthLimiter::new(nz(1024 * 1024));

        assert!(medium.write_max_bytes() >= slow.write_max_bytes());
        assert!(fast.write_max_bytes() >= medium.write_max_bytes());
    }

    #[test]
    fn write_max_with_burst_override() {
        let without_burst = BandwidthLimiter::new(nz(1024 * 1024));
        let with_small_burst = BandwidthLimiter::with_burst(nz(1024 * 1024), Some(nz(4096)));

        assert_eq!(with_small_burst.write_max_bytes(), 4096);
        assert!(without_burst.write_max_bytes() > with_small_burst.write_max_bytes());
    }

    #[test]
    fn recommended_read_size_small_buffer() {
        let limiter = BandwidthLimiter::new(nz(1024 * 1024));

        assert_eq!(limiter.recommended_read_size(100), 100);
        assert_eq!(limiter.recommended_read_size(0), 0);
        assert_eq!(limiter.recommended_read_size(1), 1);
    }

    #[test]
    fn recommended_read_size_large_buffer() {
        let limiter = BandwidthLimiter::new(nz(1024 * 1024));
        let write_max = limiter.write_max_bytes();

        assert_eq!(limiter.recommended_read_size(1_000_000), write_max);
        assert_eq!(limiter.recommended_read_size(usize::MAX), write_max);
    }
}

// ============================================================================
// Parsing Edge Cases
// ============================================================================

mod parsing_edge_cases {
    use super::*;

    #[test]
    fn parse_all_valid_suffixes() {
        let cases = [
            ("1b", 1),
            ("1k", 1024),
            ("1m", 1024 * 1024),
            ("1g", 1024 * 1024 * 1024),
            ("1t", 1024u64.pow(4)),
            ("1p", 1024u64.pow(5)),
        ];

        for (input, expected) in cases {
            if expected >= 512 {
                // Must meet minimum
                let result = parse_bandwidth_argument(input).unwrap();
                if let Some(limit) = result {
                    assert_eq!(limit.get(), expected, "Input: {input}");
                }
            }
        }
    }

    #[test]
    fn parse_decimal_variations() {
        // Different decimal separators and formats
        let _ = parse_bandwidth_argument("1.5m"); // dot
        let _ = parse_bandwidth_argument("1,5m"); // comma
        let _ = parse_bandwidth_argument("1.5e3"); // scientific
    }

    #[test]
    fn parse_case_insensitive_suffixes() {
        let lower = parse_bandwidth_argument("1k").unwrap();
        let upper = parse_bandwidth_argument("1K").unwrap();
        assert_eq!(lower, upper);
    }

    #[test]
    fn parse_minimum_values() {
        // Exactly at minimum
        let at_min = parse_bandwidth_argument("512b").unwrap();
        assert!(at_min.is_some());

        // Below minimum
        let below_min = parse_bandwidth_argument("511b");
        assert!(below_min.is_err());
    }

    #[test]
    fn parse_zero_variations() {
        let cases = ["0", "0b", "0k", "0m", "0g"];

        for case in cases {
            let result = parse_bandwidth_argument(case).expect("parse succeeds");
            assert!(result.is_none(), "{case} should be unlimited");
        }
    }

    #[test]
    fn parse_invalid_inputs() {
        let invalid = ["", "   ", "abc", "-100", "100x", "1.2.3"];

        for input in invalid {
            let result = parse_bandwidth_argument(input);
            assert!(result.is_err(), "{input:?} should be invalid");
        }
    }

    #[test]
    fn parse_burst_syntax_variations() {
        // Valid burst syntax
        let _ = parse_bandwidth_limit("1m:256k").unwrap();
        let _ = parse_bandwidth_limit("100:50").unwrap();
        let _ = parse_bandwidth_limit("1G:1M").unwrap();

        // Zero burst
        let zero_burst = parse_bandwidth_limit("1m:0").unwrap();
        assert!(zero_burst.burst().is_none());
    }
}

// ============================================================================
// Stress Tests
// ============================================================================

mod stress_tests {
    use super::*;

    #[test]
    fn stress_many_small_writes() {
        let mut session = recorded_sleep_session();
        session.clear();

        // Use faster rate for integration tests
        let mut limiter = BandwidthLimiter::new(nz(1_000_000)); // 1 MB/s

        // 1,000 tiny writes (100 bytes each)
        for _ in 0..1_000 {
            let _ = limiter.register(100);
        }

        // Total: 100 KB at 1 MB/s = 100ms
        // Wide tolerance because real wall-clock time between register() calls
        // is subtracted from the required sleep budget by the limiter.
        let total = session.total_duration();
        assert!(within_tolerance(total, Duration::from_millis(100), 80.0));
    }

    #[test]
    fn stress_alternating_sizes() {
        let mut session = recorded_sleep_session();
        session.clear();

        // Use faster rate for integration tests
        let mut limiter = BandwidthLimiter::new(nz(100_000)); // 100 KB/s

        for i in 0..100 {
            if i % 2 == 0 {
                let _ = limiter.register(10);
            } else {
                let _ = limiter.register(1000);
            }
        }

        // No panics, reasonable behavior (total ~505 bytes * 100 = 50.5 KB at 100 KB/s = ~500ms)
        let total = session.total_duration();
        assert!(total > Duration::ZERO);
    }

    #[test]
    fn stress_rapid_rate_changes() {
        // Fast rate to avoid slow tests
        let mut limiter = BandwidthLimiter::new(nz(100_000)); // 100 KB/s

        for i in 1..=50 {
            limiter.update_limit(nz(i as u64 * 10_000));
            let _ = limiter.register(1000); // Small writes
        }

        // No panics
    }

    #[test]
    fn stress_burst_changes() {
        // Fast rate to avoid slow tests
        let mut limiter = BandwidthLimiter::new(nz(100_000)); // 100 KB/s

        for i in 1..=30 {
            let burst = if i % 2 == 0 {
                Some(nz(i as u64 * 1000))
            } else {
                None
            };
            limiter.update_configuration(nz(100_000), burst);
            let _ = limiter.register(1000); // Small writes
        }

        // No panics
    }
}
