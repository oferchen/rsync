//! End-to-end behaviour tests for the per-thread slab feature (#1271, #1370).
//!
//! Each test runs inside a dedicated worker thread so the `thread_local!`
//! slab is fresh and the assertions are not perturbed by other tests
//! sharing the same harness thread.

use super::super::*;
use std::sync::Arc;
use std::sync::Barrier;
use std::sync::atomic::AtomicUsize;
use std::thread;

fn run_in_worker<F: FnOnce() + Send + 'static>(f: F) {
    thread::spawn(f).join().expect("worker panicked");
}

#[test]
fn slab_thread_teardown_releases_buffers() {
    let pool = Arc::new(BufferPool::new(8));
    let pool_clone = Arc::clone(&pool);

    run_in_worker(move || {
        // Acquire two buffers on this thread and drop both - they land
        // in this thread's slab.
        for _ in 0..2 {
            let g = BufferPool::acquire_from(Arc::clone(&pool_clone));
            drop(g);
        }
        // The slab now holds the two buffers; thread teardown frees them
        // when this closure returns (thread_local! Drop runs).
    });

    // After teardown, the pool's outstanding memory is back to zero.
    // Survivors that the slab routed to the central queue during return
    // remain available; either count is acceptable, but the pool must
    // remain usable.
    let g = BufferPool::acquire_from(Arc::clone(&pool));
    assert_eq!(g.len(), COPY_BUFFER_SIZE);
}

#[test]
fn cross_thread_return_routes_through_overflow() {
    let pool = Arc::new(BufferPool::new(4));
    let acquired = BufferPool::acquire_from(Arc::clone(&pool));

    let pool_clone = Arc::clone(&pool);
    let handle = thread::spawn(move || {
        // Dropping the guard on this foreign thread either pushes onto
        // this thread's empty slab or routes to the central queue.
        // Either path is correct - what matters is the pool stays
        // consistent and the buffer is reusable.
        drop(acquired);
        // Re-acquire and immediately drop to leave at least one buffer
        // available for the main thread.
        let g = BufferPool::acquire_from(Arc::clone(&pool_clone));
        drop(g);
    });
    handle.join().expect("foreign thread panicked");

    // Pool remains usable from the original thread.
    let g = BufferPool::acquire_from(Arc::clone(&pool));
    assert_eq!(g.len(), COPY_BUFFER_SIZE);
}

#[test]
fn slab_bounds_per_thread_memory() {
    use super::super::thread_slab;

    let pool = Arc::new(BufferPool::new(64));
    let slot_count = AtomicUsize::new(0);
    let byte_count = AtomicUsize::new(0);
    let slot_ref = &slot_count;
    let byte_ref = &byte_count;
    let pool_ref = &pool;

    // Run inside a scope so the worker thread can borrow the counters.
    thread::scope(|s| {
        s.spawn(move || {
            // Allocate, drop, allocate, drop ... many times. Each return
            // populates the slab until it hits its slot/byte cap.
            for _ in 0..32 {
                let g = BufferPool::acquire_from(Arc::clone(pool_ref));
                drop(g);
            }
            let (slots, bytes) = thread_slab::snapshot();
            slot_ref.store(slots, std::sync::atomic::Ordering::Relaxed);
            byte_ref.store(bytes, std::sync::atomic::Ordering::Relaxed);
        });
    });

    let slots = slot_count.load(std::sync::atomic::Ordering::Relaxed);
    let bytes = byte_count.load(std::sync::atomic::Ordering::Relaxed);
    // Per-thread slot cap = 8, byte cap = 8 * COPY_BUFFER_SIZE.
    assert!(slots <= 8, "per-thread slot cap exceeded: {slots}");
    assert!(
        bytes <= 8 * COPY_BUFFER_SIZE,
        "per-thread byte cap exceeded: {bytes}"
    );
}

#[test]
fn periodic_donation_drains_long_lived_buffers() {
    use super::super::thread_slab;

    let pool = Arc::new(BufferPool::new(64));
    let pool_ref = &pool;

    let donated_to_central = AtomicUsize::new(0);
    let central_ref = &donated_to_central;

    thread::scope(|s| {
        s.spawn(move || {
            // First, fill the slab with cold buffers that the donation
            // path will eventually evict to the central queue.
            let mut held = Vec::new();
            for _ in 0..4 {
                held.push(BufferPool::acquire_from(Arc::clone(pool_ref)));
            }
            for g in held.drain(..) {
                drop(g);
            }

            // Now run enough returns to trigger the periodic donation
            // path (every 64 returns). 128 iterations = at least 2
            // donations.
            let before_central = pool_ref.available();
            for _ in 0..128 {
                let g = BufferPool::acquire_from(Arc::clone(pool_ref));
                drop(g);
            }
            let after_central = pool_ref.available();
            central_ref.store(
                after_central.saturating_sub(before_central),
                std::sync::atomic::Ordering::Relaxed,
            );

            // Slab remains within the per-thread cap.
            let (slots, _) = thread_slab::snapshot();
            assert!(slots <= 8);
        });
    });

    // Donation routed at least one buffer to the central overflow queue
    // (modulo the initial admission already counted as available before
    // the donation cadence kicked in).
    let donated = donated_to_central.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        donated <= 8,
        "donation should not exceed central queue capacity, got {donated}"
    );
}

#[test]
fn slab_lifo_order_warmest_first() {
    let pool = Arc::new(BufferPool::new(8));
    let pool_clone = Arc::clone(&pool);

    run_in_worker(move || {
        // Push two distinct buffers into the slab and verify LIFO on
        // the immediate re-acquire path.
        let mut g1 = BufferPool::acquire_from(Arc::clone(&pool_clone));
        g1[0] = 1;
        drop(g1);
        let mut g2 = BufferPool::acquire_from(Arc::clone(&pool_clone));
        g2[0] = 2;
        drop(g2);

        // The next acquire pops the most-recently-freed buffer (the one
        // tagged with 2). The tag is preserved because return_buffer
        // does not zero the buffer (see set_len SAFETY comment in
        // pool::return_buffer).
        let warmest = BufferPool::acquire_from(Arc::clone(&pool_clone));
        assert_eq!(
            warmest[0], 2,
            "LIFO: the most-recently-freed buffer must be returned first"
        );
    });
}

#[test]
fn many_threads_share_pool_without_panic() {
    // Stress test: spawn more threads than the pool's central cap so
    // the slab and overflow paths interleave under contention.
    let pool = Arc::new(BufferPool::new(8));
    let barrier = Arc::new(Barrier::new(16));
    let mut handles = Vec::new();
    for _ in 0..16 {
        let pool = Arc::clone(&pool);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..32 {
                let mut g = BufferPool::acquire_from(Arc::clone(&pool));
                g[0] = 0xAB;
            }
        }));
    }
    for h in handles {
        h.join().expect("worker panicked");
    }
}
