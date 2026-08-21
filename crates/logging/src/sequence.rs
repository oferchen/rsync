//! The production-order key shared by every diagnostic producer.
//!
//! Upstream never needs one: `rwrite()` writes immediately, so the order
//! messages appear in *is* the order they were produced (upstream:
//! log.c:269 `rwrite()`). oc buffers its messages in two places that are
//! drained at different times, so it has to carry the ordering explicitly.
//! This module is the single source of that key.

use std::sync::atomic::{AtomicU64, Ordering};

/// The process-wide counter behind [`Sequence::stamp`].
///
/// Deliberately a `static AtomicU64` and not a `thread_local!`. A message can
/// be produced on a worker thread, and per-thread counters would hand out the
/// same small integers on every thread, making keys from different threads
/// incomparable - which is the one property this type exists to provide.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// When a message was produced, relative to every other message.
///
/// Ordering by this key reproduces the order upstream gets for free by writing
/// immediately. It carries no timestamp and no thread identity: it answers
/// only "which came first", which is what the funnel needs.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Sequence(u64);

impl Sequence {
    /// Issues the next key.
    ///
    /// Every call returns a distinct value, and values are handed out in
    /// increasing order across all threads.
    ///
    /// `Relaxed` is sufficient and is the deliberate choice: a single atomic's
    /// modification order is agreed by all threads, so `fetch_add` alone
    /// guarantees both uniqueness and a total order over the values. No
    /// happens-before relation with the message payload is needed, because the
    /// keys are only compared once the producers have finished and their
    /// buffers are drained - that drain is already ordered by other means.
    #[must_use]
    pub fn stamp() -> Self {
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    /// Returns the raw key.
    ///
    /// Intended for tests and for rendering; comparisons should use the
    /// derived ordering rather than unwrapping first.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::Sequence;
    use std::collections::BTreeSet;
    use std::thread;

    #[test]
    fn stamps_increase_within_a_thread() {
        let first = Sequence::stamp();
        let second = Sequence::stamp();
        assert!(
            first < second,
            "a later stamp must sort after an earlier one: {first:?} vs {second:?}"
        );
    }

    /// The property that a `thread_local!` counter would silently break: keys
    /// issued on different threads must still be distinct and comparable.
    #[test]
    fn stamps_are_unique_across_threads() {
        const THREADS: usize = 8;
        const PER_THREAD: usize = 64;

        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                thread::spawn(|| {
                    (0..PER_THREAD)
                        .map(|_| Sequence::stamp().get())
                        .collect::<Vec<_>>()
                })
            })
            .collect();

        let issued: Vec<u64> = handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("stamping thread panicked"))
            .collect();

        let distinct: BTreeSet<u64> = issued.iter().copied().collect();
        assert_eq!(
            distinct.len(),
            THREADS * PER_THREAD,
            "every stamp must be distinct, got {} for {} calls",
            distinct.len(),
            THREADS * PER_THREAD
        );
    }
}
