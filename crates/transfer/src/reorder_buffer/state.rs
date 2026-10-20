//! Constructors and read-only accessors for [`BoundedReorderBuffer`].

use std::collections::BTreeMap;
use std::time::Instant;

use super::{BoundedReorderBuffer, ClockFn, ReorderBufferStats};

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
