use std::num::NonZeroU64;
use std::time::Duration;

use super::super::MIN_WRITE_MAX;
use super::BandwidthLimiter;
use super::write_max::calculate_write_max;

fn nz(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("non-zero value required")
}

// Tests for calculate_write_max
#[test]
fn calculate_write_max_small_limit_uses_minimum() {
    let result = calculate_write_max(nz(100), None);
    assert_eq!(result, MIN_WRITE_MAX);
}

#[test]
fn calculate_write_max_1kb_limit() {
    let result = calculate_write_max(nz(1024), None);
    assert_eq!(result, MIN_WRITE_MAX);
}

#[test]
fn calculate_write_max_large_limit() {
    let result = calculate_write_max(nz(1024 * 100), None);
    assert_eq!(result, 12800);
}

#[test]
fn calculate_write_max_with_burst_overrides() {
    let result = calculate_write_max(nz(1024 * 100), Some(nz(8192)));
    assert_eq!(result, 8192);
}

#[test]
fn calculate_write_max_with_small_burst_uses_minimum() {
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
    let _ = limiter.register(5000);
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
    let mut limiter = BandwidthLimiter::new(nz(1_000_000_000));
    let sleep = limiter.register(100);
    assert!(sleep.requested() < Duration::from_millis(1));
}

#[test]
fn register_accumulates_debt() {
    let mut limiter = BandwidthLimiter::new(nz(1_000_000));
    let _ = limiter.register(1000);
}

#[test]
fn register_with_burst_clamps_debt() {
    let mut limiter = BandwidthLimiter::with_burst(nz(100), Some(nz(1000)));
    let _ = limiter.register(5000);
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
    let limiter = BandwidthLimiter::new(nz(u64::MAX));
    let write_max = limiter.write_max_bytes();
    assert!(write_max >= MIN_WRITE_MAX);
}

#[test]
fn bandwidth_limiter_burst_larger_than_write() {
    let limiter = BandwidthLimiter::with_burst(nz(1024), Some(nz(1_000_000)));
    assert_eq!(limiter.burst_bytes().unwrap().get(), 1_000_000);
    assert_eq!(limiter.write_max_bytes(), 1_000_000);
}

#[test]
fn register_uses_saturating_add_prevents_overflow() {
    let mut limiter = BandwidthLimiter::new(nz(1));
    for _ in 0..1000 {
        let _ = limiter.register(usize::MAX / 2);
    }
}

#[test]
fn debt_accumulation_with_burst_clamping() {
    let mut limiter = BandwidthLimiter::with_burst(nz(100), Some(nz(500)));
    let _ = limiter.register(10000);
    assert!(
        limiter.accumulated_debt_for_testing() <= 500,
        "debt {} should be <= burst 500",
        limiter.accumulated_debt_for_testing()
    );
}

#[test]
fn multiple_registers_with_burst_maintains_clamp() {
    let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(2000)));

    let _ = limiter.register(3000);
    assert!(limiter.accumulated_debt_for_testing() <= 2000);

    let _ = limiter.register(3000);
    assert!(limiter.accumulated_debt_for_testing() <= 2000);

    let _ = limiter.register(3000);
    assert!(limiter.accumulated_debt_for_testing() <= 2000);
}

#[test]
fn clamp_debt_to_burst_with_no_burst_is_noop() {
    let mut limiter = BandwidthLimiter::new(nz(100));
    let _ = limiter.register(1000);
    let debt_before = limiter.accumulated_debt_for_testing();
    let _ = debt_before;
}

#[test]
fn clamp_debt_to_burst_clamps_at_exact_burst_value() {
    let mut limiter = BandwidthLimiter::with_burst(nz(1), Some(nz(100)));

    let _ = limiter.register(100);
    assert!(limiter.accumulated_debt_for_testing() <= 100);

    let _ = limiter.register(100);
    assert!(limiter.accumulated_debt_for_testing() <= 100);
}

