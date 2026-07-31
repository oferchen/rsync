//! Core [`BufferPool`] implementation.
//!
//! Provides the thread-safe buffer pool backed by a lock-free
//! [`crossbeam_queue::ArrayQueue`] with a thread-local single-slot cache for
//! zero-synchronization acquire/return on the hot path. See the
//! [buffer-pool module documentation](super) for design rationale and usage
//! patterns.
//!
//! The implementation is split across focused submodules that all extend the
//! same [`BufferPool`] type:
//!
//! - [`acquire`] - the acquire/return hot path plus `admit_or_deallocate` and
//!   `pop_buffer`.
//! - [`resize`] - adaptive soft-capacity grow/shrink.
//! - [`memory`] - memory-cap reservation helpers.
//! - [`stats`] - telemetry snapshot ([`BufferPoolStats`]) and accessors.
//!
//! This hub module holds the struct definition, constructors, builder
//! methods, throughput/controller wiring, and the `Default`/`Drop` impls.

mod acquire;
mod memory;
mod resize;
mod stats;

use std::sync::atomic::{AtomicU64, AtomicUsize};

use crossbeam_queue::ArrayQueue;

use super::COPY_BUFFER_SIZE;
use super::allocator::{BufferAllocator, DefaultAllocator};
use super::buffer_controller::{AdaptiveBufferController, ControllerConfig};
use super::byte_budget::ByteBudget;
use super::memory_cap::MemoryCap;
use super::pressure::PressureTracker;
use super::throughput::ThroughputTracker;

pub use stats::BufferPoolStats;

/// Default fixed capacity for the lock-free central queue.
///
/// `ArrayQueue` requires a fixed capacity at construction time, so the
/// queue is sized to the larger of the caller-requested `max_buffers`
/// and this default. This headroom allows the adaptive resizer to grow
/// the soft capacity without having to reallocate the queue.
///
/// Matches the upper bound enforced by the adaptive resizer (`MAX_CAPACITY`
/// in `pressure.rs`). At 128 KiB per buffer (`COPY_BUFFER_SIZE`), 256
/// buffers = 32 MiB of pooled memory at the resizer's maximum soft cap.
const DEFAULT_QUEUE_CAPACITY: usize = 256;

/// Computes the fixed [`ArrayQueue`] capacity for a given soft maximum.
///
/// Returns the larger of `max_buffers` and [`DEFAULT_QUEUE_CAPACITY`], with
/// a floor of `1` because `ArrayQueue::new(0)` panics. Soft-capacity
/// enforcement (including the zero-capacity case) is handled separately
/// in `return_buffer`.
fn queue_capacity(max_buffers: usize) -> usize {
    max_buffers.max(DEFAULT_QUEUE_CAPACITY).max(1)
}

