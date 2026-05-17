//! Buffer-controller wiring and controlled-acquire feedback loop.

use super::super::*;
use std::sync::Arc;
use std::thread;

#[test]
fn no_buffer_controller_by_default() {
    let pool = BufferPool::new(4);
    assert!(!pool.has_buffer_controller());
    assert!(pool.buffer_controller().is_none());
}

#[test]
fn buffer_controller_enabled_via_builder() {
    let pool = BufferPool::new(4).with_buffer_controller(ControllerConfig::new(100 * 1024 * 1024));
    assert!(pool.has_buffer_controller());
    assert!(pool.buffer_controller().is_some());
}

#[test]
fn buffer_controller_enables_throughput_tracking() {
    // Enabling a buffer controller should automatically enable throughput
    // tracking, since the controller needs throughput samples.
    let pool = BufferPool::new(4).with_buffer_controller(ControllerConfig::new(100 * 1024 * 1024));
    assert!(pool.throughput_tracker().is_some());
}

#[test]
fn buffer_controller_preserves_existing_throughput_tracker() {
    // If throughput tracking is already enabled, adding a controller
    // should not replace the existing tracker.
    let pool = BufferPool::new(4)
        .with_throughput_tracking_alpha(0.5)
        .with_buffer_controller(ControllerConfig::new(100 * 1024 * 1024));
    assert!(pool.throughput_tracker().is_some());
    assert!(pool.has_buffer_controller());
}

#[test]
fn buffer_controller_with_builder_chain() {
    let pool = BufferPool::with_buffer_size(4, 1024)
        .with_memory_cap(8192)
        .with_adaptive_resizing()
        .with_buffer_controller(ControllerConfig::new(50 * 1024 * 1024));
    assert!(pool.has_buffer_controller());
    assert!(pool.is_adaptive());
    assert_eq!(pool.memory_cap(), Some(8192));
    assert_eq!(pool.buffer_size(), 1024);
}

#[test]
fn recommended_buffer_size_returns_controller_value_when_enabled() {
    let pool = BufferPool::new(4).with_buffer_controller(ControllerConfig::new(100 * 1024 * 1024));

    // Before any samples, the controller returns its initial size (midpoint).
    let size = pool.recommended_buffer_size();
    let controller = pool.buffer_controller().unwrap();
    assert_eq!(size, controller.buffer_size());
}

#[test]
fn record_transfer_feeds_controller() {
    let pool = BufferPool::new(4).with_buffer_controller(
        ControllerConfig::new(100 * 1024 * 1024)
            .min_size(16 * 1024)
            .max_size(4 * 1024 * 1024),
    );

    let initial_size = pool.recommended_buffer_size();

    // Record enough high-throughput samples to move the EMA past warmup
    // and feed the controller. 200 MB/s is above the 100 MB/s setpoint,
    // so the controller should shrink the buffer.
    for _ in 0..20 {
        pool.record_transfer(2_000_000, std::time::Duration::from_millis(10));
    }

    let size_after = pool.recommended_buffer_size();
    // The controller should have adjusted the size (either direction
    // depending on the relationship between throughput and setpoint).
    // We just verify it changed.
    assert_ne!(
        initial_size, size_after,
        "buffer size should change after recording transfer samples"
    );
}

#[test]
fn controller_recommended_size_supersedes_tracker() {
    // When both throughput tracking and a controller are enabled,
    // recommended_buffer_size should return the controller's value,
    // not the tracker's heuristic.
    let pool = BufferPool::new(4)
        .with_throughput_tracking()
        .with_buffer_controller(ControllerConfig::new(100 * 1024 * 1024));

    // Record samples so both tracker and controller have data.
    for _ in 0..10 {
        pool.record_transfer(1_000_000, std::time::Duration::from_secs(1));
    }

    let recommended = pool.recommended_buffer_size();
    let controller_size = pool.buffer_controller().unwrap().buffer_size();
    assert_eq!(
        recommended, controller_size,
        "recommended_buffer_size should match controller when enabled"
    );
}