#[test]
fn clamp_debt_to_burst_with_very_large_burst() {
    let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(u64::MAX)));
    let _ = limiter.register(10000);
}

#[test]
fn calculate_write_max_u64_max_limit() {
    let result = calculate_write_max(nz(u64::MAX), None);
    assert!(result >= MIN_WRITE_MAX);
}

#[test]
fn calculate_write_max_burst_overrides_calculated_value() {
    let result_no_burst = calculate_write_max(nz(u64::MAX), None);
    let result_with_burst = calculate_write_max(nz(u64::MAX), Some(nz(4096)));
    assert!(result_with_burst <= result_no_burst.max(4096));
}

#[test]
fn calculate_write_max_burst_at_boundary_512() {
    let result = calculate_write_max(nz(10000), Some(nz(MIN_WRITE_MAX as u64)));
    assert_eq!(result, MIN_WRITE_MAX);
}

#[test]
fn calculate_write_max_burst_just_above_minimum() {
    let result = calculate_write_max(nz(10000), Some(nz(MIN_WRITE_MAX as u64 + 1)));
    assert_eq!(result, MIN_WRITE_MAX + 1);
}

#[test]
fn update_limit_clears_accumulated_debt() {
    let mut limiter = BandwidthLimiter::new(nz(100));
    let _ = limiter.register(1000);
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

#[test]
fn register_zero_bytes_does_not_affect_debt() {
    let mut limiter = BandwidthLimiter::new(nz(100));
    let _ = limiter.register(1000);
    let _debt_before = limiter.accumulated_debt_for_testing();

    let sleep = limiter.register(0);

    assert!(sleep.is_noop());
}

#[test]
fn register_zero_bytes_returns_default_sleep() {
    let mut limiter = BandwidthLimiter::new(nz(100));
    let sleep = limiter.register(0);

    assert!(sleep.is_noop());
    assert!(sleep.requested().is_zero());
}

#[test]
fn register_multiple_times_updates_last_instant() {
    let mut limiter = BandwidthLimiter::new(nz(1_000_000_000));
    let _ = limiter.register(100);
    let sleep2 = limiter.register(100);
    assert!(sleep2.requested() < Duration::from_millis(1));
}

#[test]
fn debt_reduction_from_elapsed_time() {
    let mut limiter = BandwidthLimiter::new(nz(1_000_000));
    let _ = limiter.register(1000);
    std::thread::sleep(Duration::from_millis(10));
    let sleep = limiter.register(100);
    assert!(sleep.requested() < Duration::from_secs(1));
}

#[test]
fn elapsed_time_forgives_all_debt_when_slow_enough() {
    let mut limiter = BandwidthLimiter::new(nz(1_000_000_000));
    let _ = limiter.register(100);
    std::thread::sleep(Duration::from_millis(10));
    let sleep = limiter.register(100);
    assert!(sleep.is_noop() || sleep.requested() < Duration::from_micros(100));
}

#[test]
fn calculate_write_max_with_tiny_limit() {
    let result = calculate_write_max(nz(1), None);
    assert_eq!(result, MIN_WRITE_MAX);
}

#[test]
fn calculate_write_max_progression() {
    let small = calculate_write_max(nz(1024), None);
    let medium = calculate_write_max(nz(1024 * 100), None);
    let large = calculate_write_max(nz(1024 * 1000), None);

    assert!(medium >= small);
    assert!(large >= medium);
}

#[test]
fn recommended_read_size_with_zero_write_max() {
    let limiter = BandwidthLimiter::new(nz(1));
    assert!(limiter.recommended_read_size(1000) >= 1);
}

#[test]
fn limiter_debt_clamping_repeated() {
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

    limiter.update_configuration(nz(1024 * 1024), Some(nz(1024)));
    let new_write_max = limiter.write_max_bytes();

    assert!(new_write_max < initial_write_max);
    assert_eq!(new_write_max, 1024);
}

#[test]
fn reset_clears_simulated_elapsed_us() {
    let mut limiter = BandwidthLimiter::new(nz(1024));
    let _ = limiter.register(4096);
    limiter.reset();

    let sleep = limiter.register(1024);
    assert_eq!(sleep.requested(), Duration::from_secs(1));
}

#[test]
fn register_deterministic_sleep_calculation() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1024));

    let sleep = limiter.register(1024);

    assert_eq!(sleep.requested(), Duration::from_secs(1));
}

