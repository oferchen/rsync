//! Sequence-based reorder buffer for the concurrent delta pipeline.
//!
//! Workers in the concurrent delta pipeline complete in arbitrary order.
//! [`ReorderBuffer`] collects out-of-order results and yields them strictly
//! in sequence order, enabling the consumer to process results as if they
//! arrived sequentially.
//!
//! # Design
//!
//! Uses a pre-allocated ring buffer internally for O(1) insertion and O(1)
//! extraction of the next expected item. A configurable capacity bound
//! prevents unbounded memory growth when a slow item blocks delivery of
//! many subsequent items.
//!
//! Optionally, a caller can compose an
//! [`AdaptiveCapacityPolicy`] into the buffer via
//! [`ReorderBuffer::with_adaptive_policy`]. The buffer then
//! grows under sustained pressure and shrinks back toward the policy's
//! minimum once the gap closes, all while preserving the same public API.
//!
//! # Upstream Reference
//!
//! Upstream rsync processes files sequentially in `recv_files()`. This buffer
//! restores that sequential ordering after parallel dispatch, preserving the
//! invariant that post-processing (checksum verification, metadata commit)
//! sees files in file-list order.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::adaptive::{AdaptiveCapacityPolicy, AdaptiveState, ReorderStats};

pub mod histogram;

pub use histogram::HistogramStats;

/// Snapshot of [`ReorderBuffer`] diagnostic counters.
///
/// Returned by [`ReorderBuffer::metrics`]. All fields are cumulative across
/// the buffer's lifetime except [`current_depth`](Self::current_depth), which
/// reflects the instantaneous occupancy at the moment of capture.
///
/// These counters help operators diagnose pipeline stalls without resorting
/// to ad hoc instrumentation. Stall duration grows whenever an out-of-order
/// item is buffered ahead of the next expected sequence; max depth records
/// the high-water mark of buffered items observed since construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    /// Total wall-clock time the buffer spent waiting for the next expected
    /// sequence while at least one out-of-order item was buffered.
    pub stall_duration: Duration,
    /// Instantaneous number of items currently buffered.
    pub current_depth: usize,
    /// High-water mark of buffered items observed since construction.
    pub max_depth: usize,
    /// Total number of times the consumer broke the capacity bound via
    /// [`ReorderBuffer::force_insert`]. Surfaced as a prerequisite for the
    /// `force_insert` removal sequencing in `docs/design/drain-parallel-consumer-thread.md`.
    pub force_insert_count: u64,
    /// Distribution of drain-batch sizes observed by the consumer.
    ///
    /// Each `drain_ready` iteration that yielded at least one item records
    /// the batch length in a powers-of-two bucket (`1, 2, 4, ..., >=1024`).
    /// A distribution skewed toward 1 means workers arrive nearly in order;
    /// a heavy tail signals head-of-line pressure.
    pub drain_batch_size_histogram: HistogramStats,
    /// Distribution of wall-clock pauses between consecutive non-empty
    /// drains, bucketed in microsecond decades (`<1, 1-10, ..., >=10000`).
    /// Long pauses correlate with the conditions that trigger
    /// `force_insert`.
    pub drain_pause_histogram: HistogramStats,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            stall_duration: Duration::ZERO,
            current_depth: 0,
            max_depth: 0,
            force_insert_count: 0,
            drain_batch_size_histogram: HistogramStats::new_pow2(),
            drain_pause_histogram: HistogramStats::new_microseconds(),
        }
    }
}

