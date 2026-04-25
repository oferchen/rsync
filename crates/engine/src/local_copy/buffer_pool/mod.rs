//! Thread-safe buffer pool with two-level caching for reusing I/O buffers.
//!
//! This module provides a [`BufferPool`] that reduces allocation overhead during
//! file copy operations by reusing fixed-size buffers. Buffers are automatically
//! returned to the pool when the RAII guard ([`BufferGuard`] or
//! [`BorrowedBufferGuard`]) is dropped.
//!
//! # Adaptive Buffer Sizing
//!
//! The [`adaptive_buffer_size`] function selects an appropriate I/O buffer size
//! based on the file being transferred. Small files use smaller buffers to reduce
//! memory overhead, while large files use larger buffers for better throughput.
//! Use [`BufferPool::acquire_adaptive_from`] to acquire a buffer sized to match
//! the file being transferred.
//!
//! # EMA Throughput Tracking
//!
//! When enabled via [`BufferPool::with_throughput_tracking`], the pool maintains
//! an Exponential Moving Average of observed transfer throughput and can recommend
//! dynamic buffer sizes via [`BufferPool::recommended_buffer_size`]. This targets
//! ~10 ms of data per buffer, balancing syscall overhead against memory waste.
//! The [`ThroughputTracker`] is lock-free and zero-cost when not enabled.
//!
//! # Two-Level Design
//!
//! The pool uses a two-level architecture inspired by jemalloc's tcache and
//! Go's `sync.Pool`:
//!
//! 1. **Thread-local fast path** - each thread has a single-slot cache via
//!    `thread_local!`. Acquire and return check this slot first with zero
//!    synchronization (~2 ns). This absorbs 95%+ of operations under the
//!    typical rayon workload (one buffer per worker per file).
//!
//! 2. **Central pool** - a lock-free [`crossbeam_queue::ArrayQueue`] stores
//!    overflow buffers. Only accessed on thread-local miss. Push and pop
//!    are wait-free in the contended case (single CAS) with no syscalls.
//!    The soft capacity is enforced via an atomic admission counter
//!    (`compare_exchange_weak` on every return), so the central queue
//!    never exceeds the soft cap even under burst returns from many
//!    threads.
//!
//! This design eliminates the central mutex on the hot path entirely while
//! keeping the public API stable.
//!
//! # Contention Characteristics
//!
//! Under typical rsync workloads - where rayon worker threads each process
//! one file at a time - the thread-local cache handles virtually all acquire
//! and return operations with zero synchronization. The central queue is
//! only touched during initial warm-up (first acquire per thread) and when
//! a thread returns a buffer while its local slot is occupied.
//!
//! Under high-concurrency workloads (e.g., parallel delta chunk processing),
//! the thread-local cache still absorbs the majority of operations since
//! each thread processes chunks sequentially. The lock-free queue exchanges
//! mutex acquisition for a single CAS per push/pop, removing the syscall
//! tail latency that contended mutexes incur.
//!
//! # RAII Guard Pattern
//!
//! Buffers are never handed out directly. Instead, callers receive an RAII guard
//! that derefs to `[u8]` and automatically returns the buffer to the pool on
//! drop. Two guard variants are provided:
//!
//! - [`BufferGuard`] - holds an `Arc<BufferPool>`, decoupling the buffer
//!   lifetime from the pool borrow. Use this when the pool is part of a larger
//!   struct that needs to be mutably borrowed while a buffer is checked out.
//! - [`BorrowedBufferGuard`] - borrows the pool by reference, tying the guard
//!   lifetime to the pool. Lighter weight when the borrow checker allows it.
//!
//! Both guards use an internal `Option<Vec<u8>>` with take-on-drop semantics:
//! the `Drop` impl calls `Option::take` to move the buffer out, then passes it
//! to `BufferPool::return_buffer`. This pattern ensures the buffer is returned
//! exactly once, even if the guard is dropped during a panic unwind.
//!
//! # Ownership Model
//!
//! The pool is typically wrapped in [`Arc`](std::sync::Arc) so that
//! [`BufferGuard`] instances can hold an owned reference, avoiding borrow
//! checker issues when the pool is part of a larger context struct.
//!
//! # Example
//!
//! ```ignore
//! use engine::local_copy::buffer_pool::BufferPool;
//! use std::sync::Arc;
//!
//! let pool = Arc::new(BufferPool::new(4));
//! let buffer = BufferPool::acquire_from(Arc::clone(&pool));
//! // Use buffer for I/O...
//! // Buffer automatically returned to pool on drop
//! ```

mod allocator;
mod global;
mod guard;
mod memory_cap;
mod pool;
mod pressure;
mod thread_local_cache;
/// EMA-based throughput tracker for dynamic buffer sizing.
pub mod throughput;

pub use allocator::{BufferAllocator, DefaultAllocator};
pub use global::{GlobalBufferPoolConfig, global_buffer_pool, init_global_buffer_pool};
pub use guard::{BorrowedBufferGuard, BufferGuard};
pub use pool::{BufferPool, BufferPoolStats};
pub use throughput::ThroughputTracker;

use super::COPY_BUFFER_SIZE;

/// Buffer size for files smaller than 64 KB (8 KB).
pub const ADAPTIVE_BUFFER_TINY: usize = super::ADAPTIVE_BUFFER_TINY;
/// Buffer size for files in the 64 KB .. 1 MB range (32 KB).
pub const ADAPTIVE_BUFFER_SMALL: usize = super::ADAPTIVE_BUFFER_SMALL;
/// Buffer size for files in the 1 MB .. 64 MB range (128 KB).
pub const ADAPTIVE_BUFFER_MEDIUM: usize = super::ADAPTIVE_BUFFER_MEDIUM;
/// Buffer size for files in the 64 MB .. 256 MB range (512 KB).
pub const ADAPTIVE_BUFFER_LARGE: usize = super::ADAPTIVE_BUFFER_LARGE;
/// Buffer size for files 256 MB and larger (1 MB).
pub const ADAPTIVE_BUFFER_HUGE: usize = super::ADAPTIVE_BUFFER_HUGE;

/// Selects an I/O buffer size appropriate for the given file size.
///
/// The returned size balances memory consumption against throughput:
///
/// | File size          | Buffer size |
/// |--------------------|-------------|
/// | < 64 KB            | 8 KB        |
/// | 64 KB .. < 1 MB    | 32 KB       |
/// | 1 MB .. < 64 MB    | 128 KB      |
/// | 64 MB .. < 256 MB  | 512 KB      |
/// | >= 256 MB          | 1 MB        |
///
/// # Examples
///
/// ```
/// use engine::local_copy::buffer_pool::adaptive_buffer_size;
///
/// assert_eq!(adaptive_buffer_size(1_000), 8 * 1024);
/// assert_eq!(adaptive_buffer_size(500_000), 32 * 1024);
/// assert_eq!(adaptive_buffer_size(10_000_000), 128 * 1024);
/// assert_eq!(adaptive_buffer_size(100_000_000), 512 * 1024);
/// assert_eq!(adaptive_buffer_size(1_000_000_000), 1024 * 1024);
/// ```
#[must_use]
pub const fn adaptive_buffer_size(file_size: u64) -> usize {
    super::adaptive_buffer_size(file_size)
}

#[cfg(test)]
mod tests;
