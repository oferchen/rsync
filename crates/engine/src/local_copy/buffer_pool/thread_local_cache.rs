//! Thread-local buffer cache for zero-synchronization acquire/return.
//!
//! Provides a single-slot per-thread cache that sits in front of the central
//! [`BufferPool`](super::BufferPool). When a rayon worker acquires a buffer,
//! the thread-local slot is checked first (zero synchronization, ~2 ns). On
//! return, the buffer is stored in the slot if empty, avoiding the central
//! pool's lock-free queue entirely for the common single-buffer-per-thread
//! pattern.
//!
//! # Design
//!
//! The cache holds at most **one** buffer per thread via `thread_local!` with
//! `RefCell<Option<Vec<u8>>>`. This matches the dominant workload where each
//! rayon worker holds exactly one buffer at a time (one per file or one per
//! delta chunk). Overflow (slot occupied on return) routes to the central pool.
//!
//! # Cross-Platform
//!
//! Uses only `std::thread_local!` and `std::cell::RefCell` - fully portable
//! across Linux, macOS, and Windows with zero external dependencies.

use std::cell::RefCell;

thread_local! {
    /// Per-thread buffer cache slot. Initialized empty (no allocation until first store).
    static LOCAL_BUF: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

/// Takes the cached buffer from the thread-local slot, if present.
///
/// Returns `Some(buffer)` if the slot was occupied, `None` if empty.
/// Zero synchronization - purely thread-local access.
pub(super) fn try_take() -> Option<Vec<u8>> {
    LOCAL_BUF.with(|cell| cell.borrow_mut().take())
}

/// Attempts to store a buffer in the thread-local slot.
///
/// If the slot is empty, stores the buffer and returns `None`.
/// If the slot is occupied, returns `Some(buffer)` unchanged - the caller
/// should route it to the central pool.
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
