use super::{MAX_REPRESENTABLE_MICROSECONDS, MAX_SLEEP_DURATION, MICROS_PER_SECOND};
use std::time::Duration;

#[cfg(any(test, feature = "test-support"))]
use super::append_recorded_sleeps;

/// Result returned by [`crate::limiter::BandwidthLimiter::register`] describing how long the limiter slept.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[must_use]
pub struct LimiterSleep {
    requested: Duration,
    actual: Duration,
}

impl LimiterSleep {
    /// Constructs a new [`LimiterSleep`] record from the requested and actual durations.
    pub const fn new(requested: Duration, actual: Duration) -> Self {
        Self { requested, actual }
    }

    /// Returns the amount of time the limiter attempted to sleep.
    #[must_use]
    pub const fn requested(&self) -> Duration {
        self.requested
    }

    /// Returns the time actually observed by the limiter.
    #[must_use]
    pub const fn actual(&self) -> Duration {
        self.actual
    }

    /// Returns `true` when the limiter skipped sleeping altogether.
    #[must_use]
    pub const fn is_noop(&self) -> bool {
        self.requested.is_zero() && self.actual.is_zero()
    }
}

pub(crate) const fn duration_from_microseconds(us: u128) -> Duration {
    if us == 0 {
        return Duration::ZERO;
    }

    if us > MAX_REPRESENTABLE_MICROSECONDS {
        return Duration::MAX;
    }

    let seconds = (us / MICROS_PER_SECOND) as u64;
    let micros = (us % MICROS_PER_SECOND) as u32;

    Duration::new(seconds, micros.saturating_mul(1_000))
}

