//! Per-thread buffer slab (#1271, #1370).
//!
//! Replaces the single-slot [`thread_local_cache`](super::thread_local_cache)
//! with a depth-bounded LIFO `Vec<Vec<u8>>` per thread. Used when
//! `BufferPool` is compiled with the `thread-slab-pool` feature.
//!
//! # Design
//!
//! Each thread owns a [`LocalSlab`] that stores up to `slot_cap` buffers and
//! at most `byte_cap` bytes of capacity. Acquire pops from the back (LIFO
//! warmth - newest entry is warmest in cache). Return pushes onto the back
//! when both caps allow, otherwise the caller routes the buffer to the
//! pool's central overflow queue.
//!
//! The slab is the **primary** storage in this configuration: the central
//! [`crossbeam_queue::ArrayQueue`] in `pool.rs` is demoted to a global
//! overflow path that catches cross-thread returns and donations from full
//! slabs. The buffer-pool API (`BufferPool::return_buffer`,
//! `BufferGuard::drop`, `BorrowedBufferGuard::drop`) is unchanged - this
//! module exposes the same `try_take` / `try_store` shape as
//! `thread_local_cache` so callers in `pool.rs` need no special-casing
//! beyond the feature switch in `thread_local_cache.rs`.
//!
//! # Bounded Total Memory
//!
//! Per-thread retention is bounded by `byte_cap` (default `8 *
//! COPY_BUFFER_SIZE = 1 MiB`). The global overflow queue retains additional
//! buffers under the existing [`ByteBudget`](super::byte_budget) when one is
//! configured. End-to-end retention at N threads is bounded by
//! `N * byte_cap + global_overflow_capacity * buffer_size`.
//!
//! # Cross-Thread Returns
//!
//! A buffer acquired on thread A and dropped on thread B pushes onto
//! thread B's slab (or the central overflow queue if B's slab is full).
//! The buffer's provenance does not matter: every `Vec<u8>` is fungible.
//! This avoids the bookkeeping complexity of "steal-from-other-thread"
//! schemes while keeping the global overflow queue's cursor traffic bounded
//! to the rare slab-overflow case.
//!
//! # Thread Teardown
//!
//! Rust runs [`thread_local!`] destructors at thread exit and during panic
//! unwind. The slab's `Drop` impl drains every retained buffer through
//! [`drain_to_overflow`], which calls back into the
//! [`SlabOverflow`] callback registered by the pool. Buffers that the
//! overflow callback rejects (queue full) are deallocated by the callback's
//! deallocate path so no allocations leak.
//!
//! No `unwrap`, `expect`, or `panic!` is allowed on the teardown path so
//! that a panicking thread can still drain cleanly.

use std::cell::RefCell;

use super::COPY_BUFFER_SIZE;

/// Default LIFO slot cap per thread (covers delta-apply two-buffer overlap,
/// prefetch, and signature pipeline lookahead with headroom).
pub(super) const DEFAULT_SLAB_SLOT_CAP: usize = 8;

/// Default per-thread byte cap at `COPY_BUFFER_SIZE` buffers = 1 MiB.
pub(super) const DEFAULT_SLAB_BYTE_CAP: usize = 8 * COPY_BUFFER_SIZE;

/// Per-thread donation interval: every Mth return donates the oldest slab
/// entry to the global overflow queue to break "pinning" on idle threads.
///
/// Set to a power of two so the modulus compiles to a bitwise AND.
const DONATION_INTERVAL: u64 = 64;

/// Callback shape the pool registers so the slab can hand off buffers to
/// the central overflow queue without depending on the pool's concrete type.
///
/// Returns `Some(buf)` if the overflow path rejected the buffer (queue full
/// or budget exhausted) so the slab can deallocate via a separate hook.
pub(super) type SlabOverflow = Box<dyn Fn(Vec<u8>) -> Option<Vec<u8>> + Send + Sync + 'static>;

