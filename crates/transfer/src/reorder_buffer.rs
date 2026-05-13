//! Bounded sliding-window reorder buffer with backpressure.
//!
//! Implements a classic sliding-window protocol (analogous to TCP's receive
//! window) adapted for in-process reordering of out-of-order delta results.
//! The window bounds memory usage to O(W) items regardless of total transfer
//! size, making it safe for 100K+ file transfers.
//!
//! # Algorithm
//!
//! The buffer maintains a cursor (`next_expected`) and accepts items with
//! sequence numbers in the half-open range `[next_expected, next_expected + W)`.
//! Items outside this window are rejected with a backpressure signal, telling
//! the producer to slow down.
//!
//! When the item at `next_expected` arrives, all consecutive items are drained
//! in a single pass, advancing the window and freeing capacity for new inserts.
//!
//! # Upstream Reference
//!
//! upstream: receiver.c:recv_files() processes files in file-list order.
//! This buffer restores sequential ordering after parallel delta dispatch.

use std::collections::{BTreeMap, VecDeque};
use std::time::Instant;

/// Default window size for the bounded reorder buffer.
pub const DEFAULT_WINDOW_SIZE: u64 = 64;

/// Snapshot of reorder buffer stall and depth metrics.
///
/// Returned by [`BoundedReorderBuffer::metrics`]. All counters are monotonically
/// non-decreasing over the buffer's lifetime except `current_depth`, which
/// tracks the instantaneous queue depth.
///
/// The stall duration measures head-of-line blocking: how long items waited
/// because their predecessor had not yet arrived. High stall counts with
/// large mean durations indicate the parallel dispatch is bottlenecked on
/// a slow straggler, and the parallel threshold or window size may need tuning.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReorderBufferStats {
    /// Number of items currently buffered (awaiting delivery).
    pub current_depth: u64,
    /// High-water mark of buffered items across the buffer's lifetime.
    pub peak_depth: u64,
    /// Number of distinct stall episodes. A stall begins when a non-head
    /// item is inserted into an empty buffer (the head slot is missing),
    /// and ends when `next_expected` advances (the head arrives and drains).
    pub stall_count: u64,
    /// Cumulative wall-clock nanoseconds spent in stall episodes.
    pub total_stall_nanos: u64,
    /// Total number of items delivered in-order from the buffer.
    pub items_delivered: u64,
}

impl ReorderBufferStats {
    /// Returns the mean stall duration in nanoseconds.
    ///
    /// Returns zero when no stalls have occurred, avoiding division by zero.
    #[must_use]
    pub const fn mean_stall_nanos(&self) -> u64 {
        if self.stall_count == 0 {
            0
        } else {
            self.total_stall_nanos / self.stall_count
        }
    }
}

/// Clock function type used by the reorder buffer for stall timing.
///
/// Defaults to [`Instant::now`]. Tests can inject a deterministic clock
/// via [`BoundedReorderBuffer::with_clock`] for reproducible stall
/// duration assertions.
type ClockFn = Box<dyn Fn() -> Instant + Send>;

/// Bounded sliding-window reorder buffer with backpressure.
///
/// Guarantees in-order delivery while bounding memory to at most
/// `window_size` buffered items. Producers that exceed the window
/// receive a backpressure signal to throttle submission.
///
/// Tracks stall-duration and queue-depth metrics for pipeline tuning.
/// Use [`metrics`](Self::metrics) to retrieve a snapshot of counters.
///
/// # Invariant
///
/// All keys `k` in `pending` satisfy: `next_expected <= k < next_expected + window_size`.
///
/// # Examples
///
/// ```
/// use transfer::reorder_buffer::BoundedReorderBuffer;
///
/// let mut buf: BoundedReorderBuffer<&str> = BoundedReorderBuffer::new(4);
///
/// // Out-of-order arrival: 2, 1, 0
/// let drained = buf.insert(2, "c").unwrap();
/// assert!(drained.is_empty());
///
/// let drained = buf.insert(1, "b").unwrap();
/// assert!(drained.is_empty());
///
/// let drained = buf.insert(0, "a").unwrap();
/// assert_eq!(drained, vec!["a", "b", "c"]);
/// ```
#[must_use]
pub struct BoundedReorderBuffer<T> {
    /// Next sequence number expected for in-order delivery.
    next_expected: u64,
    /// Maximum number of items ahead of `next_expected` that can be buffered.
    window_size: u64,
    /// Out-of-order items waiting for gaps to fill.
    pending: BTreeMap<u64, T>,
    /// High-water mark of `pending.len()` across the buffer's lifetime.
    peak_depth: u64,
    /// Number of distinct stall episodes.
    stall_count: u64,
    /// Cumulative nanoseconds spent in stall episodes.
    total_stall_nanos: u64,
    /// Total items delivered via `drain_consecutive`.
    items_delivered: u64,
    /// Start instant of the current stall episode, if any.
    stall_start: Option<Instant>,
    /// Clock source for stall timing.
    clock: ClockFn,
    /// When `true`, items pass through without sequence-based reordering.
    ///
    /// In bypass mode, [`insert`](Self::insert) appends to a FIFO queue and
    /// returns items immediately. Sequence numbers are ignored. This
    /// eliminates BTreeMap overhead when strict ordering is unnecessary
    /// (e.g., `--delay-updates` is off and files are committed immediately).
    bypass: bool,
    /// FIFO queue used in bypass mode. Empty when `bypass` is `false`.
    bypass_queue: VecDeque<T>,
}