/// Collects out-of-order items and yields them in sequence order.
///
/// Each item must carry a unique sequence number starting from 0. The buffer
/// holds items that arrived ahead of their turn and releases them as soon as
/// a contiguous run from `next_expected` becomes available.
///
/// Internally uses a pre-allocated ring buffer (`Box<[Option<T>]>`) indexed
/// by `(sequence - next_expected) + head`, wrapping at capacity. This gives
/// O(1) insert and O(1) drain - a significant improvement over the previous
/// `BTreeMap`-based O(log n) insert approach.
///
/// # Capacity Bound
///
/// When the distance between a new item's sequence number and `next_expected`
/// exceeds `capacity`, [`insert`](ReorderBuffer::insert) returns
/// `Err(CapacityExceeded)`. The caller can then apply backpressure or drain
/// pending items before retrying.
///
/// # Examples
///
/// ```
/// use engine::concurrent_delta::reorder::ReorderBuffer;
///
/// let mut buf: ReorderBuffer<&str> = ReorderBuffer::new(64);
///
/// // Items arrive out of order.
/// assert!(buf.insert(1, "second").is_ok());
/// assert!(buf.next_in_order().is_none()); // waiting for seq 0
///
/// assert!(buf.insert(0, "first").is_ok());
/// assert_eq!(buf.next_in_order(), Some("first"));
/// assert_eq!(buf.next_in_order(), Some("second"));
/// assert!(buf.next_in_order().is_none());
/// ```
#[derive(Debug)]
pub struct ReorderBuffer<T> {
    /// Pre-allocated ring buffer slots.
    slots: Box<[Option<T>]>,
    /// Index into `slots` where `next_expected` sequence maps.
    head: usize,
    /// Next sequence number the consumer expects.
    next_expected: u64,
    /// Number of items currently stored in the ring buffer.
    count: usize,
    /// Maximum number of items allowed before rejecting inserts.
    capacity: usize,
    /// Highest occupied offset from `next_expected` plus one, used by the
    /// adaptive policy to size the live gap window. Reset to 0 whenever the
    /// buffer empties or the head advances past it.
    high_water_offset: usize,
    /// Optional adaptive capacity scaling state. `None` preserves the
    /// historical fixed-capacity behaviour.
    adaptive: Option<AdaptiveState>,
    /// When `true`, items pass through without sequence-based reordering.
    ///
    /// In bypass mode, [`insert`](Self::insert) appends to a FIFO queue and
    /// [`next_in_order`](Self::next_in_order) pops from its front. Sequence
    /// numbers are ignored - items are delivered in insertion order. This
    /// eliminates the O(1)-per-item ring buffer overhead when strict ordering
    /// is unnecessary (e.g., `--delay-updates` is off and files are committed
    /// immediately upon completion).
    bypass: bool,
    /// FIFO queue used in bypass mode. Empty when `bypass` is `false`.
    bypass_queue: VecDeque<T>,
    /// High-water mark of `count` observed since construction.
    max_depth: usize,
    /// Accumulated time spent stalled waiting for `next_expected` while
    /// out-of-order items were buffered.
    stall_duration: Duration,
    /// Marker recording when the current stall began. `Some` iff the buffer
    /// is currently stalled (count > 0 and the `next_expected` slot is empty).
    stall_started_at: Option<Instant>,
    /// Cumulative count of [`force_insert`](Self::force_insert) invocations.
    ///
    /// Stored behind an `Arc<AtomicU64>` so external observers (the
    /// consumer thread and any operator-facing diagnostics handle) can
    /// poll the counter without taking the metrics `Mutex`. Updates use
    /// `Relaxed` ordering because the value is purely diagnostic - no
    /// happens-before relationship with the delivered payloads is
    /// required.
    force_insert_count: Arc<AtomicU64>,
    /// Distribution of drain-batch sizes recorded via
    /// [`record_drain_batch`](Self::record_drain_batch).
    drain_batch_size: HistogramStats,
    /// Distribution of inter-drain pause durations recorded via
    /// [`record_drain_pause`](Self::record_drain_pause).
    drain_pause: HistogramStats,
}

/// Error returned when the reorder buffer is at capacity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapacityExceeded;

impl std::fmt::Display for CapacityExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("reorder buffer capacity exceeded")
    }
}

impl std::error::Error for CapacityExceeded {}

