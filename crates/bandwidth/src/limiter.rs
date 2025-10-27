use std::num::NonZeroU64;
use std::time::{Duration, Instant};

#[cfg(any(test, feature = "test-support"))]
use std::mem;

#[cfg(any(test, feature = "test-support"))]
use std::sync::{Mutex, MutexGuard, OnceLock};

const MICROS_PER_SECOND: u128 = 1_000_000;
const MINIMUM_SLEEP_MICROS: u128 = MICROS_PER_SECOND / 10;
const MAX_REPRESENTABLE_MICROSECONDS: u128 =
    (u64::MAX as u128) * MICROS_PER_SECOND + (MICROS_PER_SECOND - 1);
/// Maximum duration supported by [`std::thread::sleep`] without panicking on the current platform.
const MAX_SLEEP_DURATION: Duration = Duration::new(i64::MAX as u64, 999_999_999);

#[cfg(any(test, feature = "test-support"))]
fn recorded_sleeps() -> &'static Mutex<Vec<Duration>> {
    static RECORDED_SLEEPS: OnceLock<Mutex<Vec<Duration>>> = OnceLock::new();
    RECORDED_SLEEPS.get_or_init(|| Mutex::new(Vec::new()))
}

#[cfg(any(test, feature = "test-support"))]
fn recorded_sleep_session_lock() -> &'static Mutex<()> {
    static SESSION_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    SESSION_LOCK.get_or_init(|| Mutex::new(()))
}

#[cfg(any(test, feature = "test-support"))]
/// Guard that provides exclusive access to the recorded sleep durations.
///
/// Tests obtain a [`RecordedSleepSession`] at the start of a scenario, call
/// [`RecordedSleepSession::clear`] to discard previous measurements, execute the
/// code under test, and finally inspect the captured durations via
/// [`RecordedSleepSession::take`]. Holding the guard ensures concurrent tests do
/// not drain or append to the shared buffer while assertions run, eliminating
/// the data races observed when multiple tests exercised the limiter in
/// parallel.
pub struct RecordedSleepSession<'a> {
    _guard: MutexGuard<'a, ()>,
}

#[cfg(any(test, feature = "test-support"))]
impl<'a> RecordedSleepSession<'a> {
    /// Removes any previously recorded durations.
    #[inline]
    pub fn clear(&mut self) {
        recorded_sleeps()
            .lock()
            .expect("lock recorded sleeps")
            .clear();
    }

    /// Returns `true` when no sleep durations have been recorded.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        recorded_sleeps()
            .lock()
            .expect("lock recorded sleeps")
            .is_empty()
    }

    /// Returns the number of recorded sleep intervals.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        recorded_sleeps()
            .lock()
            .expect("lock recorded sleeps")
            .len()
    }

    /// Drains the recorded sleep durations, returning ownership of the vector.
    #[inline]
    pub fn take(&mut self) -> Vec<Duration> {
        let mut guard = recorded_sleeps().lock().expect("lock recorded sleeps");
        mem::take(&mut *guard)
    }

    /// Consumes the session and returns the recorded durations.
    ///
    /// This convenience helper mirrors [`take`](Self::take) while allowing
    /// callers to move the guard by value. It is particularly useful in tests
    /// that wish to collect the recorded sleeps without keeping the session
    /// borrowed mutably for the remainder of the scope.
    #[inline]
    pub fn into_vec(mut self) -> Vec<Duration> {
        self.take()
    }
}

#[cfg(any(test, feature = "test-support"))]
/// Obtains a guard that serialises access to recorded sleep durations.
#[must_use]
pub fn recorded_sleep_session() -> RecordedSleepSession<'static> {
    RecordedSleepSession {
        _guard: recorded_sleep_session_lock()
            .lock()
            .expect("lock recorded sleep session"),
    }
}

