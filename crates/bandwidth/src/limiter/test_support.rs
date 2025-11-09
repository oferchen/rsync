use std::iter::{ExactSizeIterator, FusedIterator};
use std::marker::PhantomData;
use std::mem;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

pub(crate) fn recorded_sleeps() -> &'static Mutex<Vec<Duration>> {
    static RECORDED_SLEEPS: OnceLock<Mutex<Vec<Duration>>> = OnceLock::new();
    RECORDED_SLEEPS.get_or_init(|| Mutex::new(Vec::new()))
}

fn recorded_sleep_session_lock() -> &'static Mutex<()> {
    static SESSION_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    SESSION_LOCK.get_or_init(|| Mutex::new(()))
}

pub(crate) fn append_recorded_sleeps(chunks: Vec<Duration>) {
    lock_recorded_sleeps().extend(chunks);
}

fn lock_recorded_sleeps() -> MutexGuard<'static, Vec<Duration>> {
    recorded_sleeps()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

fn lock_recorded_sleep_session() -> MutexGuard<'static, ()> {
    recorded_sleep_session_lock()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

#[cfg_attr(docsrs, doc(cfg(feature = "test-support")))]
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

impl<'a> RecordedSleepSession<'a> {
    #[inline]
    fn with_recorded_sleeps<R>(&self, op: impl FnOnce(&[Duration]) -> R) -> R {
        let guard = lock_recorded_sleeps();
        op(guard.as_slice())
    }

    #[inline]
    fn with_recorded_sleeps_mut<R>(&self, op: impl FnOnce(&mut Vec<Duration>) -> R) -> R {
        let mut guard = lock_recorded_sleeps();
        op(guard.as_mut())
    }

    /// Removes any previously recorded durations.
    #[inline]
    pub fn clear(&mut self) {
        self.with_recorded_sleeps_mut(|durations| durations.clear());
    }

