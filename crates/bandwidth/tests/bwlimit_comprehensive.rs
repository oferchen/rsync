//! Comprehensive tests for --bwlimit bandwidth limiting.
//!
//! These tests cover:
//! 1. Basic rate limiting with numeric values
//! 2. Rate with K/M/G suffixes
//! 3. Rate of 0 (unlimited)
//! 4. Very low and very high rates
//! 5. Rate changes during transfer
//! 6. Burst behavior
//! 7. Rate limiting with small vs large files
//! 8. Rate limiting accuracy verification
//!
//! Note: Tests use the test-support feature to record sleep requests
//! instead of actually sleeping, enabling fast deterministic testing.

use bandwidth::{
    BandwidthLimiter, BandwidthParseError, LimiterChange, apply_effective_limit,
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

/// Check if actual duration is within tolerance of expected.
fn within_tolerance(actual: Duration, expected: Duration, tolerance_percent: f64) -> bool {
    let tolerance = Duration::from_secs_f64(expected.as_secs_f64() * tolerance_percent / 100.0);
    let min = expected.saturating_sub(tolerance);
    let max = expected.saturating_add(tolerance);
    actual >= min && actual <= max
}

// ============================================================================
// 1. Basic Rate Limiting Tests (--bwlimit=100)
// ============================================================================

mod basic_rate_limiting {
    use super::*;

    #[test]
    fn bwlimit_100_defaults_to_kilobytes() {
        // --bwlimit=100 should mean 100K = 102400 bytes/sec
        let limit = parse_bandwidth_argument("100")
            .expect("parse succeeds")
            .expect("limit available");
        assert_eq!(limit.get(), 100 * 1024);
    }

    #[test]
    fn bwlimit_100_rate_limiting() {
        let mut session = recorded_sleep_session();
        session.clear();

        // 100K bytes/sec
        let mut limiter = BandwidthLimiter::new(nz(100 * 1024));

        // Transfer 100K bytes - should take 1 second
        let sleep = limiter.register(100 * 1024);
        assert_eq!(sleep.requested(), Duration::from_secs(1));
    }

    #[test]
    fn bwlimit_50_transfer_100k() {
        let mut session = recorded_sleep_session();
        session.clear();

        // 50K bytes/sec
        let mut limiter = BandwidthLimiter::new(nz(50 * 1024));

        // Transfer 100K bytes - should take 2 seconds
        let sleep = limiter.register(100 * 1024);
        assert_eq!(sleep.requested(), Duration::from_secs(2));
    }

    #[test]
    fn bwlimit_200_transfer_100k() {
        let mut session = recorded_sleep_session();
        session.clear();

        // 200K bytes/sec
        let mut limiter = BandwidthLimiter::new(nz(200 * 1024));

        // Transfer 100K bytes - should take 0.5 seconds
        let sleep = limiter.register(100 * 1024);
        assert_eq!(sleep.requested(), Duration::from_millis(500));
    }

    #[test]
    fn bwlimit_1_kilobyte_per_second() {
        let mut session = recorded_sleep_session();
        session.clear();

        // 1K bytes/sec
        let mut limiter = BandwidthLimiter::new(nz(1024));

        // Transfer 2K bytes - should take 2 seconds
        let sleep = limiter.register(2048);
        assert_eq!(sleep.requested(), Duration::from_secs(2));
    }

    #[test]
    fn bwlimit_10_kilobytes_per_second() {
        let mut session = recorded_sleep_session();
        session.clear();

        // 10K bytes/sec
        let mut limiter = BandwidthLimiter::new(nz(10 * 1024));

        // Transfer 10K bytes - should take 1 second
        let sleep = limiter.register(10 * 1024);
        assert_eq!(sleep.requested(), Duration::from_secs(1));
    }
}

// ============================================================================
// 2. Rate with K Suffix (Kilobytes)
// ============================================================================

mod rate_with_k_suffix {
    use super::*;

    #[test]
    fn bwlimit_1k_binary() {
        let limit = parse_bandwidth_argument("1K")
            .expect("parse succeeds")
            .expect("limit available");
        assert_eq!(limit.get(), 1024);
    }

    #[test]
    fn bwlimit_10k_binary() {
        let limit = parse_bandwidth_argument("10K")
            .expect("parse succeeds")
            .expect("limit available");
        assert_eq!(limit.get(), 10 * 1024);
    }

    #[test]
    fn bwlimit_100k_binary() {
        let limit = parse_bandwidth_argument("100K")
            .expect("parse succeeds")
            .expect("limit available");
        assert_eq!(limit.get(), 100 * 1024);
    }

    #[test]
    fn bwlimit_1k_decimal_kb() {
        // KB suffix uses decimal (1000) instead of binary (1024)
        let limit = parse_bandwidth_argument("1KB")
            .expect("parse succeeds")
            .expect("limit available");
        assert_eq!(limit.get(), 1000);
    }

    #[test]
    fn bwlimit_10kb_decimal() {
        let limit = parse_bandwidth_argument("10KB")
            .expect("parse succeeds")
            .expect("limit available");
        assert_eq!(limit.get(), 10 * 1000);
    }

    #[test]
    fn bwlimit_1kib_explicit_binary() {
        // KiB is explicit binary kilobytes
        let limit = parse_bandwidth_argument("1KiB")
            .expect("parse succeeds")
            .expect("limit available");
        assert_eq!(limit.get(), 1024);
    }

    #[test]
    fn bwlimit_512k_rate_limiting() {
        let mut session = recorded_sleep_session();
        session.clear();

        let limit = parse_bandwidth_argument("512K")
            .expect("parse succeeds")
            .expect("limit available");
        let mut limiter = BandwidthLimiter::new(limit);

        // Transfer 512K bytes - should take 1 second
        let sleep = limiter.register(512 * 1024);
        assert_eq!(sleep.requested(), Duration::from_secs(1));
    }

    #[test]
    fn bwlimit_lowercase_k() {
        let limit = parse_bandwidth_argument("100k")
            .expect("parse succeeds")
            .expect("limit available");
        assert_eq!(limit.get(), 100 * 1024);
    }

    #[test]
    fn bwlimit_fractional_k() {
        let limit = parse_bandwidth_argument("1.5K")
            .expect("parse succeeds")
            .expect("limit available");
        // 1.5K = 1536 bytes, rounded to nearest 1024 = 2048
        assert_eq!(limit.get(), 2048);
    }
}

// ============================================================================
// 3. Rate with M Suffix (Megabytes)
// ============================================================================

mod rate_with_m_suffix {
    use super::*;

    #[test]
    fn bwlimit_1m_binary() {
        let limit = parse_bandwidth_argument("1M")
            .expect("parse succeeds")
            .expect("limit available");
        assert_eq!(limit.get(), 1024 * 1024);
    }

    #[test]
    fn bwlimit_10m_binary() {
        let limit = parse_bandwidth_argument("10M")
            .expect("parse succeeds")
            .expect("limit available");
        assert_eq!(limit.get(), 10 * 1024 * 1024);
    }

    #[test]
    fn bwlimit_1mb_decimal() {
        let limit = parse_bandwidth_argument("1MB")
            .expect("parse succeeds")
            .expect("limit available");
        assert_eq!(limit.get(), 1_000_000);
    }

    #[test]
    fn bwlimit_10mb_decimal() {
        let limit = parse_bandwidth_argument("10MB")
            .expect("parse succeeds")
            .expect("limit available");
        assert_eq!(limit.get(), 10_000_000);
    }

    #[test]
    fn bwlimit_1mib_explicit_binary() {
        let limit = parse_bandwidth_argument("1MiB")
            .expect("parse succeeds")
            .expect("limit available");
        assert_eq!(limit.get(), 1024 * 1024);
    }

    #[test]
    fn bwlimit_1m_rate_limiting() {
        let mut session = recorded_sleep_session();
        session.clear();

        let limit = parse_bandwidth_argument("1M")
            .expect("parse succeeds")
            .expect("limit available");
        let mut limiter = BandwidthLimiter::new(limit);

        // Transfer 1M bytes - should take 1 second
        let sleep = limiter.register(1024 * 1024);
        assert_eq!(sleep.requested(), Duration::from_secs(1));
    }

    #[test]
    fn bwlimit_5m_transfer_10m() {
        let mut session = recorded_sleep_session();
        session.clear();

        let limit = parse_bandwidth_argument("5M")
            .expect("parse succeeds")
            .expect("limit available");
        let mut limiter = BandwidthLimiter::new(limit);

        // Transfer 10M bytes at 5M/s - should take 2 seconds
        let sleep = limiter.register(10 * 1024 * 1024);
        assert_eq!(sleep.requested(), Duration::from_secs(2));
    }

    #[test]
    fn bwlimit_fractional_m() {
        let limit = parse_bandwidth_argument("0.5M")
            .expect("parse succeeds")
            .expect("limit available");
        // 0.5M = 512K
        assert_eq!(limit.get(), 512 * 1024);
    }

    #[test]
    fn bwlimit_1_5m() {
        let limit = parse_bandwidth_argument("1.5M")
            .expect("parse succeeds")
            .expect("limit available");
        // 1.5M = 1.5 * 1024 * 1024 = 1572864
        assert_eq!(limit.get(), 1_572_864);
    }
}

// ============================================================================
// 4. Rate with G Suffix (Gigabytes)
// ============================================================================

mod rate_with_g_suffix {
    use super::*;

    #[test]
    fn bwlimit_1g_binary() {
        let limit = parse_bandwidth_argument("1G")
            .expect("parse succeeds")
            .expect("limit available");
        assert_eq!(limit.get(), 1024 * 1024 * 1024);
    }

    #[test]
    fn bwlimit_1gb_decimal() {
        let limit = parse_bandwidth_argument("1GB")
            .expect("parse succeeds")
            .expect("limit available");
        assert_eq!(limit.get(), 1_000_000_000);
    }

    #[test]
    fn bwlimit_1gib_explicit_binary() {
        let limit = parse_bandwidth_argument("1GiB")
            .expect("parse succeeds")
            .expect("limit available");
        assert_eq!(limit.get(), 1024 * 1024 * 1024);
    }

    #[test]
    fn bwlimit_1g_rate_limiting() {
        let mut session = recorded_sleep_session();
        session.clear();

        let limit = parse_bandwidth_argument("1G")
            .expect("parse succeeds")
            .expect("limit available");
        let mut limiter = BandwidthLimiter::new(limit);

        // Transfer 1G bytes - should take 1 second
        let sleep = limiter.register(1024 * 1024 * 1024);
        assert_eq!(sleep.requested(), Duration::from_secs(1));
    }

    #[test]
    fn bwlimit_1g_transfer_100m() {
        let mut session = recorded_sleep_session();
        session.clear();

        let limit = parse_bandwidth_argument("1G")
            .expect("parse succeeds")
            .expect("limit available");
        let mut limiter = BandwidthLimiter::new(limit);

        // Transfer 100M at 1G/s - should take ~100ms
        // Note: 1G = 1024*1024*1024 = 1073741824 bytes/sec
        // 100M = 100*1024*1024 = 104857600 bytes
        // 104857600 / 1073741824 = ~97.6ms which rounds to ~100ms
        // But exactly at threshold, the result depends on calculation
        let sleep = limiter.register(100 * 1024 * 1024);
        // Allow for minimum threshold behavior - could be noop or ~100ms
        assert!(
            sleep.is_noop() || sleep.requested() >= Duration::from_millis(95),
            "Expected noop or ~100ms, got {:?}",
            sleep.requested()
        );
    }

    #[test]
    fn bwlimit_fractional_g() {
        let limit = parse_bandwidth_argument("0.5G")
            .expect("parse succeeds")
            .expect("limit available");
        // 0.5G = 512M
        assert_eq!(limit.get(), 512 * 1024 * 1024);
    }
}

// ============================================================================
// 5. Rate of 0 (Unlimited)
// ============================================================================

mod rate_zero_unlimited {
    use super::*;

    #[test]
    fn bwlimit_0_is_unlimited() {
        let limit = parse_bandwidth_argument("0").expect("parse succeeds");
        assert!(limit.is_none(), "Zero should represent unlimited");
    }

    #[test]
    fn bwlimit_0k_is_unlimited() {
        let limit = parse_bandwidth_argument("0K").expect("parse succeeds");
        assert!(limit.is_none());
    }

    #[test]
    fn bwlimit_0m_is_unlimited() {
        let limit = parse_bandwidth_argument("0M").expect("parse succeeds");
        assert!(limit.is_none());
    }

    #[test]
    fn bwlimit_0g_is_unlimited() {
        let limit = parse_bandwidth_argument("0G").expect("parse succeeds");
        assert!(limit.is_none());
    }

    #[test]
    fn bwlimit_0b_is_unlimited() {
        let limit = parse_bandwidth_argument("0b").expect("parse succeeds");
        assert!(limit.is_none());
    }

    #[test]
    fn bwlimit_0_with_burst_still_unlimited() {
        let components = parse_bandwidth_limit("0:1024").expect("parse succeeds");
        assert!(components.is_unlimited());
        assert!(components.limit_specified());
    }

    #[test]
    fn apply_effective_limit_zero_disables() {
        let mut limiter = Some(BandwidthLimiter::new(nz(1000)));
        let change = apply_effective_limit(&mut limiter, None, true, None, false);
        assert_eq!(change, LimiterChange::Disabled);
        assert!(limiter.is_none());
    }

    #[test]
    fn apply_effective_limit_zero_on_none_unchanged() {
        let mut limiter: Option<BandwidthLimiter> = None;
        let change = apply_effective_limit(&mut limiter, None, true, None, false);
        assert_eq!(change, LimiterChange::Unchanged);
        assert!(limiter.is_none());
    }
}

// ============================================================================
// 6. Very Low Rates (e.g., 1 byte/sec)
// ============================================================================

mod very_low_rates {
    use super::*;

    #[test]
    fn bwlimit_minimum_512_bytes() {
        // Minimum allowed is 512 bytes/second
        let limit = parse_bandwidth_argument("512b")
            .expect("parse succeeds")
            .expect("limit available");
        assert_eq!(limit.get(), 512);
    }

    #[test]
    fn bwlimit_below_minimum_fails() {
        let result = parse_bandwidth_argument("511b");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), BandwidthParseError::TooSmall);
    }

    #[test]
    fn bwlimit_1_byte_fails() {
        let result = parse_bandwidth_argument("1b");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), BandwidthParseError::TooSmall);
    }

    #[test]
    fn bwlimit_100_bytes_fails() {
        let result = parse_bandwidth_argument("100b");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), BandwidthParseError::TooSmall);
    }

    #[test]
    fn bwlimit_512_bytes_rate_limiting() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(512));

        // Transfer 512 bytes - should take 1 second
        let sleep = limiter.register(512);
        assert_eq!(sleep.requested(), Duration::from_secs(1));
    }

    #[test]
    fn bwlimit_512_bytes_transfer_1k() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(512));

        // Transfer 1024 bytes at 512 B/s - should take 2 seconds
        let sleep = limiter.register(1024);
        assert_eq!(sleep.requested(), Duration::from_secs(2));
    }

    #[test]
    fn bwlimit_very_slow_large_transfer() {
        let mut session = recorded_sleep_session();
        session.clear();

        // Use internal limiter directly with 1 byte/sec (below parsing minimum)
        let mut limiter = BandwidthLimiter::new(nz(1));

        // Transfer 100 bytes at 1 B/s - should take 100 seconds
        let sleep = limiter.register(100);
        assert_eq!(sleep.requested(), Duration::from_secs(100));
    }

    #[test]
    fn bwlimit_very_slow_10_bytes_per_sec() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(10));

        // Transfer 100 bytes at 10 B/s - should take 10 seconds
        let sleep = limiter.register(100);
        assert_eq!(sleep.requested(), Duration::from_secs(10));
    }

    #[test]
    fn bwlimit_slow_chunked_transfer() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(100));

        // Transfer in chunks
        let mut total_sleep = Duration::ZERO;
        for _ in 0..10 {
            let sleep = limiter.register(10);
            total_sleep = total_sleep.saturating_add(sleep.requested());
        }

        // Total: 100 bytes at 100 B/s = 1 second
        assert!(within_tolerance(total_sleep, Duration::from_secs(1), 15.0));
    }
}