/// Items drained from the buffer in sequence order.
///
/// Returned by [`BoundedReorderBuffer::insert`] when insertion completes
/// a contiguous run starting at `next_expected`.
pub type DrainedItems<T> = Vec<T>;

/// Error returned when the requested sequence number falls outside the
/// current acceptance window.
///
/// This signals backpressure: the producer should wait until the window
/// advances (i.e., earlier sequence numbers are inserted and drained)
/// before retrying.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackpressureError {
    /// The sequence number that was rejected.
    pub sequence: u64,
    /// The current window start (next expected sequence).
    pub window_start: u64,
    /// The current window end (exclusive).
    pub window_end: u64,
}

impl std::fmt::Display for BackpressureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "sequence {} outside window [{}, {})",
            self.sequence, self.window_start, self.window_end
        )
    }
}

impl std::error::Error for BackpressureError {}

impl<T: std::fmt::Debug> std::fmt::Debug for BoundedReorderBuffer<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundedReorderBuffer")
            .field("next_expected", &self.next_expected)
            .field("window_size", &self.window_size)
            .field("pending", &self.pending)
            .field("peak_depth", &self.peak_depth)
            .field("stall_count", &self.stall_count)
            .field("total_stall_nanos", &self.total_stall_nanos)
            .field("items_delivered", &self.items_delivered)
            .field("stall_active", &self.stall_start.is_some())
            .field("bypass", &self.bypass)
            .finish()
    }
}

impl<T> BoundedReorderBuffer<T> {
    /// Creates a new bounded reorder buffer with the given window size.
    ///
    /// Uses [`Instant::now`] as the clock source for stall timing.
    ///
    /// # Panics
    ///
    /// Panics if `window_size` is zero.
    pub fn new(window_size: u64) -> Self {
        Self::with_clock(window_size, Box::new(Instant::now))
    }

    /// Creates a passthrough buffer that skips sequence-based reordering.
    ///
    /// In passthrough mode, items are delivered in insertion order regardless
    /// of their sequence numbers. This is an optimization for transfers where
    /// strict file-list ordering is unnecessary - for example, when
    /// `--delay-updates` is off and each file is committed immediately upon
    /// completion.
    ///
    /// The buffer allocates no BTreeMap entries. All items flow through a
    /// lightweight FIFO queue, eliminating per-item overhead of sequence
    /// tracking, gap detection, and stall measurement.
    ///
    /// # Examples
    ///
    /// ```
    /// use transfer::reorder_buffer::BoundedReorderBuffer;
    ///
    /// let mut buf: BoundedReorderBuffer<&str> = BoundedReorderBuffer::passthrough();
    ///
    /// // Items are delivered in insertion order, not sequence order.
    /// let d = buf.insert(2, "third").unwrap();
    /// assert_eq!(d, vec!["third"]);
    ///
    /// let d = buf.insert(0, "first").unwrap();
    /// assert_eq!(d, vec!["first"]);
    /// ```
    pub fn passthrough() -> Self {
        Self {
            next_expected: 0,
            window_size: 0,
            pending: BTreeMap::new(),
            peak_depth: 0,
            stall_count: 0,
            total_stall_nanos: 0,
            items_delivered: 0,
            stall_start: None,
            clock: Box::new(Instant::now),
            bypass: true,
            bypass_queue: VecDeque::new(),
        }
    }

    /// Creates a new bounded reorder buffer with a custom clock source.
    ///
    /// The `clock` function is called to obtain timestamps for stall duration
    /// measurement. Tests can inject a deterministic clock for reproducible
    /// assertions on stall timing.
    ///
    /// # Panics
    ///
    /// Panics if `window_size` is zero.
    pub fn with_clock(window_size: u64, clock: ClockFn) -> Self {
        assert!(window_size > 0, "window size must be non-zero");
        Self {
            next_expected: 0,
            window_size,
            pending: BTreeMap::new(),
            peak_depth: 0,
            stall_count: 0,
            total_stall_nanos: 0,
            items_delivered: 0,
            stall_start: None,
            clock,
            bypass: false,
            bypass_queue: VecDeque::new(),
        }
    }

    /// Returns `true` if the buffer is in passthrough (bypass) mode.
    ///
    /// In passthrough mode, items are delivered in insertion order without
    /// sequence-based reordering.
    #[must_use]
    pub const fn is_passthrough(&self) -> bool {
        self.bypass
    }