/// A thread-safe pool of reusable I/O buffers with a two-level cache.
///
/// Reduces allocation overhead during file copy operations by maintaining
/// a set of buffers that can be reused across operations. Uses a two-level
/// architecture:
///
/// 1. **Thread-local fast path** - a single-slot `thread_local!` cache per
///    thread provides zero-synchronization acquire/return for the common
///    case where each rayon worker holds one buffer at a time.
///
/// 2. **Central pool** - a lock-free [`ArrayQueue`] stores overflow buffers.
///    Acquire pops from the queue, return pushes back. Both operations
///    are wait-free in the contended case (single CAS) with no syscalls.
///    Only accessed when the thread-local slot misses (empty on acquire,
///    occupied on return).
///
/// # Capacity Enforcement
///
/// The pool has a soft maximum capacity (`max_buffers`). The underlying
/// [`ArrayQueue`] is sized at construction to hold at least
/// `DEFAULT_QUEUE_CAPACITY` buffers (or `max_buffers` if larger). The
/// soft capacity is enforced on return via an atomic
/// `compare_exchange_weak` admission counter that allows at most
/// `soft_capacity` concurrent successful admissions, so the central queue
/// never exceeds the soft cap. Thread-local cached buffers do not count
/// against this limit since they are conceptually "in use" by their thread.
///
/// # Memory Cap
///
/// An optional hard memory cap can be set via [`with_memory_cap`](Self::with_memory_cap).
/// When configured, the pool tracks outstanding (checked-out) memory and
/// blocks `acquire` calls that would exceed the cap until a buffer is
/// returned (backpressure). Use `try_acquire` / `try_acquire_from` for
/// non-blocking semantics that return `None` at the cap.
///
/// # Byte Budget
///
/// An orthogonal soft cap can be set via [`with_byte_budget`](Self::with_byte_budget).
/// The byte budget bounds total bytes of buffers *retained* in the pool
/// rather than outstanding. When admitting a returning buffer would push
/// retained bytes past the budget, the buffer is deallocated and the
/// overflow counter ([`total_byte_overflows`](Self::total_byte_overflows))
/// increments. Acquires never block; on pool miss they always allocate
/// fresh. This bounds the failure mode where a handful of large adaptive
/// buffers blow past the memory budget that a count cap alone cannot
/// express.
///
/// # Buffer Lifecycle
///
/// 1. **Acquire** - check thread-local slot, then pop from the lock-free
///    central queue, then allocate fresh.
/// 2. **Use** - caller reads/writes through the RAII guard's `Deref`/`DerefMut`.
/// 3. **Return** - guard's `Drop` impl passes the buffer back via
///    `return_buffer`, which tries the thread-local
///    slot first, then the central queue.
#[derive(Debug)]
pub struct BufferPool<A: BufferAllocator = DefaultAllocator> {
    /// Central pool of available buffers, backed by a lock-free MPMC queue.
    ///
    /// Only accessed when the thread-local cache misses. Under the typical
    /// rayon workload (one buffer per worker per file), this queue sees
    /// near-zero contention because the thread-local cache absorbs the
    /// hot path. Under heavy concurrency the queue's wait-free push/pop
    /// avoids the syscall overhead of a contended mutex.
    buffers: ArrayQueue<Vec<u8>>,
    /// Number of buffers currently held in the central queue.
    ///
    /// Maintained alongside the [`ArrayQueue`] so the soft-capacity check
    /// on return can be performed via a single `compare_exchange_weak`
    /// rather than a racy `len()` read. Decremented after each successful
    /// pop. Without this counter, multiple concurrent returns could each
    /// observe `len() < capacity` and all push, overshooting the soft cap.
    central_count: AtomicUsize,
    /// Soft maximum number of buffers to retain in the central pool.
    ///
    /// Read atomically on every return to enforce the soft cap and on
    /// every adaptive-resize evaluation. Thread-local cached buffers are
    /// not counted against this limit.
    soft_capacity: AtomicUsize,
    /// Size of each buffer in bytes.
    buffer_size: usize,
    /// Allocation strategy for creating and disposing of buffers.
    allocator: A,
    /// Optional hard memory cap with backpressure.
    memory_cap: Option<MemoryCap>,
    /// Optional soft byte budget on pool retention.
    ///
    /// When set, `admit_or_deallocate` checks this in addition to the count
    /// cap. The pool admits a buffer only when both the count slot and the
    /// byte budget have room; otherwise the buffer is deallocated and the
    /// budget's overflow counter increments. This prevents a handful of
    /// large adaptive buffers from blowing past a reasonable memory budget
    /// that a count cap alone cannot express. Acquire remains non-blocking:
    /// callers always get a buffer (fresh allocation on pool miss).
    byte_budget: Option<ByteBudget>,
    /// Optional throughput tracker for dynamic buffer sizing.
    ///
    /// When present, the pool tracks transfer throughput via EMA and uses
    /// it to recommend buffer sizes via [`recommended_buffer_size`](Self::recommended_buffer_size).
    /// The tracker is only allocated when explicitly enabled via
    /// [`with_throughput_tracking`](Self::with_throughput_tracking).
    throughput: Option<ThroughputTracker>,
    /// Optional pressure tracker for adaptive pool resizing.
    ///
    /// When present, tracks hit/miss rates and periodically adjusts the
    /// pool's soft capacity to match demand. Enabled via
    /// [`with_adaptive_resizing`](Self::with_adaptive_resizing).
    pressure: Option<PressureTracker>,
    /// Optional PID-style buffer-size controller.
    ///
    /// When present, dynamically adjusts the recommended buffer size based
    /// on throughput feedback. The controller observes bytes-per-second
    /// samples from [`record_transfer`](Self::record_transfer) and drives
    /// the buffer size toward the throughput setpoint using proportional,
    /// integral, and derivative terms. Enabled via
    /// [`with_buffer_controller`](Self::with_buffer_controller).
    ///
    /// The controller affects individual buffer sizes (the manipulated
    /// variable), while the pressure tracker affects the pool's slot count.
    /// The two loops observe orthogonal signals and compose without
    /// interference.
    buffer_controller: Option<AdaptiveBufferController>,
    /// Cumulative count of acquire operations that found a buffer in the
    /// thread-local cache or central pool (no fresh allocation needed).
    ///
    /// Always active regardless of whether adaptive resizing is enabled.
    /// Uses `Relaxed` ordering since exact precision is not required for
    /// telemetry - small counting errors under concurrent access are
    /// acceptable.
    total_hits: AtomicU64,
    /// Cumulative count of acquire operations that required a fresh
    /// allocation because no pooled buffer was available.
    ///
    /// Always active regardless of whether adaptive resizing is enabled.
    total_misses: AtomicU64,
    /// Cumulative count of pool capacity growth events.
    ///
    /// Incremented each time the adaptive resizer increases the soft
    /// capacity. Always zero when adaptive resizing is not enabled.
    total_growths: AtomicU64,
}

