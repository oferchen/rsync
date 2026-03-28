use super::*;
use std::sync::Arc;
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

    // Acquire and release a buffer
    {
        let mut buffer = pool.acquire();
        buffer[0] = 42;
    }

    // Pool should have one buffer
    assert_eq!(pool.available(), 1);

    // Acquire again - should get the reused buffer with correct length
    let buffer = pool.acquire();
    assert_eq!(pool.available(), 0);
    assert_eq!(buffer.len(), COPY_BUFFER_SIZE);
}

#[test]
fn test_pool_capacity_limit() {
    let pool = BufferPool::new(2);

    // Acquire 3 buffers
    let b1 = pool.acquire();
    let b2 = pool.acquire();
    let b3 = pool.acquire();

    // Release all
    drop(b1);
    drop(b2);
    drop(b3);

    // Only 2 should be retained
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
                // Buffer returned on drop
            }
        }));
    }

    for handle in handles {
        handle.join().expect("thread panicked");
    }

    // Pool should have at most max_buffers
    assert!(pool.available() <= 8);
}

#[test]
fn test_buffer_guard_deref() {
    let pool = BufferPool::new(4);
    let mut buffer = pool.acquire();

    // Write through DerefMut
    buffer[0] = 100;
    buffer[1] = 200;

    // Read through Deref
    assert_eq!(buffer[0], 100);
    assert_eq!(buffer[1], 200);

    // Use as slice
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
        // Fill with non-zero data
        for byte in buffer.iter_mut() {
            *byte = 0xFF;
        }
    }

    // Acquire again - length should be restored (contents are stale but
    // will be overwritten by Read::read before consumption).
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
    // default buffer size, so the buffer should come from the pool.
    let pool = Arc::new(BufferPool::new(4));

    // Pre-populate the pool with a buffer
    {
        let _buffer = BufferPool::acquire_from(Arc::clone(&pool));
        // buffer is returned on drop
    }
    assert_eq!(pool.available(), 1);

    // Acquire adaptively for a medium file -- should reuse the pooled buffer
    let buffer = BufferPool::acquire_adaptive_from(Arc::clone(&pool), 10 * 1024 * 1024);
    assert_eq!(buffer.len(), COPY_BUFFER_SIZE);
    assert_eq!(pool.available(), 0); // was taken from pool
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
    // Verify that adaptively-sized buffers are still returned to the pool
    // (resized to pool default) when dropped.
    let pool = Arc::new(BufferPool::new(4));
    assert_eq!(pool.available(), 0);

    {
        let _buffer = BufferPool::acquire_adaptive_from(Arc::clone(&pool), 1024);
        // tiny buffer is active
    }
    // After drop, buffer should be returned and resized to pool default
    assert_eq!(pool.available(), 1);

    // Next acquire from pool should get a buffer of the default size
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
    assert_eq!(pool.available(), 1);

    // The returned buffer should be at the pool's default size
    let buffer = BufferPool::acquire_from(Arc::clone(&pool));
    assert_eq!(buffer.len(), COPY_BUFFER_SIZE);
}

#[test]
fn pool_reuses_buffers_under_sequential_pressure() {
    // Allocate and return many buffers sequentially.
    // The pool should reuse buffers so that at most max_buffers
    // are retained, regardless of how many iterations run.
    let pool = Arc::new(BufferPool::new(4));

    for _ in 0..1_000 {
        let mut buf = BufferPool::acquire_from(Arc::clone(&pool));
        buf[0] = 0xAB;
    }

    // After all guards are dropped the pool holds at most max_buffers.
    assert!(pool.available() <= 4);
    // At least one buffer was returned (proves reuse path was exercised).
    assert!(pool.available() >= 1);
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

    // While all 64 buffers are checked out the pool is empty.
    assert_eq!(pool.available(), 0);

    // Drop all guards - only max_buffers should be retained.
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
    // Explicitly verify the BufferGuard Drop impl returns the buffer.
    let pool = Arc::new(BufferPool::new(4));
    assert_eq!(pool.available(), 0);

    let guard = BufferPool::acquire_from(Arc::clone(&pool));
    assert_eq!(pool.available(), 0);

    drop(guard);
    assert_eq!(pool.available(), 1);
}

#[test]
fn borrowed_guard_drop_returns_buffer_to_pool() {
    // Same verification for BorrowedBufferGuard.
    let pool = BufferPool::new(4);
    assert_eq!(pool.available(), 0);

    let guard = pool.acquire();
    assert_eq!(pool.available(), 0);

    drop(guard);
    assert_eq!(pool.available(), 1);
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
                    // Write a recognizable pattern to detect cross-thread corruption.
                    buf[0] = (id & 0xFF) as u8;
                    buf[1] = (i & 0xFF) as u8;
                    assert_eq!(buf[0], (id & 0xFF) as u8);
                    // Guard dropped here - buffer returns to pool.
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

    // Spawn threads using Arc-based acquire_from.
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

    // Now use borrowed guard on the main thread.
    let available_before = pool.available();
    {
        let _buf = pool.acquire();
    }
    // Buffer was returned; available count should be at least what it was.
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

    // Under the elastic pool, concurrent returns may briefly push above
    // the soft capacity. The pool drains back naturally on subsequent acquires.
    // The important invariant is that all retained buffers have the correct size.

    // Every buffer in the pool should now be at the default size.
    for _ in 0..pool.available() {
        let buf = BufferPool::acquire_from(Arc::clone(&pool));
        assert_eq!(buf.len(), COPY_BUFFER_SIZE);
    }
}