#[test]
fn controller_convergence_through_pool_api() {
    // End-to-end convergence test through the pool's public API.
    // Simulates recording transfers that yield steady throughput and
    // verifies the controller converges the recommended buffer size
    // to a stable value.
    let setpoint = 50 * 1024 * 1024u64; // 50 MB/s
    let pool = BufferPool::new(4).with_buffer_controller(
        ControllerConfig::new(setpoint)
            .min_size(16 * 1024)
            .max_size(2 * 1024 * 1024),
    );

    // Simulate steady 50 MB/s: 500 KB every 10 ms.
    for _ in 0..100 {
        pool.record_transfer(500_000, std::time::Duration::from_millis(10));
    }

    // Collect sizes over next 20 samples and verify stability.
    let mut sizes = Vec::with_capacity(20);
    for _ in 0..20 {
        pool.record_transfer(500_000, std::time::Duration::from_millis(10));
        sizes.push(pool.recommended_buffer_size());
    }

    let min_s = *sizes.iter().min().unwrap();
    let max_s = *sizes.iter().max().unwrap();
    // The size should have settled within a reasonable range.
    let range = max_s.saturating_sub(min_s);
    let mean = sizes.iter().sum::<usize>() / sizes.len();
    let pct = if mean > 0 {
        range as f64 / mean as f64
    } else {
        0.0
    };
    assert!(
        pct < 0.20,
        "recommended size should stabilize: min={min_s}, max={max_s}, mean={mean}, range_pct={pct:.2}"
    );
}

#[test]
fn controller_setpoint_matches_config() {
    let pool = BufferPool::new(4).with_buffer_controller(ControllerConfig::new(42 * 1024 * 1024));
    let controller = pool.buffer_controller().unwrap();
    assert_eq!(controller.setpoint(), 42 * 1024 * 1024);
}

#[test]
fn controller_reset_preserves_recommended_size() {
    let pool = BufferPool::new(4).with_buffer_controller(
        ControllerConfig::new(100 * 1024 * 1024)
            .min_size(16 * 1024)
            .max_size(1024 * 1024),
    );

    // Feed some samples to move the buffer size away from initial.
    for _ in 0..20 {
        pool.record_transfer(1_000_000, std::time::Duration::from_millis(10));
    }

    let before = pool.recommended_buffer_size();
    pool.buffer_controller().unwrap().reset();
    let after = pool.recommended_buffer_size();
    assert_eq!(
        before, after,
        "reset should preserve buffer size, only clearing PID accumulators"
    );
}

#[test]
fn controller_concurrent_record_and_recommend() {
    // Verify thread safety: multiple threads record transfers while
    // others read the recommended buffer size.
    let pool = Arc::new(
        BufferPool::new(4).with_buffer_controller(
            ControllerConfig::new(100 * 1024 * 1024)
                .min_size(16 * 1024)
                .max_size(4 * 1024 * 1024),
        ),
    );

    let writer_count = 4;
    let reader_count = 4;
    let iterations = 200;

    let mut handles = Vec::new();

    for _ in 0..writer_count {
        let pool = Arc::clone(&pool);
        handles.push(thread::spawn(move || {
            for _ in 0..iterations {
                pool.record_transfer(500_000, std::time::Duration::from_millis(10));
            }
        }));
    }

    for _ in 0..reader_count {
        let pool = Arc::clone(&pool);
        handles.push(thread::spawn(move || {
            for _ in 0..iterations {
                let size = pool.recommended_buffer_size();
                assert!(size >= 16 * 1024, "size below minimum: {size}");
                assert!(size <= 4 * 1024 * 1024, "size above maximum: {size}");
            }
        }));
    }

    for h in handles {
        h.join().expect("thread panicked");
    }
}

// --- Controlled acquire integration tests ---

#[test]
fn acquire_controlled_from_uses_controller_size() {
    // When a buffer controller is enabled, acquire_controlled_from should
    // return a buffer at the controller's recommended size, not the
    // file-size-based adaptive size.
    let pool = Arc::new(
        BufferPool::new(4).with_buffer_controller(
            ControllerConfig::new(100 * 1024 * 1024)
                .min_size(16 * 1024)
                .max_size(4 * 1024 * 1024),
        ),
    );

    // The controller's initial size is midpoint of [16 KiB, 4 MiB].
    let expected = pool.recommended_buffer_size();
    assert!(expected > 16 * 1024);
    assert!(expected < 4 * 1024 * 1024);

    // For a tiny file, acquire_adaptive would give 8 KiB, but the
    // controller should override with its recommended size.
    let buf = BufferPool::acquire_controlled_from(Arc::clone(&pool), 1024);
    assert_eq!(
        buf.len(),
        expected,
        "controlled acquire should use controller's recommended size, not adaptive"
    );
}

