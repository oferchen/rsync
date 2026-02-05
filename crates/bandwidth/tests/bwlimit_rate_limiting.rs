//! Comprehensive tests for --bwlimit bandwidth limiting behavior.
//!
//! These tests verify:
//! 1. Transfer rate is actually limited
//! 2. Various units (K, M, G, B) work correctly
//! 3. Small and large files are handled correctly
//! 4. Rate accuracy within tolerance
//! 5. Interaction with burst configuration
//!
//! Note: Tests use the test-support feature to record sleep requests
//! instead of actually sleeping, enabling fast deterministic testing.

use bandwidth::{
    parse_bandwidth_argument, parse_bandwidth_limit, BandwidthLimiter,
    recorded_sleep_session,
};
use std::num::NonZeroU64;
use std::time::Duration;

// ============================================================================
// Helper functions
// ============================================================================

fn nz(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("non-zero value required")
}

/// Calculate expected sleep duration for a given transfer size and rate.
#[allow(dead_code)]
fn expected_sleep(bytes: u64, bytes_per_second: u64) -> Duration {
    Duration::from_secs_f64(bytes as f64 / bytes_per_second as f64)
}

/// Check if actual duration is within tolerance of expected.
fn within_tolerance(actual: Duration, expected: Duration, tolerance_percent: f64) -> bool {
    let tolerance = Duration::from_secs_f64(expected.as_secs_f64() * tolerance_percent / 100.0);
    let min = expected.saturating_sub(tolerance);
    let max = expected.saturating_add(tolerance);
    actual >= min && actual <= max
}

// ============================================================================
// Unit Parsing Tests
// ============================================================================

#[test]
fn bwlimit_unit_bytes_explicit() {
    let limit = parse_bandwidth_argument("512b")
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(limit.get(), 512);
}

#[test]
fn bwlimit_unit_kilobytes_binary() {
    let limit = parse_bandwidth_argument("1K")
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(limit.get(), 1024);
}

#[test]
fn bwlimit_unit_kilobytes_decimal() {
    let limit = parse_bandwidth_argument("1KB")
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(limit.get(), 1000);
}

#[test]
fn bwlimit_unit_megabytes_binary() {
    let limit = parse_bandwidth_argument("1M")
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(limit.get(), 1024 * 1024);
}

#[test]
fn bwlimit_unit_megabytes_decimal() {
    let limit = parse_bandwidth_argument("1MB")
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(limit.get(), 1_000_000);
}

#[test]
fn bwlimit_unit_gigabytes_binary() {
    let limit = parse_bandwidth_argument("1G")
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(limit.get(), 1024 * 1024 * 1024);
}

#[test]
fn bwlimit_unit_gigabytes_decimal() {
    let limit = parse_bandwidth_argument("1GB")
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(limit.get(), 1_000_000_000);
}

#[test]
fn bwlimit_unit_iec_kibibytes() {
    let limit = parse_bandwidth_argument("1KiB")
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(limit.get(), 1024);
}

#[test]
fn bwlimit_unit_iec_mebibytes() {
    let limit = parse_bandwidth_argument("1MiB")
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(limit.get(), 1024 * 1024);
}

#[test]
fn bwlimit_unit_iec_gibibytes() {
    let limit = parse_bandwidth_argument("1GiB")
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(limit.get(), 1024 * 1024 * 1024);
}

#[test]
fn bwlimit_unit_terabytes() {
    let limit = parse_bandwidth_argument("1T")
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(limit.get(), 1024u64.pow(4));
}

#[test]
fn bwlimit_unit_default_is_kilobytes() {
    // Without suffix, the default unit is kilobytes (K)
    let limit = parse_bandwidth_argument("100")
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(limit.get(), 100 * 1024);
}

// ============================================================================
// Rate Limiting Verification Tests
// ============================================================================

#[test]
fn bwlimit_rate_limiting_1kb_per_second() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1024)); // 1 KB/s

    // Transfer 1 KB - should take 1 second
    let sleep = limiter.register(1024);
    assert_eq!(sleep.requested(), Duration::from_secs(1));
}

#[test]
fn bwlimit_rate_limiting_10kb_per_second() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(10 * 1024)); // 10 KB/s

    // Transfer 10 KB - should take 1 second
    let sleep = limiter.register(10 * 1024);
    assert_eq!(sleep.requested(), Duration::from_secs(1));
}