impl BufferPool {
    /// Creates a new buffer pool with the specified soft capacity.
    ///
    /// Uses [`DefaultAllocator`] for buffer creation. To supply a custom
    /// allocator, use [`with_allocator`](Self::with_allocator).
    ///
    /// # Arguments
    ///
    /// * `max_buffers` - Soft maximum number of buffers to retain in the
    ///   central pool. Thread-local cached buffers are additional.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let pool = BufferPool::new(num_cpus::get());
    /// ```
    #[must_use]
    pub fn new(max_buffers: usize) -> Self {
        Self {
            buffers: ArrayQueue::new(queue_capacity(max_buffers)),
            central_count: AtomicUsize::new(0),
            soft_capacity: AtomicUsize::new(max_buffers),
            buffer_size: COPY_BUFFER_SIZE,
            allocator: DefaultAllocator,
            memory_cap: None,
            byte_budget: None,
            throughput: None,
            pressure: None,
            buffer_controller: None,
            total_hits: AtomicU64::new(0),
            total_misses: AtomicU64::new(0),
            total_growths: AtomicU64::new(0),
        }
    }

    /// Creates a new buffer pool with custom buffer size.
    ///
    /// Uses [`DefaultAllocator`] for buffer creation.
    ///
    /// # Arguments
    ///
    /// * `max_buffers` - Soft maximum number of buffers to retain.
    /// * `buffer_size` - Size of each buffer in bytes.
    #[must_use]
    pub fn with_buffer_size(max_buffers: usize, buffer_size: usize) -> Self {
        Self {
            buffers: ArrayQueue::new(queue_capacity(max_buffers)),
            central_count: AtomicUsize::new(0),
            soft_capacity: AtomicUsize::new(max_buffers),
            buffer_size,
            allocator: DefaultAllocator,
            memory_cap: None,
            byte_budget: None,
            throughput: None,
            pressure: None,
            buffer_controller: None,
            total_hits: AtomicU64::new(0),
            total_misses: AtomicU64::new(0),
            total_growths: AtomicU64::new(0),
        }
    }
}

impl<A: BufferAllocator> BufferPool<A> {
    /// Creates a new buffer pool with a custom allocator.
    ///
    /// This is the fully general constructor. The allocator controls how
    /// buffers are created ([`BufferAllocator::allocate`]) and how excess
    /// buffers are disposed ([`BufferAllocator::deallocate`]).
    ///
    /// # Arguments
    ///
    /// * `max_buffers` - Maximum number of buffers to retain.
    /// * `buffer_size` - Size of each buffer in bytes.
    /// * `allocator`   - The allocation strategy to use.
    #[must_use]
    pub fn with_allocator(max_buffers: usize, buffer_size: usize, allocator: A) -> Self {
        Self {
            buffers: ArrayQueue::new(queue_capacity(max_buffers)),
            central_count: AtomicUsize::new(0),
            soft_capacity: AtomicUsize::new(max_buffers),
            buffer_size,
            allocator,
            memory_cap: None,
            byte_budget: None,
            throughput: None,
            pressure: None,
            buffer_controller: None,
            total_hits: AtomicU64::new(0),
            total_misses: AtomicU64::new(0),
            total_growths: AtomicU64::new(0),
        }
    }

    /// Sets a hard memory cap on the pool.
    ///
    /// When the total memory outstanding (buffers checked out by callers)
    /// reaches `max_bytes`, subsequent `acquire` and `acquire_from` calls
    /// block until a buffer is returned (backpressure). Use `try_acquire`
    /// or `try_acquire_from` for non-blocking semantics that return `None`
    /// when the cap is reached.
    ///
    /// The cap applies to outstanding (checked-out) buffers only. Buffers
    /// sitting idle in the pool do not count against the cap because they
    /// are available for immediate reuse without new allocation.
    ///
    /// # Panics
    ///
    /// Panics if `max_bytes` is zero.
    #[must_use]
    pub fn with_memory_cap(mut self, max_bytes: usize) -> Self {
        self.memory_cap = Some(MemoryCap::new(max_bytes));
        self
    }

