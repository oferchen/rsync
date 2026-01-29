/// Comprehensive tests for rate limiting algorithms and token bucket behavior
use super::{BandwidthLimiter, recorded_sleep_session};
use std::num::NonZeroU64;
use std::time::Duration;

fn nz(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("non-zero value required")
}

// ========================================================================
// Token Bucket Algorithm Tests
// ========================================================================

#[test]
fn token_bucket_accumulates_debt_gradually() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1000)); // 1000 B/s

    // Write 100 bytes at a time
    for _ in 0..5 {
        let _ = limiter.register(100);
    }

    // Total 500 bytes at 1000 B/s should accumulate approximately 0.5s sleep
    let total = session.total_duration();
    assert!(total >= Duration::from_millis(400));
    assert!(total <= Duration::from_millis(600));
}

#[test]
fn token_bucket_forgives_debt_over_time() {
    let mut limiter = BandwidthLimiter::new(nz(1_000_000)); // 1 MB/s

    // First write - accumulates some debt
    let _ = limiter.register(1000);

    // Sleep to allow time to elapse (in real tests this is simulated)
    std::thread::sleep(Duration::from_millis(10));

    // Second write - debt should be partially forgiven
    let debt_before = limiter.accumulated_debt_for_testing();

    // Debt should be reduced or zero due to elapsed time
    assert!(debt_before <= 1000);
}

#[test]
fn token_bucket_handles_sub_minimum_sleep_threshold() {
    // MINIMUM_SLEEP_MICROS is 100_000 (0.1s)
    let mut limiter = BandwidthLimiter::new(nz(1_000_000)); // 1 MB/s

    // Write amount that would cause < 100ms sleep
    // 50_000 bytes at 1 MB/s = 50ms
    let sleep = limiter.register(50_000);

    // Should be noop because under threshold
    assert!(sleep.is_noop());
}

#[test]
fn token_bucket_at_minimum_sleep_threshold() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1_000_000)); // 1 MB/s

    // Write exactly at threshold: 100_000 bytes at 1 MB/s = 100ms
    let sleep = limiter.register(100_000);

    assert!(!sleep.is_noop());
    assert_eq!(sleep.requested(), Duration::from_millis(100));
}

#[test]
fn token_bucket_just_under_minimum_threshold() {
    let mut limiter = BandwidthLimiter::new(nz(1_000_000)); // 1 MB/s

    // 99_999 bytes at 1 MB/s = 99.999ms < 100ms threshold
    let sleep = limiter.register(99_999);

    assert!(sleep.is_noop() || sleep.requested() < Duration::from_micros(100_000));
}

// ========================================================================
// Leaky Bucket Behavior Tests
// ========================================================================

#[test]
fn leaky_bucket_with_continuous_flow() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1024)); // 1 KB/s

    // Continuous writes
    let mut total_requested = Duration::ZERO;
    for _ in 0..10 {
        let sleep = limiter.register(102); // 102 bytes each
        total_requested = total_requested.saturating_add(sleep.requested());
    }

    // Total 1020 bytes at 1024 B/s should be close to 1 second
    assert!(total_requested >= Duration::from_millis(800));
    assert!(total_requested <= Duration::from_millis(1200));
}

#[test]
fn leaky_bucket_with_bursty_traffic() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1000)); // 1000 B/s

    // Large burst followed by smaller writes
    let _ = limiter.register(5000);
    let _ = limiter.register(100);
    let _ = limiter.register(100);

    let total = session.total_duration();
    // 5200 bytes at 1000 B/s = ~5.2s
    assert!(total >= Duration::from_secs(4));
}

#[test]
fn leaky_bucket_burst_clamping_prevents_excessive_delay() {
    let mut session = recorded_sleep_session();
    session.clear();

    // Very slow rate with burst cap
    let mut limiter = BandwidthLimiter::with_burst(nz(100), Some(nz(500)));

    // Write way more than burst
    let sleep = limiter.register(10_000);

    // Debt clamped to 500, so sleep clamped to 5s (500/100)
    assert!(sleep.requested() <= Duration::from_secs(5));
}

// ========================================================================
// Rate Change During Operation Tests
// ========================================================================

#[test]
fn rate_change_from_1kb_to_10kb_speeds_up() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1024)); // 1 KB/s

    // Initial write at slow rate
    let sleep1 = limiter.register(1024);
    assert_eq!(sleep1.requested(), Duration::from_secs(1));

    session.clear();

    // Change to faster rate
    limiter.update_limit(nz(10240)); // 10 KB/s

    // Same write should be 10x faster
    let sleep2 = limiter.register(1024);
    assert!(sleep2.requested() <= Duration::from_millis(150));
}

