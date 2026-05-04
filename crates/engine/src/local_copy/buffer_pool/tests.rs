use super::*;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::thread;

#[test]
fn test_acquire_returns_buffer() {
    let pool = BufferPool::new(4);
    let buffer = pool.acquire();
    assert_eq!(buffer.len(), COPY_BUFFER_SIZE);
}

#[test]
fn test_buffer_reuse() {
    let pool = BufferPool::new(4);

    {
        let mut buffer = pool.acquire();
        buffer[0] = 42;
    }

    // First return on this thread goes to the thread-local cache, not the
    // central pool. Re-acquire should get the reused buffer from TLS.
    let buffer = pool.acquire();
    assert_eq!(buffer.len(), COPY_BUFFER_SIZE);
}

#[test]
fn test_pool_capacity_limit() {
    let pool = BufferPool::new(2);

    let b1 = pool.acquire();
    let b2 = pool.acquire();
    let b3 = pool.acquire();

    drop(b1);
    drop(b2);
    drop(b3);

    // Pool capacity is 2, so only 2 of the 3 returned buffers are retained.
    assert_eq!(pool.available(), 2);
}

#[test]
fn test_concurrent_access() {
    let pool = Arc::new(BufferPool::new(8));
    let mut handles = vec![];

    for _ in 0..16 {
        let pool = Arc::clone(&pool);
        handles.push(thread::spawn(move || {
            for _ in 0..100 {
                let mut buffer = pool.acquire();
                buffer[0] = 1;
            }
        }));
    }

    for handle in handles {
        handle.join().expect("thread panicked");
    }

    assert!(pool.available() <= 8);
}

#[test]
fn test_buffer_guard_deref() {
    let pool = BufferPool::new(4);
    let mut buffer = pool.acquire();

    buffer[0] = 100;
    buffer[1] = 200;

    assert_eq!(buffer[0], 100);
    assert_eq!(buffer[1], 200);

    let slice: &[u8] = &buffer;
    assert_eq!(slice[0], 100);
}

#[test]
fn test_buffer_guard_as_mut_slice() {
    let pool = BufferPool::new(4);
    let mut buffer = pool.acquire();

    let slice = buffer.as_mut_slice();
    slice[0] = 42;

    assert_eq!(buffer[0], 42);
}

#[test]
fn test_custom_buffer_size() {
    let pool = BufferPool::with_buffer_size(4, 1024);
    let buffer = pool.acquire();
    assert_eq!(buffer.len(), 1024);
    assert_eq!(pool.buffer_size(), 1024);
}

#[test]
fn test_default_pool() {
    let pool = BufferPool::default();
    assert!(pool.max_buffers() > 0);
    assert_eq!(pool.buffer_size(), COPY_BUFFER_SIZE);
}

#[test]
fn test_buffer_length_restored_on_return() {
    let pool = BufferPool::new(4);

    {
        let mut buffer = pool.acquire();
        for byte in buffer.iter_mut() {
            *byte = 0xFF;
        }
    }

    // Length should be restored on re-acquire (contents are stale but will
    // be overwritten by Read::read before consumption).
    let buffer = pool.acquire();
    assert_eq!(buffer.len(), COPY_BUFFER_SIZE);
}

#[test]
fn adaptive_size_zero_byte_file() {
    assert_eq!(adaptive_buffer_size(0), ADAPTIVE_BUFFER_TINY);
}

#[test]
fn adaptive_size_one_byte_file() {
    assert_eq!(adaptive_buffer_size(1), ADAPTIVE_BUFFER_TINY);
}

#[test]
fn adaptive_size_tiny_file() {
    // A 1 KB file should get an 8 KB buffer.
    assert_eq!(adaptive_buffer_size(1024), ADAPTIVE_BUFFER_TINY);
}

#[test]
fn adaptive_size_just_below_tiny_threshold() {
    // 64 KB - 1 byte: still in the tiny range.
    assert_eq!(adaptive_buffer_size(64 * 1024 - 1), ADAPTIVE_BUFFER_TINY);
}

#[test]
fn adaptive_size_at_tiny_threshold() {
    // Exactly 64 KB: enters the small range.
    assert_eq!(adaptive_buffer_size(64 * 1024), ADAPTIVE_BUFFER_SMALL);
}

#[test]
fn adaptive_size_small_file() {
    // 500 KB file should get a 32 KB buffer.
    assert_eq!(adaptive_buffer_size(500 * 1024), ADAPTIVE_BUFFER_SMALL);
}

#[test]
fn adaptive_size_just_below_small_threshold() {
    // 1 MB - 1 byte: still in the small range.
    assert_eq!(adaptive_buffer_size(1024 * 1024 - 1), ADAPTIVE_BUFFER_SMALL);
}

#[test]
fn adaptive_size_at_small_threshold() {
    // Exactly 1 MB: enters the medium range.
    assert_eq!(adaptive_buffer_size(1024 * 1024), ADAPTIVE_BUFFER_MEDIUM);
}

#[test]
fn adaptive_size_medium_file() {
    // 10 MB file should get a 128 KB buffer.
    assert_eq!(
        adaptive_buffer_size(10 * 1024 * 1024),
        ADAPTIVE_BUFFER_MEDIUM
    );
}

#[test]
fn adaptive_size_just_below_medium_threshold() {
    // 64 MB - 1 byte: still in the medium range.
    assert_eq!(
        adaptive_buffer_size(64 * 1024 * 1024 - 1),
        ADAPTIVE_BUFFER_MEDIUM
    );
}

#[test]
fn adaptive_size_at_medium_threshold() {
    // Exactly 64 MB: enters the large range.
    assert_eq!(
        adaptive_buffer_size(64 * 1024 * 1024),
        ADAPTIVE_BUFFER_LARGE
    );
}

#[test]
fn adaptive_size_large_file() {
    // 100 MB file should get a 512 KB buffer.
    assert_eq!(
        adaptive_buffer_size(100 * 1024 * 1024),
        ADAPTIVE_BUFFER_LARGE
    );
}

#[test]
fn adaptive_size_at_large_threshold() {
    // Exactly 256 MB: enters the huge range.
    assert_eq!(
        adaptive_buffer_size(256 * 1024 * 1024),
        ADAPTIVE_BUFFER_HUGE
    );
}

#[test]
fn adaptive_size_very_large_file() {
    // 1 GB file should get a 1 MB buffer.
    assert_eq!(
        adaptive_buffer_size(1024 * 1024 * 1024),
        ADAPTIVE_BUFFER_HUGE
    );
}

#[test]
fn adaptive_size_huge_file() {
    // 100 GB file should get a 1 MB buffer.
    assert_eq!(
        adaptive_buffer_size(100 * 1024 * 1024 * 1024),
        ADAPTIVE_BUFFER_HUGE
    );
}

#[test]
fn adaptive_size_max_u64() {
    // Maximum possible file size should still return the huge buffer.
    assert_eq!(adaptive_buffer_size(u64::MAX), ADAPTIVE_BUFFER_HUGE);
}

#[test]
fn adaptive_size_monotonically_non_decreasing() {
    // Buffer sizes should never decrease as file size increases.
    let file_sizes: Vec<u64> = vec![
        0,
        1,
        1024,
        64 * 1024 - 1,
        64 * 1024,
        512 * 1024,
        1024 * 1024 - 1,
        1024 * 1024,
        32 * 1024 * 1024,
        64 * 1024 * 1024 - 1,
        64 * 1024 * 1024,
        256 * 1024 * 1024 - 1,
        256 * 1024 * 1024,
        1024 * 1024 * 1024,
    ];
    let mut prev_size = 0;
    for &file_size in &file_sizes {
        let buf_size = adaptive_buffer_size(file_size);
        assert!(
            buf_size >= prev_size,
            "buffer size decreased from {prev_size} to {buf_size} at file size {file_size}"
        );
        prev_size = buf_size;
    }
}

