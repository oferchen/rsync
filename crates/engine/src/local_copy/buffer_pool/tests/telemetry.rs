//! Hit / miss / growth counters and the BufferPoolStats snapshot.

use super::super::*;
use std::sync::Arc;
use std::thread;

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
    let stats = BufferPoolStats {
        total_hits: 0,
        total_misses: 0,
        total_growths: 0,
        total_byte_overflows: 0,
    };
    assert_eq!(stats.hit_rate(), 0.0);
    assert_eq!(stats.total_acquires(), 0);
}

#[test]
fn stats_hit_rate_all_hits() {
    let stats = BufferPoolStats {
        total_hits: 100,
        total_misses: 0,
        total_growths: 0,
        total_byte_overflows: 0,
    };
    assert!((stats.hit_rate() - 1.0).abs() < f64::EPSILON);
}

#[test]
fn stats_hit_rate_all_misses() {
    let stats = BufferPoolStats {
        total_hits: 0,
        total_misses: 50,
        total_growths: 0,
        total_byte_overflows: 0,
    };
    assert_eq!(stats.hit_rate(), 0.0);
    assert_eq!(stats.total_acquires(), 50);
}

#[test]
fn stats_debug_and_clone() {
    let stats = BufferPoolStats {
        total_hits: 10,
        total_misses: 5,
        total_growths: 1,
        total_byte_overflows: 2,
    };
    let cloned = stats;
    assert_eq!(stats, cloned);
    let debug = format!("{stats:?}");
    assert!(debug.contains("total_hits"));
    assert!(debug.contains("total_misses"));
    assert!(debug.contains("total_growths"));
    assert!(debug.contains("total_byte_overflows"));
}