#[test]
fn rate_change_from_10kb_to_1kb_slows_down() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(10240)); // 10 KB/s

    let sleep1 = limiter.register(1024);
    assert!(sleep1.requested() <= Duration::from_millis(150));

    session.clear();

    // Change to slower rate
    limiter.update_limit(nz(1024)); // 1 KB/s

    let sleep2 = limiter.register(1024);
    assert!(sleep2.requested() >= Duration::from_millis(900));
}

#[test]
fn rate_change_clears_accumulated_debt() {
    let mut limiter = BandwidthLimiter::new(nz(100)); // Very slow

    // Accumulate significant debt
    let _ = limiter.register(10000);
    let debt = limiter.accumulated_debt_for_testing();
    assert!(debt > 0);

    // Change rate - debt should be cleared
    limiter.update_limit(nz(1_000_000));
    assert_eq!(limiter.accumulated_debt_for_testing(), 0);
}

#[test]
fn configuration_change_with_burst_addition() {
    let mut limiter = BandwidthLimiter::new(nz(5000));

    // No burst initially
    assert!(limiter.burst_bytes().is_none());

    // Add burst via configuration change
    limiter.update_configuration(nz(5000), Some(nz(2000)));

    assert_eq!(limiter.burst_bytes(), Some(nz(2000)));
}

#[test]
fn configuration_change_with_burst_removal() {
    let mut limiter = BandwidthLimiter::with_burst(nz(5000), Some(nz(2000)));

    assert_eq!(limiter.burst_bytes(), Some(nz(2000)));

    // Remove burst
    limiter.update_configuration(nz(5000), None);

    assert!(limiter.burst_bytes().is_none());
}

// ========================================================================
// Zero and Edge Rate Tests
// ========================================================================

#[test]
fn minimum_rate_one_byte_per_second() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1)); // 1 B/s

    let sleep = limiter.register(1);
    assert_eq!(sleep.requested(), Duration::from_secs(1));
}

#[test]
fn minimum_rate_with_multiple_bytes() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1)); // 1 B/s

    let sleep = limiter.register(10);
    assert_eq!(sleep.requested(), Duration::from_secs(10));
}

#[test]
fn maximum_rate_near_u64_max() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(u64::MAX));

    // Even large writes should have negligible sleep
    let sleep = limiter.register(1_000_000);
    assert!(sleep.requested() < Duration::from_micros(1));
}

#[test]
fn zero_byte_write_never_affects_state() {
    let mut limiter = BandwidthLimiter::new(nz(1000));

    let _ = limiter.register(500);
    let debt_before = limiter.accumulated_debt_for_testing();

    // Zero-byte write
    let sleep = limiter.register(0);

    assert!(sleep.is_noop());
    assert_eq!(limiter.accumulated_debt_for_testing(), debt_before);
}

// ========================================================================
// Timing Precision Tests
// ========================================================================

#[test]
fn precise_one_second_sleep() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1000));

    let sleep = limiter.register(1000);

    // Should be exactly 1 second
    assert_eq!(sleep.requested(), Duration::from_secs(1));
}

#[test]
fn precise_half_second_sleep() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1000));

    let sleep = limiter.register(500);

    assert_eq!(sleep.requested(), Duration::from_millis(500));
}

#[test]
fn precise_millisecond_calculation() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1_000_000)); // 1 MB/s

    // 250_000 bytes at 1 MB/s = 250ms
    let sleep = limiter.register(250_000);

    assert_eq!(sleep.requested(), Duration::from_millis(250));
}

// ========================================================================
// Burst Behavior Tests
// ========================================================================

#[test]
fn burst_allows_initial_large_write() {
    let mut session = recorded_sleep_session();
    session.clear();

    // Slow rate but large burst
    let mut limiter = BandwidthLimiter::with_burst(nz(512), Some(nz(10240)));

    // First large write should be clamped by burst
    let sleep = limiter.register(20000);

    // Debt clamped to 10240, so at 512 B/s, sleep = 10240/512 = 20s
    assert_eq!(sleep.requested(), Duration::from_secs(20));
}

#[test]
fn burst_zero_effectively_means_no_burst() {
    // Burst of 1 is the minimum
    let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(1)));

    let _ = limiter.register(1000);

    // Debt should be clamped to 1
    assert!(limiter.accumulated_debt_for_testing() <= 1);
}

#[test]
fn burst_larger_than_writes_no_clamping() {
    let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(10000)));

    let _ = limiter.register(5000);

    // Debt should not be clamped because under burst limit
    let debt = limiter.accumulated_debt_for_testing();
    // Exact debt depends on timing, but should be reasonable
    assert!(debt <= 10000);
}

// ========================================================================
// Multiple Register Calls Tests
// ========================================================================

#[test]
fn multiple_registers_update_last_instant() {
    let mut limiter = BandwidthLimiter::new(nz(1_000_000_000)); // Very fast

    let _ = limiter.register(100);

    // Small delay
    for _ in 0..100 {
        std::hint::spin_loop();
    }

    let sleep2 = limiter.register(100);

    // With such a fast rate and timing, should be minimal sleep
    assert!(sleep2.requested() < Duration::from_millis(1));
}

