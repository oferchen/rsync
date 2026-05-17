//! Thread-local buffer cache for zero-synchronization acquire/return.
//!
//! Provides per-thread caching that sits in front of the central
//! [`BufferPool`](super::BufferPool). When a rayon worker acquires a buffer,
//! the thread-local cache is checked first (zero synchronization, ~2 ns). On
//! return, the buffer is stored in the cache, avoiding the central pool's
//! lock-free queue entirely for the common single-buffer-per-thread pattern.
//!
//! # Design
//!
//! Two implementations are available, selected at compile time:
//!
//! - **Default (no feature)** - a single-slot `RefCell<Option<Vec<u8>>>` per
//!   thread. Matches the dominant workload where each rayon worker holds
//!   exactly one buffer at a time. Overflow (slot occupied on return) routes
//!   to the central pool.
//! - **`thread-slab-pool` feature** - a depth-bounded LIFO `Vec<Vec<u8>>` per
//!   thread (see [`super::thread_slab`]). Useful when worker threads hold
//!   multiple buffers concurrently (delta apply, prefetch pipelines) and
//!   benefit from a warmer cache without paying central-queue cursor traffic
//!   on every return.
//!
//! Both implementations expose the same [`try_take`] / [`try_store`] surface
//! so `pool.rs` is unchanged across the feature switch.
//!
//! # Cross-Platform
//!
//! Uses only `std::thread_local!` and `std::cell::RefCell` - fully portable
//! across Linux, macOS, and Windows with zero external dependencies.

#[cfg(not(feature = "thread-slab-pool"))]
use std::cell::RefCell;

#[cfg(not(feature = "thread-slab-pool"))]
thread_local! {
    /// Per-thread buffer cache slot. Initialized empty (no allocation until first store).
    static LOCAL_BUF: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

/// Takes the cached buffer from this thread's cache, if any.
///
/// Default build: pops the single TLS slot. With `thread-slab-pool`: pops
/// the warmest entry from the per-thread LIFO slab.
#[cfg(not(feature = "thread-slab-pool"))]
pub(super) fn try_take() -> Option<Vec<u8>> {
    LOCAL_BUF.with(|cell| cell.borrow_mut().take())
}

/// Stores a buffer in this thread's cache.
///
/// Default build: stores into the single TLS slot if empty, otherwise
/// returns `Some(buf)` so the caller routes to the central pool. With
/// `thread-slab-pool`: pushes onto the per-thread slab if both slot and
/// byte caps allow, otherwise returns `Some(buf)` for the same routing.
#[cfg(not(feature = "thread-slab-pool"))]
pub(super) fn try_store(buf: Vec<u8>) -> Option<Vec<u8>> {
    LOCAL_BUF.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Some(buf);
            None
        } else {
            Some(buf)
        }
    })
}

#[cfg(feature = "thread-slab-pool")]
pub(super) fn try_take() -> Option<Vec<u8>> {
    super::thread_slab::try_take()
}

#[cfg(feature = "thread-slab-pool")]
pub(super) fn try_store(buf: Vec<u8>) -> Option<Vec<u8>> {
    super::thread_slab::try_store(buf)
}