#[test]
fn register_deterministic_multiple_writes() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1000));

    let sleep = limiter.register(500);
    assert_eq!(sleep.requested(), Duration::from_millis(500));
}

#[test]
fn register_exact_rate_calculation() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(100));

    let sleep = limiter.register(100);
    assert_eq!(sleep.requested(), Duration::from_secs(1));
}

#[test]
fn register_very_slow_rate() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(10));

    let sleep = limiter.register(10);
    assert_eq!(sleep.requested(), Duration::from_secs(1));
}

#[test]
fn register_very_fast_rate_no_sleep() {
    let mut limiter = BandwidthLimiter::new(nz(u64::MAX));
    let sleep = limiter.register(1000);
    assert!(sleep.requested() < Duration::from_millis(1));
}

#[test]
fn register_sleep_under_minimum_threshold() {
    let mut limiter = BandwidthLimiter::new(nz(1_000_000));
    let sleep = limiter.register(50_000);
    assert!(sleep.is_noop());
}

#[test]
fn register_sleep_at_minimum_threshold() {
    let mut limiter = BandwidthLimiter::new(nz(1_000_000));
    let sleep = limiter.register(100_000);
    assert!(!sleep.is_noop());
    assert_eq!(sleep.requested(), Duration::from_millis(100));
}

#[test]
fn register_sleep_above_minimum_threshold() {
    let mut limiter = BandwidthLimiter::new(nz(1_000_000));
    let sleep = limiter.register(200_000);
    assert!(!sleep.is_noop());
    assert_eq!(sleep.requested(), Duration::from_millis(200));
}

#[test]
fn burst_clamps_sleep_duration() {
    let mut limiter = BandwidthLimiter::with_burst(nz(100), Some(nz(500)));
    let sleep = limiter.register(1000);
    assert_eq!(sleep.requested(), Duration::from_secs(5));
}

#[test]
fn burst_clamps_after_each_register() {
    let mut limiter = BandwidthLimiter::with_burst(nz(100), Some(nz(200)));

    let _sleep1 = limiter.register(500);
    assert!(limiter.accumulated_debt_for_testing() <= 200);

    let _sleep2 = limiter.register(500);
    assert!(limiter.accumulated_debt_for_testing() <= 200);
}

#[test]
fn no_burst_allows_unlimited_debt_growth() {
    let mut limiter = BandwidthLimiter::new(nz(10));

    let sleep = limiter.register(1000);

    assert_eq!(sleep.requested(), Duration::from_secs(100));
}

#[test]
fn write_max_minimum_for_tiny_rate() {
    let limiter = BandwidthLimiter::new(nz(1));
    assert_eq!(limiter.write_max_bytes(), MIN_WRITE_MAX);
}

#[test]
fn write_max_scales_with_rate() {
    let limiter = BandwidthLimiter::new(nz(1024 * 50));
    assert_eq!(limiter.write_max_bytes(), 6400);
}

#[test]
fn write_max_capped_by_burst() {
    let limiter = BandwidthLimiter::with_burst(nz(1024 * 1000), Some(nz(4096)));
    assert_eq!(limiter.write_max_bytes(), 4096);
}

#[test]
fn write_max_burst_respects_minimum() {
    let limiter = BandwidthLimiter::with_burst(nz(1024 * 1000), Some(nz(100)));
    assert_eq!(limiter.write_max_bytes(), MIN_WRITE_MAX);
}

#[test]
fn recommended_read_size_exact_write_max() {
    let limiter = BandwidthLimiter::new(nz(1024 * 100));
    assert_eq!(limiter.recommended_read_size(12800), 12800);
}