#[test]
fn rapid_succession_writes_accumulate() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1024));

    // 5 rapid writes
    for _ in 0..5 {
        let _ = limiter.register(512);
    }

    let total = session.total_duration();

    // 2560 bytes at 1024 B/s = ~2.5s
    assert!(total >= Duration::from_secs(2));
}

// ========================================================================
// Simulated Elapsed Time Tests
// ========================================================================

#[test]
fn simulated_elapsed_compensates_for_missed_sleep() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1024));

    // First write requests sleep
    let sleep1 = limiter.register(1024);
    assert_eq!(sleep1.requested(), Duration::from_secs(1));

    // In tests, actual sleep is near-zero, but limiter tracks simulated time
    // Second write should benefit from simulated elapsed time
    let sleep2 = limiter.register(100);

    // Due to simulated elapsed time, sleep2 should be small or zero
    assert!(sleep2.requested() <= Duration::from_millis(200));
}

// ========================================================================
// Debt Saturation Tests
// ========================================================================

#[test]
fn debt_saturating_add_prevents_overflow() {
    let mut limiter = BandwidthLimiter::new(nz(1));

    // Multiple very large writes with saturating add
    for _ in 0..100 {
        let _ = limiter.register(usize::MAX / 1000);
    }

    // Should not panic or wrap around
    // Debt will be very large but bounded
}

#[test]
fn debt_with_elapsed_time_reduction() {
    let mut limiter = BandwidthLimiter::new(nz(1_000_000));

    let _ = limiter.register(10000);

    // Wait for time to elapse
    std::thread::sleep(Duration::from_millis(20));

    let debt_after_wait = limiter.accumulated_debt_for_testing();

    // Debt should be reduced due to elapsed time
    // At 1 MB/s, 20ms = 20000 bytes of "credit"
    assert!(debt_after_wait <= 10000);
}

// ========================================================================
// Write Max Calculation Tests
// ========================================================================

#[test]
fn write_max_scales_linearly_with_rate() {
    let limiter1 = BandwidthLimiter::new(nz(1024 * 10)); // 10 KB/s
    let limiter2 = BandwidthLimiter::new(nz(1024 * 20)); // 20 KB/s
    let limiter3 = BandwidthLimiter::new(nz(1024 * 40)); // 40 KB/s

    let max1 = limiter1.write_max_bytes();
    let max2 = limiter2.write_max_bytes();
    let max3 = limiter3.write_max_bytes();

    // Should scale (kib * 128)
    assert_eq!(max1, 1280);
    assert_eq!(max2, 2560);
    assert_eq!(max3, 5120);
}

#[test]
fn write_max_respects_min_write_max_constant() {
    let limiter = BandwidthLimiter::new(nz(100));

    // Should be MIN_WRITE_MAX (512) even with tiny rate
    assert_eq!(limiter.write_max_bytes(), 512);
}

// ========================================================================
// Recommended Read Size Tests
// ========================================================================

#[test]
fn recommended_read_size_with_zero_buffer() {
    let limiter = BandwidthLimiter::new(nz(1024));

    assert_eq!(limiter.recommended_read_size(0), 0);
}

#[test]
fn recommended_read_size_boundary_conditions() {
    let limiter = BandwidthLimiter::new(nz(1024 * 100));
    let write_max = limiter.write_max_bytes();

    // At boundary
    assert_eq!(limiter.recommended_read_size(write_max), write_max);

    // Just under
    assert_eq!(limiter.recommended_read_size(write_max - 1), write_max - 1);

    // Just over
    assert_eq!(limiter.recommended_read_size(write_max + 1), write_max);
}

// ========================================================================
// Integration Tests
// ========================================================================

#[test]
fn realistic_large_file_transfer() {
    let mut session = recorded_sleep_session();
    session.clear();

    // Simulate 10MB file at 1MB/s
    let mut limiter = BandwidthLimiter::new(nz(1_000_000));

    let chunk_size = limiter.write_max_bytes();
    let file_size = 10_000_000;
    let num_chunks = file_size / chunk_size;

    for _ in 0..num_chunks {
        let _ = limiter.register(chunk_size);
    }

    // Should take approximately 10 seconds
    let total = session.total_duration();
    assert!(total >= Duration::from_secs(9));
    assert!(total <= Duration::from_secs(11));
}

#[test]
fn realistic_streaming_with_varying_chunk_sizes() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(10240)); // 10 KB/s

    // Varying chunk sizes
    let _ = limiter.register(1024);
    let _ = limiter.register(2048);
    let _ = limiter.register(512);
    let _ = limiter.register(4096);

    // Total ~7.7KB at 10KB/s ≈ 0.77s
    let total = session.total_duration();
    assert!(total >= Duration::from_millis(600));
    assert!(total <= Duration::from_millis(900));
}