    /// Returns `true` when no sleep durations have been recorded.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.with_recorded_sleeps(|durations| durations.is_empty())
    }

    /// Returns the number of recorded sleep intervals.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.with_recorded_sleeps(|durations| durations.len())
    }

    /// Returns a snapshot of the recorded sleep durations without clearing them.
    ///
    /// Unlike [`take`](Self::take), this helper clones the currently buffered
    /// durations so tests can assert on the pacing schedule multiple times while
    /// allowing additional sleeps to accumulate. The method honours the
    /// `test-support` feature gate just like the rest of the guard API and keeps
    /// the shared buffer intact for subsequent assertions.
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(feature = "test-support")]
    /// # {
    /// use bandwidth::{recorded_sleep_session, BandwidthLimiter};
    /// use std::num::NonZeroU64;
    ///
    /// let mut session = recorded_sleep_session();
    /// session.clear();
    ///
    /// let mut limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
    /// let _ = limiter.register(2048);
    ///
    /// let snapshot = session.snapshot();
    /// assert!(!snapshot.is_empty());
    /// assert_eq!(snapshot, session.take());
    /// # }
    /// ```
    #[inline]
    pub fn snapshot(&self) -> Vec<Duration> {
        self.with_recorded_sleeps(|durations| durations.to_vec())
    }

    /// Returns the most recently recorded sleep duration without draining the buffer.
    ///
    /// The helper avoids the allocation performed by [`snapshot`](Self::snapshot) when
    /// callers only need to inspect the last pacing interval. It mirrors upstream
    /// rsync's debugging hooks, which expose the tail of the limiter history to
    /// diagnose pacing regressions without discarding prior measurements. When no
    /// sleeps have been recorded the method returns `None`.
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(feature = "test-support")]
    /// # {
    /// use bandwidth::{recorded_sleep_session, BandwidthLimiter};
    /// use std::num::NonZeroU64;
    ///
    /// let mut session = recorded_sleep_session();
    /// session.clear();
    ///
    /// let mut limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
    /// let _ = limiter.register(2048);
    ///
    /// assert!(session.last_duration().is_some());
    /// assert_eq!(session.last_duration(), session.snapshot().last().copied());
    /// # }
    /// ```
    #[inline]
    #[must_use]
    pub fn last_duration(&self) -> Option<Duration> {
        self.with_recorded_sleeps(|durations| durations.last().copied())
    }

    /// Returns the cumulative duration of all recorded sleeps without consuming them.
    ///
    /// The helper allows callers to assert on the pacing budget enforced by the limiter
    /// while keeping the underlying samples available for further inspection. Durations
    /// are summed using saturation to mirror the behaviour of [`Duration::saturating_add`]
    /// and avoid panicking when large totals are accumulated. Calling the method multiple
    /// times returns the same value until the buffer is cleared or drained.
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(feature = "test-support")]
    /// # {
    /// # use std::time::Duration;
    /// use bandwidth::{recorded_sleep_session, BandwidthLimiter};
    /// use std::num::NonZeroU64;
    ///
    /// let mut session = recorded_sleep_session();
    /// session.clear();
    ///
    /// let mut limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
    /// let _ = limiter.register(2048);
    ///
    /// assert_eq!(session.total_duration(), Duration::from_secs(2));
    /// assert_eq!(session.total_duration(), Duration::from_secs(2));
    /// assert_eq!(session.take(), [Duration::from_secs(2)]);
    /// # }
    /// ```
    #[inline]
    #[must_use]
    pub fn total_duration(&self) -> Duration {
        self.with_recorded_sleeps(|durations| {
            durations
                .iter()
                .copied()
                .fold(Duration::ZERO, |acc, chunk| acc.saturating_add(chunk))
        })
    }

    /// Drains the recorded sleep durations, returning ownership of the vector.
    #[inline]
    pub fn take(&mut self) -> Vec<Duration> {
        self.with_recorded_sleeps_mut(mem::take)
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

    /// Iterates over the recorded sleep durations without consuming them.
    ///
    /// The iterator owns a lock on the shared buffer for the duration of the
    /// traversal, guaranteeing exclusive access while avoiding additional
    /// allocations. Unlike [`take`](Self::take), `iter` keeps the recorded
    /// samples intact so callers can perform multiple passes or follow-up
    /// assertions after collecting the yielded durations. The iterator
    /// implements [`DoubleEndedIterator`](std::iter::DoubleEndedIterator),
    /// allowing callers to inspect the most recent sleeps first by calling
    /// [`rev`](std::iter::Iterator::rev) without draining the underlying
    /// storage.
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(feature = "test-support")]
    /// # {
    /// use bandwidth::{recorded_sleep_session, BandwidthLimiter};
    /// use std::num::NonZeroU64;
    ///
    /// let mut session = recorded_sleep_session();
    /// session.clear();
    ///
    /// let mut limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
    /// let _ = limiter.register(2048);
    ///
    /// let durations: Vec<_> = session.iter().collect();
    /// assert_eq!(durations, session.take());
    /// # }
    /// ```
    #[inline]
    pub fn iter(&self) -> RecordedSleepIter<'_> {
        let guard = lock_recorded_sleeps();
        let end = guard.len();

        RecordedSleepIter {
            guard,
            index: 0,
            end,
            _session: PhantomData,
        }
    }
}

/// Converts a [`RecordedSleepSession`] into an iterator over the captured durations.
///
/// The iterator owns the collected sleep intervals, allowing tests to consume the
/// samples using idiomatic iterator combinators while releasing the underlying
/// mutex as soon as the guard is moved. The helper mirrors [`into_vec`](RecordedSleepSession::into_vec)
/// but avoids an intermediate allocation when callers immediately iterate over the
/// returned durations.
///
/// # Examples
///
/// ```
/// # #[cfg(feature = "test-support")]
/// # {
/// use bandwidth::{recorded_sleep_session, BandwidthLimiter};
/// use std::num::NonZeroU64;
///
/// let mut limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
/// let mut session = recorded_sleep_session();
/// session.clear();
/// let _ = limiter.register(1024);
///
/// let durations: Vec<_> = session.into_iter().collect();
/// assert_eq!(durations.len(), 1);
/// # }
/// ```
impl<'a> IntoIterator for RecordedSleepSession<'a> {
    type Item = Duration;
    type IntoIter = std::vec::IntoIter<Duration>;

