use std::num::NonZeroU64;
use std::time::{Duration, Instant};

#[cfg(any(test, feature = "test-support"))]
use std::mem;

#[cfg(any(test, feature = "test-support"))]
use std::sync::{Mutex, MutexGuard, OnceLock};

const MICROS_PER_SECOND: u128 = 1_000_000;
const MINIMUM_SLEEP_MICROS: u128 = MICROS_PER_SECOND / 10;

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

    let seconds = us / MICROS_PER_SECOND;
    let micros = (us % MICROS_PER_SECOND) as u32;

    if seconds >= u128::from(u64::MAX) {
        Duration::MAX
    } else {
        Duration::new(seconds as u64, micros.saturating_mul(1_000))
    }
}

fn sleep_for(duration: Duration) {
    if duration.is_zero() {
        return;
    }

    #[cfg(any(test, feature = "test-support"))]
    {
        recorded_sleeps()
            .lock()
            .expect("lock recorded sleeps")
            .push(duration);
    }

    #[cfg(not(any(test, feature = "test-support")))]
    {
        std::thread::sleep(duration);
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
mod tests {
    use super::{
        BandwidthLimiter, MINIMUM_SLEEP_MICROS, apply_effective_limit, duration_from_microseconds,
        recorded_sleep_session, sleep_for,
    };
    use std::num::NonZeroU64;
    use std::time::Duration;

    #[test]
    fn limiter_limits_chunk_size_for_slow_rates() {
        let limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
        assert_eq!(limiter.recommended_read_size(8192), 512);
        assert_eq!(limiter.recommended_read_size(256), 256);
    }

    #[test]
    fn limiter_supports_sub_kib_per_second_limits() {
        let limiter = BandwidthLimiter::new(NonZeroU64::new(600).unwrap());
        assert_eq!(limiter.recommended_read_size(8192), 512);
        assert_eq!(limiter.recommended_read_size(256), 256);
    }

    #[test]
    fn limiter_preserves_buffer_for_fast_rates() {
        let limiter = BandwidthLimiter::new(NonZeroU64::new(8 * 1024 * 1024).unwrap());
        assert_eq!(limiter.recommended_read_size(8192), 8192);
    }

    #[test]
    fn limiter_respects_custom_burst() {
        let limiter = BandwidthLimiter::with_burst(
            NonZeroU64::new(8 * 1024 * 1024).unwrap(),
            NonZeroU64::new(2048),
        );
        assert_eq!(limiter.recommended_read_size(8192), 2048);
    }

    #[test]
    fn limiter_records_sleep_for_large_writes() {
        let mut session = recorded_sleep_session();
        session.clear();
        let mut limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
        limiter.register(4096);
        let recorded = session.take();
        assert!(
            recorded
                .iter()
                .any(|duration| duration >= &Duration::from_micros(MINIMUM_SLEEP_MICROS as u64))
        );
    }

    #[test]
    fn limiter_records_precise_sleep_for_single_second() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
        limiter.register(1024);

        let recorded = session.take();
        assert_eq!(recorded, [Duration::from_secs(1)]);
    }

    #[test]
    fn limiter_clamps_debt_to_configured_burst() {
        let mut session = recorded_sleep_session();
        session.clear();

        let burst = NonZeroU64::new(4096).expect("non-zero burst");
        let mut limiter = BandwidthLimiter::with_burst(
            NonZeroU64::new(8 * 1024 * 1024).expect("non-zero limit"),
            Some(burst),
        );

        limiter.register(1 << 20);

        assert!(
            limiter.accumulated_debt_for_testing() <= u128::from(burst.get()),
            "debt exceeds configured burst"
        );
    }

    #[test]
    fn recorded_sleep_session_into_vec_consumes_guard() {
        let mut session = recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
        limiter.register(2048);

        let recorded = session.into_vec();
        assert!(!recorded.is_empty());

        let mut follow_up = recorded_sleep_session();
        assert!(follow_up.is_empty());
        let _ = follow_up.take();
    }

    #[test]
    fn limiter_update_limit_resets_internal_state() {
        let mut session = recorded_sleep_session();
        session.clear();

        let new_limit = NonZeroU64::new(8 * 1024 * 1024).unwrap();
        let mut baseline = BandwidthLimiter::new(new_limit);
        baseline.register(4096);
        let baseline_sleeps = session.take();

        session.clear();

        let mut limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
        limiter.register(4096);
        session.clear();

        limiter.update_limit(new_limit);
        limiter.register(4096);
        assert_eq!(limiter.limit_bytes(), new_limit);
        assert_eq!(limiter.recommended_read_size(1 << 20), 1 << 20);

        let updated_sleeps = session.take();
        assert_eq!(updated_sleeps, baseline_sleeps);
    }

    #[test]
    fn limiter_update_configuration_resets_state_and_updates_burst() {
        let mut session = recorded_sleep_session();
        session.clear();

        let initial_limit = NonZeroU64::new(1024).unwrap();
        let initial_burst = NonZeroU64::new(4096).unwrap();
        let mut limiter = BandwidthLimiter::with_burst(initial_limit, Some(initial_burst));
        limiter.register(8192);
        assert!(limiter.accumulated_debt_for_testing() > 0);

        let new_limit = NonZeroU64::new(8 * 1024 * 1024).unwrap();
        let new_burst = NonZeroU64::new(2048).unwrap();
        limiter.update_configuration(new_limit, Some(new_burst));

        assert_eq!(limiter.limit_bytes(), new_limit);
        assert_eq!(limiter.burst_bytes(), Some(new_burst));
        assert_eq!(limiter.accumulated_debt_for_testing(), 0);

        session.clear();
        limiter.register(1024);
        let recorded = session.take();
        assert!(
            recorded.is_empty()
                || recorded
                    .iter()
                    .all(|duration| duration.as_micros() <= MINIMUM_SLEEP_MICROS)
        );
    }

    #[test]
    fn limiter_reset_clears_state_and_preserves_configuration() {
        let mut session = recorded_sleep_session();
        session.clear();

        let limit = NonZeroU64::new(1024).unwrap();
        let mut baseline = BandwidthLimiter::new(limit);
        baseline.register(4096);
        let baseline_sleeps = session.take();

        session.clear();

        let mut limiter = BandwidthLimiter::new(limit);
        limiter.register(4096);
        assert!(limiter.accumulated_debt_for_testing() > 0);

        session.clear();

        limiter.reset();
        assert_eq!(limiter.limit_bytes(), limit);
        assert_eq!(limiter.burst_bytes(), None);
        assert_eq!(limiter.accumulated_debt_for_testing(), 0);

        limiter.register(4096);
        let reset_sleeps = session.take();
        assert_eq!(reset_sleeps, baseline_sleeps);
    }

    #[test]
    fn apply_effective_limit_disables_limiter_when_unrestricted() {
        let mut limiter = Some(BandwidthLimiter::new(NonZeroU64::new(1024).unwrap()));

        apply_effective_limit(&mut limiter, None, true, None, false);

        assert!(limiter.is_none());
    }

    #[test]
    fn apply_effective_limit_caps_existing_limit() {
        let mut limiter = Some(BandwidthLimiter::new(
            NonZeroU64::new(8 * 1024 * 1024).unwrap(),
        ));
        let cap = NonZeroU64::new(1024 * 1024).unwrap();

        apply_effective_limit(&mut limiter, Some(cap), true, None, false);

        let limiter = limiter.expect("limiter should remain active");
        assert_eq!(limiter.limit_bytes(), cap);
    }

    #[test]
    fn apply_effective_limit_initialises_limiter_when_absent() {
        let mut limiter = None;
        let cap = NonZeroU64::new(4 * 1024 * 1024).unwrap();

        apply_effective_limit(&mut limiter, Some(cap), true, None, false);

        let limiter = limiter.expect("limiter should be created");
        assert_eq!(limiter.limit_bytes(), cap);
    }

    #[test]
    fn apply_effective_limit_updates_burst_when_specified() {
        let limit = NonZeroU64::new(4 * 1024 * 1024).unwrap();
        let mut limiter = Some(BandwidthLimiter::new(limit));
        let burst = NonZeroU64::new(2048).unwrap();

        apply_effective_limit(&mut limiter, Some(limit), true, Some(burst), true);

        let limiter = limiter.expect("limiter should remain active");
        assert_eq!(limiter.limit_bytes(), limit);
        assert_eq!(limiter.burst_bytes(), Some(burst));
    }

    #[test]
    fn apply_effective_limit_updates_burst_only_when_explicit() {
        let burst = NonZeroU64::new(1024).unwrap();
        let mut limiter = Some(BandwidthLimiter::with_burst(
            NonZeroU64::new(2 * 1024 * 1024).unwrap(),
            Some(burst),
        ));

        let current_limit = NonZeroU64::new(2 * 1024 * 1024).unwrap();

        // Reaffirming the existing limit without marking a burst override keeps the original burst.
        apply_effective_limit(&mut limiter, Some(current_limit), true, None, false);
        assert_eq!(
            limiter
                .as_ref()
                .expect("limiter should remain active")
                .burst_bytes(),
            Some(burst)
        );

        // Explicit overrides update the burst even when the rate remains unchanged.
        let new_burst = NonZeroU64::new(4096).unwrap();
        apply_effective_limit(
            &mut limiter,
            Some(current_limit),
            true,
            Some(new_burst),
            true,
        );
        assert_eq!(
            limiter
                .as_ref()
                .expect("limiter should remain active")
                .burst_bytes(),
            Some(new_burst)
        );

        // Burst-only overrides honour the existing limiter but leave absent limiters untouched.
        apply_effective_limit(&mut limiter, None, false, Some(burst), true);
        assert_eq!(
            limiter
                .as_ref()
                .expect("limiter should remain active")
                .burst_bytes(),
            Some(burst)
        );

        let mut absent: Option<BandwidthLimiter> = None;
        apply_effective_limit(&mut absent, None, false, Some(new_burst), true);
        assert!(absent.is_none());
    }

    #[test]
    fn apply_effective_limit_ignores_unspecified_burst_override() {
        let burst = NonZeroU64::new(4096).unwrap();
        let limit = NonZeroU64::new(4 * 1024 * 1024).unwrap();
        let mut limiter = Some(BandwidthLimiter::with_burst(limit, Some(burst)));

        let replacement_burst = NonZeroU64::new(1024).unwrap();
        apply_effective_limit(
            &mut limiter,
            Some(limit),
            true,
            Some(replacement_burst),
            false,
        );

        assert_eq!(
            limiter
                .as_ref()
                .expect("limiter should remain active")
                .burst_bytes(),
            Some(burst)
        );
    }

    #[test]
    fn apply_effective_limit_ignores_unspecified_burst_when_creating_limiter() {
        let limit = NonZeroU64::new(3 * 1024 * 1024).unwrap();
        let mut limiter = None;
        let replacement_burst = NonZeroU64::new(2048).unwrap();

        apply_effective_limit(
            &mut limiter,
            Some(limit),
            true,
            Some(replacement_burst),
            false,
        );

        let limiter = limiter.expect("limiter should be created");
        assert_eq!(limiter.limit_bytes(), limit);
        assert!(limiter.burst_bytes().is_none());
    }

    #[test]
    fn duration_from_microseconds_returns_zero_for_zero_input() {
        assert_eq!(duration_from_microseconds(0), Duration::ZERO);
    }

    #[test]
    fn duration_from_microseconds_converts_fractional_seconds() {
        let micros = super::MICROS_PER_SECOND + 123;
        let duration = duration_from_microseconds(micros);
        assert_eq!(duration.as_secs(), 1);
        assert_eq!(duration.subsec_nanos(), 123_000);
    }

    #[test]
    fn duration_from_microseconds_saturates_to_duration_max() {
        let micros = u128::from(u64::MAX)
            .saturating_mul(super::MICROS_PER_SECOND)
            .saturating_add(1);
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
}