    /// Inserts an item and returns any consecutive items ready for delivery.
    ///
    /// If `seq` is within the acceptance window `[next_expected, next_expected + window_size)`,
    /// the item is buffered. If this insertion completes a contiguous run starting
    /// at `next_expected`, all consecutive items are drained and returned.
    ///
    /// Returns `Err(BackpressureError)` if `seq` is outside the window. Items
    /// with `seq < next_expected` (already delivered) are silently ignored and
    /// return an empty drain.
    ///
    /// Stall metrics are updated on each call: a stall episode begins when
    /// a non-head item is inserted into an empty buffer, and ends when the
    /// head item arrives and triggers a drain.
    ///
    /// # Errors
    ///
    /// Returns [`BackpressureError`] when `seq >= next_expected + window_size`.
    #[must_use = "drained items must be processed to maintain ordering"]
    pub fn insert(&mut self, seq: u64, item: T) -> Result<DrainedItems<T>, BackpressureError> {
        if self.bypass {
            self.items_delivered += 1;
            return Ok(vec![item]);
        }
        // Already delivered - ignore duplicate/stale submissions.
        if seq < self.next_expected {
            return Ok(Vec::new());
        }

        let window_end = self.next_expected.saturating_add(self.window_size);
        if seq >= window_end {
            return Err(BackpressureError {
                sequence: seq,
                window_start: self.next_expected,
                window_end,
            });
        }

        // Detect stall start: inserting a non-head item into an empty buffer
        // means the head is missing and subsequent items will block.
        let was_empty = self.pending.is_empty();
        if was_empty && seq != self.next_expected && self.stall_start.is_none() {
            self.stall_start = Some((self.clock)());
            self.stall_count += 1;
        }

        self.pending.insert(seq, item);

        // Update peak depth after insertion.
        let depth = self.pending.len() as u64;
        if depth > self.peak_depth {
            self.peak_depth = depth;
        }

        let prev_expected = self.next_expected;
        let drained = self.drain_consecutive();

        // If the drain advanced next_expected, the stall (if any) has ended.
        if self.next_expected > prev_expected {
            if let Some(start) = self.stall_start.take() {
                let elapsed = (self.clock)().duration_since(start);
                self.total_stall_nanos += elapsed.as_nanos() as u64;
            }
        }

        Ok(drained)
    }

    /// Drains all consecutive items starting from `next_expected`.
    fn drain_consecutive(&mut self) -> DrainedItems<T> {
        let mut items = Vec::new();
        while let Some(item) = self.pending.remove(&self.next_expected) {
            items.push(item);
            self.next_expected += 1;
            self.items_delivered += 1;
        }
        items
    }

    /// Returns a snapshot of stall-duration and queue-depth metrics.
    ///
    /// The returned stats reflect the cumulative state since the buffer was
    /// created. All counters are monotonically non-decreasing except
    /// `current_depth`, which tracks the instantaneous queue depth.
    #[must_use]
    pub fn metrics(&self) -> ReorderBufferStats {
        ReorderBufferStats {
            current_depth: self.pending.len() as u64,
            peak_depth: self.peak_depth,
            stall_count: self.stall_count,
            total_stall_nanos: self.total_stall_nanos,
            items_delivered: self.items_delivered,
        }
    }

    /// Returns the next sequence number expected for in-order delivery.
    #[must_use]
    pub const fn next_expected(&self) -> u64 {
        self.next_expected
    }

    /// Returns the number of items currently buffered (awaiting delivery).
    #[must_use]
    pub fn buffered_count(&self) -> usize {
        self.pending.len()
    }

    /// Returns how many more items can be accepted before backpressure kicks in.
    ///
    /// This is `window_size - buffered_count`, representing the remaining
    /// capacity in the current window.
    #[must_use]
    pub fn window_remaining(&self) -> u64 {
        self.window_size - self.pending.len() as u64
    }

    /// Returns the configured window size.
    #[must_use]
    pub const fn window_size(&self) -> u64 {
        self.window_size
    }

    /// Returns `true` if no items are buffered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_order_delivery_yields_immediately() {
        let mut buf = BoundedReorderBuffer::new(8);

        let d = buf.insert(0, "a").unwrap();
        assert_eq!(d, vec!["a"]);

        let d = buf.insert(1, "b").unwrap();
        assert_eq!(d, vec!["b"]);

        let d = buf.insert(2, "c").unwrap();
        assert_eq!(d, vec!["c"]);

