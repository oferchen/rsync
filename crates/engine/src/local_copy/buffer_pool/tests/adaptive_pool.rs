//! Adaptive pool-capacity resizing (grow on pressure, shrink when idle).

use super::super::*;
use super::support::AdaptiveTrackingAllocator;
use std::sync::{Arc, Barrier};
use std::thread;

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

// The shrink-on-idle integration test below pre-fills the central pool by
// relying on the single-slot TLS path (one buffer absorbed by TLS, 63
// routed to the central queue). With the per-thread slab feature on, the
// slab swallows up to 8 returns per thread before routing to the central
// queue, which changes the central-queue occupancy and the hit/miss ratio
// the assertion is calibrated against. Slab-backend equivalents live in
// `tests/slab.rs`.
#[cfg(not(feature = "thread-slab-pool"))]
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
    //    misses, but capacity=MAX_CAPACITY -> Hold -> counters reset to 0).
    // 3. Spawn 64 fresh threads (cold TLS -> pop_buffer for each):
    //    - 63 buffers in pool -> 63 HITs, 1 MISS (pool empty for last thread)
    //    - miss_rate = 1/64 ~ 1.6% < 10% (ok)
    //    - At op 64: available=0, utilization = 0/256 = 0% < 30% (ok)
    //    - evaluate() -> Shrink(128)
    let pool = Arc::new(BufferPool::with_buffer_size(256, 1024).with_adaptive_resizing());
    assert_eq!(pool.max_buffers(), 256);

    // Phase 1: Pre-fill the central pool.
    // Acquire 64 buffers simultaneously - all go through pop_buffer (TLS is
    // empty, pool is empty -> 64 fresh allocations, all MISSes). At ops=64,
    // maybe_resize fires: miss_rate=100% but capacity=256=MAX_CAPACITY, so
    // grow is capped -> Hold. Tracker resets to zero.
    {
        let bufs: Vec<_> = (0..64)
            .map(|_| BufferPool::acquire_from(Arc::clone(&pool)))
            .collect();
        drop(bufs);
        // Drop order: first buffer -> TLS (empty slot), remaining 63 -> central
        // pool (TLS occupied). Central pool now holds 63 buffers.
    }

    // Phase 2: 64 fresh threads, each with cold TLS -> pop_buffer on every
    // acquire. Pool drains from 63 -> 0: 63 HITs + 1 MISS = 1.6% miss rate.
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
    // Verifies that concurrent allocation pressure grows the pool, without
    // depending on a race in the resize-check boundary.
    //
    // The resize evaluation only fires when a thread observes the shared op
    // counter at an exact multiple of CHECK_INTERVAL (64). That counter is
    // sampled with a plain load that races concurrent increments: under
    // contention every thread can load a value already bumped past 64 by its
    // peers, so no thread observes the boundary and the pool never resizes.
    // The old design ran two 64-op concurrent rounds hoping one boundary would
    // land; under load both could be missed, capacity stayed at 2, and the
    // assertion flaked.
    //
    // Fix: build the miss pressure concurrently (worker threads that are
    // provably all in-flight via a barrier), keeping the op count strictly
    // below CHECK_INTERVAL so no check fires mid-phase, then cross the boundary
    // from a single thread where the counter cannot be raced. The evaluation is
    // then guaranteed to observe the accumulated high miss rate and grow.
    const THREADS: usize = 15;
    const PER_THREAD: usize = 4; // 15 * 4 = 60 ops, below CHECK_INTERVAL (64).

    let pool = Arc::new(BufferPool::with_buffer_size(2, 1024).with_adaptive_resizing());
    let initial = pool.max_buffers();
    assert_eq!(initial, 2);

    // Concurrent phase: every worker holds all its buffers simultaneously (the
    // barrier guarantees all acquisitions are in-flight before any release), so
    // the capacity-2 pool cannot serve them and every acquire is a genuine
    // concurrent miss. Total ops (60) stay under CHECK_INTERVAL, so no resize
    // check fires here and the misses accumulate for the evaluation below.
    let barrier = Arc::new(Barrier::new(THREADS));
    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let pool = Arc::clone(&pool);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let held: Vec<_> = (0..PER_THREAD)
                    .map(|_| BufferPool::acquire_from(Arc::clone(&pool)))
                    .collect();
                barrier.wait();
                drop(held);
            })
        })
        .collect();
    for h in handles {
        h.join().expect("thread panicked");
    }

    // Boundary-crossing phase: single-threaded acquires push the op counter to
    // exactly CHECK_INTERVAL. With no concurrent writers the boundary is
    // observed deterministically, evaluate() sees the accumulated high miss
    // rate, and the pool grows. If the grow path regresses (threshold, wiring,
    // or evaluate logic), the counter still crosses the boundary but capacity
    // stays at 2 and this assertion fails.
    let _settle: Vec<_> = (0..PER_THREAD)
        .map(|_| BufferPool::acquire_from(Arc::clone(&pool)))
        .collect();

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