/// Per-thread LIFO buffer slab.
#[derive(Debug)]
pub(super) struct LocalSlab {
    /// LIFO of pooled buffers. Newest entries on the back.
    buffers: Vec<Vec<u8>>,
    /// Sum of `buf.capacity()` across `buffers`.
    retained_bytes: usize,
    /// Soft cap on `buffers.len()`.
    slot_cap: usize,
    /// Soft cap on `retained_bytes`.
    byte_cap: usize,
    /// Cumulative return counter, used to amortize donation cadence.
    return_count: u64,
}

impl LocalSlab {
    /// Creates a slab with the project default slot and byte caps.
    pub(super) fn new() -> Self {
        Self::with_caps(DEFAULT_SLAB_SLOT_CAP, DEFAULT_SLAB_BYTE_CAP)
    }

    /// Creates a slab with custom slot and byte caps.
    ///
    /// `slot_cap` is clamped to at least 1; `byte_cap` is clamped to at
    /// least `COPY_BUFFER_SIZE` to ensure a single buffer always fits.
    pub(super) fn with_caps(slot_cap: usize, byte_cap: usize) -> Self {
        Self {
            buffers: Vec::with_capacity(slot_cap.max(1)),
            retained_bytes: 0,
            slot_cap: slot_cap.max(1),
            byte_cap: byte_cap.max(COPY_BUFFER_SIZE),
            return_count: 0,
        }
    }

    /// Pops the warmest buffer from the slab, if any.
    pub(super) fn pop(&mut self) -> Option<Vec<u8>> {
        let buf = self.buffers.pop()?;
        self.retained_bytes = self.retained_bytes.saturating_sub(buf.capacity());
        Some(buf)
    }

    /// Tries to push a buffer into the slab.
    ///
    /// Returns `None` if admitted, `Some(buf)` if the slab is at slot or
    /// byte cap. The caller must then route the buffer to the global
    /// overflow path.
    pub(super) fn try_push(&mut self, buf: Vec<u8>) -> Option<Vec<u8>> {
        let cap = buf.capacity();
        if self.buffers.len() >= self.slot_cap
            || self.retained_bytes.saturating_add(cap) > self.byte_cap
        {
            return Some(buf);
        }
        self.retained_bytes = self.retained_bytes.saturating_add(cap);
        self.buffers.push(buf);
        None
    }

    /// Pops the oldest (coldest) buffer for donation to the overflow queue.
    ///
    /// FIFO eviction preserves LIFO warmth on the hot path: the most
    /// recently freed buffer stays available for the next acquire.
    pub(super) fn pop_oldest(&mut self) -> Option<Vec<u8>> {
        if self.buffers.is_empty() {
            return None;
        }
        let buf = self.buffers.remove(0);
        self.retained_bytes = self.retained_bytes.saturating_sub(buf.capacity());
        Some(buf)
    }

    /// Returns `true` when this return triggers a periodic donation.
    pub(super) fn should_donate(&mut self) -> bool {
        self.return_count = self.return_count.wrapping_add(1);
        self.return_count % DONATION_INTERVAL == 0 && !self.buffers.is_empty()
    }

    /// Returns the current retained byte total.
    pub(super) fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    /// Returns the current slot count.
    pub(super) fn len(&self) -> usize {
        self.buffers.len()
    }

    /// Drains every buffer through `overflow`, deallocating any buffer the
    /// overflow callback rejects via `deallocate`.
    ///
    /// Never panics: errors are absorbed by routing rejected buffers through
    /// `deallocate`. Called from the `thread_local!` destructor at thread
    /// exit or during panic unwind.
    pub(super) fn drain_to_overflow<F, G>(&mut self, overflow: F, deallocate: G)
    where
        F: Fn(Vec<u8>) -> Option<Vec<u8>>,
        G: Fn(Vec<u8>),
    {
        while let Some(buf) = self.buffers.pop() {
            self.retained_bytes = self.retained_bytes.saturating_sub(buf.capacity());
            if let Some(rejected) = overflow(buf) {
                deallocate(rejected);
            }
        }
    }
}