        assert_eq!(buf.next_expected(), 3);
        assert!(buf.is_empty());
    }

    #[test]
    fn out_of_order_with_gap_fill() {
        let mut buf = BoundedReorderBuffer::new(8);

        let d = buf.insert(2, "c").unwrap();
        assert!(d.is_empty());

        let d = buf.insert(1, "b").unwrap();
        assert!(d.is_empty());

        // Filling the gap at 0 drains 0, 1, 2.
        let d = buf.insert(0, "a").unwrap();
        assert_eq!(d, vec!["a", "b", "c"]);
        assert_eq!(buf.next_expected(), 3);
    }

    #[test]
    fn backpressure_enforcement() {
        let mut buf: BoundedReorderBuffer<i32> = BoundedReorderBuffer::new(4);

        // Insert seq 0 (delivered immediately).
        let d = buf.insert(0, 0).unwrap();
        assert_eq!(d, vec![0]);
        // Window is now [1, 5).

        // seq 5 is outside [1, 5) - backpressure.
        let err = buf.insert(5, 5).unwrap_err();
        assert_eq!(err.sequence, 5);
        assert_eq!(err.window_start, 1);
        assert_eq!(err.window_end, 5);
    }

    #[test]
    fn window_advancement_opens_new_slots() {
        let mut buf = BoundedReorderBuffer::new(4);
        // Window is [0, 4). Insert 0, 1, 2, 3.
        let d = buf.insert(0, 'a').unwrap();
        assert_eq!(d, vec!['a']);
        let d = buf.insert(1, 'b').unwrap();
        assert_eq!(d, vec!['b']);
        let d = buf.insert(2, 'c').unwrap();
        assert_eq!(d, vec!['c']);
        let d = buf.insert(3, 'd').unwrap();
        assert_eq!(d, vec!['d']);
        // All delivered, next_expected = 4, window = [4, 8).

        // seq 7 is within [4, 8).
        let d = buf.insert(7, 'h').unwrap();
        assert!(d.is_empty());

        // seq 8 is outside [4, 8).
        assert!(buf.insert(8, 'i').is_err());

        // Fill 4, 5, 6 to drain through 7.
        let d = buf.insert(4, 'e').unwrap();
        assert_eq!(d, vec!['e']);
        // 4 drained, next_expected=5, but 5 not present - empty.
        // Wait, 4 drains immediately since next_expected=4 and item 4 arrives.
        // Actually insert(4) sees next_expected=4, inserts, drains 4 (only 4 consecutive).
        // next_expected becomes 5. 5 not in buffer. Returns ['e'].

        let d = buf.insert(5, 'f').unwrap();
        assert_eq!(d, vec!['f']);
        // next_expected = 6, 6 not in buffer.

        let d = buf.insert(6, 'g').unwrap();
        assert_eq!(d, vec!['g', 'h']);
        // next_expected was 6, inserts 6, drains 6 then 7 (consecutive).
        assert_eq!(buf.next_expected(), 8);

        // Now seq 8 is within window [8, 12).
        let d = buf.insert(8, 'i').unwrap();
        assert_eq!(d, vec!['i']);
    }

    #[test]
    fn stale_sequence_ignored() {
        let mut buf = BoundedReorderBuffer::new(4);
        buf.insert(0, 10).unwrap();
        buf.insert(1, 20).unwrap();
        // next_expected is now 2.

        // Inserting seq 0 again (already delivered) is silently ignored.
        let d = buf.insert(0, 99).unwrap();
        assert!(d.is_empty());
        assert_eq!(buf.next_expected(), 2);
    }

    #[test]
    fn window_remaining_tracks_capacity() {
        let mut buf = BoundedReorderBuffer::new(4);
        assert_eq!(buf.window_remaining(), 4);

        buf.insert(2, "x").unwrap();
        assert_eq!(buf.window_remaining(), 3);

        buf.insert(3, "y").unwrap();
        assert_eq!(buf.window_remaining(), 2);

        // Fill gap - drains 0, 1 not present, so only 2, 3 stay buffered.
        // Actually 0 is not in buffer either, so nothing drains.
        assert_eq!(buf.buffered_count(), 2);
    }

    #[test]
    fn contiguous_drain_amortized() {
        let mut buf = BoundedReorderBuffer::new(64);

        // Insert 1..=50 (all out of order, gap at 0).
        for i in (1..=50).rev() {
            let d = buf.insert(i, i).unwrap();
            assert!(d.is_empty());
        }
        assert_eq!(buf.buffered_count(), 50);

        // Insert 0 - drains all 51 items in one call.
        let d = buf.insert(0, 0).unwrap();
        assert_eq!(d.len(), 51);
        for (i, &val) in d.iter().enumerate() {
            assert_eq!(val, i as u64);
        }
        assert!(buf.is_empty());
        assert_eq!(buf.next_expected(), 51);
    }

    #[test]
    #[should_panic(expected = "window size must be non-zero")]
    fn zero_window_panics() {
        let _: BoundedReorderBuffer<i32> = BoundedReorderBuffer::new(0);
    }

    #[test]
    fn backpressure_error_display() {
        let err = BackpressureError {
            sequence: 10,
            window_start: 3,
            window_end: 7,
        };
        assert_eq!(err.to_string(), "sequence 10 outside window [3, 7)");
    }

    #[test]
    fn window_size_accessor() {
        let buf: BoundedReorderBuffer<u8> = BoundedReorderBuffer::new(32);
        assert_eq!(buf.window_size(), 32);
    }

    /// Property test: a random permutation of 0..N always yields 0..N in order.
    mod prop {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn random_permutation_yields_sorted(n in 1u64..256) {
                let window = n.max(1);
                let mut buf = BoundedReorderBuffer::new(window);

                // Generate a random-ish permutation using a simple shuffle.
                let mut indices: Vec<u64> = (0..n).collect();
                // Deterministic "shuffle" based on n for reproducibility.
                indices.reverse();

                let mut all_drained = Vec::new();
                for seq in indices {
                    match buf.insert(seq, seq) {
                        Ok(d) => all_drained.extend(d),
                        Err(_) => {
                            // With window == n, this should not happen.
                            panic!("backpressure with window == n");
                        }
                    }
                }

                // Verify all items delivered in order.
                prop_assert_eq!(all_drained.len(), n as usize);
                for (i, &val) in all_drained.iter().enumerate() {
                    prop_assert_eq!(val, i as u64);
                }
            }

            #[test]
            fn backpressure_respects_window(window in 1u64..64, overshoot in 0u64..100) {
                let mut buf: BoundedReorderBuffer<u64> = BoundedReorderBuffer::new(window);
                let target = window + overshoot;
                let result = buf.insert(target, target);
                prop_assert!(result.is_err());
                let err = result.unwrap_err();
                prop_assert_eq!(err.sequence, target);
                prop_assert_eq!(err.window_start, 0);
                prop_assert_eq!(err.window_end, window);
            }
        }
    }

    /// Property tests for failure-mode behaviour.
    ///
    /// `BoundedReorderBuffer<T>` is fully generic over the payload `T`; it
    /// has no built-in error channel. The surrounding pipeline propagates
    /// I/O errors out-of-band. To exercise failure modes against the buffer
    /// itself, these tests use `Result<u64, io::Error>` as the payload and
    /// verify two invariants:
    ///
    /// 1. Network-error propagation: errors interleaved with successes
    ///    survive reorder unchanged, in the original sequence order, with
    ///    no item loss or reordering between `Ok` and `Err` variants.
    /// 2. Mid-transfer abort: when the consumer drops the receiving
    ///    channel partway through a transfer, the producer thread
    ///    terminates without panicking and the buffer's pending map is
    ///    deallocated by ordinary `Drop`.
    mod property_failure_tests {
        use super::*;
        use proptest::collection::vec;
        use proptest::prelude::*;
        use std::io;
        use std::panic::{AssertUnwindSafe, catch_unwind};
        use std::sync::mpsc;
        use std::thread;
        use std::time::{Duration, Instant};

        type Payload = Result<u64, io::Error>;

        // `io::Error` is not `Clone`, which `Just` requires, so the strategy
        // returns a `()`-tagged discriminator and the test bodies materialise
        // a fresh `io::Error` at use time.
        fn payload_strategy() -> impl Strategy<Value = Result<u64, ()>> {
            prop_oneof![any::<u64>().prop_map(Ok), Just(Err(())),]
        }

        /// Deterministic permutation of `0..n` seeded from a proptest u64.
        ///
        /// Avoids wall-clock randomness so failures shrink reliably.
        fn lcg_shuffle(n: usize, seed: u64) -> Vec<usize> {
            const A: u64 = 1664525;
            const C: u64 = 1013904223;
            let mut keyed: Vec<(u64, usize)> = (0..n)
                .map(|i| {
                    let key = seed
                        .wrapping_mul(A)
                        .wrapping_add(C)
                        .wrapping_add((i as u64).wrapping_mul(A));
                    (key, i)
                })
                .collect();
            keyed.sort_unstable_by_key(|&(k, _)| k);
            keyed.into_iter().map(|(_, i)| i).collect()
        }

        proptest! {
            /// Errors interleaved with successes flow through reorder
            /// without being dropped, reordered, or coerced into `Ok`.
            #[test]
            fn network_error_propagation(
                payloads in vec(payload_strategy(), 1..64),
                seed in any::<u64>(),
            ) {
                let n = payloads.len();
                let window = (n as u64).max(1);
                let mut buf: BoundedReorderBuffer<Payload> =
                    BoundedReorderBuffer::new(window);

                let order = lcg_shuffle(n, seed);
                let mut drained: Vec<Payload> = Vec::with_capacity(n);

                for seq_idx in order {
                    let payload: Payload = match &payloads[seq_idx] {
                        Ok(v) => Ok(*v),
                        Err(_) => Err(io::Error::other("simulated network failure")),
                    };
                    let out = buf
                        .insert(seq_idx as u64, payload)
                        .expect("window == n so backpressure cannot fire");
                    drained.extend(out);
                }

                prop_assert_eq!(drained.len(), n);
                prop_assert!(buf.is_empty());
                prop_assert_eq!(buf.next_expected(), n as u64);

                for (i, (got, want)) in drained.iter().zip(payloads.iter()).enumerate() {
                    match (got, want) {
                        (Ok(a), Ok(b)) => {
                            prop_assert_eq!(a, b, "ok payload at seq {} mismatched", i);
                        }
                        (Err(_), Err(_)) => {}
                        (Ok(_), Err(_)) | (Err(_), Ok(_)) => {
                            prop_assert!(
                                false,
                                "result discriminant mismatch at seq {}",
                                i,
                            );
                        }
                    }
                }
            }

            /// When the consumer drops the channel mid-transfer, the
            /// producer thread must terminate cleanly: no panic, no
            /// deadlock, no leak (buffer dropped at thread exit).
            #[test]
            fn abort_no_leak_no_panic(
                total in 4u64..64,
                abort_after in 1u64..32,
            ) {
                let abort_after = abort_after.min(total);
                let window = total;
                let (tx, rx) = mpsc::channel::<Payload>();

                let producer = thread::spawn(move || {
                    let mut buf: BoundedReorderBuffer<Payload> =
                        BoundedReorderBuffer::new(window);
                    catch_unwind(AssertUnwindSafe(|| {
                        for seq in 0..total {
                            let payload: Payload = if seq % 7 == 0 {
                                Err(io::Error::other("simulated mid-transfer error"))
                            } else {
                                Ok(seq)
                            };
                            let drained = match buf.insert(seq, payload) {
                                Ok(d) => d,
                                Err(_) => return,
                            };
                            for item in drained {
                                if tx.send(item).is_err() {
                                    // Consumer dropped rx: stop cleanly.
                                    return;
                                }
                            }
                        }
                    }))
                });

                for _ in 0..abort_after {
                    let _ = rx
                        .recv_timeout(Duration::from_secs(5))
                        .expect("producer must deliver before timeout");
                }
                drop(rx);

                let deadline = Instant::now() + Duration::from_secs(5);
                while !producer.is_finished() {
                    if Instant::now() >= deadline {
                        panic!("producer did not terminate within deadline after abort");
                    }
                    thread::sleep(Duration::from_millis(10));
                }

                let outcome = producer.join().expect("producer thread panicked");
                prop_assert!(
                    outcome.is_ok(),
                    "producer body panicked under abort",
                );
            }
        }
    }

    /// Tests for stall-duration and queue-depth metrics.
    mod metrics_tests {
        use super::*;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{Duration, Instant};

        /// Builds a deterministic clock that advances by `step` on each call.
        ///
        /// Returns a boxed closure suitable for `BoundedReorderBuffer::with_clock`.
        /// The clock starts at `Instant::now()` and increments monotonically.
        fn fake_clock(step: Duration) -> ClockFn {
            let base = Instant::now();
            let counter = Arc::new(AtomicU64::new(0));
            Box::new(move || {
                let n = counter.fetch_add(1, Ordering::Relaxed);
                base + step * n as u32
            })
        }

        #[test]
        fn in_order_delivery_no_stall() {
            let mut buf: BoundedReorderBuffer<&str> =
                BoundedReorderBuffer::with_clock(8, fake_clock(Duration::from_millis(10)));

            buf.insert(0, "a").unwrap();
            buf.insert(1, "b").unwrap();
            buf.insert(2, "c").unwrap();
            buf.insert(3, "d").unwrap();

            let m = buf.metrics();
            assert_eq!(
                m.stall_count, 0,
                "in-order delivery must produce zero stalls"
            );
            assert_eq!(m.total_stall_nanos, 0);
            assert_eq!(m.mean_stall_nanos(), 0);
            assert_eq!(m.items_delivered, 4);
            assert_eq!(m.current_depth, 0);
            // Each in-order insert momentarily has depth 1 before draining.
            assert_eq!(m.peak_depth, 1);
        }

        #[test]
        fn out_of_order_produces_stall() {
            let step = Duration::from_millis(100);
            let mut buf: BoundedReorderBuffer<&str> =
                BoundedReorderBuffer::with_clock(8, fake_clock(step));

            // Insert seq 3 first - stall begins (non-head into empty buffer).
            // Clock call 0: entry check, clock call 1: after drain (no drain).
            buf.insert(3, "d").unwrap();
            buf.insert(1, "b").unwrap();
            buf.insert(2, "c").unwrap();

            let m = buf.metrics();
            assert_eq!(m.stall_count, 1, "one stall episode expected");
            assert_eq!(m.current_depth, 3);
            assert_eq!(m.peak_depth, 3);
            assert_eq!(m.items_delivered, 0);
            assert!(
                m.total_stall_nanos == 0,
                "stall not yet resolved - total should be 0"
            );

            // Insert seq 0 - fills the gap, stall ends, all 4 drain.
            buf.insert(0, "a").unwrap();

            let m = buf.metrics();
            assert_eq!(m.stall_count, 1);
            assert_eq!(m.items_delivered, 4);
            assert_eq!(m.current_depth, 0);
            assert_eq!(m.peak_depth, 4);
            assert!(
                m.total_stall_nanos > 0,
                "stall resolved - total_stall_nanos must be positive"
            );
        }

        #[test]
        fn multiple_stall_episodes() {
            let mut buf: BoundedReorderBuffer<u64> =
                BoundedReorderBuffer::with_clock(16, fake_clock(Duration::from_millis(50)));

            // Episode 1: insert seq 2, then 1, then 0.
            buf.insert(2, 2).unwrap();
            buf.insert(1, 1).unwrap();
            buf.insert(0, 0).unwrap();

            let m = buf.metrics();
            assert_eq!(m.stall_count, 1);
            assert_eq!(m.items_delivered, 3);
            assert!(m.total_stall_nanos > 0);

            // Episode 2: insert seq 5, then 4, then 3.
            buf.insert(5, 5).unwrap();
            buf.insert(4, 4).unwrap();
            buf.insert(3, 3).unwrap();

            let m = buf.metrics();
            assert_eq!(m.stall_count, 2, "two distinct stall episodes");
            assert_eq!(m.items_delivered, 6);
        }

        #[test]
        fn peak_depth_monotonically_nondecreasing() {
            let mut buf: BoundedReorderBuffer<u64> = BoundedReorderBuffer::new(64);

            let mut last_peak = 0u64;

            // Insert in reverse order 0..50 to build up depth.
            for i in (0..50).rev() {
                buf.insert(i, i).unwrap();
                let m = buf.metrics();
                assert!(
                    m.peak_depth >= last_peak,
                    "peak_depth must be monotonically non-decreasing: {} < {}",
                    m.peak_depth,
                    last_peak
                );
                last_peak = m.peak_depth;
            }

            assert_eq!(last_peak, 50, "peak should reach 50 items");

            // After full drain, peak stays at 50.
            let m = buf.metrics();
            assert_eq!(m.peak_depth, 50);
            assert_eq!(m.current_depth, 0);
        }

        #[test]
        fn queue_depth_tracks_buffered_count() {
            let mut buf: BoundedReorderBuffer<&str> = BoundedReorderBuffer::new(16);

            buf.insert(3, "d").unwrap();
            assert_eq!(buf.metrics().current_depth, 1);

            buf.insert(5, "f").unwrap();
            assert_eq!(buf.metrics().current_depth, 2);

            buf.insert(4, "e").unwrap();
            assert_eq!(buf.metrics().current_depth, 3);

            // Insert head - drains 0 items since 0 is missing, but seq 3 is
            // head-adjacent. Actually next_expected is 0, so inserting 3,5,4
            // leaves the head at 0. Nothing drains.
            assert_eq!(buf.metrics().current_depth, 3);
            assert_eq!(buf.metrics().current_depth, buf.buffered_count() as u64);
        }

        #[test]
        fn items_delivered_counts_all_drained() {
            let mut buf: BoundedReorderBuffer<u64> = BoundedReorderBuffer::new(8);

            for i in 0..5 {
                buf.insert(i, i).unwrap();
            }
            assert_eq!(buf.metrics().items_delivered, 5);

            // Insert more after gap.
            buf.insert(7, 7).unwrap();
            buf.insert(6, 6).unwrap();
            buf.insert(5, 5).unwrap();
            assert_eq!(buf.metrics().items_delivered, 8);
        }

        #[test]
        fn mean_stall_nanos_derived_correctly() {
            let stats = ReorderBufferStats {
                current_depth: 0,
                peak_depth: 5,
                stall_count: 4,
                total_stall_nanos: 1_000_000,
                items_delivered: 10,
            };
            assert_eq!(stats.mean_stall_nanos(), 250_000);
        }

        #[test]
        fn mean_stall_nanos_zero_when_no_stalls() {
            let stats = ReorderBufferStats::default();
            assert_eq!(stats.mean_stall_nanos(), 0);
        }

        #[test]
        fn stale_insert_does_not_affect_metrics() {
            let mut buf: BoundedReorderBuffer<u64> = BoundedReorderBuffer::new(8);
            buf.insert(0, 0).unwrap();
            buf.insert(1, 1).unwrap();

            let m_before = buf.metrics();

            // Stale insert (seq 0 already delivered).
            buf.insert(0, 99).unwrap();

            let m_after = buf.metrics();
            assert_eq!(m_before.stall_count, m_after.stall_count);
            assert_eq!(m_before.peak_depth, m_after.peak_depth);
            assert_eq!(m_before.items_delivered, m_after.items_delivered);
        }

        /// Property test: monotonic counters across random permutations.
        mod prop_metrics {
            use super::*;
            use proptest::prelude::*;

            proptest! {
                #[test]
                fn counters_monotonic_across_permutation(n in 1u64..128) {
                    let window = n.max(1);
                    let mut buf = BoundedReorderBuffer::new(window);

                    let mut indices: Vec<u64> = (0..n).collect();
                    indices.reverse();

                    let mut prev_stall_count = 0u64;
                    let mut prev_peak = 0u64;
                    let mut prev_total_stall = 0u64;

                    for seq in indices {
                        buf.insert(seq, seq).unwrap();
                        let m = buf.metrics();

                        prop_assert!(
                            m.stall_count >= prev_stall_count,
                            "stall_count must be monotonically non-decreasing"
                        );
                        prop_assert!(
                            m.peak_depth >= prev_peak,
                            "peak_depth must be monotonically non-decreasing"
                        );
                        prop_assert!(
                            m.total_stall_nanos >= prev_total_stall,
                            "total_stall_nanos must be monotonically non-decreasing"
                        );
                        prop_assert!(
                            m.peak_depth >= m.current_depth,
                            "peak_depth must be >= current_depth"
                        );

                        prev_stall_count = m.stall_count;
                        prev_peak = m.peak_depth;
                        prev_total_stall = m.total_stall_nanos;
                    }

                    // After full drain, all items delivered.
                    let final_m = buf.metrics();
                    prop_assert_eq!(final_m.items_delivered, n);
                    prop_assert_eq!(final_m.current_depth, 0);
                }

                #[test]
                fn identity_permutation_zero_stalls(n in 1u64..128) {
                    let window = n.max(1);
                    let mut buf = BoundedReorderBuffer::new(window);

                    for seq in 0..n {
                        buf.insert(seq, seq).unwrap();
                    }

                    let m = buf.metrics();
                    prop_assert_eq!(m.stall_count, 0, "identity permutation must have zero stalls");
                    prop_assert_eq!(m.total_stall_nanos, 0);
                    prop_assert_eq!(m.items_delivered, n);
                }
            }
        }
    }

    /// Tests for passthrough (bypass) mode.
    mod passthrough_tests {
        use super::*;

        #[test]
        fn passthrough_delivers_immediately() {
            let mut buf: BoundedReorderBuffer<&str> = BoundedReorderBuffer::passthrough();
            assert!(buf.is_passthrough());

            let d = buf.insert(5, "hello").unwrap();
            assert_eq!(d, vec!["hello"]);

            let d = buf.insert(0, "world").unwrap();
            assert_eq!(d, vec!["world"]);
        }

        #[test]
        fn passthrough_no_backpressure() {
            let mut buf: BoundedReorderBuffer<u64> = BoundedReorderBuffer::passthrough();
            // Any sequence number is accepted.
            let d = buf.insert(u64::MAX - 1, 999).unwrap();
            assert_eq!(d, vec![999]);
        }

        #[test]
        fn passthrough_no_reordering() {
            let mut buf: BoundedReorderBuffer<&str> = BoundedReorderBuffer::passthrough();

            // Insert out of order: 2, 0, 1.
            let d = buf.insert(2, "c").unwrap();
            assert_eq!(d, vec!["c"]);

            let d = buf.insert(0, "a").unwrap();
            assert_eq!(d, vec!["a"]);

            let d = buf.insert(1, "b").unwrap();
            assert_eq!(d, vec!["b"]);
        }

        #[test]
        fn passthrough_metrics_track_delivery() {
            let mut buf: BoundedReorderBuffer<u64> = BoundedReorderBuffer::passthrough();
            buf.insert(0, 10).unwrap();
            buf.insert(1, 20).unwrap();
            buf.insert(2, 30).unwrap();

            let m = buf.metrics();
            assert_eq!(m.items_delivered, 3);
            assert_eq!(m.current_depth, 0);
            assert_eq!(m.peak_depth, 0);
            assert_eq!(m.stall_count, 0);
            assert_eq!(m.total_stall_nanos, 0);
        }

        #[test]
        fn passthrough_buffered_count_is_zero() {
            let mut buf: BoundedReorderBuffer<u64> = BoundedReorderBuffer::passthrough();
            buf.insert(0, 42).unwrap();
            // Items pass through immediately - nothing buffered.
            assert_eq!(buf.buffered_count(), 0);
            assert!(buf.is_empty());
        }

        #[test]
        fn passthrough_window_size_is_zero() {
            let buf: BoundedReorderBuffer<u64> = BoundedReorderBuffer::passthrough();
            assert_eq!(buf.window_size(), 0);
        }

        #[test]
        fn passthrough_next_expected_stays_zero() {
            let mut buf: BoundedReorderBuffer<u64> = BoundedReorderBuffer::passthrough();
            buf.insert(0, 10).unwrap();
            buf.insert(1, 20).unwrap();
            // In bypass mode, next_expected is not advanced.
            assert_eq!(buf.next_expected(), 0);
        }

        #[test]
        fn passthrough_large_batch() {
            let mut buf: BoundedReorderBuffer<u64> = BoundedReorderBuffer::passthrough();
            let mut all_drained = Vec::new();

            for i in 0..500u64 {
                let d = buf.insert(499 - i, i).unwrap();
                all_drained.extend(d);
            }

            assert_eq!(all_drained.len(), 500);
            // Values arrive in insertion order (0, 1, 2, ..., 499).
            for (i, &val) in all_drained.iter().enumerate() {
                assert_eq!(val, i as u64);
            }
        }

        #[test]
        fn non_passthrough_flag_is_false() {
            let buf: BoundedReorderBuffer<u64> = BoundedReorderBuffer::new(4);
            assert!(!buf.is_passthrough());
        }
    }
}