#[test]
fn adaptive_size_constants_are_powers_of_two() {
    // I/O buffers should be powers of two for optimal alignment.
    assert!(ADAPTIVE_BUFFER_TINY.is_power_of_two());
    assert!(ADAPTIVE_BUFFER_SMALL.is_power_of_two());
    assert!(ADAPTIVE_BUFFER_MEDIUM.is_power_of_two());
    assert!(ADAPTIVE_BUFFER_LARGE.is_power_of_two());
    assert!(ADAPTIVE_BUFFER_HUGE.is_power_of_two());
}

#[test]
#[allow(clippy::assertions_on_constants)]
fn adaptive_size_constants_ordered() {
    assert!(ADAPTIVE_BUFFER_TINY < ADAPTIVE_BUFFER_SMALL);
    assert!(ADAPTIVE_BUFFER_SMALL < ADAPTIVE_BUFFER_MEDIUM);
    assert!(ADAPTIVE_BUFFER_MEDIUM < ADAPTIVE_BUFFER_LARGE);
    assert!(ADAPTIVE_BUFFER_LARGE < ADAPTIVE_BUFFER_HUGE);
}

#[test]
fn adaptive_size_medium_equals_default_buffer() {
    // The medium adaptive size should match the default COPY_BUFFER_SIZE
    // so the pool can reuse buffers for medium-sized files.
    assert_eq!(ADAPTIVE_BUFFER_MEDIUM, COPY_BUFFER_SIZE);
}

#[test]
fn acquire_adaptive_from_uses_pool_for_medium_files() {
    // For files in the medium range, the adaptive size matches the pool's
    // default buffer size, so the buffer should be reused via TLS.
    let pool = Arc::new(BufferPool::new(4));

    // Pre-populate TLS with a buffer (first return goes to TLS).
    {
        let _buffer = BufferPool::acquire_from(Arc::clone(&pool));
    }

    // Acquire adaptively for a medium file - should reuse via TLS fast path.
    let buffer = BufferPool::acquire_adaptive_from(Arc::clone(&pool), 10 * 1024 * 1024);
    assert_eq!(buffer.len(), COPY_BUFFER_SIZE);
}

#[test]
fn acquire_adaptive_from_allocates_small_buffer_for_tiny_file() {
    let pool = Arc::new(BufferPool::new(4));
    let buffer = BufferPool::acquire_adaptive_from(Arc::clone(&pool), 1024);
    assert_eq!(buffer.len(), ADAPTIVE_BUFFER_TINY);
}

#[test]
fn acquire_adaptive_from_allocates_small_buffer_for_small_file() {
    let pool = Arc::new(BufferPool::new(4));
    let buffer = BufferPool::acquire_adaptive_from(Arc::clone(&pool), 500 * 1024);
    assert_eq!(buffer.len(), ADAPTIVE_BUFFER_SMALL);
}

#[test]
fn acquire_adaptive_from_allocates_large_buffer_for_large_file() {
    let pool = Arc::new(BufferPool::new(4));
    let buffer = BufferPool::acquire_adaptive_from(Arc::clone(&pool), 100 * 1024 * 1024);
    assert_eq!(buffer.len(), ADAPTIVE_BUFFER_LARGE);
}

#[test]
fn acquire_adaptive_from_returns_buffer_to_pool() {
    // Verify that adaptively-sized buffers are returned and resized to
    // pool default. First return goes to TLS, so verify via re-acquire.
    let pool = Arc::new(BufferPool::new(4));

    {
        let _buffer = BufferPool::acquire_adaptive_from(Arc::clone(&pool), 1024);
    }

    // Buffer returned to TLS (resized to pool default); re-acquire gets it.
    let buffer = BufferPool::acquire_from(Arc::clone(&pool));
    assert_eq!(buffer.len(), COPY_BUFFER_SIZE);
}

#[test]
fn acquire_adaptive_from_large_buffer_returns_resized() {
    // Even a 512 KB buffer gets resized to the default on return.
    let pool = Arc::new(BufferPool::new(4));
    {
        let buffer = BufferPool::acquire_adaptive_from(Arc::clone(&pool), 100 * 1024 * 1024);
        assert_eq!(buffer.len(), ADAPTIVE_BUFFER_LARGE);
    }

    // Buffer returned to TLS (resized to pool default); re-acquire gets it.
    let buffer = BufferPool::acquire_from(Arc::clone(&pool));
    assert_eq!(buffer.len(), COPY_BUFFER_SIZE);
}

#[test]
fn pool_reuses_buffers_under_sequential_pressure() {
    // Allocate and return many buffers sequentially on one thread.
    // With TLS, the single buffer cycles through the thread-local cache
    // on every iteration - the central pool may not be touched at all.
    let pool = Arc::new(BufferPool::new(4));

    for _ in 0..1_000 {
        let mut buf = BufferPool::acquire_from(Arc::clone(&pool));
        buf[0] = 0xAB;
    }

    // Central pool holds at most max_buffers. The hot buffer lives in TLS.
    assert!(pool.available() <= 4);
}

#[test]
fn pool_size_stays_bounded_under_burst_allocation() {
    // Acquire many buffers simultaneously (simulating a burst), then
    // release them all. The pool must not grow beyond max_buffers.
    let max = 4;
    let pool = Arc::new(BufferPool::new(max));

    let guards: Vec<_> = (0..64)
        .map(|_| BufferPool::acquire_from(Arc::clone(&pool)))
        .collect();

    assert_eq!(pool.available(), 0);

    // After dropping all guards only max_buffers should be retained.
    drop(guards);
    assert_eq!(pool.available(), max);
}

#[test]
fn empty_pool_allocates_fresh_buffer() {
    // When no buffers are available the pool creates a new one
    // rather than blocking.
    let pool = Arc::new(BufferPool::new(2));
    assert_eq!(pool.available(), 0);

    let buf = BufferPool::acquire_from(Arc::clone(&pool));
    assert_eq!(buf.len(), COPY_BUFFER_SIZE);
    // Pool is still empty because the buffer is checked out.
    assert_eq!(pool.available(), 0);
}

#[test]
fn drop_returns_buffer_to_pool() {
    // Verify the BufferGuard Drop impl returns the buffer for reuse.
    // First return on a thread goes to TLS, so we verify reuse via
    // a subsequent acquire rather than pool.available().
    let pool = Arc::new(BufferPool::new(4));
    assert_eq!(pool.available(), 0);

    let guard = BufferPool::acquire_from(Arc::clone(&pool));
    assert_eq!(pool.available(), 0);

    drop(guard);
    // Buffer is in TLS, not central pool.

    // Verify reuse: next acquire should get the buffer from TLS.
    let buf = BufferPool::acquire_from(Arc::clone(&pool));
    assert_eq!(buf.len(), COPY_BUFFER_SIZE);
}

#[test]
fn borrowed_guard_drop_returns_buffer_to_pool() {
    // Same verification for BorrowedBufferGuard.
    let pool = BufferPool::new(4);
    assert_eq!(pool.available(), 0);

    let guard = pool.acquire();
    assert_eq!(pool.available(), 0);

    drop(guard);
    // Buffer is in TLS, not central pool.

    // Verify reuse: next acquire should get the buffer from TLS.
    let buf = pool.acquire();
    assert_eq!(buf.len(), COPY_BUFFER_SIZE);
}