#[test]
fn acquire_controlled_from_falls_back_to_adaptive_without_controller() {
    // Without a controller, acquire_controlled_from should behave like
    // acquire_adaptive_from.
    let pool = Arc::new(BufferPool::new(4));

    let buf_tiny = BufferPool::acquire_controlled_from(Arc::clone(&pool), 1024);
    assert_eq!(buf_tiny.len(), ADAPTIVE_BUFFER_TINY);

    let buf_large = BufferPool::acquire_controlled_from(Arc::clone(&pool), 100 * 1024 * 1024);
    assert_eq!(buf_large.len(), ADAPTIVE_BUFFER_LARGE);
}

#[test]
fn acquire_controlled_uses_pool_for_matching_size() {
    // When the controller's recommended size happens to match the pool's
    // default buffer size, the thread-local cache should be used.
    let pool = Arc::new(
        BufferPool::with_buffer_size(4, COPY_BUFFER_SIZE).with_buffer_controller(
            ControllerConfig::new(100 * 1024 * 1024)
                .min_size(COPY_BUFFER_SIZE)
                .max_size(COPY_BUFFER_SIZE),
        ),
    );

    // Controller is clamped to pool default -> uses TLS/pool path.
    let buf = BufferPool::acquire_controlled_from(Arc::clone(&pool), 10 * 1024 * 1024);
    assert_eq!(buf.len(), COPY_BUFFER_SIZE);
}

#[test]
fn acquire_controlled_borrowed_variant() {
    // Verify the borrowed acquire_controlled method works.
    let pool = BufferPool::new(4).with_buffer_controller(
        ControllerConfig::new(100 * 1024 * 1024)
            .min_size(16 * 1024)
            .max_size(4 * 1024 * 1024),
    );

    let expected = pool.recommended_buffer_size();
    let buf = pool.acquire_controlled(1024);
    assert_eq!(buf.len(), expected);
}

#[test]
fn controlled_acquire_size_grows_when_throughput_below_setpoint() {
    // Feed low-throughput samples so the controller grows the recommended
    // buffer size, then verify acquire_controlled_from reflects the growth.
    let setpoint = 100 * 1024 * 1024u64; // 100 MB/s
    let pool = Arc::new(
        BufferPool::new(4).with_buffer_controller(
            ControllerConfig::new(setpoint)
                .min_size(16 * 1024)
                .max_size(4 * 1024 * 1024),
        ),
    );

    let initial_size = pool.recommended_buffer_size();

    // Record low throughput (1 MB/s << 100 MB/s setpoint).
    for _ in 0..30 {
        pool.record_transfer(10_000, std::time::Duration::from_millis(10));
    }

    let grown_size = pool.recommended_buffer_size();
    assert!(
        grown_size > initial_size,
        "controller should grow buffer when throughput is below setpoint: initial={initial_size}, after={grown_size}"
    );

    // Controlled acquire should reflect the grown size.
    let buf = BufferPool::acquire_controlled_from(Arc::clone(&pool), 1024);
    assert_eq!(buf.len(), grown_size);
}

#[test]
fn controlled_acquire_size_shrinks_when_throughput_above_setpoint() {
    // Feed high-throughput samples so the controller shrinks the recommended
    // buffer size, then verify acquire_controlled_from reflects the shrink.
    let setpoint = 10 * 1024 * 1024u64; // 10 MB/s
    let pool = Arc::new(
        BufferPool::new(4).with_buffer_controller(
            ControllerConfig::new(setpoint)
                .min_size(16 * 1024)
                .max_size(4 * 1024 * 1024),
        ),
    );

    let initial_size = pool.recommended_buffer_size();

    // Record high throughput (1 GB/s >> 10 MB/s setpoint).
    for _ in 0..30 {
        pool.record_transfer(10_000_000, std::time::Duration::from_millis(10));
    }

    let shrunk_size = pool.recommended_buffer_size();
    assert!(
        shrunk_size < initial_size,
        "controller should shrink buffer when throughput is above setpoint: initial={initial_size}, after={shrunk_size}"
    );

    // Controlled acquire should reflect the shrunk size.
    let buf = BufferPool::acquire_controlled_from(Arc::clone(&pool), 1024);
    assert_eq!(buf.len(), shrunk_size);
}