#[test]
fn bwlimit_rate_limiting_1mb_per_second() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1024 * 1024)); // 1 MB/s

    // Transfer 1 MB - should take 1 second
    let sleep = limiter.register(1024 * 1024);
    assert_eq!(sleep.requested(), Duration::from_secs(1));
}

#[test]
fn bwlimit_rate_limiting_partial_second() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1000)); // 1000 bytes/second

    // Transfer 500 bytes - should take 0.5 seconds
    let sleep = limiter.register(500);
    assert_eq!(sleep.requested(), Duration::from_millis(500));
}

#[test]
fn bwlimit_rate_limiting_multiple_seconds() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1024)); // 1 KB/s

    // Transfer 5 KB - should take 5 seconds
    let sleep = limiter.register(5 * 1024);
    assert_eq!(sleep.requested(), Duration::from_secs(5));
}

// ============================================================================
// Small File Tests
// ============================================================================

#[test]
fn bwlimit_small_file_100_bytes() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1000)); // 1000 bytes/second

    // Transfer 100 bytes - should take 0.1 seconds
    let sleep = limiter.register(100);
    assert_eq!(sleep.requested(), Duration::from_millis(100));
}

#[test]
fn bwlimit_small_file_below_minimum_sleep_threshold() {
    let mut session = recorded_sleep_session();
    session.clear();

    // High rate, small transfer - sleep below threshold should be noop
    let mut limiter = BandwidthLimiter::new(nz(100_000_000)); // 100 MB/s

    // Transfer 1000 bytes at 100 MB/s = 0.01 ms sleep
    let sleep = limiter.register(1000);

    // Should be a noop because sleep would be below minimum threshold
    assert!(sleep.is_noop() || sleep.requested() < Duration::from_millis(100));
}

#[test]
fn bwlimit_small_file_accumulated_debt() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1024)); // 1 KB/s

    // Transfer many small chunks that together trigger sleep
    for _ in 0..16 {
        let _ = limiter.register(64);
    }

    // Total: 1024 bytes at 1 KB/s should have required 1 second total sleep
    let total = session.total_duration();
    assert!(total >= Duration::from_millis(900), "Total sleep {total:?} should be at least 900ms");
}

// ============================================================================
// Large File Tests
// ============================================================================

#[test]
fn bwlimit_large_file_1mb() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1024 * 1024)); // 1 MB/s

    // Transfer 1 MB - should take 1 second
    let sleep = limiter.register(1024 * 1024);
    assert_eq!(sleep.requested(), Duration::from_secs(1));
}

#[test]
fn bwlimit_large_file_10mb() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1024 * 1024)); // 1 MB/s

    // Transfer 10 MB - should take 10 seconds
    let sleep = limiter.register(10 * 1024 * 1024);
    assert_eq!(sleep.requested(), Duration::from_secs(10));
}

#[test]
fn bwlimit_large_file_100mb_chunked() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(10 * 1024 * 1024)); // 10 MB/s
    let chunk_size = limiter.write_max_bytes();

    // Transfer 100 MB in chunks
    let total_bytes = 100 * 1024 * 1024;
    let mut remaining = total_bytes;

    while remaining > 0 {
        let to_transfer = remaining.min(chunk_size);
        let _ = limiter.register(to_transfer);
        remaining -= to_transfer;
    }

    // Total: 100 MB at 10 MB/s should have required 10 seconds total sleep
    let total = session.total_duration();
    // Allow 20% tolerance for accumulated timing
    assert!(
        within_tolerance(total, Duration::from_secs(10), 20.0),
        "Total sleep {total:?} should be close to 10 seconds"
    );
}

// ============================================================================
// Accuracy / Tolerance Tests
// ============================================================================

#[test]
fn bwlimit_accuracy_exact_rate() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 2048; // 2 KB/s
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // Transfer exactly rate bytes - should take exactly 1 second
    let sleep = limiter.register(rate as usize);
    assert_eq!(
        sleep.requested(),
        Duration::from_secs(1),
        "Transfer of {rate} bytes at {rate} B/s should take exactly 1 second"
    );
}