#[test]
fn concurrent_checkout_return_from_multiple_threads() {
    // Hammer the pool from many threads with rapid acquire/release
    // cycles. Validates absence of deadlocks, data races, and that
    // the pool invariant (available <= max_buffers) always holds.
    let pool = Arc::new(BufferPool::new(8));
    let iterations = 500;
    let thread_count = 16;

    let handles: Vec<_> = (0..thread_count)
        .map(|id| {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                for i in 0..iterations {
                    let mut buf = BufferPool::acquire_from(Arc::clone(&pool));
                    // Recognizable pattern to detect cross-thread corruption.
                    buf[0] = (id & 0xFF) as u8;
                    buf[1] = (i & 0xFF) as u8;
                    assert_eq!(buf[0], (id & 0xFF) as u8);
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("worker thread panicked");
    }

    assert!(pool.available() <= 8);
}

#[test]
fn concurrent_mixed_guard_types() {
    // Exercise both Arc-based and borrow-based guards from threads.
    // The borrowed guard can only be used within a single thread
    // (lifetime tied to pool), but we test that concurrent Arc-based
    // and sequential borrow-based access both work correctly.
    let pool = Arc::new(BufferPool::new(4));

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                for _ in 0..200 {
                    let _buf = BufferPool::acquire_from(Arc::clone(&pool));
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked");
    }

    assert!(pool.available() <= 4);

    let available_before = pool.available();
    {
        let _buf = pool.acquire();
    }
    // After return, available count should be at least what it was before.
    assert!(pool.available() >= available_before);
}

#[test]
fn concurrent_held_buffers_force_new_allocations() {
    // Hold some buffers while other threads acquire and release.
    // Verifies the pool allocates fresh buffers when empty and that
    // held guards do not interfere with new acquisitions.
    let pool = Arc::new(BufferPool::new(2));

    // Hold 2 buffers on the main thread, exhausting the pool.
    let _held1 = BufferPool::acquire_from(Arc::clone(&pool));
    let _held2 = BufferPool::acquire_from(Arc::clone(&pool));
    assert_eq!(pool.available(), 0);

    // Spawn threads that acquire and release buffers - they all get
    // fresh allocations since the pool is empty and 2 buffers are held.
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                for _ in 0..100 {
                    let mut buf = BufferPool::acquire_from(Arc::clone(&pool));
                    buf[0] = 0xFF;
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked");
    }

    // Pool accepted returns up to max_buffers while threads ran.
    assert!(pool.available() <= 2);

    // Release held buffers. Pool is already at capacity so excess
    // buffers are dropped.
    drop(_held1);
    drop(_held2);
    assert!(pool.available() <= 2);
}

#[test]
fn adaptive_buffers_returned_under_concurrent_pressure() {
    // Mix adaptive and default-sized buffer acquisitions concurrently.
    // All returned buffers should be resized to pool default.
    let pool = Arc::new(BufferPool::new(8));

    let file_sizes: Vec<u64> = vec![
        512,               // tiny
        100 * 1024,        // small
        10 * 1024 * 1024,  // medium (matches pool default)
        100 * 1024 * 1024, // large
        500 * 1024 * 1024, // huge
    ];

    let handles: Vec<_> = file_sizes
        .into_iter()
        .map(|size| {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                for _ in 0..100 {
                    let buf = BufferPool::acquire_adaptive_from(Arc::clone(&pool), size);
                    assert!(!buf.is_empty());
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked");
    }

    // With TLS + lock-free ArrayQueue, concurrent returns go through each
    // thread's TLS slot first, then to the central queue (soft capacity
    // enforced via an atomic length check). The important invariant is
    // that all retained buffers have the correct size.

    // Every buffer in the pool should now be at the default size.
    for _ in 0..pool.available() {
        let buf = BufferPool::acquire_from(Arc::clone(&pool));
        assert_eq!(buf.len(), COPY_BUFFER_SIZE);
    }
}

#[test]
fn repeated_acquire_release_cycle_reuses_same_buffers() {
    // Verify the pool actually recycles buffers by checking that the
    // buffer count stabilizes. With TLS, one buffer lives in thread-local
    // cache and the rest in the central pool.
    let pool = Arc::new(BufferPool::new(2));

    // First cycle - buffers are freshly allocated.
    {
        let _a = BufferPool::acquire_from(Arc::clone(&pool));
        let _b = BufferPool::acquire_from(Arc::clone(&pool));
    }
    // Drop order: _b first (reverse decl), then _a.
    // _b → TLS, _a → TLS full → central pool.
    assert_eq!(pool.available(), 1);

    // Second cycle - _a comes from TLS, _b from central pool.
    {
        let _a = BufferPool::acquire_from(Arc::clone(&pool));
        assert_eq!(pool.available(), 1);
        let _b = BufferPool::acquire_from(Arc::clone(&pool));
        assert_eq!(pool.available(), 0);
    }
    // All returned: 1 in TLS + 1 in central.
    assert_eq!(pool.available(), 1);

    // After 100 more cycles the central pool still holds 1 (+ 1 in TLS).
    for _ in 0..100 {
        let _a = BufferPool::acquire_from(Arc::clone(&pool));
        let _b = BufferPool::acquire_from(Arc::clone(&pool));
    }
    assert_eq!(pool.available(), 1);
}

#[test]
fn zero_capacity_pool_never_retains_buffers() {
    // Edge case: a pool with max_buffers=0 always allocates fresh
    // buffers and never retains returned ones.
    let pool = Arc::new(BufferPool::new(0));

    {
        let buf = BufferPool::acquire_from(Arc::clone(&pool));
        assert_eq!(buf.len(), COPY_BUFFER_SIZE);
    }
    assert_eq!(pool.available(), 0);

    // Even after many cycles, nothing is retained.
    for _ in 0..50 {
        let _buf = BufferPool::acquire_from(Arc::clone(&pool));
    }
    assert_eq!(pool.available(), 0);
}

#[test]
fn single_capacity_pool_reuses_one_buffer() {
    // A pool with capacity 1. With TLS, effective single-thread capacity
    // is 1 (TLS) + 1 (central) = 2 retained buffers.
    let pool = Arc::new(BufferPool::new(1));

    {
        let _buf = BufferPool::acquire_from(Arc::clone(&pool));
    }
    // Buffer went to TLS, central pool is empty.
    assert_eq!(pool.available(), 0);

    // Acquire two simultaneously.
    let a = BufferPool::acquire_from(Arc::clone(&pool)); // from TLS
    assert_eq!(pool.available(), 0);
    let b = BufferPool::acquire_from(Arc::clone(&pool)); // fresh alloc
    assert_eq!(pool.available(), 0);

    // Drop both: a → TLS, b → TLS full → central (cap=1, accepts).
    drop(a);
    drop(b);
    assert_eq!(pool.available(), 1);

    // Acquire 3: a from TLS, b from central, c fresh.
    let a = BufferPool::acquire_from(Arc::clone(&pool));
    let b = BufferPool::acquire_from(Arc::clone(&pool));
    let c = BufferPool::acquire_from(Arc::clone(&pool));
    assert_eq!(pool.available(), 0);

    // Drop all 3: a → TLS, b → central (cap=1), c → TLS full + central full → dealloc.
    drop(a);
    drop(b);
    drop(c);
    assert_eq!(pool.available(), 1);
}

/// A test-only allocator that counts allocations and deallocations.
#[derive(Debug)]
struct TrackingAllocator {
    alloc_count: std::sync::atomic::AtomicUsize,
    dealloc_count: std::sync::atomic::AtomicUsize,
}

impl TrackingAllocator {
    fn new() -> Self {
        Self {
            alloc_count: std::sync::atomic::AtomicUsize::new(0),
            dealloc_count: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn alloc_count(&self) -> usize {
        self.alloc_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn dealloc_count(&self) -> usize {
        self.dealloc_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl BufferAllocator for TrackingAllocator {
    fn allocate(&self, size: usize) -> Vec<u8> {
        self.alloc_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        vec![0u8; size]
    }

    fn deallocate(&self, _buffer: Vec<u8>) {
        self.dealloc_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

#[test]
fn with_allocator_uses_custom_allocator() {
    let pool = BufferPool::with_allocator(4, 1024, TrackingAllocator::new());
    assert_eq!(pool.buffer_size(), 1024);
    assert_eq!(pool.allocator().alloc_count(), 0);

    let buf = pool.acquire();
    assert_eq!(buf.len(), 1024);
    assert_eq!(pool.allocator().alloc_count(), 1);
}

#[test]
fn custom_allocator_deallocate_called_on_overflow() {
    // Pool with capacity 1. With TLS, need 3 returns to trigger overflow:
    // 1st → TLS, 2nd → central (cap=1), 3rd → TLS full + central full → dealloc.
    let pool = Arc::new(BufferPool::with_allocator(1, 512, TrackingAllocator::new()));

    let a = BufferPool::acquire_from(Arc::clone(&pool));
    let b = BufferPool::acquire_from(Arc::clone(&pool));
    let c = BufferPool::acquire_from(Arc::clone(&pool));
    assert_eq!(pool.allocator().alloc_count(), 3);

    drop(a); // → TLS
    assert_eq!(pool.available(), 0);
    assert_eq!(pool.allocator().dealloc_count(), 0);

    drop(b); // TLS full → central (cap=1, accepts)
    assert_eq!(pool.available(), 1);
    assert_eq!(pool.allocator().dealloc_count(), 0);

    drop(c); // TLS full, central full → deallocate
    assert_eq!(pool.available(), 1);
    assert_eq!(pool.allocator().dealloc_count(), 1);
}

#[test]
fn custom_allocator_with_arc_guards() {
    let pool = Arc::new(BufferPool::with_allocator(
        4,
        2048,
        TrackingAllocator::new(),
    ));

    {
        let mut buf = BufferPool::acquire_from(Arc::clone(&pool));
        buf[0] = 0xAB;
        assert_eq!(buf[0], 0xAB);
    }

    // Buffer returned to TLS, not central pool.
    assert_eq!(pool.allocator().alloc_count(), 1);
}

#[test]
fn custom_allocator_adaptive_acquire() {
    let pool = Arc::new(BufferPool::with_allocator(
        4,
        COPY_BUFFER_SIZE,
        TrackingAllocator::new(),
    ));

    // Tiny file - non-standard size, allocator should be used
    let buf = BufferPool::acquire_adaptive_from(Arc::clone(&pool), 1024);
    assert_eq!(buf.len(), ADAPTIVE_BUFFER_TINY);
    assert_eq!(pool.allocator().alloc_count(), 1);
}

#[test]
fn allocator_accessor_returns_reference() {
    let pool = BufferPool::with_allocator(2, 256, TrackingAllocator::new());
    let _alloc: &TrackingAllocator = pool.allocator();
    assert_eq!(_alloc.alloc_count(), 0);
}

#[test]
fn no_memory_cap_by_default() {
    let pool = BufferPool::new(4);
    assert_eq!(pool.memory_cap(), None);
    assert_eq!(pool.memory_usage(), 0);
}

#[test]
fn memory_cap_is_set() {
    let pool = BufferPool::with_buffer_size(4, 1024).with_memory_cap(4096);
    assert_eq!(pool.memory_cap(), Some(4096));
}

#[test]
fn memory_usage_tracks_outstanding_buffers() {
    let pool = BufferPool::with_buffer_size(4, 1024).with_memory_cap(8192);
    assert_eq!(pool.memory_usage(), 0);

    let buf1 = pool.acquire();
    assert_eq!(pool.memory_usage(), 1024);

    let buf2 = pool.acquire();
    assert_eq!(pool.memory_usage(), 2048);

    drop(buf1);
    assert_eq!(pool.memory_usage(), 1024);

    drop(buf2);
    assert_eq!(pool.memory_usage(), 0);
}

#[test]
fn allocation_under_cap_succeeds() {
    // Cap allows 4 buffers of 1024 bytes each.
    let pool = BufferPool::with_buffer_size(8, 1024).with_memory_cap(4096);

    let b1 = pool.acquire();
    let b2 = pool.acquire();
    let b3 = pool.acquire();
    let b4 = pool.acquire();

    assert_eq!(pool.memory_usage(), 4096);
    assert_eq!(b1.len(), 1024);
    assert_eq!(b2.len(), 1024);
    assert_eq!(b3.len(), 1024);
    assert_eq!(b4.len(), 1024);
}

#[test]
fn try_acquire_returns_none_at_cap() {
    // Cap allows exactly 2 buffers of 1024 bytes.
    let pool = BufferPool::with_buffer_size(4, 1024).with_memory_cap(2048);

    let _b1 = pool.acquire();
    let _b2 = pool.acquire();
    assert_eq!(pool.memory_usage(), 2048);

    // At cap - try_acquire should return None.
    assert!(pool.try_acquire().is_none());
}

#[test]
fn try_acquire_succeeds_after_return() {
    let pool = BufferPool::with_buffer_size(4, 1024).with_memory_cap(2048);

    let b1 = pool.acquire();
    let _b2 = pool.acquire();

    // At cap.
    assert!(pool.try_acquire().is_none());

    // Return one buffer.
    drop(b1);
    assert_eq!(pool.memory_usage(), 1024);

    // Now try_acquire should succeed.
    let b3 = pool.try_acquire();
    assert!(b3.is_some());
    assert_eq!(pool.memory_usage(), 2048);
}

#[test]
fn try_acquire_from_returns_none_at_cap() {
    let pool = Arc::new(BufferPool::with_buffer_size(4, 1024).with_memory_cap(1024));

    let _b1 = BufferPool::acquire_from(Arc::clone(&pool));
    assert!(BufferPool::try_acquire_from(Arc::clone(&pool)).is_none());
}

#[test]
fn acquire_blocks_then_succeeds_on_return() {
    // Cap allows exactly 1 buffer. Acquire one, then spawn a thread
    // that acquires (blocks). Return the buffer from the main thread
    // to unblock.
    let pool = Arc::new(BufferPool::with_buffer_size(4, 1024).with_memory_cap(1024));

    let b1 = BufferPool::acquire_from(Arc::clone(&pool));
    assert_eq!(pool.memory_usage(), 1024);

    let pool2 = Arc::clone(&pool);
    let handle = thread::spawn(move || {
        // This should block until the main thread drops b1.
        let buf = BufferPool::acquire_from(pool2);
        assert_eq!(buf.len(), 1024);
    });

    // Give the spawned thread time to reach the blocking acquire.
    thread::sleep(std::time::Duration::from_millis(50));

    // Return the buffer - this should unblock the waiting thread.
    drop(b1);

    handle.join().expect("blocking acquire thread panicked");
    assert_eq!(pool.memory_usage(), 0);
}

#[test]
fn memory_cap_with_concurrent_pressure() {
    // Cap allows 4 buffers of 1024 bytes. 8 threads compete.
    let pool = Arc::new(BufferPool::with_buffer_size(8, 1024).with_memory_cap(4096));

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                for _ in 0..100 {
                    let mut buf = BufferPool::acquire_from(Arc::clone(&pool));
                    buf[0] = 0xAB;
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked");
    }

    assert_eq!(pool.memory_usage(), 0);
}

#[test]
fn memory_cap_with_builder_chain() {
    let pool = BufferPool::with_allocator(4, 512, TrackingAllocator::new()).with_memory_cap(2048);

    assert_eq!(pool.memory_cap(), Some(2048));
    assert_eq!(pool.buffer_size(), 512);

    let b1 = pool.acquire();
    assert_eq!(pool.allocator().alloc_count(), 1);
    assert_eq!(pool.memory_usage(), 512);
    drop(b1);
}

#[test]
#[should_panic(expected = "memory cap must be greater than zero")]
fn memory_cap_zero_panics() {
    let _ = BufferPool::new(4).with_memory_cap(0);
}

#[test]
fn memory_usage_without_cap_is_zero() {
    // Without a cap, memory_usage always returns 0 (no tracking overhead).
    let pool = BufferPool::new(4);
    let _buf = pool.acquire();
    assert_eq!(pool.memory_usage(), 0);
}

#[test]
fn memory_cap_backpressure_multiple_waiters() {
    // Cap allows 1 buffer. Two threads compete for it; they must
    // take turns.
    let pool = Arc::new(BufferPool::with_buffer_size(4, 1024).with_memory_cap(1024));
    let completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let handles: Vec<_> = (0..2)
        .map(|_| {
            let pool = Arc::clone(&pool);
            let completed = Arc::clone(&completed);
            thread::spawn(move || {
                for _ in 0..50 {
                    let mut buf = BufferPool::acquire_from(Arc::clone(&pool));
                    buf[0] = 0xFF;
                }
                completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked");
    }

    assert_eq!(completed.load(std::sync::atomic::Ordering::Relaxed), 2);
    assert_eq!(pool.memory_usage(), 0);
}

#[test]
fn no_throughput_tracker_by_default() {
    let pool = BufferPool::new(4);
    assert!(pool.throughput_tracker().is_none());
    // Without tracking, recommended_buffer_size returns pool default.
    assert_eq!(pool.recommended_buffer_size(), COPY_BUFFER_SIZE);
}

#[test]
fn throughput_tracking_enabled() {
    let pool = BufferPool::new(4).with_throughput_tracking();
    assert!(pool.throughput_tracker().is_some());
    assert_eq!(pool.throughput_tracker().unwrap().sample_count(), 0);
}

#[test]
fn throughput_tracking_custom_alpha() {
    let pool = BufferPool::new(4).with_throughput_tracking_alpha(0.5);
    assert!(pool.throughput_tracker().is_some());
}

#[test]
fn record_transfer_noop_without_tracking() {
    let pool = BufferPool::new(4);
    // Should not panic.
    pool.record_transfer(1_000_000, std::time::Duration::from_secs(1));
}

#[test]
fn record_transfer_updates_throughput() {
    let pool = BufferPool::new(4).with_throughput_tracking();
    pool.record_transfer(1_000_000, std::time::Duration::from_secs(1));
    let tracker = pool.throughput_tracker().unwrap();
    assert!(tracker.throughput_bps() > 0.0);
    assert_eq!(tracker.sample_count(), 1);
}

#[test]
fn recommended_buffer_size_adapts_to_throughput() {
    use super::throughput::{MAX_BUFFER_SIZE, MIN_BUFFER_SIZE};

    let pool = BufferPool::new(4).with_throughput_tracking_alpha(0.5);

    // No samples yet - returns minimum.
    assert_eq!(pool.recommended_buffer_size(), MIN_BUFFER_SIZE);

    // Record high throughput (100 MB/s) during warmup.
    for _ in 0..8 {
        pool.record_transfer(100_000_000, std::time::Duration::from_secs(1));
    }

    let size = pool.recommended_buffer_size();
    assert!(size > MIN_BUFFER_SIZE, "expected larger buffer, got {size}");
    assert!(
        size <= MAX_BUFFER_SIZE,
        "expected bounded buffer, got {size}"
    );
    assert!(size.is_power_of_two(), "expected power of two, got {size}");
}

#[test]
fn recommended_buffer_size_respects_memory_cap() {
    // Memory cap of 32 KB -> max buffer = 32K / 4 = 8 KB.
    let pool = BufferPool::with_buffer_size(4, 4096)
        .with_memory_cap(32 * 1024)
        .with_throughput_tracking_alpha(0.5);

    // Record very high throughput to push toward max.
    for _ in 0..8 {
        pool.record_transfer(1_000_000_000, std::time::Duration::from_secs(1));
    }

    let size = pool.recommended_buffer_size();
    // memory_cap / 4 = 8192, clamped to that.
    assert!(
        size <= 8192,
        "expected size <= 8192 with memory cap, got {size}"
    );
}

#[test]
fn throughput_tracking_with_builder_chain() {
    let pool = BufferPool::with_buffer_size(4, 1024)
        .with_memory_cap(8192)
        .with_throughput_tracking();

    assert!(pool.throughput_tracker().is_some());
    assert_eq!(pool.memory_cap(), Some(8192));
    assert_eq!(pool.buffer_size(), 1024);
}

#[test]
fn concurrent_throughput_recording() {
    let pool = Arc::new(BufferPool::new(4).with_throughput_tracking());
    let thread_count = 8;
    let iterations = 200;

    let handles: Vec<_> = (0..thread_count)
        .map(|id| {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                for i in 0..iterations {
                    let bytes = ((id + 1) * 50_000) + i * 1000;
                    pool.record_transfer(bytes, std::time::Duration::from_millis(10));
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked");
    }

    let tracker = pool.throughput_tracker().unwrap();
    assert_eq!(
        tracker.sample_count(),
        thread_count as u32 * iterations as u32
    );
    assert!(tracker.throughput_bps() > 0.0);
}

#[test]
fn concurrent_burst_returns_respect_capacity() {
    // Validates the lock-free ArrayQueue + TLS design: when many threads
    // return buffers simultaneously, the central queue retains at most
    // soft_capacity (enforced exactly by the admission counter via
    // compare_exchange_weak). Each thread also retains one buffer in TLS.
    use std::sync::Barrier;

    let thread_count = 32;
    let soft_cap = 4;
    let pool = Arc::new(BufferPool::new(soft_cap));
    let barrier = Arc::new(Barrier::new(thread_count));

    // Each thread acquires a buffer, waits at the barrier, then drops it.
    // This forces all returns to happen near-simultaneously.
    let handles: Vec<_> = (0..thread_count)
        .map(|_| {
            let pool = Arc::clone(&pool);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let buf = BufferPool::acquire_from(pool);
                barrier.wait();
                drop(buf);
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked");
    }

    // Central queue retains at most soft_capacity (lock-free admission via
    // compare_exchange_weak guarantees no overshoot). Each thread's TLS
    // slot also holds one buffer but those are invisible to available().
    // TLS buffers are reclaimed when threads exit.
    let available = pool.available();
    assert!(
        available <= soft_cap,
        "expected <= {soft_cap} buffers in central pool, got {available}"
    );

    // Verify reuse: acquiring from the pool gets a buffer (from central
    // pool since the spawned threads' TLS slots are gone).
    if available > 0 {
        let _buf = BufferPool::acquire_from(Arc::clone(&pool));
        assert!(pool.available() < available);
    }
}

#[test]
fn sequential_returns_respect_soft_capacity() {
    // Sequential returns on a single thread: first goes to TLS, rest go
    // to central pool up to soft_capacity. Excess is deallocated.
    let pool = BufferPool::new(2);

    let buffers: Vec<_> = (0..8).map(|_| pool.acquire()).collect();
    drop(buffers);

    // 1st → TLS, 2nd → central (1), 3rd → central (2=cap), 4th-8th → dealloc.
    assert_eq!(pool.available(), 2);
}

#[test]
fn tls_absorbs_first_return() {
    // First buffer returned on a thread goes to TLS, not central pool.
    let pool = Arc::new(BufferPool::new(4));

    {
        let _buf = BufferPool::acquire_from(Arc::clone(&pool));
    }
    // Buffer is in TLS - central pool is empty.
    assert_eq!(pool.available(), 0);

    // But re-acquiring on the same thread gets it from TLS.
    let buf = BufferPool::acquire_from(Arc::clone(&pool));
    assert_eq!(buf.len(), COPY_BUFFER_SIZE);
}

#[test]
fn tls_overflow_routes_to_central_pool() {
    // Second return on same thread (TLS occupied) routes to central pool.
    let pool = Arc::new(BufferPool::new(4));

    let a = BufferPool::acquire_from(Arc::clone(&pool));
    let b = BufferPool::acquire_from(Arc::clone(&pool));

    drop(a); // → TLS
    assert_eq!(pool.available(), 0);

    drop(b); // TLS full → central pool
    assert_eq!(pool.available(), 1);
}

#[test]
fn tls_provides_fast_path_acquire() {
    // Acquire-return-acquire cycle on same thread hits TLS both times.
    let pool = Arc::new(BufferPool::with_allocator(
        4,
        1024,
        TrackingAllocator::new(),
    ));

    // First acquire: fresh allocation.
    {
        let _buf = BufferPool::acquire_from(Arc::clone(&pool));
    }
    assert_eq!(pool.allocator().alloc_count(), 1);

    // Second acquire: from TLS (no new allocation).
    {
        let _buf = BufferPool::acquire_from(Arc::clone(&pool));
    }
    assert_eq!(pool.allocator().alloc_count(), 1);
}

#[test]
fn tls_wrong_size_buffer_discarded() {
    // A buffer in TLS from a different-sized pool config is discarded.
    let pool_a = Arc::new(BufferPool::with_buffer_size(4, 1024));
    let pool_b = Arc::new(BufferPool::with_buffer_size(4, 2048));

    // Store a 1024-byte buffer in TLS via pool_a.
    {
        let _buf = BufferPool::acquire_from(Arc::clone(&pool_a));
    }

    // Acquire from pool_b (expects 2048). TLS buffer is wrong size -
    // should be discarded and a fresh 2048-byte buffer allocated.
    let buf = BufferPool::acquire_from(Arc::clone(&pool_b));
    assert_eq!(buf.len(), 2048);
}

#[test]
fn tls_per_thread_isolation() {
    // Each thread has its own TLS slot. Verify buffers don't leak between threads.
    use std::sync::Barrier;

    let pool = Arc::new(BufferPool::with_buffer_size(2, 1024));
    let barrier = Arc::new(Barrier::new(2));

    let handles: Vec<_> = (0..2)
        .map(|id| {
            let pool = Arc::clone(&pool);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                {
                    let mut buf = BufferPool::acquire_from(Arc::clone(&pool));
                    buf[0] = id as u8;
                }
                // Buffer is now in this thread's TLS.

                barrier.wait();

                // Re-acquire must get own buffer from TLS, not another thread's.
                let buf = BufferPool::acquire_from(pool);
                assert_eq!(buf[0], id as u8);
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked");
    }
}

#[test]
fn adaptive_resizing_disabled_by_default() {
    let pool = BufferPool::new(4);
    assert!(!pool.is_adaptive());
}

#[test]
fn adaptive_resizing_enabled_via_builder() {
    let pool = BufferPool::new(4).with_adaptive_resizing();
    assert!(pool.is_adaptive());
}

#[test]
fn adaptive_resizing_with_builder_chain() {
    let pool = BufferPool::with_buffer_size(4, 1024)
        .with_memory_cap(8192)
        .with_adaptive_resizing();
    assert!(pool.is_adaptive());
    assert_eq!(pool.memory_cap(), Some(8192));
    assert_eq!(pool.buffer_size(), 1024);
}

/// A counting allocator that tracks allocations for adaptive resizing tests.
#[derive(Debug)]
struct AdaptiveTrackingAllocator {
    alloc_count: AtomicUsize,
    dealloc_count: AtomicUsize,
}

impl AdaptiveTrackingAllocator {
    fn new() -> Self {
        Self {
            alloc_count: AtomicUsize::new(0),
            dealloc_count: AtomicUsize::new(0),
        }
    }

    fn alloc_count(&self) -> usize {
        self.alloc_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn dealloc_count(&self) -> usize {
        self.dealloc_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl BufferAllocator for AdaptiveTrackingAllocator {
    fn allocate(&self, size: usize) -> Vec<u8> {
        self.alloc_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        vec![0u8; size]
    }

    fn deallocate(&self, _buffer: Vec<u8>) {
        self.dealloc_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

#[test]
fn adaptive_pool_grows_under_pressure() {
    // Start with a tiny pool (capacity 2) and force many misses by
    // holding all buffers checked out simultaneously.
    let pool = Arc::new(
        BufferPool::with_allocator(2, 1024, AdaptiveTrackingAllocator::new())
            .with_adaptive_resizing(),
    );
    let initial_capacity = pool.max_buffers();
    assert_eq!(initial_capacity, 2);

    // Hold buffers to exhaust the pool, then acquire more to force misses.
    // Each acquire beyond the pool's capacity triggers a miss. After 64
    // operations (the check interval), the pool should grow.
    let mut held = Vec::new();
    for _ in 0..128 {
        held.push(BufferPool::acquire_from(Arc::clone(&pool)));
    }

    // The pool should have grown due to high miss rate.
    let new_capacity = pool.max_buffers();
    assert!(
        new_capacity > initial_capacity,
        "expected capacity > {initial_capacity}, got {new_capacity}"
    );

    drop(held);
}

#[test]
fn adaptive_pool_shrinks_when_idle() {
    // Integration test for adaptive pool shrinking through the public API.
    //
    // Mathematical analysis of shrink conditions:
    //   - utilization = available / capacity < 0.30
    //   - miss_rate = misses / total < 0.10
    //   - ops >= CHECK_INTERVAL (64)
    //   - Each fresh thread has cold TLS, so acquire_from calls pop_buffer()
    //   - TLS buffers are lost on thread exit (not returned to central pool)
    //
    // Strategy:
    // 1. Start at MAX_CAPACITY (256) so pre-fill misses can't cause growth.
    // 2. Pre-fill: hold 64 buffers, then drop. Main thread's TLS absorbs 1,
    //    central pool receives 63. Pressure tracker resets at ops=64 (all
    //    misses, but capacity=MAX_CAPACITY → Hold → counters reset to 0).
    // 3. Spawn 64 fresh threads (cold TLS → pop_buffer for each):
    //    - 63 buffers in pool → 63 HITs, 1 MISS (pool empty for last thread)
    //    - miss_rate = 1/64 ≈ 1.6% < 10% ✓
    //    - At op 64: available=0, utilization = 0/256 = 0% < 30% ✓
    //    - evaluate() → Shrink(128)
    let pool = Arc::new(BufferPool::with_buffer_size(256, 1024).with_adaptive_resizing());
    assert_eq!(pool.max_buffers(), 256);

    // Phase 1: Pre-fill the central pool.
    // Acquire 64 buffers simultaneously - all go through pop_buffer (TLS is
    // empty, pool is empty → 64 fresh allocations, all MISSes). At ops=64,
    // maybe_resize fires: miss_rate=100% but capacity=256=MAX_CAPACITY, so
    // grow is capped → Hold. Tracker resets to zero.
    {
        let bufs: Vec<_> = (0..64)
            .map(|_| BufferPool::acquire_from(Arc::clone(&pool)))
            .collect();
        drop(bufs);
        // Drop order: first buffer → TLS (empty slot), remaining 63 → central
        // pool (TLS occupied). Central pool now holds 63 buffers.
    }

    // Phase 2: 64 fresh threads, each with cold TLS → pop_buffer on every
    // acquire. Pool drains from 63 → 0: 63 HITs + 1 MISS = 1.6% miss rate.
    // The 64th pop_buffer triggers maybe_resize with available=0, cap=256.
    let handles: Vec<_> = (0..64)
        .map(|_| {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                let _buf = BufferPool::acquire_from(pool);
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked");
    }

    let new_capacity = pool.max_buffers();
    assert!(
        new_capacity < 256,
        "expected capacity < 256 (shrink), got {new_capacity}"
    );
}

#[test]
fn adaptive_pool_holds_steady_under_balanced_load() {
    // Pre-fill the pool, then use it at a rate that roughly matches capacity.
    // The pool should not resize.
    let pool = Arc::new(BufferPool::with_buffer_size(8, 1024).with_adaptive_resizing());

    // Pre-populate with 8 buffers.
    let bufs: Vec<_> = (0..8)
        .map(|_| BufferPool::acquire_from(Arc::clone(&pool)))
        .collect();
    drop(bufs);

    // Acquire and release one at a time - all hits from pool/TLS.
    for _ in 0..256 {
        let _buf = BufferPool::acquire_from(Arc::clone(&pool));
    }

    // Capacity should stay at 8 (balanced utilization).
    let capacity = pool.max_buffers();
    assert!(
        (4..=16).contains(&capacity),
        "expected capacity near 8, got {capacity}"
    );
}

#[test]
fn adaptive_pool_concurrent_growth() {
    // Many threads each acquire multiple buffers concurrently, forcing fresh
    // allocations (misses). Each thread has a cold TLS cache so every acquire
    // goes through pop_buffer() which records the miss. We need >= 64 ops
    // (CHECK_INTERVAL) to trigger a resize evaluation. With 16 threads * 4
    // acquires = 64 ops, all misses, the pool should grow from capacity 2.
    let pool = Arc::new(BufferPool::with_buffer_size(2, 1024).with_adaptive_resizing());
    let initial = pool.max_buffers();

    // Run two rounds to ensure at least one resize check fires.
    for _ in 0..2 {
        let handles: Vec<_> = (0..16)
            .map(|_| {
                let pool = Arc::clone(&pool);
                thread::spawn(move || {
                    // Hold 4 buffers each to force misses (pool starts near-empty).
                    let held: Vec<_> = (0..4)
                        .map(|_| BufferPool::acquire_from(Arc::clone(&pool)))
                        .collect();
                    drop(held);
                })
            })
            .collect();

        for h in handles {
            h.join().expect("thread panicked");
        }
    }

    // Pool should have grown from the initial 2.
    let final_cap = pool.max_buffers();
    assert!(
        final_cap > initial,
        "expected capacity > {initial}, got {final_cap}"
    );
}

#[test]
fn adaptive_pool_does_not_grow_without_feature() {
    // Without adaptive resizing, capacity is fixed.
    let pool = Arc::new(BufferPool::with_buffer_size(2, 1024));

    let mut held = Vec::new();
    for _ in 0..128 {
        held.push(BufferPool::acquire_from(Arc::clone(&pool)));
    }

    assert_eq!(pool.max_buffers(), 2);
    drop(held);
}

#[test]
fn adaptive_pool_shrink_respects_minimum() {
    // Pool should never shrink below the minimum (2).
    let pool = Arc::new(BufferPool::with_buffer_size(4, 1024).with_adaptive_resizing());

    // Light usage to trigger shrink.
    for _ in 0..512 {
        let _buf = BufferPool::acquire_from(Arc::clone(&pool));
    }

    let capacity = pool.max_buffers();
    assert!(capacity >= 2, "expected capacity >= 2, got {capacity}");
}

#[test]
fn adaptive_pool_grow_respects_maximum() {
    // Pool should never grow beyond 256.
    let pool = Arc::new(BufferPool::with_buffer_size(128, 1024).with_adaptive_resizing());

    // Force heavy misses by holding many buffers.
    let mut held = Vec::new();
    for _ in 0..1024 {
        held.push(BufferPool::acquire_from(Arc::clone(&pool)));
    }

    let capacity = pool.max_buffers();
    assert!(capacity <= 256, "expected capacity <= 256, got {capacity}");
    drop(held);
}

#[test]
fn adaptive_pool_with_custom_allocator() {
    let pool = Arc::new(
        BufferPool::with_allocator(2, 512, AdaptiveTrackingAllocator::new())
            .with_adaptive_resizing(),
    );

    // Force misses to trigger growth.
    let held: Vec<_> = (0..128)
        .map(|_| BufferPool::acquire_from(Arc::clone(&pool)))
        .collect();

    assert!(pool.max_buffers() > 2);
    assert!(pool.allocator().alloc_count() > 2);

    drop(held);
}

#[test]
fn adaptive_pool_deallocates_on_shrink() {
    let pool = Arc::new(
        BufferPool::with_allocator(16, 512, AdaptiveTrackingAllocator::new())
            .with_adaptive_resizing(),
    );

    // Pre-fill the pool.
    let bufs: Vec<_> = (0..16)
        .map(|_| BufferPool::acquire_from(Arc::clone(&pool)))
        .collect();
    drop(bufs);

    let deallocs_before = pool.allocator().dealloc_count();

    // Light usage to trigger shrink. Pool has 16 buffers but low demand.
    for _ in 0..512 {
        let _buf = BufferPool::acquire_from(Arc::clone(&pool));
    }

    // If the pool shrank, it should have deallocated excess buffers.
    if pool.max_buffers() < 16 {
        assert!(
            pool.allocator().dealloc_count() > deallocs_before,
            "expected deallocations after shrink"
        );
    }
}

#[test]
fn telemetry_starts_at_zero() {
    let pool = BufferPool::new(4);
    assert_eq!(pool.total_hits(), 0);
    assert_eq!(pool.total_misses(), 0);
    assert_eq!(pool.total_acquires(), 0);
    assert_eq!(pool.hit_rate(), 0.0);
}

#[test]
fn telemetry_first_acquire_is_miss() {
    let pool = BufferPool::new(4);
    let _buf = pool.acquire();
    // First acquire on a fresh pool: TLS empty, central pool empty -> miss.
    assert_eq!(pool.total_misses(), 1);
    assert_eq!(pool.total_acquires(), 1);
}

#[test]
fn telemetry_tls_reuse_is_hit() {
    let pool = BufferPool::new(4);
    // First acquire: miss (fresh allocation).
    {
        let _buf = pool.acquire();
    }
    // Buffer returned to TLS.
    // Second acquire: hit (from TLS).
    let _buf = pool.acquire();
    assert!(pool.total_hits() >= 1);
    assert_eq!(pool.total_acquires(), 2);
}

#[test]
fn telemetry_hit_rate_calculation() {
    let pool = Arc::new(BufferPool::new(4));
    // First acquire: miss.
    {
        let _buf = BufferPool::acquire_from(Arc::clone(&pool));
    }
    // Second acquire: hit from TLS.
    {
        let _buf = BufferPool::acquire_from(Arc::clone(&pool));
    }
    let rate = pool.hit_rate();
    assert!(rate > 0.0, "expected hit rate > 0, got {rate}");
    assert!(rate <= 1.0, "expected hit rate <= 1, got {rate}");
}

#[test]
fn telemetry_cumulative_across_many_acquires() {
    let pool = Arc::new(BufferPool::new(4));
    let iterations = 100;
    for _ in 0..iterations {
        let _buf = BufferPool::acquire_from(Arc::clone(&pool));
    }
    assert_eq!(pool.total_acquires(), iterations);
    // First acquire is a miss, rest are TLS hits.
    assert_eq!(pool.total_hits(), iterations - 1);
    assert_eq!(pool.total_misses(), 1);
}

#[test]
fn telemetry_concurrent_counting() {
    let pool = Arc::new(BufferPool::new(8));
    let thread_count = 8u64;
    let iterations = 200u64;

    let handles: Vec<_> = (0..thread_count)
        .map(|_| {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                for _ in 0..iterations {
                    let _buf = BufferPool::acquire_from(Arc::clone(&pool));
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked");
    }

    let total = pool.total_acquires();
    assert_eq!(
        total,
        thread_count * iterations,
        "expected {}, got {total}",
        thread_count * iterations
    );
    assert_eq!(total, pool.total_hits() + pool.total_misses());
}

#[test]
fn telemetry_with_adaptive_resizing() {
    // Telemetry counters work independently of adaptive resizing.
    let pool = Arc::new(BufferPool::new(4).with_adaptive_resizing());
    for _ in 0..100 {
        let _buf = BufferPool::acquire_from(Arc::clone(&pool));
    }
    assert_eq!(pool.total_acquires(), 100);
}

#[test]
fn telemetry_try_acquire_counts_hits() {
    let pool = BufferPool::with_buffer_size(4, 1024).with_memory_cap(4096);
    // First acquire: miss.
    {
        let _buf = pool.acquire();
    }
    // Second acquire via try_acquire: TLS hit.
    {
        let _buf = pool.try_acquire();
    }
    assert!(pool.total_hits() >= 1);
    assert_eq!(pool.total_acquires(), 2);
}

#[test]
fn telemetry_try_acquire_from_counts_hits() {
    let pool = Arc::new(BufferPool::with_buffer_size(4, 1024).with_memory_cap(4096));
    // First acquire: miss.
    {
        let _buf = BufferPool::acquire_from(Arc::clone(&pool));
    }
    // Second acquire via try_acquire_from: TLS hit.
    {
        let _buf = BufferPool::try_acquire_from(Arc::clone(&pool));
    }
    assert!(pool.total_hits() >= 1);
    assert_eq!(pool.total_acquires(), 2);
}

#[test]
fn stats_returns_snapshot() {
    let pool = Arc::new(BufferPool::new(4));
    // First acquire: miss.
    {
        let _buf = BufferPool::acquire_from(Arc::clone(&pool));
    }
    // Second acquire: TLS hit.
    {
        let _buf = BufferPool::acquire_from(Arc::clone(&pool));
    }
    let stats = pool.stats();
    assert_eq!(stats.total_acquires(), 2);
    assert_eq!(stats.total_misses, 1);
    assert_eq!(stats.total_hits, 1);
    assert_eq!(stats.total_growths, 0);
    assert!((stats.hit_rate() - 0.5).abs() < f64::EPSILON);
}

#[test]
fn stats_growths_zero_without_adaptive() {
    let pool = Arc::new(BufferPool::new(2));
    let mut held = Vec::new();
    for _ in 0..128 {
        held.push(BufferPool::acquire_from(Arc::clone(&pool)));
    }
    assert_eq!(pool.total_growths(), 0);
    assert_eq!(pool.stats().total_growths, 0);
    drop(held);
}

#[test]
fn stats_growths_incremented_on_adaptive_grow() {
    let pool = Arc::new(BufferPool::with_buffer_size(2, 1024).with_adaptive_resizing());
    let initial = pool.max_buffers();

    // Hold many buffers to force misses and trigger growth.
    let mut held = Vec::new();
    for _ in 0..128 {
        held.push(BufferPool::acquire_from(Arc::clone(&pool)));
    }

    let new_capacity = pool.max_buffers();
    if new_capacity > initial {
        assert!(
            pool.total_growths() >= 1,
            "expected at least 1 growth event, got {}",
            pool.total_growths()
        );
        assert!(
            pool.stats().total_growths >= 1,
            "stats().total_growths should match total_growths()"
        );
    }
    drop(held);
}

#[test]
fn stats_hit_rate_empty() {
    let stats = super::pool::BufferPoolStats {
        total_hits: 0,
        total_misses: 0,
        total_growths: 0,
    };
    assert_eq!(stats.hit_rate(), 0.0);
    assert_eq!(stats.total_acquires(), 0);
}

#[test]
fn stats_hit_rate_all_hits() {
    let stats = super::pool::BufferPoolStats {
        total_hits: 100,
        total_misses: 0,
        total_growths: 0,
    };
    assert!((stats.hit_rate() - 1.0).abs() < f64::EPSILON);
}

#[test]
fn stats_hit_rate_all_misses() {
    let stats = super::pool::BufferPoolStats {
        total_hits: 0,
        total_misses: 50,
        total_growths: 0,
    };
    assert_eq!(stats.hit_rate(), 0.0);
    assert_eq!(stats.total_acquires(), 50);
}

#[test]
fn stats_debug_and_clone() {
    let stats = super::pool::BufferPoolStats {
        total_hits: 10,
        total_misses: 5,
        total_growths: 1,
    };
    let cloned = stats;
    assert_eq!(stats, cloned);
    let debug = format!("{stats:?}");
    assert!(debug.contains("total_hits"));
    assert!(debug.contains("total_misses"));
    assert!(debug.contains("total_growths"));
}

#[test]
fn lock_free_acquire_release_under_scoped_concurrency() {
    // Hammers the lock-free ArrayQueue acquire/release path from many
    // scoped threads with no per-iteration TLS reuse. Each iteration
    // allocates a small Vec of guards so successive returns must traverse
    // the central queue (TLS holds at most one buffer per thread). The
    // test verifies that the `crossbeam_queue::ArrayQueue` plus the
    // `compare_exchange_weak` admission counter never overshoots the soft
    // capacity, even under sustained contention from many cores.
    let soft_cap = 8;
    let pool = BufferPool::new(soft_cap);
    let thread_count = 16;
    let iterations = 500;

    std::thread::scope(|scope| {
        for _ in 0..thread_count {
            scope.spawn(|| {
                for i in 0..iterations {
                    // Hold three buffers concurrently per iteration so
                    // returns overflow the per-thread TLS slot and reach
                    // the lock-free queue.
                    let mut guards: [Option<BorrowedBufferGuard<'_>>; 3] = [None, None, None];
                    for slot in &mut guards {
                        *slot = Some(pool.acquire());
                    }
                    for (n, slot) in guards.iter_mut().enumerate() {
                        let mut g = slot.take().expect("guard present");
                        g[0] = ((i + n) & 0xFF) as u8;
                    }
                }
            });
        }
    });

    // Soft capacity is never exceeded - the admission counter strictly
    // gates the queue length. TLS slots hold one buffer per thread but
    // are released when those threads exit (scope join above).
    let observed = pool.available();
    assert!(
        observed <= soft_cap,
        "central queue exceeded soft cap: {observed} > {soft_cap}"
    );
}