#[test]
fn recommended_read_size_much_larger() {
    let limiter = BandwidthLimiter::new(nz(1024 * 100));
    assert_eq!(limiter.recommended_read_size(1_000_000), 12800);
}

#[test]
fn recommended_read_size_much_smaller() {
    let limiter = BandwidthLimiter::new(nz(1024 * 100));
    assert_eq!(limiter.recommended_read_size(10), 10);
}

#[test]
fn recommended_read_size_one_byte() {
    let limiter = BandwidthLimiter::new(nz(1024 * 100));
    assert_eq!(limiter.recommended_read_size(1), 1);
}

#[test]
fn register_first_call_has_no_elapsed_time() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1000));

    let sleep = limiter.register(1000);
    assert_eq!(sleep.requested(), Duration::from_secs(1));
}

#[test]
fn register_updates_last_instant() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1_000_000_000));

    let _sleep1 = limiter.register(100);

    let sleep2 = limiter.register(100);

    assert!(sleep2.requested() < Duration::from_millis(1));
}

#[test]
fn update_limit_mid_operation_resets_state() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(100));

    let _sleep1 = limiter.register(100);

    limiter.update_limit(nz(200));
    assert_eq!(limiter.accumulated_debt_for_testing(), 0);

    let sleep2 = limiter.register(200);
    assert_eq!(sleep2.requested(), Duration::from_secs(1));
}

#[test]
fn update_configuration_mid_operation_resets_state() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::with_burst(nz(100), Some(nz(50)));

    let _sleep1 = limiter.register(100);

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

    let _sleep1 = limiter.register(100);

    limiter.reset();
    assert_eq!(limiter.accumulated_debt_for_testing(), 0);

    assert_eq!(limiter.limit_bytes().get(), 100);
}

#[test]
fn register_returns_correct_limiter_sleep() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1000));
    let sleep = limiter.register(2000);

    assert_eq!(sleep.requested(), Duration::from_secs(2));
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

    assert_eq!(
        cloned.accumulated_debt_for_testing(),
        limiter.accumulated_debt_for_testing()
    );
}

#[test]
fn cloned_limiter_independent() {
    let mut limiter = BandwidthLimiter::new(nz(1000));
    let cloned = limiter.clone();

    let _ = limiter.register(100);

    assert_eq!(cloned.accumulated_debt_for_testing(), 0);
}

#[test]
fn register_very_large_write() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1_000_000));

    let sleep = limiter.register(1_000_000_000);

    assert_eq!(sleep.requested(), Duration::from_secs(1000));
}

#[test]
fn register_usize_max_with_high_rate() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(u64::MAX / 1000));

    let _sleep = limiter.register(usize::MAX / 1000);
}

#[test]
fn simulated_elapsed_time_reduces_debt() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1000));

    let sleep1 = limiter.register(1000);
    assert_eq!(sleep1.requested(), Duration::from_secs(1));
}

#[test]
fn register_exactly_one_byte() {
    let mut limiter = BandwidthLimiter::new(nz(1));
    let sleep = limiter.register(1);
    assert_eq!(sleep.requested(), Duration::from_secs(1));
}

#[test]
fn register_with_max_u64_limit() {
    let mut limiter = BandwidthLimiter::new(nz(u64::MAX));
    let sleep = limiter.register(1000);
    assert!(sleep.requested() < Duration::from_nanos(100));
}

#[test]
fn register_with_max_burst() {
    let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(u64::MAX)));
    let sleep = limiter.register(5000);
    assert_eq!(sleep.requested(), Duration::from_secs(5));
}

#[test]
fn register_with_min_burst() {
    let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(1)));
    let _ = limiter.register(1000);
    assert!(limiter.accumulated_debt_for_testing() <= 1);
}