impl<T> ReorderBuffer<T> {
    /// Creates a new reorder buffer with the given capacity bound.
    ///
    /// Pre-allocates a ring buffer of `capacity` slots, trading memory for
    /// O(1) insert and drain operations.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "reorder buffer capacity must be non-zero");
        let slots: Vec<Option<T>> = (0..capacity).map(|_| None).collect();
        Self {
            slots: slots.into_boxed_slice(),
            head: 0,
            next_expected: 0,
            count: 0,
            capacity,
            high_water_offset: 0,
            adaptive: None,
            bypass: false,
            bypass_queue: VecDeque::new(),
            max_depth: 0,
            stall_duration: Duration::ZERO,
            stall_started_at: None,
            force_insert_count: Arc::new(AtomicU64::new(0)),
            drain_batch_size: HistogramStats::new_pow2(),
            drain_pause: HistogramStats::new_microseconds(),
        }
    }

    /// Creates a passthrough buffer that skips sequence-based reordering.
    ///
    /// In passthrough mode, items are delivered in insertion order regardless
    /// of their sequence numbers. This is an optimization for transfers where
    /// strict file-list ordering is unnecessary - for example, when
    /// `--delay-updates` is off and each file is committed immediately upon
    /// completion.
    ///
    /// The buffer allocates no ring buffer slots. All items flow through a
    /// lightweight FIFO queue, eliminating the per-item overhead of sequence
    /// tracking, slot indexing, and gap detection.
    ///
    /// # Examples
    ///
    /// ```
    /// use engine::concurrent_delta::reorder::ReorderBuffer;
    ///
    /// let mut buf: ReorderBuffer<&str> = ReorderBuffer::passthrough();
    ///
    /// // Items are delivered in insertion order, not sequence order.
    /// buf.insert(2, "third").unwrap();
    /// buf.insert(0, "first").unwrap();
    /// assert_eq!(buf.next_in_order(), Some("third"));
    /// assert_eq!(buf.next_in_order(), Some("first"));
    /// ```
    #[must_use]
    pub fn passthrough() -> Self {
        Self {
            slots: Vec::new().into_boxed_slice(),
            head: 0,
            next_expected: 0,
            count: 0,
            capacity: 0,
            high_water_offset: 0,
            adaptive: None,
            bypass: true,
            bypass_queue: VecDeque::new(),
            max_depth: 0,
            stall_duration: Duration::ZERO,
            stall_started_at: None,
            force_insert_count: Arc::new(AtomicU64::new(0)),
            drain_batch_size: HistogramStats::new_pow2(),
            drain_pause: HistogramStats::new_microseconds(),
        }
    }

    /// Creates a reorder buffer governed by an
    /// [`AdaptiveCapacityPolicy`].
    ///
    /// The buffer starts at `policy.min` slots and resizes between `min` and
    /// `max` based on observed pressure. See [`AdaptiveCapacityPolicy`] for
    /// the grow / shrink rules.
    ///
    /// # Panics
    ///
    /// Panics if `policy.min` is zero (validated by the policy constructor).
    #[must_use]
    pub fn with_adaptive_policy(policy: AdaptiveCapacityPolicy) -> Self {
        let mut buf = Self::new(policy.min);
        buf.adaptive = Some(AdaptiveState::new(policy));
        buf
    }

    /// Returns `true` if the buffer is in passthrough (bypass) mode.
    ///
    /// In passthrough mode, items are delivered in insertion order without
    /// sequence-based reordering.
    #[must_use]
    pub const fn is_passthrough(&self) -> bool {
        self.bypass
    }

    /// Records a depth observation against the high-water mark.
    fn observe_depth(&mut self) {
        if self.count > self.max_depth {
            self.max_depth = self.count;
        }
    }

    /// Updates the stall timer to reflect the current buffer state.
    ///
    /// The buffer is "stalled" whenever it holds at least one item but the
    /// slot for `next_expected` is empty - i.e., callers are waiting on a
    /// gap. `Instant::now()` is only invoked on the edges (stall start and
    /// stall end), keeping steady-state inserts allocation-free.
    fn refresh_stall_state(&mut self) {
        if self.bypass {
            return;
        }
        let stalled = self.count > 0 && self.slots[self.head].is_none();
        match (stalled, self.stall_started_at) {
            (true, None) => {
                self.stall_started_at = Some(Instant::now());
            }
            (false, Some(started)) => {
                self.stall_duration = self.stall_duration.saturating_add(started.elapsed());
                self.stall_started_at = None;
            }
            _ => {}
        }
    }

    /// Computes the ring buffer index for a given sequence number.
    ///
    /// Returns `None` if the sequence is behind `next_expected` or the
    /// offset from `next_expected` exceeds capacity.
    fn slot_index(&self, sequence: u64) -> Option<usize> {
        if sequence < self.next_expected {
            return None;
        }
        let offset = (sequence - self.next_expected) as usize;
        if offset >= self.capacity {
            return None;
        }
        Some((self.head + offset) % self.capacity)
    }

    /// Inserts an item with the given sequence number.
    ///
    /// If the item's sequence equals `next_expected`, it can be retrieved
    /// immediately via [`next_in_order`](Self::next_in_order). Otherwise it
    /// is buffered until all preceding items have been yielded.
    ///
    /// Returns `Err(CapacityExceeded)` if the sequence offset from
    /// `next_expected` exceeds the ring buffer capacity. The item is not
    /// consumed on error.
    ///
    /// # Errors
    ///
    /// Returns [`CapacityExceeded`] when the buffer is full or the sequence
    /// offset exceeds capacity.
    pub fn insert(&mut self, sequence: u64, item: T) -> Result<(), CapacityExceeded> {
        if self.bypass {
            self.bypass_queue.push_back(item);
            self.count += 1;
            self.observe_depth();
            return Ok(());
        }
        // Adaptive policy may grow the ring before insert to avoid the error.
        if self.adaptive.is_some() && self.slot_index(sequence).is_none() {
            self.try_adaptive_preinsert_grow(sequence);
        }
        let idx = self.slot_index(sequence).ok_or(CapacityExceeded)?;
        if self.slots[idx].is_none() {
            self.count += 1;
        }
        self.slots[idx] = Some(item);
        if sequence >= self.next_expected {
            let offset_plus_one = (sequence - self.next_expected) as usize + 1;
            if offset_plus_one > self.high_water_offset {
                self.high_water_offset = offset_plus_one;
            }
        }
        self.observe_depth();
        self.refresh_stall_state();
        self.maybe_adapt_capacity();
        Ok(())
    }

    /// Grows the ring (within policy bounds) when an incoming sequence would
    /// otherwise be rejected. Honours the policy's `max` cap; if the sequence
    /// still cannot fit, the caller's `insert` returns `CapacityExceeded`.
    fn try_adaptive_preinsert_grow(&mut self, sequence: u64) {
        let Some(state) = self.adaptive.as_ref() else {
            return;
        };
        if sequence < self.next_expected {
            return;
        }
        let needed = (sequence - self.next_expected) as usize + 1;
        let max = state.policy.max;
        if self.capacity >= max {
            return;
        }
        let target = needed.min(max);
        if target <= self.capacity {
            return;
        }
        self.grow(target);
        if let Some(state) = self.adaptive.as_mut() {
            state.grow_events += 1;
            state.reset_window();
        }
    }

    /// Records a utilization sample and applies grow / shrink decisions when
    /// an adaptive policy is configured. No-op for fixed-capacity buffers.
    fn maybe_adapt_capacity(&mut self) {
        if self.adaptive.is_none() {
            return;
        }
        let utilization = self.count as f32 / self.capacity as f32;
        let gap_window = self.high_water_offset;
        let count = self.count;
        let capacity = self.capacity;

        let (should_grow, should_shrink, target_grow, target_shrink) = {
            let state = self.adaptive.as_mut().expect("adaptive state present");
            state.record_sample(utilization);
            let grow = state.should_grow(count, capacity, gap_window);
            let shrink = !grow && state.should_shrink();
            let tg = if grow {
                state.policy.next_grow(capacity)
            } else {
                capacity
            };
            // Floor for shrink keeps every buffered item addressable.
            let floor = gap_window.max(state.policy.min);
            let ts = if shrink {
                state.policy.next_shrink(capacity, floor)
            } else {
                capacity
            };
            (grow, shrink, tg, ts)
        };

        if should_grow && target_grow > self.capacity {
            self.grow(target_grow);
            let state = self.adaptive.as_mut().expect("adaptive state present");
            state.grow_events += 1;
            state.reset_window();
        } else if should_shrink && target_shrink < self.capacity {
            self.resize_to(target_shrink);
            let state = self.adaptive.as_mut().expect("adaptive state present");
            state.shrink_events += 1;
            state.reset_window();
        }
    }

    /// Returns the next in-order item if available.
    ///
    /// Yields the item with sequence number equal to `next_expected` and
    /// advances the expected counter and head pointer. Returns `None` if
    /// that item has not yet been inserted.
    pub fn next_in_order(&mut self) -> Option<T> {
        if self.bypass {
            let item = self.bypass_queue.pop_front()?;
            self.count -= 1;
            self.next_expected += 1;
            return Some(item);
        }
        let item = self.slots[self.head].take()?;
        self.head = (self.head + 1) % self.capacity;
        self.next_expected += 1;
        self.count -= 1;
        // The high-water mark is tracked relative to next_expected, so it
        // shifts down as we deliver. Saturate at zero when the buffer empties.
        self.high_water_offset = self.high_water_offset.saturating_sub(1);
        if self.count == 0 {
            self.high_water_offset = 0;
        }
        self.refresh_stall_state();
        Some(item)
    }

    /// Drains all contiguous in-order items starting from `next_expected`.
    ///
    /// Returns an iterator that yields items as long as the next expected
    /// sequence number is present in the buffer. Useful for batch processing
    /// after inserting multiple items.
    pub fn drain_ready(&mut self) -> DrainReady<'_, T> {
        DrainReady { buffer: self }
    }

    /// Returns the next sequence number the buffer expects.
    #[must_use]
    pub const fn next_expected(&self) -> u64 {
        self.next_expected
    }

    /// Returns the number of items currently buffered (not yet yielded).
    #[must_use]
    pub const fn buffered_count(&self) -> usize {
        self.count
    }

    /// Returns `true` if no items are buffered.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Returns the capacity bound.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns the in-flight reorder window: the number of ring slots spanned
    /// between the delivery cursor ([`next_expected`](Self::next_expected)) and
    /// the furthest-ahead buffered item.
    ///
    /// This is how far out-of-order arrivals currently reach, distinct from
    /// [`buffered_count`](Self::buffered_count) (how many slots are occupied).
    /// A window approaching [`capacity`](Self::capacity) is what forces a
    /// `force_insert` or a spill, so it is the leading indicator of spill
    /// pressure - whereas the occupied count can stay low while the window
    /// stretches under a single far-ahead arrival.
    #[must_use]
    pub const fn in_flight_window(&self) -> usize {
        self.high_water_offset
    }

    /// Returns a snapshot of diagnostic counters for the buffer.
    ///
    /// The returned [`Metrics`] captures the cumulative stall duration, the
    /// instantaneous occupancy, and the high-water mark of buffered items.
    /// Operators can poll this method to surface pipeline-stall diagnostics
    /// without instrumenting the hot path themselves.
    ///
    /// If a stall is currently in progress, the time since the stall began
    /// is included in the reported duration so consumers see a monotonically
    /// non-decreasing total even between stall-end events.
    #[must_use]
    pub fn metrics(&self) -> Metrics {
        let in_flight = self
            .stall_started_at
            .map(|t| t.elapsed())
            .unwrap_or_default();
        Metrics {
            stall_duration: self.stall_duration.saturating_add(in_flight),
            current_depth: self.count,
            max_depth: self.max_depth,
            force_insert_count: self.force_insert_count.load(Ordering::Relaxed),
            drain_batch_size_histogram: self.drain_batch_size,
            drain_pause_histogram: self.drain_pause,
        }
    }

    /// Records a drain-batch observation in the metrics histogram.
    ///
    /// Called by the consumer thread after a [`drain_ready`](Self::drain_ready)
    /// iteration that yielded at least one item; `batch_size` is the
    /// number of items delivered before the next gap. Zero counts are
    /// silently dropped (no batch was produced, so the sample would
    /// distort the distribution toward an empty bucket).
    pub fn record_drain_batch(&mut self, batch_size: usize) {
        self.drain_batch_size.record_count(batch_size);
    }

    /// Records the wall-clock pause between consecutive non-empty drain
    /// iterations in the metrics histogram.
    ///
    /// Called by the consumer thread with the elapsed time since the
    /// previous successful drain. Buckets are microsecond decades so
    /// operators can read off the typical pause magnitude at a glance.
    pub fn record_drain_pause(&mut self, pause: Duration) {
        self.drain_pause.record_duration(pause);
    }

    /// Returns a shared handle to the cumulative `force_insert` counter.
    ///
    /// The returned `Arc<AtomicU64>` aliases the buffer's internal counter,
    /// so the latest value is visible to every caller holding a clone. The
    /// counter increments by exactly one each time
    /// [`force_insert`](Self::force_insert) is invoked - including when the
    /// buffer is in bypass mode and when the sequence is behind the delivery
    /// cursor (still observed so operators see stale-result diagnostics).
    /// Reads should use [`Ordering::Relaxed`] - the value is purely
    /// diagnostic.
    #[must_use]
    pub fn force_insert_counter(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.force_insert_count)
    }

    /// Returns adaptive capacity counters (grow/shrink event totals plus the
    /// current capacity). Counters are zero for fixed-capacity buffers.
    #[must_use]
    pub fn stats(&self) -> ReorderStats {
        match self.adaptive.as_ref() {
            Some(s) => ReorderStats {
                grow_events: s.grow_events,
                shrink_events: s.shrink_events,
                capacity: self.capacity,
            },
            None => ReorderStats {
                grow_events: 0,
                shrink_events: 0,
                capacity: self.capacity,
            },
        }
    }

    /// Inserts an item regardless of the capacity bound.
    ///
    /// Used by the consumer loop to break deadlocks when the buffer is full
    /// and [`drain_ready`](Self::drain_ready) cannot make progress because
    /// `next_expected` is not yet buffered. For sequences within the existing
    /// ring capacity, this inserts directly. For sequences beyond capacity,
    /// the ring is grown to accommodate the item.
    pub fn force_insert(&mut self, sequence: u64, item: T) {
        self.force_insert_count.fetch_add(1, Ordering::Relaxed);
        if self.bypass {
            self.bypass_queue.push_back(item);
            self.count += 1;
            self.observe_depth();
            return;
        }
        if let Some(idx) = self.slot_index(sequence) {
            if self.slots[idx].is_none() {
                self.count += 1;
            }
            self.slots[idx] = Some(item);
        } else if sequence >= self.next_expected {
            // Sequence exceeds current capacity - grow the ring buffer.
            let offset = (sequence - self.next_expected) as usize;
            let new_capacity = offset + 1;
            self.grow(new_capacity);
            let idx = (self.head + offset) % self.capacity;
            if self.slots[idx].is_none() {
                self.count += 1;
            }
            self.slots[idx] = Some(item);
        } else {
            // Silently ignore sequences behind next_expected (already delivered).
            return;
        }
        if sequence >= self.next_expected {
            let offset_plus_one = (sequence - self.next_expected) as usize + 1;
            if offset_plus_one > self.high_water_offset {
                self.high_water_offset = offset_plus_one;
            }
        }
        self.observe_depth();
        self.refresh_stall_state();
    }

    /// Grows the ring buffer to at least `min_capacity` slots.
    ///
    /// Linearizes the ring (head moves to index 0) during the resize. When
    /// invoked from the adaptive policy `min_capacity` already encodes the
    /// desired target; for legacy `force_insert` callers the doubling fallback
    /// preserves the original amortized growth contract.
    fn grow(&mut self, min_capacity: usize) {
        let new_cap = match self.adaptive.as_ref() {
            Some(_) => min_capacity.max(self.capacity + 1),
            None => min_capacity.max(self.capacity * 2),
        };
        self.resize_to(new_cap);
    }

    /// Reallocates the ring to exactly `new_capacity` slots, linearizing
    /// occupied entries to indices `0..count`. Caller must guarantee
    /// `new_capacity >= high_water_offset` so no buffered item is evicted.
    fn resize_to(&mut self, new_capacity: usize) {
        debug_assert!(
            new_capacity >= self.high_water_offset,
            "resize would evict buffered items: new_capacity={}, high_water={}",
            new_capacity,
            self.high_water_offset
        );
        let mut new_slots: Vec<Option<T>> = (0..new_capacity).map(|_| None).collect();
        let copy_len = self.capacity.min(new_capacity);
        for (i, slot) in new_slots.iter_mut().enumerate().take(copy_len) {
            let src = (self.head + i) % self.capacity;
            *slot = self.slots[src].take();
        }
        self.slots = new_slots.into_boxed_slice();
        self.head = 0;
        self.capacity = new_capacity;
    }

    /// Removes and returns the item at the given sequence number, if present.
    ///
    /// Unlike [`next_in_order`](Self::next_in_order), this does not advance the
    /// delivery cursor - it only removes the item from its slot. Used by the
    /// spill layer to extract items for disk serialization without changing
    /// the buffer's ordering state.
    ///
    /// Returns `None` if the sequence is outside the current window or if
    /// the slot is empty.
    pub fn take(&mut self, sequence: u64) -> Option<T> {
        if self.bypass {
            // Bypass mode does not support indexed extraction.
            return None;
        }
        let idx = self.slot_index(sequence)?;
        let item = self.slots[idx].take()?;
        self.count -= 1;
        // Do not adjust high_water_offset here - it remains valid as an
        // upper bound. The adaptive policy will naturally recalibrate.
        self.refresh_stall_state();
        Some(item)
    }

    /// Validates that all items have been delivered with no gaps in the sequence.
    ///
    /// Call this after all producers have finished and all results have been
    /// drained. Panics if any items remain in the buffer - their presence
    /// indicates a gap in the sequence (a work item was lost upstream).
    ///
    /// # Panics
    ///
    /// Panics if the buffer still contains pending items, meaning one or more
    /// sequence numbers between `next_expected` and the lowest buffered sequence
    /// were never inserted - a fatal correctness violation.
    pub fn finish(self) {
        if self.count > 0 {
            if self.bypass {
                panic!(
                    "ReorderBuffer (bypass): {} items remain undelivered",
                    self.count,
                );
            }
            // Find the first occupied slot to report the gap.
            let mut first_seq = self.next_expected;
            for i in 0..self.capacity {
                let idx = (self.head + i) % self.capacity;
                if self.slots[idx].is_some() {
                    first_seq = self.next_expected + i as u64;
                    break;
                }
            }
            panic!(
                "ReorderBuffer: sequence gap detected - expected seq {} but next buffered is seq {} \
                 ({} items stranded)",
                self.next_expected, first_seq, self.count,
            );
        }
    }
}

/// Iterator that drains contiguous in-order items from a [`ReorderBuffer`].
///
/// Created by [`ReorderBuffer::drain_ready`]. Yields items as long as the
/// next expected sequence number is present.
#[derive(Debug)]
pub struct DrainReady<'a, T> {
    buffer: &'a mut ReorderBuffer<T>,
}

impl<T> Iterator for DrainReady<'_, T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        self.buffer.next_in_order()
    }
}

#[cfg(test)]
mod tests;
