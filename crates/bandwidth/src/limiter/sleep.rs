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

pub(crate) fn duration_from_microseconds(us: u128) -> Duration {
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
        let debug = format!("{:?}", sleep);
        assert!(debug.contains("LimiterSleep"));
    }

    #[test]
    fn duration_from_microseconds_zero_returns_zero() {
        assert_eq!(duration_from_microseconds(0), Duration::ZERO);
    }

    #[test]
    fn duration_from_microseconds_converts_correctly() {
        assert_eq!(duration_from_microseconds(1_000_000), Duration::from_secs(1));
        assert_eq!(duration_from_microseconds(500_000), Duration::from_millis(500));
        assert_eq!(duration_from_microseconds(1_500_000), Duration::from_millis(1500));
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
}