#[test]
fn bwlimit_accuracy_half_rate() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 4000; // 4 KB/s
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // Transfer half the rate - should take exactly 0.5 seconds
    let sleep = limiter.register((rate / 2) as usize);
    assert_eq!(
        sleep.requested(),
        Duration::from_millis(500),
        "Transfer of {} bytes at {rate} B/s should take 500ms",
        rate / 2
    );
}

#[test]
fn bwlimit_accuracy_quarter_rate() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 4000; // 4 KB/s
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // Transfer quarter the rate - should take exactly 0.25 seconds
    let sleep = limiter.register((rate / 4) as usize);
    assert_eq!(
        sleep.requested(),
        Duration::from_millis(250),
        "Transfer of {} bytes at {rate} B/s should take 250ms",
        rate / 4
    );
}

#[test]
fn bwlimit_accuracy_double_rate() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 1024; // 1 KB/s
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // Transfer double the rate - should take exactly 2 seconds
    let sleep = limiter.register((rate * 2) as usize);
    assert_eq!(
        sleep.requested(),
        Duration::from_secs(2),
        "Transfer of {} bytes at {rate} B/s should take 2 seconds",
        rate * 2
    );
}

// ============================================================================
// Burst Configuration Tests
// ============================================================================

#[test]
fn bwlimit_burst_clamps_debt() {
    let mut session = recorded_sleep_session();
    session.clear();

    // 1 KB/s rate with 2 KB burst
    let mut limiter = BandwidthLimiter::with_burst(nz(1024), Some(nz(2048)));

    // Transfer 10 KB - debt should be clamped to burst (2 KB)
    let sleep = limiter.register(10 * 1024);

    // With debt clamped to 2 KB at 1 KB/s, sleep should be 2 seconds
    assert_eq!(
        sleep.requested(),
        Duration::from_secs(2),
        "With 2 KB burst cap, sleep should be 2 seconds max"
    );
}

#[test]
fn bwlimit_burst_allows_initial_burst() {
    let mut session = recorded_sleep_session();
    session.clear();

    // Slow rate but large burst
    let mut limiter = BandwidthLimiter::with_burst(nz(512), Some(nz(4096)));

    // Transfer 2 KB - debt should be clamped to burst (4 KB)
    let sleep = limiter.register(2048);

    // Debt is 2048, at 512 B/s = 4 seconds max
    assert!(
        sleep.requested() <= Duration::from_secs(4),
        "Sleep {:?} should be at most 4 seconds",
        sleep.requested()
    );
}

#[test]
fn bwlimit_burst_repeated_writes_stay_clamped() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(500)));

    // Multiple large writes - debt should stay clamped
    for _ in 0..5 {
        let sleep = limiter.register(2000);
        // Each write's debt clamped to 500 bytes at 1000 B/s = 0.5 seconds
        assert!(
            sleep.requested() <= Duration::from_millis(500),
            "Sleep {:?} should be at most 500ms",
            sleep.requested()
        );
    }
}

#[test]
fn bwlimit_burst_parsing_with_limit() {
    let components = parse_bandwidth_limit("4M:32K")
        .expect("parse succeeds");

    assert_eq!(components.rate().unwrap().get(), 4 * 1024 * 1024);
    assert_eq!(components.burst().map(|b| b.get()), Some(32 * 1024));
}

#[test]
fn bwlimit_burst_parsing_without_burst() {
    let components = parse_bandwidth_limit("2M")
        .expect("parse succeeds");

    assert_eq!(components.rate().unwrap().get(), 2 * 1024 * 1024);
    assert!(components.burst().is_none());
}

// ============================================================================
// Write Max / Recommended Read Size Tests
// ============================================================================

#[test]
fn bwlimit_write_max_scales_with_rate() {
    // Slow rate - small write max
    let slow = BandwidthLimiter::new(nz(1024)); // 1 KB/s
    let slow_max = slow.write_max_bytes();

    // Fast rate - larger write max
    let fast = BandwidthLimiter::new(nz(10 * 1024 * 1024)); // 10 MB/s
    let fast_max = fast.write_max_bytes();

    assert!(fast_max > slow_max, "Fast rate should have larger write_max");
}

#[test]
fn bwlimit_recommended_read_size_respects_write_max() {
    let limiter = BandwidthLimiter::new(nz(1024)); // 1 KB/s
    let write_max = limiter.write_max_bytes();

    // Buffer larger than write_max should return write_max
    assert_eq!(limiter.recommended_read_size(1_000_000), write_max);

    // Buffer smaller than write_max should return buffer size
    assert_eq!(limiter.recommended_read_size(100), 100);
}