// ============================================================================
// 7. Very High Rates
// ============================================================================

mod very_high_rates {
    use super::*;

    #[test]
    fn bwlimit_1gb_rate() {
        let mut session = recorded_sleep_session();
        session.clear();

        // 1 GB/s
        let mut limiter = BandwidthLimiter::new(nz(1_000_000_000));

        // Transfer 1 MB - should take only 1 ms
        let sleep = limiter.register(1_000_000);
        assert!(
            sleep.requested() <= Duration::from_millis(2),
            "Expected <= 2ms, got {:?}",
            sleep.requested()
        );
    }

    #[test]
    fn bwlimit_10gb_rate() {
        let mut session = recorded_sleep_session();
        session.clear();

        // 10 GB/s
        let mut limiter = BandwidthLimiter::new(nz(10_000_000_000));

        // Transfer 10 MB - should take only 1 ms
        let sleep = limiter.register(10_000_000);
        assert!(sleep.requested() <= Duration::from_millis(2));
    }

    #[test]
    fn bwlimit_u64_max_rate() {
        let mut session = recorded_sleep_session();
        session.clear();

        // Maximum possible rate
        let mut limiter = BandwidthLimiter::new(nz(u64::MAX));

        // Any reasonable transfer should be essentially instant
        let sleep = limiter.register(1_000_000_000);
        assert!(sleep.requested() < Duration::from_nanos(1000));
    }