    /// Sets a soft byte budget on pool retention.
    ///
    /// Caps the total bytes of buffers retained in the central pool. The
    /// effective admission cap is `min(count_cap, byte_cap)`: a buffer is
    /// admitted only when both a count slot and the byte budget have room.
    /// When either limit would be exceeded, the buffer is deallocated; the
    /// byte-budget rejection path additionally increments
    /// [`total_byte_overflows`](Self::total_byte_overflows). Acquire never
    /// blocks - callers always get a buffer (fresh allocation on pool miss).
    ///
    /// This addresses a failure mode of the count-only cap: a small number
    /// of adaptive large-file buffers (e.g. 1 MiB each) blow past any
    /// reasonable memory budget even with a modest slot count. Pairing the
    /// count slot with a byte budget bounds retained memory directly.
    ///
    /// Orthogonal to [`with_memory_cap`](Self::with_memory_cap), which sets
    /// a hard ceiling on outstanding (checked-out) memory and blocks
    /// acquires beyond the cap. Either, both, or neither may be configured.
    ///
    /// # Panics
    ///
    /// Panics if `max_bytes` is zero. Pass no byte budget at all to leave
    /// the pool uncapped on retained bytes.
    #[must_use]
    pub fn with_byte_budget(mut self, max_bytes: usize) -> Self {
        self.byte_budget = Some(ByteBudget::new(max_bytes));
        self
    }

    /// Enables throughput tracking with the default EMA smoothing factor.
    ///
    /// When enabled, the pool maintains an EMA-based throughput estimate
    /// that can be queried via [`recommended_buffer_size`](Self::recommended_buffer_size).
    /// Callers record transfer samples via [`record_transfer`](Self::record_transfer)
    /// and the pool recommends a buffer size that targets ~10 ms of data
    /// per buffer, clamped between 4 KiB and 256 KiB (or memory_cap / 4).
    ///
    /// Throughput tracking is zero-cost when not enabled - no atomic
    /// operations or memory overhead are incurred.
    #[must_use]
    pub fn with_throughput_tracking(mut self) -> Self {
        self.throughput = Some(ThroughputTracker::new());
        self
    }

    /// Enables throughput tracking with a custom EMA smoothing factor.
    ///
    /// See [`with_throughput_tracking`](Self::with_throughput_tracking) for details.
    ///
    /// # Panics
    ///
    /// Panics if `alpha` is not in `(0.0, 1.0]`.
    #[must_use]
    pub fn with_throughput_tracking_alpha(mut self, alpha: f64) -> Self {
        self.throughput = Some(ThroughputTracker::with_alpha(alpha));
        self
    }

    /// Enables adaptive resizing based on allocation pressure.
    ///
    /// When enabled, the pool tracks hit/miss rates using atomic counters
    /// and periodically adjusts its soft capacity:
    ///
    /// - **Grow**: When the miss rate exceeds 20% (too many fresh allocations),
    ///   the capacity is doubled (up to 256).
    /// - **Shrink**: When pool utilization drops below 30% and miss rate is
    ///   low, the capacity is halved (down to 2).
    ///
    /// Pressure evaluation occurs every 64 acquire operations, amortizing
    /// the cost. Between checks, only two `Relaxed` atomic increments are
    /// performed per acquire - negligible overhead on the hot path.
    ///
    /// Adaptive resizing is zero-cost when not enabled.
    #[must_use]
    pub fn with_adaptive_resizing(mut self) -> Self {
        self.pressure = Some(PressureTracker::new());
        self
    }

    /// Enables PID-style buffer-size control based on throughput feedback.
    ///
    /// When enabled, the controller dynamically adjusts the recommended
    /// buffer size returned by [`recommended_buffer_size`](Self::recommended_buffer_size)
    /// by observing throughput samples fed through [`record_transfer`](Self::record_transfer).
    /// The controller drives the buffer size toward a target throughput
    /// (the setpoint) using proportional, integral, and derivative terms,
    /// which eliminates steady-state offset and damps overshoot.
    ///
    /// This feature is orthogonal to adaptive resizing (which adjusts pool
    /// slot count) and throughput tracking (which provides the EMA estimate).
    /// The controller consumes throughput data and emits a buffer-size
    /// recommendation; the existing grow/shrink loop continues to manage
    /// pool capacity independently.
    ///
    /// Throughput tracking is automatically enabled when a buffer controller
    /// is configured, since the controller requires throughput samples.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use engine::local_copy::buffer_pool::{BufferPool, ControllerConfig};
    ///
    /// let pool = BufferPool::new(4)
    ///     .with_buffer_controller(ControllerConfig::new(100 * 1024 * 1024));
    /// ```
    #[must_use]
    pub fn with_buffer_controller(mut self, config: ControllerConfig) -> Self {
        self.buffer_controller = Some(config.build());
        // The controller requires throughput samples, so ensure tracking is on.
        if self.throughput.is_none() {
            self.throughput = Some(ThroughputTracker::new());
        }
        self
    }