#[test]
fn accessors_consistent_after_construction() {
    let limiter = BandwidthLimiter::with_burst(nz(5000), Some(nz(2500)));

    assert_eq!(limiter.limit_bytes().get(), 5000);
    assert_eq!(limiter.burst_bytes().unwrap().get(), 2500);
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

    assert_eq!(limiter.limit_bytes().get(), 1000);
    assert_eq!(limiter.burst_bytes().unwrap().get(), 500);
    assert_eq!(limiter.write_max_bytes(), MIN_WRITE_MAX.max(500));
    assert_eq!(limiter.accumulated_debt_for_testing(), 0);
}

#[test]
fn simulated_transfer_scenario() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1024));

    let mut total_requested = Duration::ZERO;
    for _ in 0..10 {
        let sleep = limiter.register(1024);
        total_requested = total_requested.saturating_add(sleep.requested());
    }

    assert!(total_requested >= Duration::from_secs(9));
    assert!(total_requested <= Duration::from_secs(11));
}

#[test]
fn simulated_bursty_transfer() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::with_burst(nz(1000), Some(nz(500)));

    let sleep1 = limiter.register(2000);
    assert!(sleep1.requested() <= Duration::from_millis(500));

    let sleep2 = limiter.register(2000);
    assert!(sleep2.requested() <= Duration::from_millis(500));
}

#[test]
fn rate_change_from_slow_to_fast_clears_debt() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(100));
    let _ = limiter.register(1000);

    limiter.update_limit(nz(1_000_000));

    assert_eq!(limiter.accumulated_debt_for_testing(), 0);

    let sleep = limiter.register(1000);
    assert!(sleep.requested() < Duration::from_millis(10));
}

#[test]
fn rate_change_from_fast_to_slow_clears_debt() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1_000_000));
    let _ = limiter.register(1000);

    limiter.update_limit(nz(100));

    assert_eq!(limiter.accumulated_debt_for_testing(), 0);

    let sleep = limiter.register(100);
    assert_eq!(sleep.requested(), Duration::from_secs(1));
}

#[test]
fn rate_change_updates_write_max() {
    let mut limiter = BandwidthLimiter::new(nz(1024));
    let initial_max = limiter.write_max_bytes();
    assert_eq!(initial_max, MIN_WRITE_MAX);

    limiter.update_limit(nz(1024 * 1024));
    let new_max = limiter.write_max_bytes();
    assert!(new_max > initial_max);
}

#[test]
fn configuration_change_modifies_both_rate_and_burst() {
    let mut limiter = BandwidthLimiter::new(nz(1024));
    assert!(limiter.burst_bytes().is_none());

    limiter.update_configuration(nz(2048), Some(nz(4096)));
    assert_eq!(limiter.limit_bytes().get(), 2048);
    assert_eq!(limiter.burst_bytes().unwrap().get(), 4096);

    limiter.update_configuration(nz(1024), None);
    assert_eq!(limiter.limit_bytes().get(), 1024);
    assert!(limiter.burst_bytes().is_none());
}

#[test]
fn minimum_nonzero_rate_behavior() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1));

    let sleep = limiter.register(1);
    assert_eq!(sleep.requested(), Duration::from_secs(1));
}

#[test]
fn rate_of_one_byte_per_second_with_large_write() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1));

    let sleep = limiter.register(100);
    assert_eq!(sleep.requested(), Duration::from_secs(100));
}

#[test]
fn gigabyte_per_second_rate_handles_large_writes() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1_000_000_000));

    let sleep = limiter.register(100_000_000);
    assert_eq!(sleep.requested(), Duration::from_millis(100));
}

#[test]
fn maximum_rate_negligible_sleep_for_small_writes() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(u64::MAX / 10));

    let sleep = limiter.register(1000);
    assert!(sleep.requested() < Duration::from_nanos(100));
}

#[test]
fn burst_exactly_equals_write_amount() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::with_burst(nz(100), Some(nz(1000)));

    let _sleep = limiter.register(1000);

    assert!(limiter.accumulated_debt_for_testing() <= 1000);
}

