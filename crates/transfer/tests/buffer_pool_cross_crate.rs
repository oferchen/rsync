//! Integration test verifying that the `engine` buffer pool types are accessible
//! from downstream crates (`transfer` depends on `engine`).

use std::sync::Arc;

use engine::{BufferGuard, BufferPool, BufferPoolStats, DefaultAllocator, global_buffer_pool};

#[test]
fn acquire_and_return_via_public_api() {
    let pool = Arc::new(BufferPool::with_buffer_size(4, 64));

    // Acquire a buffer through the Arc-based API.
    let mut guard: BufferGuard = BufferPool::acquire_from(Arc::clone(&pool));
    assert_eq!(guard.len(), 64);

    // Write through the guard.
    guard[0] = 0xAB;
    assert_eq!(guard[0], 0xAB);

    // Drop returns the buffer to the pool.
    drop(guard);
    assert_eq!(pool.available(), 1);
}

#[test]
fn borrowed_guard_via_public_api() {
    let pool = BufferPool::with_buffer_size(4, 32);

    let guard = pool.acquire();
    assert_eq!(guard.len(), 32);
    drop(guard);

    assert_eq!(pool.available(), 1);
}

#[test]
fn global_pool_accessible_cross_crate() {
    let pool = global_buffer_pool();
    assert!(pool.buffer_size() > 0);
    assert!(pool.max_buffers() > 0);
}

#[test]
fn stats_accessible_cross_crate() {
    let pool = Arc::new(BufferPool::with_buffer_size(2, 128));

    // First acquire is a miss (pool starts empty).
    let guard = BufferPool::acquire_from(Arc::clone(&pool));
    drop(guard);

    // Second acquire should hit the pool.
    let guard = BufferPool::acquire_from(Arc::clone(&pool));
    drop(guard);

    let stats: BufferPoolStats = pool.stats();
    assert!(stats.total_acquires() >= 2);
}

#[test]
fn default_allocator_is_accessible() {
    let _alloc: DefaultAllocator = DefaultAllocator;
}