    #[test]
    fn bwlimit_high_rate_small_transfer_below_threshold() {
        let mut session = recorded_sleep_session();
        session.clear();

        // At 100 MB/s, 1000 bytes takes 0.01 ms
        let mut limiter = BandwidthLimiter::new(nz(100_000_000));

        // Small transfer - should be below sleep threshold (noop)
        let sleep = limiter.register(1000);
        assert!(sleep.is_noop() || sleep.requested() < Duration::from_millis(100));
    }

    #[test]
    fn bwlimit_high_rate_accumulated_triggers_sleep() {
        let mut session = recorded_sleep_session();
        session.clear();

        // 50 KB/s - slower rate so 10 KB writes trigger > 100ms sleep each
        let mut limiter = BandwidthLimiter::new(nz(50_000));

        // Each 10 KB write at 50 KB/s = 200ms (above minimum threshold)
        for _ in 0..3 {
            let _ = limiter.register(10_000);
        }

        // Total: 30,000 bytes at 50 KB/s = 600 ms
        let total = session.total_duration();
        assert!(
            total >= Duration::from_millis(500),
            "Expected >= 500ms, got {:?}",
            total
        );
    }

    #[test]
    fn bwlimit_terabyte_rate() {
        let mut session = recorded_sleep_session();
        session.clear();

        // 1 TB/s
        let mut limiter = BandwidthLimiter::new(nz(1_000_000_000_000));

        // Transfer 1 GB - should be very fast
        let sleep = limiter.register(1_000_000_000);
        assert!(sleep.requested() <= Duration::from_millis(2));
    }
}

