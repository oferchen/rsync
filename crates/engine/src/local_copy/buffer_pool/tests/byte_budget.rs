//! Byte-budget cap tests (#2245): retention by bytes, not by buffer count.

use super::super::*;
use std::sync::Arc;
use std::thread;

#[test]
fn byte_budget_default_is_none() {
    let pool = BufferPool::with_buffer_size(4, 1024);
    assert_eq!(pool.byte_budget(), None);
    assert_eq!(pool.retained_bytes(), 0);
    assert_eq!(pool.total_byte_overflows(), 0);
}

#[test]
fn byte_budget_is_set_via_builder() {
    let pool = BufferPool::with_buffer_size(4, 1024).with_byte_budget(8192);
    assert_eq!(pool.byte_budget(), Some(8192));
}

#[test]
fn byte_budget_allows_returns_below_cap() {
    // Cap large enough for 4 buffers of 1024 bytes. No allocation tracking
    // race here: we drop guards one at a time from a single thread.
    let pool = BufferPool::with_buffer_size(8, 1024).with_byte_budget(8 * 1024);
    {
        let mut guards = Vec::new();
        for _ in 0..4 {
            guards.push(pool.acquire());
        }
        // Drop guards: first goes to TLS, rest to central pool via byte budget.
        drop(guards);
    }
    // Pool should not have rejected any return - all retained bytes fit.
    assert_eq!(pool.total_byte_overflows(), 0);
}

// The byte-budget admission tests below trace returns through the
// single-slot TLS path: the first return per thread fills the TLS slot,
// subsequent returns hit the central byte budget. With the per-thread slab
// feature on, the slab swallows up to 8 returns per thread (1 MiB cap)
// before routing to the central queue, so the byte budget never gets the
// chance to reject. The slab is its own retention layer with its own caps
// and is covered by `tests/slab.rs`.
#[cfg(not(feature = "thread-slab-pool"))]
#[test]
fn byte_budget_falls_through_to_direct_alloc_at_cap() {
    // Budget tight enough that only one buffer (1024 bytes) fits in the
    // central pool. The TLS cache holds one separately - this verifies
    // the central admission path rejects past the cap.
    let pool = Arc::new(BufferPool::with_buffer_size(8, 1024).with_byte_budget(1024));

    // Acquire many buffers and drop them all. The first return per thread
    // fills the TLS slot; subsequent returns go through the central
    // admit_or_deallocate path where the byte budget gates them.
    let pool_clone = Arc::clone(&pool);
    thread::spawn(move || {
        let mut guards = Vec::new();
        for _ in 0..5 {
            guards.push(BufferPool::acquire_from(Arc::clone(&pool_clone)));
        }
        drop(guards);
    })
    .join()
    .expect("worker thread panicked");

    // First return fills TLS (no central admission), next return claims the
    // single allowed byte budget slot, remaining returns hit the cap and
    // get deallocated - producing overflow events.
    assert!(
        pool.total_byte_overflows() >= 1,
        "expected at least one overflow event, got {}",
        pool.total_byte_overflows()
    );
    // Acquire still works after overflow - no blocking, fresh allocation
    // from outside the pool.
    let g = BufferPool::acquire_from(Arc::clone(&pool));
    assert_eq!(g.len(), 1024);
}

#[cfg(not(feature = "thread-slab-pool"))]
#[test]
fn byte_budget_overflow_counter_accumulates() {
    let pool = Arc::new(BufferPool::with_buffer_size(8, 1024).with_byte_budget(1024));

    // Run multiple worker threads. Each thread's first return fills its
    // own TLS slot, subsequent returns race for the single central slot;
    // all losers count as overflows.
    let mut handles = Vec::new();
    for _ in 0..4 {
        let pool = Arc::clone(&pool);
        handles.push(thread::spawn(move || {
            let mut guards = Vec::new();
            for _ in 0..3 {
                guards.push(BufferPool::acquire_from(Arc::clone(&pool)));
            }
            drop(guards);
        }));
    }
    for h in handles {
        h.join().expect("worker thread panicked");
    }

    // With 4 workers x 3 buffers = 12 returns, of which up to 4 fill TLS
    // and 1 fits in the central pool, leaving multiple rejected returns.
    assert!(
        pool.total_byte_overflows() >= 1,
        "expected overflows from heavy contention, got {}",
        pool.total_byte_overflows()
    );
}

