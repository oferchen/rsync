//! Thread-local cache (TLS slot) admission, isolation, and overflow routing.

use super::super::*;
use super::support::TrackingAllocator;
use std::sync::Arc;
use std::thread;

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

// Sequential return / TLS-overflow assertions track the single-slot TLS
// path: first return per thread fills the slot, the next routes to the
// central queue. With the per-thread slab feature on, the slab absorbs
// up to 8 returns per thread before central admission runs, so these
// `available()` counts do not hold. Slab-backend coverage lives in
// `tests/slab.rs`.
#[cfg(not(feature = "thread-slab-pool"))]
#[test]
fn sequential_returns_respect_soft_capacity() {
    // Sequential returns on a single thread: first goes to TLS, rest go
    // to central pool up to soft_capacity. Excess is deallocated.
    let pool = BufferPool::new(2);

    let buffers: Vec<_> = (0..8).map(|_| pool.acquire()).collect();
    drop(buffers);

    // 1st -> TLS, 2nd -> central (1), 3rd -> central (2=cap), 4th-8th -> dealloc.
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

#[cfg(not(feature = "thread-slab-pool"))]
#[test]
fn tls_overflow_routes_to_central_pool() {
    // Second return on same thread (TLS occupied) routes to central pool.
    let pool = Arc::new(BufferPool::new(4));

    let a = BufferPool::acquire_from(Arc::clone(&pool));
    let b = BufferPool::acquire_from(Arc::clone(&pool));

    drop(a); // -> TLS
    assert_eq!(pool.available(), 0);

    drop(b); // TLS full -> central pool
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