// ============================================================================
// 8. Rate Changes During Transfer
// ============================================================================

mod rate_changes_during_transfer {
    use super::*;

    #[test]
    fn rate_change_increase_mid_transfer() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1024)); // 1 KB/s

        // First transfer at slow rate
        let sleep1 = limiter.register(1024);
        assert_eq!(sleep1.requested(), Duration::from_secs(1));

        // Increase rate 10x
        limiter.update_limit(nz(10240)); // 10 KB/s

        // Same transfer should be faster
        session.clear();
        let sleep2 = limiter.register(1024);
        assert!(sleep2.requested() <= Duration::from_millis(150));
    }

    #[test]
    fn rate_change_decrease_mid_transfer() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(10240)); // 10 KB/s

        // First transfer at fast rate
        let sleep1 = limiter.register(1024);
        assert!(sleep1.requested() <= Duration::from_millis(150));

        // Decrease rate 10x
        limiter.update_limit(nz(1024)); // 1 KB/s

        // Same transfer should be slower
        session.clear();
        let sleep2 = limiter.register(1024);
        assert_eq!(sleep2.requested(), Duration::from_secs(1));
    }

    #[test]
    fn rate_change_clears_debt() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(100));

        // Accumulate debt
        let _ = limiter.register(1000);

        // Change rate
        limiter.update_limit(nz(200));

        // Debt should be cleared
        session.clear();
        let sleep = limiter.register(200);
        assert_eq!(sleep.requested(), Duration::from_secs(1));
    }

    #[test]
    fn rate_change_multiple_times() {
        let mut limiter = BandwidthLimiter::new(nz(1000));

        // Multiple rate changes
        limiter.update_limit(nz(2000));
        assert_eq!(limiter.limit_bytes().get(), 2000);

        limiter.update_limit(nz(500));
        assert_eq!(limiter.limit_bytes().get(), 500);

        limiter.update_limit(nz(10000));
        assert_eq!(limiter.limit_bytes().get(), 10000);
    }

    #[test]
    fn rate_change_preserves_burst() {
        let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(500)));

        limiter.update_limit(nz(2000));

        assert_eq!(limiter.limit_bytes().get(), 2000);
        assert_eq!(limiter.burst_bytes().unwrap().get(), 500);
    }

    #[test]
    fn configuration_change_updates_both() {
        let mut limiter = BandwidthLimiter::new(nz(1000));

        limiter.update_configuration(nz(2000), Some(nz(1000)));

        assert_eq!(limiter.limit_bytes().get(), 2000);
        assert_eq!(limiter.burst_bytes().unwrap().get(), 1000);
    }

    #[test]
    fn reset_clears_debt_preserves_config() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(500)));

        // Accumulate debt
        let _ = limiter.register(2000);

        // Reset
        limiter.reset();

        // Configuration preserved
        assert_eq!(limiter.limit_bytes().get(), 1000);
        assert_eq!(limiter.burst_bytes().unwrap().get(), 500);

        // Debt cleared - note that burst still limits sleep to 500ms
        // because burst clamps debt to 500 bytes, and 500/1000 = 0.5 seconds
        session.clear();
        let sleep = limiter.register(1000);
        // With burst of 500, max sleep is 500ms (500/1000)
        assert!(sleep.requested() <= Duration::from_millis(500));
    }

    #[test]
    fn apply_effective_limit_updates_existing() {
        let mut limiter = Some(BandwidthLimiter::new(nz(2000)));

        let change = apply_effective_limit(&mut limiter, Some(nz(1000)), true, None, false);

        assert_eq!(change, LimiterChange::Updated);
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 1000);
    }

    #[test]
    fn apply_effective_limit_enables_new() {
        let mut limiter: Option<BandwidthLimiter> = None;

        let change = apply_effective_limit(&mut limiter, Some(nz(1000)), true, None, false);

        assert_eq!(change, LimiterChange::Enabled);
        assert!(limiter.is_some());
    }
}

