//! Throughput-tracker integration and recommended-buffer-size sizing.

use super::super::*;
use std::sync::Arc;
use std::thread;

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
    use super::super::throughput::{MAX_BUFFER_SIZE, MIN_BUFFER_SIZE};

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