#[test]
fn repeated_acquire_release_cycle_reuses_same_buffers() {
    // Verify the pool actually recycles buffers by checking that
    // the pool count stabilizes rather than growing.
    let pool = Arc::new(BufferPool::new(2));

    // First cycle - buffers are freshly allocated.
    {
        let _a = BufferPool::acquire_from(Arc::clone(&pool));
        let _b = BufferPool::acquire_from(Arc::clone(&pool));
    }
    assert_eq!(pool.available(), 2);

    // Second cycle - buffers should come from the pool.
    {
        let _a = BufferPool::acquire_from(Arc::clone(&pool));
        assert_eq!(pool.available(), 1);
        let _b = BufferPool::acquire_from(Arc::clone(&pool));
        assert_eq!(pool.available(), 0);
    }
    // All returned.
    assert_eq!(pool.available(), 2);

    // After 100 more cycles the pool still holds exactly max_buffers.
    for _ in 0..100 {
        let _a = BufferPool::acquire_from(Arc::clone(&pool));
        let _b = BufferPool::acquire_from(Arc::clone(&pool));
    }
    assert_eq!(pool.available(), 2);
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
    // A pool with capacity 1 should cycle a single buffer.
    let pool = Arc::new(BufferPool::new(1));

    {
        let _buf = BufferPool::acquire_from(Arc::clone(&pool));
    }
    assert_eq!(pool.available(), 1);

    // Acquire two simultaneously - second must be a fresh allocation.
    let a = BufferPool::acquire_from(Arc::clone(&pool));
    assert_eq!(pool.available(), 0);
    let b = BufferPool::acquire_from(Arc::clone(&pool));
    assert_eq!(pool.available(), 0);

    drop(a);
    assert_eq!(pool.available(), 1);
    // Dropping b exceeds capacity - it should be discarded.
    drop(b);
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
    // Pool with capacity 1 - second returned buffer triggers deallocate.
    let pool = Arc::new(BufferPool::with_allocator(1, 512, TrackingAllocator::new()));

    let a = BufferPool::acquire_from(Arc::clone(&pool));
    let b = BufferPool::acquire_from(Arc::clone(&pool));
    assert_eq!(pool.allocator().alloc_count(), 2);

    drop(a); // returned to pool (pool was empty)
    assert_eq!(pool.available(), 1);
    assert_eq!(pool.allocator().dealloc_count(), 0);

    drop(b); // pool is full - deallocate is called
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

    assert_eq!(pool.available(), 1);
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
                    // Guard dropped here - returns buffer.
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

// --- Throughput tracking integration tests ---

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
fn elastic_pool_absorbs_concurrent_burst_returns() {
    // Validates the elastic pool design: when many threads return buffers
    // simultaneously, the SegQueue absorbs the burst rather than dropping
    // buffers. After the burst, buffers are available for reuse.
    use std::sync::Barrier;

    let thread_count = 32;
    let pool = Arc::new(BufferPool::new(4));
    let barrier = Arc::new(Barrier::new(thread_count));

    // Each thread acquires a buffer, waits at the barrier, then drops it.
    // This forces all returns to happen near-simultaneously - exactly the
    // burst pattern that defeated the old ArrayQueue-based pool.
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

    // The pool should have retained buffers. With the old ArrayQueue(4),
    // many burst returns would hit a full queue and deallocate. With elastic
    // SegQueue, the soft cap still limits retention but concurrent returns
    // that pass the capacity check before others increment pool_len may
    // push beyond the soft cap - which is the desired behavior.
    let available = pool.available();
    assert!(
        available >= 1 && available <= thread_count,
        "expected 1..={thread_count} buffers, got {available}"
    );

    // Verify that acquiring from the pool reuses buffers (no fresh allocation
    // needed for at least one acquire).
    let _buf = BufferPool::acquire_from(Arc::clone(&pool));
    assert!(pool.available() < available);
}

#[test]
fn elastic_pool_drains_to_soft_capacity() {
    // After a burst that pushes above soft capacity, sequential returns
    // should stop retaining once the pool reaches soft capacity.
    let pool = BufferPool::new(2);

    // Acquire 8 buffers, then drop them one at a time.
    let buffers: Vec<_> = (0..8).map(|_| pool.acquire()).collect();
    drop(buffers);

    // Sequential returns respect the soft cap.
    assert_eq!(pool.available(), 2);
}
