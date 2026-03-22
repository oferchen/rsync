//! Buffer allocation strategies for [`BufferPool`](super::BufferPool).
//!
//! The [`BufferAllocator`] trait decouples buffer creation and disposal from
//! the pool itself, enabling dependency inversion - callers can swap allocation
//! strategies (standard `Vec`, memory-mapped regions, lock-free slabs) without
//! changing pool or guard code.

/// Strategy trait for allocating and deallocating I/O buffers.
///
/// Implementations must be `Send + Sync` because the pool is shared across
/// rayon worker threads. The default implementation ([`DefaultAllocator`])
/// uses `Vec::with_capacity` followed by a zero-fill via `vec![0u8; size]`,
/// matching the original `BufferPool` behavior.
///
/// # Contract
///
/// - [`allocate`](Self::allocate) must return a `Vec<u8>` with at least `size`
///   bytes of initialized content (i.e., `len() >= size`). Callers will
///   overwrite the contents via [`Read::read`](std::io::Read::read) before
///   consuming data, so the initial byte values are irrelevant.
/// - [`deallocate`](Self::deallocate) receives a buffer that is no longer
///   needed by the pool (e.g., when the pool is at capacity). The default
///   implementation simply drops it. Custom allocators may return the memory
///   to a backing region or slab.
pub trait BufferAllocator: Send + Sync + std::fmt::Debug {
    /// Allocates a buffer of at least `size` bytes.
    ///
    /// The returned `Vec<u8>` must have `len() >= size`. Contents may be
    /// uninitialized from the caller's perspective - they will be overwritten
    /// before consumption.
    fn allocate(&self, size: usize) -> Vec<u8>;

    /// Disposes of a buffer the pool no longer needs.
    ///
    /// Called when the pool is at capacity and cannot accept a returned buffer.
    /// The default implementation drops the buffer, freeing the heap allocation.
    fn deallocate(&self, _buffer: Vec<u8>) {
        // Default: drop the buffer (release memory back to the system allocator).
    }
}

/// The default allocation strategy - zero-initialized heap `Vec<u8>`.
///
/// This reproduces the original `BufferPool` behavior: each buffer is a
/// freshly allocated `vec![0u8; size]`. Suitable for all general-purpose
/// I/O workloads.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultAllocator;

impl BufferAllocator for DefaultAllocator {
    fn allocate(&self, size: usize) -> Vec<u8> {
        vec![0u8; size]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_allocator_returns_correct_size() {
        let alloc = DefaultAllocator;
        let buf = alloc.allocate(1024);
        assert_eq!(buf.len(), 1024);
        assert!(buf.iter().all(|&b| b == 0));
    }

    #[test]
    fn default_allocator_zero_size() {
        let alloc = DefaultAllocator;
        let buf = alloc.allocate(0);
        assert!(buf.is_empty());
    }

    #[test]
    fn default_allocator_deallocate_does_not_panic() {
        let alloc = DefaultAllocator;
        let buf = alloc.allocate(4096);
        alloc.deallocate(buf);
    }

    #[test]
    fn default_allocator_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DefaultAllocator>();
    }

    /// A test-only allocator that tracks allocation count via an atomic counter.
    #[derive(Debug)]
    struct CountingAllocator {
        allocated: std::sync::atomic::AtomicUsize,
        deallocated: std::sync::atomic::AtomicUsize,
    }

    impl CountingAllocator {
        fn new() -> Self {
            Self {
                allocated: std::sync::atomic::AtomicUsize::new(0),
                deallocated: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn allocated_count(&self) -> usize {
            self.allocated.load(std::sync::atomic::Ordering::Relaxed)
        }

        fn deallocated_count(&self) -> usize {
            self.deallocated.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    impl BufferAllocator for CountingAllocator {
        fn allocate(&self, size: usize) -> Vec<u8> {
            self.allocated
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            vec![0u8; size]
        }

        fn deallocate(&self, _buffer: Vec<u8>) {
            self.deallocated
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    #[test]
    fn custom_allocator_tracks_allocations() {
        let alloc = CountingAllocator::new();
        let buf1 = alloc.allocate(256);
        let buf2 = alloc.allocate(512);
        assert_eq!(alloc.allocated_count(), 2);

        alloc.deallocate(buf1);
        assert_eq!(alloc.deallocated_count(), 1);

        alloc.deallocate(buf2);
        assert_eq!(alloc.deallocated_count(), 2);
    }
}