#[test]
fn controlled_acquire_returned_buffer_resized_to_pool_default() {
    // A buffer acquired via acquire_controlled_from at a non-default size
    // should be resized to the pool's default on return, so a subsequent
    // standard acquire gets the correct size.
    let pool = Arc::new(
        BufferPool::new(4).with_buffer_controller(
            ControllerConfig::new(100 * 1024 * 1024)
                .min_size(16 * 1024)
                .max_size(4 * 1024 * 1024),
        ),
    );

    // Acquire a controlled buffer (likely non-default size).
    {
        let buf = BufferPool::acquire_controlled_from(Arc::clone(&pool), 1024);
        let _len = buf.len(); // May differ from COPY_BUFFER_SIZE.
    }

    // Standard acquire should get a buffer at pool's default size.
    let buf = BufferPool::acquire_from(Arc::clone(&pool));
    assert_eq!(buf.len(), COPY_BUFFER_SIZE);
}

#[test]
fn controlled_acquire_concurrent_safety() {
    // Multiple threads acquire controlled buffers while another thread
    // records throughput. All buffer sizes must stay within the
    // controller's min/max bounds.
    let min = 16 * 1024;
    let max = 4 * 1024 * 1024;
    let pool = Arc::new(
        BufferPool::new(8).with_buffer_controller(
            ControllerConfig::new(100 * 1024 * 1024)
                .min_size(min)
                .max_size(max),
        ),
    );

    let mut handles = Vec::new();

    // Writer threads record throughput.
    for _ in 0..2 {
        let pool = Arc::clone(&pool);
        handles.push(thread::spawn(move || {
            for _ in 0..200 {
                pool.record_transfer(500_000, std::time::Duration::from_millis(10));
            }
        }));
    }

    // Reader threads acquire controlled buffers.
    for _ in 0..4 {
        let pool = Arc::clone(&pool);
        handles.push(thread::spawn(move || {
            for _ in 0..100 {
                let buf = BufferPool::acquire_controlled_from(Arc::clone(&pool), 1024);
                let len = buf.len();
                assert!(len >= min, "controlled buffer below min: {len} < {min}");
                assert!(len <= max, "controlled buffer above max: {len} > {max}");
            }
        }));
    }

    for h in handles {
        h.join().expect("thread panicked");
    }
}

#[test]
fn controlled_acquire_end_to_end_feedback_loop() {
    // End-to-end test: simulate a transfer loop where each iteration
    // acquires a controlled buffer and records its throughput. The buffer
    // size should converge toward a stable value.
    let setpoint = 50 * 1024 * 1024u64; // 50 MB/s
    let pool = Arc::new(
        BufferPool::new(4).with_buffer_controller(
            ControllerConfig::new(setpoint)
                .min_size(16 * 1024)
                .max_size(2 * 1024 * 1024),
        ),
    );

    // Simulate steady 50 MB/s: 500 KB every 10 ms.
    for _ in 0..100 {
        let _buf = BufferPool::acquire_controlled_from(Arc::clone(&pool), 10 * 1024 * 1024);
        pool.record_transfer(500_000, std::time::Duration::from_millis(10));
    }

    // Collect buffer sizes over the next 20 iterations.
    let mut sizes = Vec::with_capacity(20);
    for _ in 0..20 {
        let buf = BufferPool::acquire_controlled_from(Arc::clone(&pool), 10 * 1024 * 1024);
        sizes.push(buf.len());
        pool.record_transfer(500_000, std::time::Duration::from_millis(10));
    }

    let min_s = *sizes.iter().min().unwrap();
    let max_s = *sizes.iter().max().unwrap();
    let mean = sizes.iter().sum::<usize>() / sizes.len();
    let range_pct = if mean > 0 {
        (max_s - min_s) as f64 / mean as f64
    } else {
        0.0
    };
    assert!(
        range_pct < 0.20,
        "buffer size should stabilize in feedback loop: min={min_s}, max={max_s}, mean={mean}, range_pct={range_pct:.2}"
    );
}
