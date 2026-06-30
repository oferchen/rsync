//! Memory-cap reservation helpers for [`BufferPool`].
//!
//! Thin wrappers over the optional [`MemoryCap`](super::super::memory_cap::MemoryCap)
//! that gate acquire backpressure and release on return. Each helper is a
//! no-op when no memory cap is configured.

use super::super::allocator::BufferAllocator;
use super::BufferPool;

impl<A: BufferAllocator> BufferPool<A> {
    /// Atomically waits for and reserves `requested` bytes of capacity.
    ///
    /// When a memory cap is configured, blocks until outstanding memory
    /// plus `requested` is within the cap, then atomically increments
    /// outstanding. No-op when no cap is configured.
    pub(super) fn wait_and_reserve_memory(&self, requested: usize) {
        if let Some(cap) = &self.memory_cap {
            cap.wait_and_reserve(requested);
        }
    }

    /// Tries to atomically reserve `requested` bytes without blocking.
    ///
    /// Returns `true` unconditionally when no cap is configured.
    pub(super) fn try_reserve_memory(&self, requested: usize) -> bool {
        match &self.memory_cap {
            Some(cap) => cap.try_reserve(requested),
            None => true,
        }
    }

    /// Records that `size` bytes have been returned and wakes waiters.
    pub(super) fn track_return(&self, size: usize) {
        if let Some(cap) = &self.memory_cap {
            cap.track_return(size);
        }
    }
}