fn duration_from_microseconds(us: u128) -> Duration {
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

fn sleep_for(duration: Duration) {
    if duration.is_zero() {
        return;
    }

    let effective = duration.min(MAX_SLEEP_DURATION);

    #[cfg(any(test, feature = "test-support"))]
    {
        recorded_sleeps()
            .lock()
            .expect("lock recorded sleeps")
            .push(effective);
    }

    #[cfg(not(any(test, feature = "test-support")))]
    {
        std::thread::sleep(effective);
    }
}

/// Returns the maximum chunk size used when throttling a stream.
///
/// Upstream rsync sizes write batches proportionally to the configured
/// byte-per-second limit (128 KiB per KiB/s) while respecting the optional
/// burst override supplied by daemon modules. The helper mirrors that logic so
/// all limiter constructors share a single source of truth.
fn calculate_write_max(limit: NonZeroU64, burst: Option<NonZeroU64>) -> usize {
    let kib = if limit.get() < 1024 {
        1
    } else {
        limit.get() / 1024
    };

    let base_write_max = u128::from(kib).saturating_mul(128).max(512);
    let mut write_max = base_write_max.min(usize::MAX as u128) as usize;

    if let Some(burst) = burst {
        let burst = burst.get().min(usize::MAX as u64);
        write_max = usize::try_from(burst).unwrap_or(usize::MAX).max(1);
    }

    write_max
}

/// Applies a module-specific bandwidth cap to an optional limiter, mirroring upstream rsync's
/// precedence rules.
///
/// When a daemon module defines `bwlimit`, rsync enforces the strictest byte-per-second rate while
/// allowing the module to override the configured burst size. Centralising the precedence logic
/// keeps higher layers from duplicating the combination rules and ensures daemon and client
/// behaviour stays in sync.
pub fn apply_effective_limit(
    limiter: &mut Option<BandwidthLimiter>,
    limit: Option<NonZeroU64>,
    limit_specified: bool,
    burst: Option<NonZeroU64>,
    burst_specified: bool,
) {
    if !limit_specified && !burst_specified {
        return;
    }

    if limit_specified {
        match limit {
            Some(limit) => match limiter {
                Some(existing) => {
                    let target_limit = existing.limit_bytes().min(limit);
                    let current_burst = existing.burst_bytes();
                    let target_burst = if burst_specified {
                        burst
                    } else {
                        current_burst
                    };

                    let limit_changed = target_limit != existing.limit_bytes();
                    let burst_changed = target_burst != current_burst;

                    if limit_changed || burst_changed {
                        existing.update_configuration(target_limit, target_burst);
                    }
                }
                None => {
                    let effective_burst = if burst_specified { burst } else { None };
                    *limiter = Some(BandwidthLimiter::with_burst(limit, effective_burst));
                }
            },
            None => {
                *limiter = None;
                return;
            }
        }
    }

    if burst_specified && !limit_specified {
        if let Some(existing) = limiter.as_mut() {
            if existing.burst_bytes() != burst {
                existing.update_configuration(existing.limit_bytes(), burst);
            }
        }
    }
}

/// Token-bucket style limiter that mirrors upstream rsync's pacing rules.
#[doc(alias = "--bwlimit")]
#[derive(Clone, Debug)]
pub struct BandwidthLimiter {
    limit_bytes: NonZeroU64,
    write_max: usize,
    burst_bytes: Option<NonZeroU64>,
    total_written: u128,
    last_instant: Option<Instant>,
    simulated_elapsed_us: u128,
}

impl BandwidthLimiter {
    /// Constructs a new limiter from the supplied byte-per-second rate.
    #[must_use]
    pub fn new(limit: NonZeroU64) -> Self {
        Self::with_burst(limit, None)
    }

    /// Constructs a new limiter from the supplied rate and optional burst size.
    #[must_use]
    pub fn with_burst(limit: NonZeroU64, burst: Option<NonZeroU64>) -> Self {
        let write_max = calculate_write_max(limit, burst);

        Self {
            limit_bytes: limit,
            write_max,
            burst_bytes: burst,
            total_written: 0,
            last_instant: None,
            simulated_elapsed_us: 0,
        }
    }

    /// Updates the limiter so a new byte-per-second limit takes effect.
    ///
    /// Upstream rsync applies daemon-imposed caps by resetting its pacing state
    /// before continuing the transfer with the negotiated limit. Mirroring that
    /// behaviour keeps previously accumulated debt from leaking into the new
    /// configuration and ensures subsequent calls behave as if the limiter had
    /// been freshly constructed with the supplied rate.
    pub fn update_limit(&mut self, limit: NonZeroU64) {
        self.update_configuration(limit, self.burst_bytes);
    }

    /// Updates the limiter so both the rate and burst configuration take effect.
    ///
    /// Upstream rsync resets its token bucket whenever the daemon imposes a new
    /// `--bwlimit=RATE[:BURST]` combination. Reusing that behaviour keeps
    /// previously accumulated debt from leaking into the new configuration and
    /// ensures subsequent calls behave as if the limiter had just been
    /// constructed via [`BandwidthLimiter::with_burst`].
    #[doc(alias = "--bwlimit")]
    pub fn update_configuration(&mut self, limit: NonZeroU64, burst: Option<NonZeroU64>) {
        let write_max = calculate_write_max(limit, burst);

        self.limit_bytes = limit;
        self.write_max = write_max;
        self.burst_bytes = burst;
        self.total_written = 0;
        self.last_instant = None;
        self.simulated_elapsed_us = 0;
    }

    /// Resets the limiter while keeping the current configuration.
    ///
    /// Upstream rsync calls `bwlimit_reset()` when a transfer needs to discard
    /// previously accumulated debt without changing the negotiated
    /// `--bwlimit` values (for example when a daemon session switches from the
    /// greeting phase to file transfers). Clearing the tracked debt and
    /// timestamps mirrors that behaviour so subsequent writes observe the same
    /// pacing shape as a freshly constructed limiter with the existing
    /// parameters.
    pub fn reset(&mut self) {
        self.total_written = 0;
        self.last_instant = None;
        self.simulated_elapsed_us = 0;
    }

    #[inline]
    fn clamp_debt_to_burst(&mut self) {
        if let Some(burst) = self.burst_bytes {
            let limit = u128::from(burst.get());
            self.total_written = self.total_written.min(limit);
        }
    }

    /// Returns the configured limit in bytes per second.
    #[must_use]
    pub const fn limit_bytes(&self) -> NonZeroU64 {
        self.limit_bytes
    }

    /// Returns the configured burst size in bytes, if any.
    #[must_use]
    pub const fn burst_bytes(&self) -> Option<NonZeroU64> {
        self.burst_bytes
    }

    /// Returns the maximum chunk size that should be written before sleeping.
    #[must_use]
    pub fn recommended_read_size(&self, buffer_len: usize) -> usize {
        let limit = self.write_max.max(1);
        buffer_len.min(limit)
    }

    /// Records a completed write and sleeps if the limiter accumulated debt.
    pub fn register(&mut self, bytes: usize) {
        if bytes == 0 {
            return;
        }

        self.total_written = self.total_written.saturating_add(bytes as u128);
        self.clamp_debt_to_burst();

        let start = Instant::now();
        let bytes_per_second = u128::from(self.limit_bytes.get());

        let mut elapsed_us = self.simulated_elapsed_us;
        if let Some(previous) = self.last_instant {
            let elapsed = start.duration_since(previous);
            let measured = elapsed.as_micros().min(u128::from(u64::MAX));
            elapsed_us = elapsed_us.saturating_add(measured);
        }
        self.simulated_elapsed_us = 0;
        if elapsed_us > 0 {
            let allowed = elapsed_us.saturating_mul(bytes_per_second) / MICROS_PER_SECOND;
            if allowed >= self.total_written {
                self.total_written = 0;
            } else {
                self.total_written -= allowed;
            }
        }

        self.clamp_debt_to_burst();

        let sleep_us = self.total_written.saturating_mul(MICROS_PER_SECOND) / bytes_per_second;

        if sleep_us < MINIMUM_SLEEP_MICROS {
            self.last_instant = Some(start);
            return;
        }

        let requested = duration_from_microseconds(sleep_us);
        if !requested.is_zero() {
            sleep_for(requested);
        }

        let end = Instant::now();
        let elapsed_us = end
            .checked_duration_since(start)
            .map(|duration| duration.as_micros().min(u128::from(u64::MAX)))
            .unwrap_or(0);
        if sleep_us > elapsed_us {
            self.simulated_elapsed_us = sleep_us - elapsed_us;
        }
        let remaining_us = sleep_us.saturating_sub(elapsed_us);
        let leftover = remaining_us.saturating_mul(bytes_per_second) / MICROS_PER_SECOND;

        self.total_written = leftover;
        self.clamp_debt_to_burst();
        self.last_instant = Some(end);
    }

    /// Returns the outstanding byte debt accumulated by the limiter.
    ///
    /// The accessor is compiled for tests (and the `test-support` feature) so
    /// scenarios can assert on the internal pacing state without relying on
    /// private fields. Production builds omit the helper entirely.
    #[cfg(any(test, feature = "test-support"))]
    #[allow(dead_code)]
    pub(crate) fn accumulated_debt_for_testing(&self) -> u128 {
        self.total_written
    }
}

#[cfg(test)]
mod tests;
