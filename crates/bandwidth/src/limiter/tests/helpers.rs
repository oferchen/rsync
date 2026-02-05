use super::{MAX_SLEEP_DURATION, duration_from_microseconds, recorded_sleep_session, sleep_for};
use std::time::Duration;

#[test]
fn duration_from_microseconds_returns_zero_for_zero_input() {
    assert_eq!(duration_from_microseconds(0), Duration::ZERO);
}

#[test]
fn duration_from_microseconds_converts_fractional_seconds() {
    let micros = super::super::MICROS_PER_SECOND + 123;
    let duration = duration_from_microseconds(micros);
    assert_eq!(duration.as_secs(), 1);
    assert_eq!(duration.subsec_nanos(), 123_000);
}

#[test]
fn duration_from_microseconds_handles_u64_max_seconds_with_fraction() {
    let micros = u128::from(u64::MAX)
        .saturating_mul(super::super::MICROS_PER_SECOND)
        .saturating_add(1);
    let duration = duration_from_microseconds(micros);
    assert_eq!(duration.as_secs(), u64::MAX);
    assert_eq!(duration.subsec_micros(), 1);
}

#[test]
fn duration_from_microseconds_saturates_when_exceeding_supported_range() {
    let micros = super::super::MAX_REPRESENTABLE_MICROSECONDS.saturating_add(1);
    assert_eq!(duration_from_microseconds(micros), Duration::MAX);
}

#[test]
fn sleep_for_zero_duration_skips_recording() {
    let mut session = recorded_sleep_session();
    session.clear();

    sleep_for(Duration::ZERO);

    assert!(session.is_empty());
    let _ = session.take();
}

#[test]
fn sleep_for_clamps_to_maximum_duration() {
    let mut session = recorded_sleep_session();
    session.clear();

    // Request a duration that exceeds what std::thread::sleep supports without panicking.
    let requested = Duration::from_secs(u64::MAX);
    sleep_for(requested);

    let recorded = session.take();
    let remainder = requested.saturating_sub(MAX_SLEEP_DURATION);

    if remainder.is_zero() {
        assert_eq!(recorded, [MAX_SLEEP_DURATION]);
    } else {
        assert_eq!(recorded, [MAX_SLEEP_DURATION, remainder]);
    }
}

#[test]
fn sleep_for_splits_large_durations_into_chunks() {
    let mut session = recorded_sleep_session();
    session.clear();

    let requested = Duration::MAX;
    sleep_for(requested);

    let recorded = session.take();
    assert!(!recorded.is_empty());

    let total = recorded
        .iter()
        .copied()
        .try_fold(Duration::ZERO, |acc, chunk| acc.checked_add(chunk))
        .expect("sum fits within Duration::MAX");
    assert_eq!(total, requested);
    assert!(
        recorded
            .iter()
            .all(|chunk| !chunk.is_zero() && *chunk <= MAX_SLEEP_DURATION)
    );
}

// ==================== Additional duration_from_microseconds tests ====================

#[test]
fn duration_from_microseconds_handles_exact_second_boundaries() {
    // Exactly 1 second
    let one_sec = super::super::MICROS_PER_SECOND;
    assert_eq!(duration_from_microseconds(one_sec), Duration::from_secs(1));

    // Exactly 60 seconds
    let one_min = 60 * super::super::MICROS_PER_SECOND;
    assert_eq!(duration_from_microseconds(one_min), Duration::from_secs(60));

    // Exactly 3600 seconds (1 hour)
    let one_hour = 3600 * super::super::MICROS_PER_SECOND;
    assert_eq!(
        duration_from_microseconds(one_hour),
        Duration::from_secs(3600)
    );
}

#[test]
fn duration_from_microseconds_handles_small_values() {
    // 1 microsecond
    assert_eq!(duration_from_microseconds(1), Duration::from_micros(1));

    // 999,999 microseconds (just under 1 second)
    let almost_sec = duration_from_microseconds(999_999);
    assert_eq!(almost_sec.as_secs(), 0);
    assert_eq!(almost_sec.subsec_micros(), 999_999);
}

#[test]
fn duration_from_microseconds_handles_millis_boundary() {
    // Exactly 1 millisecond = 1000 microseconds
    assert_eq!(duration_from_microseconds(1_000), Duration::from_millis(1));

    // 1.5 milliseconds
    let one_and_half = duration_from_microseconds(1_500);
    assert_eq!(one_and_half.as_micros(), 1_500);
}