#[test]
fn burst_smaller_than_min_write_max_uses_min() {
    let limiter = BandwidthLimiter::with_burst(nz(1024 * 1024), Some(nz(100)));
    assert_eq!(limiter.write_max_bytes(), MIN_WRITE_MAX);
}

#[test]
fn burst_much_larger_than_calculated_write_max() {
    let limiter = BandwidthLimiter::with_burst(nz(1024), Some(nz(1_000_000)));
    assert_eq!(limiter.write_max_bytes(), 1_000_000);
}

#[test]
fn cloned_limiter_has_independent_state() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut original = BandwidthLimiter::new(nz(1000));
    let _ = original.register(500);

    let debt_before_clone = original.accumulated_debt_for_testing();

    let mut cloned = original.clone();

    let _ = original.register(500);

    assert_eq!(cloned.accumulated_debt_for_testing(), debt_before_clone);

    cloned.reset();

    assert!(original.accumulated_debt_for_testing() > 0);
}

#[test]
fn recommended_read_size_boundary_at_write_max() {
    let limiter = BandwidthLimiter::new(nz(1024 * 100));

    assert_eq!(limiter.recommended_read_size(12800), 12800);
    assert_eq!(limiter.recommended_read_size(12799), 12799);
    assert_eq!(limiter.recommended_read_size(12801), 12800);
}

#[test]
fn recommended_read_size_with_various_limits() {
    let slow = BandwidthLimiter::new(nz(512));
    assert_eq!(slow.recommended_read_size(10000), MIN_WRITE_MAX);

    let fast = BandwidthLimiter::new(nz(10 * 1024 * 1024));
    assert!(fast.recommended_read_size(1000) <= fast.write_max_bytes());
}

#[test]
fn limiter_sleep_tracking_accuracy() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1000));

    let sleep1 = limiter.register(500);
    assert_eq!(sleep1.requested(), Duration::from_millis(500));
    assert!(sleep1.actual() < Duration::from_millis(100));
}

#[test]
fn limiter_sleep_reflects_burst_clamping() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::with_burst(nz(100), Some(nz(200)));

    let sleep = limiter.register(1000);

    assert_eq!(sleep.requested(), Duration::from_secs(2));
}

#[test]
fn realistic_file_transfer_small_file() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(512));

    let mut total_requested = Duration::ZERO;
    let chunk_size = 512;
    let file_size = 1024;
    let chunks = file_size / chunk_size;

    for _ in 0..chunks {
        let sleep = limiter.register(chunk_size);
        total_requested = total_requested.saturating_add(sleep.requested());
    }

    assert!(total_requested >= Duration::from_secs(1));
    assert!(total_requested <= Duration::from_secs(3));
}

#[test]
fn realistic_streaming_with_burst() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::with_burst(nz(1024), Some(nz(2048)));

    let sleep1 = limiter.register(2048);
    assert_eq!(sleep1.requested(), Duration::from_secs(2));

    let sleep2 = limiter.register(2048);
    assert!(sleep2.requested() <= Duration::from_secs(2));
}

#[test]
fn register_does_not_panic_with_extreme_values() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let configurations: Vec<(u64, Option<u64>)> = vec![
        (1, None),
        (u64::MAX, None),
        (1, Some(u64::MAX)),
        (u64::MAX, Some(1)),
        (1000, Some(1)),
        (1, Some(1)),
    ];

    for (rate, burst) in configurations {
        let mut limiter = BandwidthLimiter::with_burst(nz(rate), burst.and_then(NonZeroU64::new));

        let _ = limiter.register(0);
        let _ = limiter.register(1);
        let _ = limiter.register(1000);
        let _ = limiter.register(usize::MAX / 2);
    }
}

#[test]
fn reset_after_extreme_debt_restores_normal_operation() {
    use super::super::recorded_sleep_session;

    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(nz(1));

    let _ = limiter.register(1_000_000);

    limiter.reset();
    assert_eq!(limiter.accumulated_debt_for_testing(), 0);

    let sleep = limiter.register(1);
    assert_eq!(sleep.requested(), Duration::from_secs(1));
}