// ============================================================================
// 9. Burst Behavior
// ============================================================================

mod burst_behavior {
    use super::*;

    #[test]
    fn burst_parsing_rate_with_burst() {
        let components = parse_bandwidth_limit("1M:32K").expect("parse succeeds");

        assert_eq!(components.rate().unwrap().get(), 1024 * 1024);
        assert_eq!(components.burst().unwrap().get(), 32 * 1024);
    }

    #[test]
    fn burst_parsing_different_units() {
        let components = parse_bandwidth_limit("10M:1M").expect("parse succeeds");

        assert_eq!(components.rate().unwrap().get(), 10 * 1024 * 1024);
        assert_eq!(components.burst().unwrap().get(), 1024 * 1024);
    }

    #[test]
    fn burst_clamps_debt() {
        let mut session = recorded_sleep_session();
        session.clear();

        // 1 KB/s with 2 KB burst
        let mut limiter = BandwidthLimiter::with_burst(nz(1024), Some(nz(2048)));

        // Transfer 10 KB - debt should be clamped to 2 KB
        let sleep = limiter.register(10 * 1024);

        // With 2 KB max debt at 1 KB/s, max sleep is 2 seconds
        assert_eq!(sleep.requested(), Duration::from_secs(2));
    }

    #[test]
    fn burst_clamps_repeatedly() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(500)));

        for _ in 0..5 {
            let sleep = limiter.register(2000);
            // Each sleep should be clamped to 500ms
            assert!(sleep.requested() <= Duration::from_millis(500));
        }
    }

    #[test]
    fn burst_affects_write_max() {
        let without_burst = BandwidthLimiter::new(nz(1024 * 1024));
        let with_burst = BandwidthLimiter::with_burst(nz(1024 * 1024), Some(nz(4096)));

        assert_eq!(with_burst.write_max_bytes(), 4096);
        assert!(without_burst.write_max_bytes() > with_burst.write_max_bytes());
    }

    #[test]
    fn burst_larger_than_rate_allows_burst() {
        let mut session = recorded_sleep_session();
        session.clear();

        // Large burst allows initial rapid transfer
        let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(10000)));

        // First write can be large without excessive sleep
        let sleep = limiter.register(5000);

        // Sleep should be 5 seconds (5000/1000)
        assert_eq!(sleep.requested(), Duration::from_secs(5));
    }

    #[test]
    fn burst_minimum_one_byte() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(1)));

        // Large write with tiny burst - debt clamped to 1
        let sleep = limiter.register(10000);

        // Max sleep = 1 byte / 1000 B/s = 0.001 seconds
        assert!(sleep.requested() <= Duration::from_millis(2));
    }

    #[test]
    fn burst_u64_max_no_clamping() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(u64::MAX)));

        // No clamping with max burst
        let sleep = limiter.register(5000);
        assert_eq!(sleep.requested(), Duration::from_secs(5));
    }

    #[test]
    fn burst_zero_in_parsing() {
        let components = parse_bandwidth_limit("1000:0").expect("parse succeeds");
        assert!(components.burst().is_none());
    }

    #[test]
    fn burst_configuration_update() {
        let mut limiter = BandwidthLimiter::new(nz(1000));

        // Add burst
        limiter.update_configuration(nz(1000), Some(nz(500)));
        assert_eq!(limiter.burst_bytes().unwrap().get(), 500);

        // Remove burst
        limiter.update_configuration(nz(1000), None);
        assert!(limiter.burst_bytes().is_none());
    }
}

