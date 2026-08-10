//! Memory-cap and try-acquire backpressure behaviour.

use super::super::*;
use super::support::TrackingAllocator;
use std::sync::Arc;
use std::thread;

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
