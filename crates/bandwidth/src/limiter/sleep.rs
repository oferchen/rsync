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
