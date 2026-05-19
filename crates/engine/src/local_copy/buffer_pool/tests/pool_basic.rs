//! Basic pool operations: acquire, release, guards, sizing, adaptive sizing.

use super::super::*;
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

    {
        let mut buffer = pool.acquire();
        buffer[0] = 42;
    }

    // First return on this thread goes to the thread-local cache, not the
    // central pool. Re-acquire should get the reused buffer from TLS.
    let buffer = pool.acquire();
    assert_eq!(buffer.len(), COPY_BUFFER_SIZE);
}

// `available()` returns the central-queue depth only. With the per-thread
// slab feature on, both returns land in the slab (cap = 8 slots) and the
// central queue stays empty, so this assertion does not hold. Slab-backend
// coverage lives in `tests/slab.rs`.
#[cfg(not(feature = "thread-slab-pool"))]
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