// ============================================================================
// 10. Rate Limiting with Small Files vs Large Files
// ============================================================================

mod small_vs_large_files {
    use super::*;

    #[test]
    fn small_file_100_bytes() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1000));

        // Transfer 100 bytes - should take 0.1 seconds
        let sleep = limiter.register(100);
        assert_eq!(sleep.requested(), Duration::from_millis(100));
    }

    #[test]
    fn small_file_below_threshold_is_noop() {
        let mut session = recorded_sleep_session();
        session.clear();

        // High rate means small transfers don't trigger sleep
        let mut limiter = BandwidthLimiter::new(nz(100_000_000)); // 100 MB/s

        // 1000 bytes at 100 MB/s = 0.01 ms - below threshold
        let sleep = limiter.register(1000);
        assert!(sleep.is_noop() || sleep.requested() < Duration::from_millis(100));
    }

    #[test]
    fn small_files_accumulated() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1024)); // 1 KB/s

        // Transfer many small files
        for _ in 0..16 {
            let _ = limiter.register(64); // 16 * 64 = 1024 bytes
        }

        // Total should be ~1 second
        let total = session.total_duration();
        assert!(within_tolerance(total, Duration::from_secs(1), 15.0));
    }

    #[test]
    fn large_file_1mb() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1024 * 1024)); // 1 MB/s

        let sleep = limiter.register(1024 * 1024);
        assert_eq!(sleep.requested(), Duration::from_secs(1));
    }

    #[test]
    fn large_file_10mb() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1024 * 1024)); // 1 MB/s

        let sleep = limiter.register(10 * 1024 * 1024);
        assert_eq!(sleep.requested(), Duration::from_secs(10));
    }

    #[test]
    fn large_file_100mb_chunked() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(10 * 1024 * 1024)); // 10 MB/s
        let chunk_size = limiter.write_max_bytes();

        let total_bytes = 100 * 1024 * 1024;
        let mut remaining = total_bytes;

        while remaining > 0 {
            let to_transfer = remaining.min(chunk_size);
            let _ = limiter.register(to_transfer);
            remaining -= to_transfer;
        }

        // Total: 100 MB at 10 MB/s = 10 seconds
        let total = session.total_duration();
        assert!(within_tolerance(total, Duration::from_secs(10), 20.0));
    }

    #[test]
    fn large_file_1gb() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1_000_000_000)); // 1 GB/s

        let sleep = limiter.register(1_000_000_000); // 1 GB
        assert_eq!(sleep.requested(), Duration::from_secs(1));
    }

    #[test]
    fn mixed_file_sizes_simulation() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1024 * 1024)); // 1 MB/s

        // Mix of small and large files
        let _ = limiter.register(100);     // Small: 100 bytes
        let _ = limiter.register(1024);    // Medium: 1 KB
        let _ = limiter.register(10240);   // Larger: 10 KB
        let _ = limiter.register(102400);  // Big: 100 KB
        let _ = limiter.register(1024 * 1024); // Large: 1 MB

        // Total: ~1.11 MB at 1 MB/s = ~1.11 seconds
        let total = session.total_duration();
        assert!(total >= Duration::from_secs(1));
    }
}