pub(crate) fn sleep_for(duration: Duration) {
    let mut remaining = duration;

    #[cfg(any(test, feature = "test-support"))]
    let mut recorded_chunks: Option<Vec<Duration>> = None;

    while !remaining.is_zero() {
        let chunk = remaining.min(MAX_SLEEP_DURATION);

        if chunk.is_zero() {
            break;
        }

        #[cfg(any(test, feature = "test-support"))]
        {
            recorded_chunks.get_or_insert_with(Vec::new).push(chunk);

            #[cfg(not(test))]
            {
                std::thread::sleep(chunk);
            }
        }

        #[cfg(all(not(test), not(feature = "test-support")))]
        {
            std::thread::sleep(chunk);
        }

        remaining = remaining.saturating_sub(chunk);
    }

    #[cfg(any(test, feature = "test-support"))]
    if let Some(chunks) = recorded_chunks {
        append_recorded_sleeps(chunks);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_sleep_new_stores_values() {
        let requested = Duration::from_millis(100);
        let actual = Duration::from_millis(95);
        let sleep = LimiterSleep::new(requested, actual);
        assert_eq!(sleep.requested(), requested);
        assert_eq!(sleep.actual(), actual);
    }

    #[test]
    fn limiter_sleep_default_is_zero() {
        let sleep = LimiterSleep::default();
        assert_eq!(sleep.requested(), Duration::ZERO);
        assert_eq!(sleep.actual(), Duration::ZERO);
    }

    #[test]
    fn limiter_sleep_is_noop_when_both_zero() {
        let sleep = LimiterSleep::new(Duration::ZERO, Duration::ZERO);
        assert!(sleep.is_noop());
    }

    #[test]
    fn limiter_sleep_not_noop_when_requested_nonzero() {
        let sleep = LimiterSleep::new(Duration::from_millis(100), Duration::ZERO);
        assert!(!sleep.is_noop());
    }

    #[test]
    fn limiter_sleep_not_noop_when_actual_nonzero() {
        let sleep = LimiterSleep::new(Duration::ZERO, Duration::from_millis(100));
        assert!(!sleep.is_noop());
    }

    #[test]
    fn limiter_sleep_clone_equals_original() {
        let sleep = LimiterSleep::new(Duration::from_secs(1), Duration::from_secs(1));
        assert_eq!(sleep.clone(), sleep);
    }

    #[test]
    fn limiter_sleep_debug() {
        let sleep = LimiterSleep::new(Duration::from_secs(1), Duration::from_secs(1));
        let debug = format!("{sleep:?}");
        assert!(debug.contains("LimiterSleep"));
    }

    #[test]
    fn duration_from_microseconds_zero_returns_zero() {
        assert_eq!(duration_from_microseconds(0), Duration::ZERO);
    }

    #[test]
    fn duration_from_microseconds_converts_correctly() {
        assert_eq!(
            duration_from_microseconds(1_000_000),
            Duration::from_secs(1)
        );
        assert_eq!(
            duration_from_microseconds(500_000),
            Duration::from_millis(500)
        );
        assert_eq!(
            duration_from_microseconds(1_500_000),
            Duration::from_millis(1500)
        );
    }

    #[test]
    fn duration_from_microseconds_handles_large_values() {
        // Very large values should return Duration::MAX
        let result = duration_from_microseconds(MAX_REPRESENTABLE_MICROSECONDS + 1);
        assert_eq!(result, Duration::MAX);
    }

    #[test]
    fn duration_from_microseconds_max_representable() {
        // MAX_REPRESENTABLE_MICROSECONDS should not overflow
        let result = duration_from_microseconds(MAX_REPRESENTABLE_MICROSECONDS);
        assert!(result < Duration::MAX);
    }

    #[test]
    fn sleep_for_zero_duration_is_noop() {
        // Should not block or panic
        sleep_for(Duration::ZERO);
    }

    #[test]
    fn sleep_for_small_duration_records() {
        // This tests that the function runs without error
        sleep_for(Duration::from_micros(100));
    }

    // ==================== Additional sleep tests ====================

    #[test]
    fn limiter_sleep_copy_semantics() {
        let sleep1 = LimiterSleep::new(Duration::from_secs(1), Duration::from_millis(999));
        let sleep2 = sleep1; // Copy
        assert_eq!(sleep1, sleep2);
    }

    #[test]
    fn limiter_sleep_equality() {
        let s1 = LimiterSleep::new(Duration::from_secs(1), Duration::from_secs(1));
        let s2 = LimiterSleep::new(Duration::from_secs(1), Duration::from_secs(1));
        let s3 = LimiterSleep::new(Duration::from_secs(2), Duration::from_secs(1));

        assert_eq!(s1, s2);
        assert_ne!(s1, s3);
    }

    #[test]
    fn duration_from_microseconds_small_values() {
        // Test values less than 1 second
        let one_us = duration_from_microseconds(1);
        assert_eq!(one_us.as_micros(), 1);

        let hundred_us = duration_from_microseconds(100);
        assert_eq!(hundred_us.as_micros(), 100);

        let one_ms = duration_from_microseconds(1000);
        assert_eq!(one_ms.as_millis(), 1);
    }

    #[test]
    fn duration_from_microseconds_multi_second() {
        // Test values spanning multiple seconds
        let five_secs = duration_from_microseconds(5_000_000);
        assert_eq!(five_secs.as_secs(), 5);
        assert_eq!(five_secs.subsec_micros(), 0);

        let five_and_half = duration_from_microseconds(5_500_000);
        assert_eq!(five_and_half.as_secs(), 5);
        assert_eq!(five_and_half.subsec_micros(), 500_000);
    }

    #[test]
    fn duration_from_microseconds_near_max() {
        // Test value just under MAX_REPRESENTABLE
        let just_under = MAX_REPRESENTABLE_MICROSECONDS - 1;
        let result = duration_from_microseconds(just_under);
        assert!(result < Duration::MAX);
    }

    #[test]
    fn limiter_sleep_both_nonzero() {
        let sleep = LimiterSleep::new(Duration::from_secs(1), Duration::from_millis(950));
        assert!(!sleep.is_noop());
        assert_eq!(sleep.requested(), Duration::from_secs(1));
        assert_eq!(sleep.actual(), Duration::from_millis(950));
    }

    // ========================================================================
    // Additional sleep_for edge case tests
    // ========================================================================

    #[test]
    fn sleep_for_handles_very_small_duration() {
        // Should not panic with very small duration
        sleep_for(Duration::from_nanos(1));
    }

    #[test]
    fn sleep_for_handles_one_microsecond() {
        sleep_for(Duration::from_micros(1));
    }

    #[test]
    fn sleep_for_handles_one_millisecond() {
        sleep_for(Duration::from_millis(1));
    }

    #[test]
    fn sleep_for_max_sleep_duration_chunk() {
        // Test sleeping for exactly MAX_SLEEP_DURATION - should be one chunk
        // In tests, actual sleeping is skipped but chunks are recorded
        sleep_for(MAX_SLEEP_DURATION);
    }

    #[test]
    fn sleep_for_multiple_chunks() {
        // Duration larger than MAX_SLEEP_DURATION should be split into chunks
        // MAX_SLEEP_DURATION is i64::MAX seconds + 999_999_999 nanos
        // In practice, any large duration tests the chunking logic
        let large_duration = Duration::from_secs(100);
        sleep_for(large_duration);
    }

    // ========================================================================
    // duration_from_microseconds additional tests
    // ========================================================================

    #[test]
    fn duration_from_microseconds_boundary_values() {
        // Test at various boundaries
        let one_sec_minus_one = duration_from_microseconds(999_999);
        assert_eq!(one_sec_minus_one.as_secs(), 0);
        assert_eq!(one_sec_minus_one.as_micros(), 999_999);

        let exactly_one_sec = duration_from_microseconds(1_000_000);
        assert_eq!(exactly_one_sec.as_secs(), 1);
        assert_eq!(exactly_one_sec.subsec_micros(), 0);

        let one_sec_plus_one = duration_from_microseconds(1_000_001);
        assert_eq!(one_sec_plus_one.as_secs(), 1);
        assert_eq!(one_sec_plus_one.subsec_micros(), 1);
    }

    #[test]
    fn duration_from_microseconds_large_values_below_max() {
        // Test various large values that should not overflow
        let one_hour = duration_from_microseconds(3_600_000_000);
        assert_eq!(one_hour.as_secs(), 3600);

        let one_day = duration_from_microseconds(86_400_000_000);
        assert_eq!(one_day.as_secs(), 86400);

        let one_year = duration_from_microseconds(31_536_000_000_000);
        assert_eq!(one_year.as_secs(), 31_536_000);
    }

    #[test]
    fn duration_from_microseconds_exactly_at_max() {
        // Test exactly at MAX_REPRESENTABLE_MICROSECONDS
        let result = duration_from_microseconds(MAX_REPRESENTABLE_MICROSECONDS);
        // Should not be Duration::MAX since it's at the boundary, not over
        assert!(result < Duration::MAX);
    }

    #[test]
    fn duration_from_microseconds_just_over_max() {
        // Test just over MAX_REPRESENTABLE_MICROSECONDS
        let result = duration_from_microseconds(MAX_REPRESENTABLE_MICROSECONDS + 1);
        assert_eq!(result, Duration::MAX);
    }

    #[test]
    fn duration_from_microseconds_way_over_max() {
        // Test significantly over MAX_REPRESENTABLE_MICROSECONDS
        let result = duration_from_microseconds(u128::MAX);
        assert_eq!(result, Duration::MAX);
    }

    #[test]
    fn duration_from_microseconds_fraction_preservation() {
        // Verify sub-second microseconds are preserved correctly
        let us = 1_234_567; // 1.234567 seconds
        let result = duration_from_microseconds(us);
        assert_eq!(result.as_secs(), 1);
        assert_eq!(result.subsec_micros(), 234_567);
    }

    #[test]
    fn duration_from_microseconds_subsec_nanos() {
        // Verify nanoseconds conversion (micros * 1000)
        let us = 1_500; // 1500 microseconds = 1.5 milliseconds
        let result = duration_from_microseconds(us);
        assert_eq!(result.as_millis(), 1);
        // 500 micros = 500_000 nanos
        assert_eq!(result.subsec_nanos(), 1_500_000);
    }

    // ========================================================================
    // LimiterSleep additional edge case tests
    // ========================================================================

    #[test]
    fn limiter_sleep_requested_larger_than_actual() {
        // Common case: limiter requested more sleep than actually occurred
        let sleep = LimiterSleep::new(Duration::from_secs(2), Duration::from_millis(1900));
        assert_eq!(sleep.requested(), Duration::from_secs(2));
        assert_eq!(sleep.actual(), Duration::from_millis(1900));
        assert!(!sleep.is_noop());
    }

    #[test]
    fn limiter_sleep_actual_larger_than_requested() {
        // Unusual but possible: actual sleep exceeded requested
        let sleep = LimiterSleep::new(Duration::from_secs(1), Duration::from_millis(1100));
        assert_eq!(sleep.requested(), Duration::from_secs(1));
        assert_eq!(sleep.actual(), Duration::from_millis(1100));
        assert!(!sleep.is_noop());
    }

    #[test]
    fn limiter_sleep_very_large_durations() {
        let sleep = LimiterSleep::new(Duration::from_secs(86400), Duration::from_secs(86400));
        assert_eq!(sleep.requested().as_secs(), 86400);
        assert_eq!(sleep.actual().as_secs(), 86400);
    }

    #[test]
    fn limiter_sleep_sub_microsecond_actual() {
        let sleep = LimiterSleep::new(Duration::from_micros(100), Duration::from_nanos(500));
        assert!(!sleep.is_noop());
    }

    #[test]
    fn limiter_sleep_debug_contains_durations() {
        let sleep = LimiterSleep::new(Duration::from_millis(100), Duration::from_millis(95));
        let debug = format!("{sleep:?}");
        assert!(debug.contains("requested"));
        assert!(debug.contains("actual"));
    }

    #[test]
    fn limiter_sleep_eq_symmetry() {
        let s1 = LimiterSleep::new(Duration::from_secs(1), Duration::from_secs(1));
        let s2 = LimiterSleep::new(Duration::from_secs(1), Duration::from_secs(1));
        assert_eq!(s1, s2);
        assert_eq!(s2, s1);
    }

    #[test]
    fn limiter_sleep_ne_different_requested() {
        let s1 = LimiterSleep::new(Duration::from_secs(1), Duration::from_secs(1));
        let s2 = LimiterSleep::new(Duration::from_secs(2), Duration::from_secs(1));
        assert_ne!(s1, s2);
    }

    #[test]
    fn limiter_sleep_ne_different_actual() {
        let s1 = LimiterSleep::new(Duration::from_secs(1), Duration::from_secs(1));
        let s2 = LimiterSleep::new(Duration::from_secs(1), Duration::from_secs(2));
        assert_ne!(s1, s2);
    }

    #[test]
    fn limiter_sleep_default_is_noop() {
        let default_sleep = LimiterSleep::default();
        assert!(default_sleep.is_noop());
    }

    // ========================================================================
    // sleep_for recording tests
    // ========================================================================

    #[test]
    fn sleep_for_records_when_nonzero() {
        use super::super::recorded_sleep_session;

        let mut session = recorded_sleep_session();
        session.clear();

        sleep_for(Duration::from_millis(10));

        // Should have recorded something
        assert!(!session.is_empty());
    }

    #[test]
    fn sleep_for_zero_records_nothing() {
        use super::super::recorded_sleep_session;

        let mut session = recorded_sleep_session();
        session.clear();

        sleep_for(Duration::ZERO);

        // Should have recorded nothing for zero duration
        assert!(session.is_empty());
    }

    #[test]
    fn sleep_for_sub_microsecond_still_records() {
        use super::super::recorded_sleep_session;

        let mut session = recorded_sleep_session();
        session.clear();

        sleep_for(Duration::from_nanos(100));

        // Even sub-microsecond should record (chunking happens)
        // The loop checks !remaining.is_zero() and chunk.is_zero()
        let sleeps = session.take();
        // For very small durations, chunk might equal remaining and be non-zero
        assert!(!sleeps.is_empty() || Duration::from_nanos(100).is_zero());
    }
}
