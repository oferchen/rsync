//! Sequential, burst, and concurrent acquire/release contention scenarios,
//! plus custom-allocator integration and the lock-free queue stress test.

use super::super::*;
use super::support::TrackingAllocator;
use std::sync::Arc;
use std::thread;

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

// The reuse, single-capacity, and overflow tests below all rely on the
// single-slot TLS semantic (first return per thread fills TLS, the next
// reaches the central queue). With the per-thread slab feature on, the
// slab absorbs up to 8 returns per thread before the central queue sees
// anything, so the `pool.available()` counts the tests assert do not
// match. Slab-equivalent coverage lives in `tests/slab.rs`.
#[cfg(not(feature = "thread-slab-pool"))]
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
    // _b -> TLS, _a -> TLS full -> central pool.
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

#[cfg(not(feature = "thread-slab-pool"))]
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

    // Drop both: a -> TLS, b -> TLS full -> central (cap=1, accepts).
    drop(a);
    drop(b);
    assert_eq!(pool.available(), 1);

    // Acquire 3: a from TLS, b from central, c fresh.
    let a = BufferPool::acquire_from(Arc::clone(&pool));
    let b = BufferPool::acquire_from(Arc::clone(&pool));
    let c = BufferPool::acquire_from(Arc::clone(&pool));
    assert_eq!(pool.available(), 0);

    // Drop all 3: a -> TLS, b -> central (cap=1), c -> TLS full + central full -> dealloc.
    drop(a);
    drop(b);
    drop(c);
    assert_eq!(pool.available(), 1);
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

#[cfg(not(feature = "thread-slab-pool"))]
#[test]
fn custom_allocator_deallocate_called_on_overflow() {
    // Pool with capacity 1. With TLS, need 3 returns to trigger overflow:
    // 1st -> TLS, 2nd -> central (cap=1), 3rd -> TLS full + central full -> dealloc.
    let pool = Arc::new(BufferPool::with_allocator(1, 512, TrackingAllocator::new()));

    let a = BufferPool::acquire_from(Arc::clone(&pool));
    let b = BufferPool::acquire_from(Arc::clone(&pool));
    let c = BufferPool::acquire_from(Arc::clone(&pool));
    assert_eq!(pool.allocator().alloc_count(), 3);

    drop(a); // -> TLS
    assert_eq!(pool.available(), 0);
    assert_eq!(pool.allocator().dealloc_count(), 0);

    drop(b); // TLS full -> central (cap=1, accepts)
    assert_eq!(pool.available(), 1);
    assert_eq!(pool.allocator().dealloc_count(), 0);

    drop(c); // TLS full, central full -> deallocate
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

/// Regression: the local-copy transfer loop acquires one copy buffer per file
/// on a worker thread. It must reuse a single pool-default buffer across
/// sequential files (allocations O(worker threads)), not allocate a fresh
/// buffer per file (O(files)).
///
/// Before the lazy-acquisition fix, the transfer site called
/// `acquire_controlled_from(pool, file_size)`, which - with no active
/// controller - derived a *file-size-adaptive* buffer size. For any file
/// below 1 MB that size (8 KB / 32 KB) never matched the 128 KB pool default,
/// so every acquire took the fresh-allocation slow path and skipped the
/// thread-local cache: 20k small files churned 20k * 128 KB. Acquiring at the
/// pool default via `acquire_from` restores the intended single-buffer reuse.
#[cfg(not(feature = "thread-slab-pool"))]
#[test]
fn acquire_from_reuses_one_buffer_across_sequential_files() {
    let pool = Arc::new(BufferPool::with_allocator(
        4,
        COPY_BUFFER_SIZE,
        TrackingAllocator::new(),
    ));

    // Simulate copying many small files back to back on one worker thread:
    // acquire the copy buffer, use it, drop it, repeat. The single TLS slot
    // must carry the same buffer forward so only the first iteration allocates.
    for _ in 0..1_000 {
        let mut buf = BufferPool::acquire_from(Arc::clone(&pool));
        buf[0] = 0x5A;
    }

    // At most one allocation total, not one per file. (A cold worker thread
    // allocates the first buffer; a thread whose TLS slot already holds a
    // pool-default buffer from earlier work allocates zero.) This is the
    // O(threads) vs O(files) invariant the regression protects.
    assert!(
        pool.allocator().alloc_count() <= 1,
        "expected buffer reuse, got {} allocations for 1000 files",
        pool.allocator().alloc_count()
    );
}

/// Documents the churn the fix removed: requesting a sub-pool-default adaptive
/// size (what the old transfer site did for small files) forces a fresh
/// allocation on every acquire, defeating the thread-local cache entirely.
#[cfg(not(feature = "thread-slab-pool"))]
#[test]
fn adaptive_small_size_acquire_allocates_per_file() {
    let pool = Arc::new(BufferPool::with_allocator(
        4,
        COPY_BUFFER_SIZE,
        TrackingAllocator::new(),
    ));

    // Tiny "files" -> 8 KB adaptive size, never matches the 128 KB default,
    // so each acquire allocates fresh and the return path reallocates to the
    // pool size. This is the pathology the transfer site no longer triggers.
    for _ in 0..8 {
        let _buf = BufferPool::acquire_adaptive_from(Arc::clone(&pool), 1024);
    }

    assert_eq!(pool.allocator().alloc_count(), 8);
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