#[test]
fn bwlimit_burst_affects_write_max() {
    // Same rate, different bursts
    let no_burst = BandwidthLimiter::new(nz(10 * 1024 * 1024));
    let small_burst = BandwidthLimiter::with_burst(nz(10 * 1024 * 1024), Some(nz(2048)));

    assert!(
        small_burst.write_max_bytes() <= no_burst.write_max_bytes(),
        "Burst should cap write_max"
    );
}

// ============================================================================
// Edge Case Tests
// ============================================================================

#[test]
fn bwlimit_minimum_rate_512_bytes() {
    let limit = parse_bandwidth_argument("512b")
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(limit.get(), 512);

    // Create limiter and verify it works
    let mut limiter = BandwidthLimiter::new(limit);
    let sleep = limiter.register(512);
    assert_eq!(sleep.requested(), Duration::from_secs(1));
}

#[test]
fn bwlimit_zero_is_unlimited() {
    let limit = parse_bandwidth_argument("0").expect("parse succeeds");
    assert!(limit.is_none(), "Zero should represent unlimited");
}

#[test]
fn bwlimit_very_high_rate() {
    let mut session = recorded_sleep_session();
    session.clear();

    // 1 GB/s rate
    let mut limiter = BandwidthLimiter::new(nz(1_000_000_000));

    // Transfer 1 MB - should be very fast (1ms)
    let sleep = limiter.register(1_000_000);
    assert!(
        sleep.requested() <= Duration::from_millis(10),
        "At 1 GB/s, 1 MB should transfer in <10ms"
    );
}

#[test]
fn bwlimit_register_zero_bytes_is_noop() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1024));

    let sleep = limiter.register(0);
    assert!(sleep.is_noop(), "Zero bytes should be a noop");
    assert!(session.is_empty(), "No sleep should be recorded for zero bytes");
}

#[test]
fn bwlimit_reset_clears_debt() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1024));

    // Accumulate some debt
    let _ = limiter.register(2048);

    // Reset should clear debt
    limiter.reset();

    // Next register should start fresh
    session.clear();
    let sleep = limiter.register(1024);
    assert_eq!(sleep.requested(), Duration::from_secs(1));
}

#[test]
fn bwlimit_update_limit_resets_limiter() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(100));

    // Accumulate some debt
    let _ = limiter.register(1000);

    // Update limit should reset internal state
    limiter.update_limit(nz(200));

    // After update, subsequent register should use new rate
    let sleep = limiter.register(200);
    // 200 bytes at 200 bytes/s = 1 second
    assert_eq!(sleep.requested(), Duration::from_secs(1));
}

// ============================================================================
// Fractional Value Tests
// ============================================================================

#[test]
fn bwlimit_fractional_value_half_megabyte() {
    let limit = parse_bandwidth_argument("0.5M")
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(limit.get(), 512 * 1024);
}

#[test]
fn bwlimit_fractional_value_1_5_megabytes() {
    let limit = parse_bandwidth_argument("1.5M")
        .expect("parse succeeds")
        .expect("limit available");
    // 1.5 * 1024 * 1024 = 1,572,864
    assert_eq!(limit.get(), 1_572_864);
}

#[test]
fn bwlimit_fractional_value_with_decimal_suffix() {
    let limit = parse_bandwidth_argument("1.5MB")
        .expect("parse succeeds")
        .expect("limit available");
    // 1.5 * 1,000,000 = 1,500,000
    assert_eq!(limit.get(), 1_500_000);
}

// ============================================================================
// Simulated Transfer Tests
// ============================================================================

#[test]
fn bwlimit_simulated_small_file_transfer() {
    let mut session = recorded_sleep_session();
    session.clear();

    // Simulate 10 KB file at 2 KB/s
    let file_size = 10 * 1024;
    let rate = 2 * 1024;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    let chunk_size = limiter.write_max_bytes();
    let mut remaining = file_size;

    while remaining > 0 {
        let to_transfer = remaining.min(chunk_size);
        let _ = limiter.register(to_transfer);
        remaining -= to_transfer;
    }

    // Should have taken approximately 5 seconds (10 KB / 2 KB/s)
    let total = session.total_duration();
    assert!(
        within_tolerance(total, Duration::from_secs(5), 15.0),
        "Transfer should take about 5 seconds, got {total:?}"
    );
}

