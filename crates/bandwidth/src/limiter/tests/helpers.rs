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