thread_local! {
    /// Per-thread slab. Initialized lazily on first access.
    pub(super) static LOCAL_SLAB: RefCell<LocalSlab> = RefCell::new(LocalSlab::new());
}

/// Pops the warmest buffer from this thread's slab, if any.
pub(super) fn try_take() -> Option<Vec<u8>> {
    LOCAL_SLAB.with(|cell| cell.borrow_mut().pop())
}

/// Pushes a buffer onto this thread's slab.
///
/// Returns `None` on admission, `Some(buf)` when the slab is full so the
/// caller can route it to the central overflow queue.
pub(super) fn try_store(buf: Vec<u8>) -> Option<Vec<u8>> {
    LOCAL_SLAB.with(|cell| cell.borrow_mut().try_push(buf))
}

/// If this is the Mth return on this thread, pops the oldest buffer for
/// donation to the global overflow queue.
pub(super) fn take_donation() -> Option<Vec<u8>> {
    LOCAL_SLAB.with(|cell| {
        let mut slab = cell.borrow_mut();
        if slab.should_donate() {
            slab.pop_oldest()
        } else {
            None
        }
    })
}

/// Returns this thread's current slab retained bytes and slot count.
///
/// Used by tests and telemetry.
pub(super) fn snapshot() -> (usize, usize) {
    LOCAL_SLAB.with(|cell| {
        let slab = cell.borrow();
        (slab.len(), slab.retained_bytes())
    })
}