#[test]
fn duration_from_microseconds_near_max_representable() {
    // Just under MAX_REPRESENTABLE_MICROSECONDS
    let near_max = super::super::MAX_REPRESENTABLE_MICROSECONDS - 1;
    let duration = duration_from_microseconds(near_max);
    assert!(duration < Duration::MAX);
}

// ==================== sleep_for comprehensive tests ====================

#[test]
fn sleep_for_small_durations_record_correctly() {
    let mut session = recorded_sleep_session();
    session.clear();

    // Various small durations
    let durations = [
        Duration::from_micros(1),
        Duration::from_micros(100),
        Duration::from_millis(1),
        Duration::from_millis(10),
        Duration::from_millis(100),
    ];

    for expected in durations {
        session.clear();
        sleep_for(expected);
        let recorded = session.take();

        // Should record the exact duration (single chunk)
        let total: Duration = recorded.iter().copied().sum();
        assert_eq!(total, expected);
    }
}

#[test]
fn sleep_for_exact_max_sleep_duration() {
    let mut session = recorded_sleep_session();
    session.clear();

    sleep_for(MAX_SLEEP_DURATION);

    let recorded = session.take();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0], MAX_SLEEP_DURATION);
}

#[test]
fn sleep_for_just_over_max_creates_two_chunks() {
    let mut session = recorded_sleep_session();
    session.clear();

    let just_over = MAX_SLEEP_DURATION + Duration::from_micros(1);
    sleep_for(just_over);

    let recorded = session.take();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0], MAX_SLEEP_DURATION);
    assert_eq!(recorded[1], Duration::from_micros(1));
}

#[test]
fn sleep_for_double_max_creates_two_max_chunks() {
    let mut session = recorded_sleep_session();
    session.clear();

    let double = MAX_SLEEP_DURATION.saturating_mul(2);
    sleep_for(double);

    let recorded = session.take();
    let total: Duration = recorded.iter().copied().sum();
    assert_eq!(total, double);

    // All chunks should be at most MAX_SLEEP_DURATION
    assert!(recorded.iter().all(|d| *d <= MAX_SLEEP_DURATION));
}

#[test]
fn sleep_for_many_small_calls() {
    let mut session = recorded_sleep_session();
    session.clear();

    // Many small sleeps
    for _ in 0..100 {
        sleep_for(Duration::from_millis(10));
    }

    let recorded = session.take();
    assert_eq!(recorded.len(), 100);

    let total: Duration = recorded.iter().copied().sum();
    assert_eq!(total, Duration::from_secs(1));
}

#[test]
fn sleep_for_alternating_sizes() {
    let mut session = recorded_sleep_session();
    session.clear();

    // Alternating small and large durations
    sleep_for(Duration::from_millis(1));
    sleep_for(Duration::from_secs(1));
    sleep_for(Duration::from_micros(100));
    sleep_for(Duration::from_secs(2));

    let recorded = session.take();
    let total: Duration = recorded.iter().copied().sum();

    let expected = Duration::from_millis(1)
        + Duration::from_secs(1)
        + Duration::from_micros(100)
        + Duration::from_secs(2);
    assert_eq!(total, expected);
}

#[test]
fn sleep_for_nanoseconds() {
    let mut session = recorded_sleep_session();
    session.clear();

    // Very small duration (nanoseconds)
    sleep_for(Duration::from_nanos(500));

    let recorded = session.take();
    // Should still record (non-zero)
    assert!(!recorded.is_empty());

    let total: Duration = recorded.iter().copied().sum();
    assert_eq!(total, Duration::from_nanos(500));
}

// ==================== Boundary and overflow tests ====================

#[test]
fn duration_from_microseconds_various_overflow_boundaries() {
    // Test values around potential overflow points
    let test_values: Vec<u128> = vec![
        u128::from(u64::MAX),
        u128::from(u64::MAX) + 1,
        u128::from(u64::MAX) * 2,
        super::super::MICROS_PER_SECOND * u128::from(u64::MAX),
    ];

    for micros in test_values {
        let result = duration_from_microseconds(micros);
        // Should not panic, may return Duration::MAX
        assert!(result <= Duration::MAX);
    }
}

#[test]
fn sleep_for_preserves_exact_total_duration() {
    let mut session = recorded_sleep_session();
    session.clear();

    // A specific large duration that will be chunked
    let specific = Duration::new(u64::MAX / 2, 123_456_789);
    sleep_for(specific);

    let recorded = session.take();
    let total = recorded
        .iter()
        .copied()
        .try_fold(Duration::ZERO, |acc, chunk| acc.checked_add(chunk))
        .expect("sum should fit");

    assert_eq!(total, specific);
}