    /// Records a transfer sample for throughput tracking.
    ///
    /// When throughput tracking is enabled, folds the sample into the EMA
    /// estimate. When a buffer controller is also enabled, feeds the
    /// current throughput to the PID controller so it can adjust the
    /// recommended buffer size.
    ///
    /// No-op if throughput tracking is not enabled. This method is safe
    /// to call from any thread.
    pub fn record_transfer(&self, bytes: usize, duration: std::time::Duration) {
        if let Some(tracker) = &self.throughput {
            tracker.record_transfer(bytes, duration);
            if let Some(controller) = &self.buffer_controller {
                let bps = tracker.throughput_bps();
                if bps > 0.0 {
                    controller.observe(bps as u64);
                }
            }
        }
    }

    /// Returns a recommended buffer size based on observed throughput.
    ///
    /// When a buffer controller is enabled, returns the controller's
    /// PID-driven recommendation - this supersedes the EMA-based heuristic
    /// because the controller actively drives toward the throughput setpoint
    /// and damps oscillation via its derivative term.
    ///
    /// When only throughput tracking is enabled (no controller), uses the
    /// EMA estimate to compute a buffer size targeting ~10 ms of data. The
    /// result is clamped between 4 KiB and the lesser of 256 KiB or
    /// `memory_cap / 4`.
    ///
    /// When neither is enabled, returns the pool's configured `buffer_size`.
    #[must_use]
    pub fn recommended_buffer_size(&self) -> usize {
        // PID controller takes priority when present.
        if let Some(controller) = &self.buffer_controller {
            return controller.buffer_size();
        }
        match &self.throughput {
            Some(tracker) => {
                let max = self
                    .memory_cap
                    .as_ref()
                    .map(|cap| cap.limit() / 4)
                    .unwrap_or(super::throughput::MAX_BUFFER_SIZE);
                tracker.recommended_buffer_size(max)
            }
            None => self.buffer_size,
        }
    }

    /// Returns a reference to the throughput tracker, if enabled.
    pub fn throughput_tracker(&self) -> Option<&ThroughputTracker> {
        self.throughput.as_ref()
    }

    /// Returns a reference to the buffer controller, if enabled.
    pub fn buffer_controller(&self) -> Option<&AdaptiveBufferController> {
        self.buffer_controller.as_ref()
    }

    /// Returns `true` if a PID-style buffer controller is enabled.
    #[must_use]
    pub fn has_buffer_controller(&self) -> bool {
        self.buffer_controller.is_some()
    }
}

impl Default for BufferPool<DefaultAllocator> {
    /// Creates a buffer pool sized for the host's available parallelism.
    ///
    /// Capacity is set to the number of hardware threads
    /// ([`std::thread::available_parallelism`]), falling back to 4 if
    /// detection fails. This matches the typical rayon thread pool size,
    /// ensuring one pooled buffer per worker thread. No memory cap is set.
    fn default() -> Self {
        let max_buffers = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(4);
        Self::new(max_buffers)
    }
}

impl<A: BufferAllocator> Drop for BufferPool<A> {
    /// Prints a telemetry summary to stderr when the `OC_RSYNC_BUFFER_POOL_STATS`
    /// environment variable is set to `"1"`.
    ///
    /// The env var is checked only at drop time to avoid any overhead during
    /// normal operation.
    fn drop(&mut self) {
        if std::env::var("OC_RSYNC_BUFFER_POOL_STATS").as_deref() == Ok("1") {
            let stats = self.stats();
            eprintln!(
                "BufferPool stats: reuses={} allocations={} growths={} byte_overflows={} hit_rate={:.1}%",
                stats.total_hits,
                stats.total_misses,
                stats.total_growths,
                stats.total_byte_overflows,
                self.hit_rate() * 100.0,
            );
        }
    }
}