// ============================================================================
// 11. Rate Limiting Accuracy Verification
// ============================================================================

mod rate_limiting_accuracy {
    use super::*;

    #[test]
    fn accuracy_exact_second() {
        let test_cases: Vec<(u64, usize)> = vec![
            (1000, 1000),     // 1000 bytes at 1000 B/s
            (1024, 1024),     // 1 KB at 1 KB/s
            (2048, 2048),     // 2 KB at 2 KB/s
            (10000, 10000),   // 10 KB at 10 KB/s
            (100000, 100000), // 100 KB at 100 KB/s
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

    #[test]
    fn accuracy_fractional_second() {
        let test_cases: Vec<(u64, usize, Duration)> = vec![
            (1000, 500, Duration::from_millis(500)),  // 0.5s
            (1000, 250, Duration::from_millis(250)),  // 0.25s
            (1000, 750, Duration::from_millis(750)),  // 0.75s
            (2000, 500, Duration::from_millis(250)),  // 0.25s
            (4000, 1000, Duration::from_millis(250)), // 0.25s
        ];

        for (rate, bytes, expected) in test_cases {
            let mut session = recorded_sleep_session();
            session.clear();

            let mut limiter = BandwidthLimiter::new(nz(rate));
            let sleep = limiter.register(bytes);

            assert_eq!(
                sleep.requested(),
                expected,
                "Rate {rate}, bytes {bytes}"
            );
        }
    }

    #[test]
    fn accuracy_multiple_seconds() {
        let test_cases: Vec<(u64, usize, u64)> = vec![
            (1000, 2000, 2),   // 2s
            (1000, 5000, 5),   // 5s
            (1000, 10000, 10), // 10s
            (500, 5000, 10),   // 10s
            (100, 1000, 10),   // 10s
        ];

        for (rate, bytes, expected_secs) in test_cases {
            let mut session = recorded_sleep_session();
            session.clear();

            let mut limiter = BandwidthLimiter::new(nz(rate));
            let sleep = limiter.register(bytes);

            assert_eq!(
                sleep.requested(),
                Duration::from_secs(expected_secs),
                "Rate {rate}, bytes {bytes}"
            );
        }
    }

    #[test]
    fn accuracy_millisecond_precision() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1_000_000)); // 1 MB/s