    fn into_iter(mut self) -> Self::IntoIter {
        self.take().into_iter()
    }
}

impl<'session, 'iter> IntoIterator for &'iter RecordedSleepSession<'session>
where
    'session: 'iter,
{
    type Item = Duration;
    type IntoIter = RecordedSleepIter<'iter>;

    /// Iterates over the recorded durations without consuming the guard.
    ///
    /// The implementation forwards to [`RecordedSleepSession::iter`], keeping
    /// the shared buffer intact so callers can perform multiple passes while
    /// still holding the session lock.
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'session, 'iter> IntoIterator for &'iter mut RecordedSleepSession<'session>
where
    'session: 'iter,
{
    type Item = Duration;
    type IntoIter = RecordedSleepIter<'iter>;

    /// Iterates over the recorded durations without consuming the guard.
    ///
    /// The implementation forwards to [`RecordedSleepSession::iter`], keeping
    /// the shared buffer intact so callers can perform multiple passes while
    /// still holding the session lock.
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg_attr(docsrs, doc(cfg(feature = "test-support")))]
/// Iterator over the durations captured by a [`RecordedSleepSession`].
///
/// The iterator keeps the underlying buffer intact, allowing tests to inspect
/// the pacing schedule multiple times while retaining exclusive access to the
/// shared storage guarded by [`recorded_sleep_session`]. Instances are created
/// via [`RecordedSleepSession::iter`]. It also implements
/// [`DoubleEndedIterator`](std::iter::DoubleEndedIterator) so callers can walk
/// the recorded sleeps in reverse order without draining them.
pub struct RecordedSleepIter<'a> {
    guard: MutexGuard<'static, Vec<Duration>>,
    index: usize,
    end: usize,
    _session: PhantomData<&'a RecordedSleepSession<'a>>,
}

impl Iterator for RecordedSleepIter<'_> {
    type Item = Duration;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.end {
            return None;
        }

        let item = self.guard.get(self.index).copied();
        if item.is_some() {
            self.index += 1;
        }
        item
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.end.saturating_sub(self.index);
        (remaining, Some(remaining))
    }
}

impl DoubleEndedIterator for RecordedSleepIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.index >= self.end {
            return None;
        }

        self.end = self.end.saturating_sub(1);
        self.guard.get(self.end).copied()
    }
}

impl ExactSizeIterator for RecordedSleepIter<'_> {
    fn len(&self) -> usize {
        self.end.saturating_sub(self.index)
    }
}

impl FusedIterator for RecordedSleepIter<'_> {}

#[cfg_attr(docsrs, doc(cfg(feature = "test-support")))]
/// Obtains a guard that serialises access to recorded sleep durations.
///
/// Callers can invoke this helper directly or rely on the [`Default`]
/// implementation for [`RecordedSleepSession`] when constructing helper structs.
#[must_use]
pub fn recorded_sleep_session() -> RecordedSleepSession<'static> {
    RecordedSleepSession {
        _guard: lock_recorded_sleep_session(),
    }
}

#[cfg_attr(docsrs, doc(cfg(feature = "test-support")))]
impl Default for RecordedSleepSession<'static> {
    /// Returns a [`RecordedSleepSession`] guard that serialises access to the
    /// shared sleep buffer.
    ///
    /// The implementation forwards to [`recorded_sleep_session()`], ensuring
    /// idiomatic construction via [`Default::default`]. Holding the returned
    /// guard provides exclusive access to the recorded durations until it is
    /// dropped.
    fn default() -> Self {
        recorded_sleep_session()
    }
}