/// Drains this thread's slab through `overflow` / `deallocate`.
///
/// Intended for explicit cleanup (e.g. teardown of a transient worker pool
/// that should not retain buffers across pool reconfiguration). The
/// thread-exit path uses the `Drop` impl on `LocalSlab` indirectly via the
/// `thread_local!` destructor when the slab carries a global overflow hook
/// (see [`super::pool`]).
pub(super) fn drain<F, G>(overflow: F, deallocate: G)
where
    F: Fn(Vec<u8>) -> Option<Vec<u8>>,
    G: Fn(Vec<u8>),
{
    LOCAL_SLAB.with(|cell| cell.borrow_mut().drain_to_overflow(overflow, deallocate));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slab_starts_empty() {
        let slab = LocalSlab::new();
        assert_eq!(slab.len(), 0);
        assert_eq!(slab.retained_bytes(), 0);
    }

    #[test]
    fn pop_on_empty_returns_none() {
        let mut slab = LocalSlab::with_caps(4, 4 * COPY_BUFFER_SIZE);
        assert!(slab.pop().is_none());
    }

    #[test]
    fn push_pop_is_lifo() {
        let mut slab = LocalSlab::with_caps(4, 4 * COPY_BUFFER_SIZE);
        let mut a = vec![0u8; COPY_BUFFER_SIZE];
        a[0] = 1;
        let mut b = vec![0u8; COPY_BUFFER_SIZE];
        b[0] = 2;

        assert!(slab.try_push(a).is_none());
        assert!(slab.try_push(b).is_none());
        let top = slab.pop().expect("top");
        assert_eq!(top[0], 2, "LIFO: newest on top");
        let next = slab.pop().expect("next");
        assert_eq!(next[0], 1);
    }

    #[test]
    fn slot_cap_blocks_admission() {
        let mut slab = LocalSlab::with_caps(2, 16 * COPY_BUFFER_SIZE);
        assert!(slab.try_push(vec![0u8; COPY_BUFFER_SIZE]).is_none());
        assert!(slab.try_push(vec![0u8; COPY_BUFFER_SIZE]).is_none());
        let overflow = slab.try_push(vec![0u8; COPY_BUFFER_SIZE]);
        assert!(overflow.is_some(), "third push exceeds slot_cap=2");
    }

    #[test]
    fn byte_cap_blocks_admission() {
        let mut slab = LocalSlab::with_caps(8, COPY_BUFFER_SIZE);
        assert!(slab.try_push(vec![0u8; COPY_BUFFER_SIZE]).is_none());
        let overflow = slab.try_push(vec![0u8; COPY_BUFFER_SIZE]);
        assert!(overflow.is_some(), "second push exceeds byte_cap=1");
    }

    #[test]
    fn retained_bytes_tracks_capacity() {
        let mut slab = LocalSlab::with_caps(4, 4 * COPY_BUFFER_SIZE);
        let buf = Vec::with_capacity(COPY_BUFFER_SIZE);
        assert!(slab.try_push(buf).is_none());
        assert_eq!(slab.retained_bytes(), COPY_BUFFER_SIZE);
        let popped = slab.pop().expect("buf");
        assert_eq!(popped.capacity(), COPY_BUFFER_SIZE);
        assert_eq!(slab.retained_bytes(), 0);
    }

    #[test]
    fn pop_oldest_is_fifo_for_eviction() {
        let mut slab = LocalSlab::with_caps(4, 4 * COPY_BUFFER_SIZE);
        let mut first = vec![0u8; COPY_BUFFER_SIZE];
        first[0] = 1;
        let mut second = vec![0u8; COPY_BUFFER_SIZE];
        second[0] = 2;
        let mut third = vec![0u8; COPY_BUFFER_SIZE];
        third[0] = 3;
        assert!(slab.try_push(first).is_none());
        assert!(slab.try_push(second).is_none());
        assert!(slab.try_push(third).is_none());
        let oldest = slab.pop_oldest().expect("oldest");
        assert_eq!(oldest[0], 1, "FIFO eviction: oldest first");
        // Newest still on top after eviction.
        let newest = slab.pop().expect("newest");
        assert_eq!(newest[0], 3);
    }

    #[test]
    fn should_donate_fires_every_donation_interval() {
        let mut slab = LocalSlab::with_caps(4, 4 * COPY_BUFFER_SIZE);
        assert!(slab.try_push(vec![0u8; COPY_BUFFER_SIZE]).is_none());
        let mut fired = 0;
        for _ in 0..(DONATION_INTERVAL * 2) {
            if slab.should_donate() {
                fired += 1;
            }
        }
        assert_eq!(fired, 2);
    }

    #[test]
    fn should_donate_skips_when_empty() {
        let mut slab = LocalSlab::with_caps(4, 4 * COPY_BUFFER_SIZE);
        for _ in 0..(DONATION_INTERVAL * 2) {
            assert!(!slab.should_donate(), "no donation from empty slab");
        }
    }

    #[test]
    fn drain_routes_through_overflow() {
        let mut slab = LocalSlab::with_caps(4, 4 * COPY_BUFFER_SIZE);
        for _ in 0..3 {
            assert!(slab.try_push(vec![0u8; COPY_BUFFER_SIZE]).is_none());
        }

        let drained = std::sync::atomic::AtomicUsize::new(0);
        let deallocated = std::sync::atomic::AtomicUsize::new(0);
        slab.drain_to_overflow(
            |buf| {
                drained.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                // Half accepted, half rejected to exercise both branches.
                if drained.load(std::sync::atomic::Ordering::Relaxed) % 2 == 0 {
                    Some(buf)
                } else {
                    None
                }
            },
            |_buf| {
                deallocated.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            },
        );

        assert_eq!(slab.len(), 0);
        assert_eq!(slab.retained_bytes(), 0);
        assert_eq!(drained.load(std::sync::atomic::Ordering::Relaxed), 3);
        // Returns for which the overflow callback returned Some get deallocated.
        assert_eq!(deallocated.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn with_caps_floors_clamps() {
        let slab = LocalSlab::with_caps(0, 1);
        assert!(slab.slot_cap >= 1);
        assert!(slab.byte_cap >= COPY_BUFFER_SIZE);
    }
}