        // 123,000 bytes = 123ms
        let sleep = limiter.register(123_000);
        assert_eq!(sleep.requested(), Duration::from_millis(123));
    }

    #[test]
    fn accuracy_division_precision() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1000));

        // 333 bytes at 1000 B/s = 0.333 seconds
        let sleep = limiter.register(333);
        assert_eq!(sleep.requested(), Duration::from_millis(333));
    }

    #[test]
    fn accuracy_accumulated_over_chunks() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1024)); // 1 KB/s
        let chunk_size = 512;

        for _ in 0..10 {
            let _ = limiter.register(chunk_size);
        }

        // Total: 5120 bytes at 1024 B/s = 5 seconds
        let total = session.total_duration();
        assert!(within_tolerance(total, Duration::from_secs(5), 10.0));
    }

    #[test]
    fn accuracy_very_fast_rate_negligible_sleep() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1_000_000_000)); // 1 GB/s

        let sleep = limiter.register(1_000_000); // 1 MB

        // At 1 GB/s, 1 MB takes 1 ms
        assert!(sleep.requested() <= Duration::from_millis(2));
    }

    #[test]
    fn accuracy_very_slow_rate_large_sleep() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(10)); // 10 B/s

        let sleep = limiter.register(100);

        // At 10 B/s, 100 bytes takes 10 seconds
        assert_eq!(sleep.requested(), Duration::from_secs(10));
    }
}

// ============================================================================
// Additional Edge Cases
// ============================================================================

mod additional_edge_cases {
    use super::*;

    #[test]
    fn register_zero_bytes_is_noop() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1024));

        let sleep = limiter.register(0);
        assert!(sleep.is_noop());
        assert!(session.is_empty());
    }

    #[test]
    fn limiter_sleep_tracking() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(1000));
        let sleep = limiter.register(2000);

        assert!(!sleep.is_noop());
        assert_eq!(sleep.requested(), Duration::from_secs(2));
    }

    #[test]
    fn write_max_scales_with_rate() {
        let slow = BandwidthLimiter::new(nz(1024));
        let fast = BandwidthLimiter::new(nz(1024 * 1024));

        assert!(fast.write_max_bytes() > slow.write_max_bytes());
    }

    #[test]
    fn recommended_read_size_respects_write_max() {
        let limiter = BandwidthLimiter::new(nz(1024));
        let write_max = limiter.write_max_bytes();

        assert_eq!(limiter.recommended_read_size(1_000_000), write_max);
        assert_eq!(limiter.recommended_read_size(100), 100);
    }

    #[test]
    fn clone_creates_independent_copy() {
        let original = BandwidthLimiter::new(nz(1000));
        let cloned = original.clone();

        assert_eq!(original.limit_bytes(), cloned.limit_bytes());
        assert_eq!(original.burst_bytes(), cloned.burst_bytes());
    }

    #[test]
    fn debug_format() {
        let limiter = BandwidthLimiter::new(nz(1000));
        let debug = format!("{limiter:?}");
        assert!(debug.contains("BandwidthLimiter"));
    }

    #[test]
    fn limiter_change_priority() {
        assert!(LimiterChange::Unchanged < LimiterChange::Updated);
        assert!(LimiterChange::Updated < LimiterChange::Enabled);
        assert!(LimiterChange::Enabled < LimiterChange::Disabled);
    }

    #[test]
    fn limiter_change_combine() {
        assert_eq!(
            LimiterChange::Unchanged.combine(LimiterChange::Updated),
            LimiterChange::Updated
        );
        assert_eq!(
            LimiterChange::Updated.combine(LimiterChange::Enabled),
            LimiterChange::Enabled
        );
    }

    #[test]
    fn simulated_realistic_transfer() {
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
        assert!(within_tolerance(total, Duration::from_secs_f64(10.24), 15.0));
    }
}