#[cfg(not(feature = "thread-slab-pool"))]
#[test]
fn byte_budget_capacity_recycles_after_acquire() {
    let pool = Arc::new(BufferPool::with_buffer_size(8, 1024).with_byte_budget(1024));

    // Pre-load the central pool with one buffer via a dedicated worker
    // thread (so TLS fill on the main thread does not intercept it later).
    let p = Arc::clone(&pool);
    thread::spawn(move || {
        let g1 = BufferPool::acquire_from(Arc::clone(&p));
        let g2 = BufferPool::acquire_from(Arc::clone(&p));
        drop(g1); // -> worker TLS
        drop(g2); // -> central pool (budget reserves 1024)
    })
    .join()
    .expect("worker thread panicked");

    // Central pool should hold one buffer; the byte budget tracks 1024.
    assert_eq!(pool.retained_bytes(), 1024);
    assert_eq!(pool.available(), 1);

    // Acquire on this thread pops the central buffer, releasing the
    // byte-budget reservation.
    let overflows_before = pool.total_byte_overflows();
    let g = BufferPool::acquire_from(Arc::clone(&pool));
    assert_eq!(pool.retained_bytes(), 0);
    drop(g);

    // Returned to main TLS, central count unchanged. Acquire from a new
    // worker to verify the pool still admits within the recycled budget.
    let p = Arc::clone(&pool);
    thread::spawn(move || {
        let g1 = BufferPool::acquire_from(Arc::clone(&p));
        let g2 = BufferPool::acquire_from(Arc::clone(&p));
        drop(g1);
        drop(g2);
    })
    .join()
    .expect("worker thread panicked");

    // At most one new overflow possible - capacity was fully released.
    assert!(pool.total_byte_overflows() <= overflows_before + 1);
}

#[cfg(not(feature = "thread-slab-pool"))]
#[test]
fn byte_budget_with_count_cap_is_min_of_both() {
    // Count cap is 1 buffer, byte budget admits 4 buffers worth: count cap
    // wins. The overflow counter does not increment on count-cap rejection
    // (only on byte-budget rejection).
    let pool = Arc::new(BufferPool::with_buffer_size(1, 1024).with_byte_budget(4 * 1024));

    let p = Arc::clone(&pool);
    thread::spawn(move || {
        let g1 = BufferPool::acquire_from(Arc::clone(&p));
        let g2 = BufferPool::acquire_from(Arc::clone(&p));
        let g3 = BufferPool::acquire_from(Arc::clone(&p));
        drop(g1); // TLS
        drop(g2); // central (count cap = 1, fits)
        drop(g3); // count cap rejected
    })
    .join()
    .expect("worker thread panicked");

    assert_eq!(pool.available(), 1);
    // Byte budget reservation made for g3 was released after the count cap
    // rejected it - retained should still equal exactly one buffer.
    assert_eq!(pool.retained_bytes(), 1024);
}

#[cfg(not(feature = "thread-slab-pool"))]
#[test]
fn byte_budget_stats_field_exposed() {
    let pool = Arc::new(BufferPool::with_buffer_size(8, 1024).with_byte_budget(1024));
    let p = Arc::clone(&pool);
    thread::spawn(move || {
        let mut guards = Vec::new();
        for _ in 0..4 {
            guards.push(BufferPool::acquire_from(Arc::clone(&p)));
        }
        drop(guards);
    })
    .join()
    .expect("worker thread panicked");

    let stats = pool.stats();
    assert_eq!(stats.total_byte_overflows, pool.total_byte_overflows());
    assert!(stats.total_byte_overflows >= 1);
}

#[test]
#[should_panic(expected = "byte budget must be greater than zero")]
fn byte_budget_zero_panics() {
    let _ = BufferPool::with_buffer_size(4, 1024).with_byte_budget(0);
}

#[test]
fn byte_budget_does_not_block_acquires() {
    // Even when the byte budget is fully exhausted by retained buffers,
    // acquire must not block - it falls back to fresh allocation. Use a
    // very tight budget and verify the acquire returns immediately.
    let pool = Arc::new(BufferPool::with_buffer_size(8, 1024).with_byte_budget(1024));

    // Saturate retained bytes from a worker thread (TLS slot on the worker
    // is irrelevant from the main thread's view).
    let p = Arc::clone(&pool);
    thread::spawn(move || {
        let mut guards = Vec::new();
        for _ in 0..3 {
            guards.push(BufferPool::acquire_from(Arc::clone(&p)));
        }
        drop(guards);
    })
    .join()
    .expect("worker thread panicked");

    // Acquire on the main thread. With the byte budget full and the
    // central pool admitting at most one buffer at a time, this must
    // still return quickly with a freshly allocated buffer.
    let start = std::time::Instant::now();
    let g = BufferPool::acquire_from(Arc::clone(&pool));
    let elapsed = start.elapsed();
    assert_eq!(g.len(), 1024);
    assert!(
        elapsed < std::time::Duration::from_millis(100),
        "acquire should not block on a full byte budget, elapsed={elapsed:?}"
    );
}