#[test]
fn bwlimit_simulated_large_file_transfer() {
    let mut session = recorded_sleep_session();
    session.clear();

    // Simulate 1 MB file at 100 KB/s
    let file_size = 1024 * 1024;
    let rate = 100 * 1024;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    let chunk_size = limiter.write_max_bytes();
    let mut remaining = file_size;

    while remaining > 0 {
        let to_transfer = remaining.min(chunk_size);
        let _ = limiter.register(to_transfer);
        remaining -= to_transfer;
    }

    // Should have taken approximately 10.24 seconds (1 MB / 100 KB/s)
    let expected = Duration::from_secs_f64(file_size as f64 / rate as f64);
    let total = session.total_duration();
    assert!(
        within_tolerance(total, expected, 15.0),
        "Transfer should take about {expected:?}, got {total:?}"
    );
}

// ============================================================================
// Comparison with Expected Behavior Tests
// ============================================================================

#[test]
fn bwlimit_behavior_matches_upstream_semantics_byte_suffix() {
    // Upstream rsync: 512b means exactly 512 bytes/second
    let limit = parse_bandwidth_argument("512b")
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(limit.get(), 512);
}

#[test]
fn bwlimit_behavior_matches_upstream_semantics_k_suffix() {
    // Upstream rsync: K suffix uses binary kilobytes (1024)
    let limit = parse_bandwidth_argument("10K")
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(limit.get(), 10 * 1024);
}

#[test]
fn bwlimit_behavior_matches_upstream_semantics_kb_suffix() {
    // Upstream rsync: KB suffix uses decimal kilobytes (1000)
    let limit = parse_bandwidth_argument("10KB")
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(limit.get(), 10 * 1000);
}

#[test]
fn bwlimit_behavior_matches_upstream_semantics_default_unit() {
    // Upstream rsync: bare numbers default to kilobytes (K)
    let bare = parse_bandwidth_argument("100")
        .expect("parse succeeds")
        .expect("limit available");
    let explicit = parse_bandwidth_argument("100K")
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(bare, explicit);
}

#[test]
fn bwlimit_behavior_matches_upstream_semantics_zero_unlimited() {
    // Upstream rsync: 0 means unlimited
    let limit = parse_bandwidth_argument("0").expect("parse succeeds");
    assert!(limit.is_none());
}

#[test]
fn bwlimit_behavior_matches_upstream_semantics_minimum_512() {
    // Upstream rsync: minimum is 512 bytes/second
    let at_min = parse_bandwidth_argument("512b").expect("parse succeeds");
    assert!(at_min.is_some());

    let below_min = parse_bandwidth_argument("511b");
    assert!(below_min.is_err());
}

#[test]
fn bwlimit_behavior_matches_upstream_semantics_burst() {
    // Upstream rsync: RATE:BURST syntax
    let components = parse_bandwidth_limit("1M:32K")
        .expect("parse succeeds");

    assert_eq!(components.rate().unwrap().get(), 1024 * 1024);
    assert_eq!(components.burst().unwrap().get(), 32 * 1024);
}

// ============================================================================
// Rate Limiting Duration Calculation Tests
// ============================================================================

#[test]
fn bwlimit_duration_calculation_precise() {
    let test_cases: Vec<(u64, usize, Duration)> = vec![
        // (rate bytes/s, transfer bytes, expected duration)
        (1000, 1000, Duration::from_secs(1)),
        (1000, 500, Duration::from_millis(500)),
        (1000, 2000, Duration::from_secs(2)),
        (2000, 1000, Duration::from_millis(500)),
        (500, 1000, Duration::from_secs(2)),
        (1024, 1024, Duration::from_secs(1)),
        (1024, 512, Duration::from_millis(500)),
        (10240, 10240, Duration::from_secs(1)),
    ];

    for (rate, bytes, expected) in test_cases {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(nz(rate));
        let sleep = limiter.register(bytes);

        assert_eq!(
            sleep.requested(),
            expected,
            "Rate {rate} B/s, transfer {bytes} bytes: expected {expected:?}, got {:?}",
            sleep.requested()
        );
    }
}
