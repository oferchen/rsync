#![deny(unsafe_code)]
//! Bounded-concurrency parallel I/O using tokio `spawn_blocking` + `Semaphore`.
//!
//! Provides a generic `map_blocking` helper that runs I/O-bound closures
//! (stat, chmod, chown) on a bounded thread pool, limiting concurrency to
//! `available_parallelism() * 2` (matching `engine::async_io::batch`).
//!
//! Unlike rayon's work-stealing pool, this approach gives explicit control
//! over the number of in-flight I/O operations, preventing file descriptor
//! exhaustion on large file lists.

use std::sync::Arc;

use tokio::sync::Semaphore;

/// Maximum concurrency to prevent file descriptor exhaustion.
///
/// Each stat/metadata op holds 1-2 fds; 64 workers = 128 fds max.
const MAX_CONCURRENT_UPPER_BOUND: usize = 64;

/// Minimum concurrency - always at least one worker.
const MAX_CONCURRENT_LOWER_BOUND: usize = 1;

/// Returns the I/O parallelism level for metadata operations.
///
/// Priority: `RSYNC_MAX_CONCURRENT` env var > CPU-derived default
/// (`available_parallelism() * 2`). Clamped to `[1, 64]`.
///
/// Mirrors `engine::async_io::batch::resolve_max_concurrent`.
pub(crate) fn io_parallelism() -> usize {
    let cpu_default = || {
        std::thread::available_parallelism()
            .map(|n| n.get() * 2)
            .unwrap_or(4)
    };
    let raw = if let Ok(env_val) = std::env::var("RSYNC_MAX_CONCURRENT") {
        env_val.parse::<usize>().unwrap_or_else(|_| cpu_default())
    } else {
        cpu_default()
    };
    raw.clamp(MAX_CONCURRENT_LOWER_BOUND, MAX_CONCURRENT_UPPER_BOUND)
}

/// Runs `f` on each item in parallel using `tokio::task::spawn_blocking`,
/// bounded by a semaphore to limit concurrency to [`io_parallelism()`].
///
/// Returns results in the same order as the input. Creates a temporary
/// single-threaded tokio runtime internally - the caller does not need
/// to be in an async context.
///
/// For lists smaller than `min_parallel`, falls back to sequential
/// `Iterator::map` to avoid runtime creation overhead.
pub(crate) fn map_blocking<T, R, F>(items: Vec<T>, min_parallel: usize, f: F) -> Vec<R>
where
    T: Send + 'static,
    R: Send + 'static,
    F: Fn(T) -> R + Send + Sync + 'static,
{
    if items.is_empty() {
        return Vec::new();
    }

    if items.len() < min_parallel {
        return items.into_iter().map(&f).collect();
    }

    let parallelism = io_parallelism();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime creation should not fail");

    rt.block_on(async {
        let semaphore = Arc::new(Semaphore::new(parallelism));
        let f = Arc::new(f);
        let mut handles = Vec::with_capacity(items.len());

        for item in items {
            let permit = semaphore
                .clone()
                .acquire_owned()
                .await
                .expect("semaphore should not be closed");
            let f = f.clone();
            handles.push(tokio::task::spawn_blocking(move || {
                let _permit = permit;
                f(item)
            }));
        }

        let mut results = Vec::with_capacity(handles.len());
        for handle in handles {
            results.push(handle.await.expect("spawn_blocking task should not panic"));
        }
        results
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_io_parallelism_within_bounds() {
        let p = io_parallelism();
        assert!(p >= MAX_CONCURRENT_LOWER_BOUND);
        assert!(p <= MAX_CONCURRENT_UPPER_BOUND);
    }

    #[test]
    fn test_map_blocking_empty() {
        let results: Vec<i32> = map_blocking(Vec::new(), 4, |x: i32| x * 2);
        assert!(results.is_empty());
    }

    #[test]
    fn test_map_blocking_sequential_fallback() {
        let items: Vec<i32> = (0..3).collect();
        let results = map_blocking(items, 10, |x| x * 2);
        assert_eq!(results, vec![0, 2, 4]);
    }

    #[test]
    fn test_map_blocking_parallel() {
        let items: Vec<i32> = (0..100).collect();
        let results = map_blocking(items, 4, |x| x + 1);
        let expected: Vec<i32> = (1..101).collect();
        assert_eq!(results, expected);
    }

    #[test]
    fn test_map_blocking_preserves_order() {
        let items: Vec<u64> = (0..50).collect();
        let results = map_blocking(items, 4, |x| {
            // Introduce variable delay to test ordering
            if x % 2 == 0 {
                std::thread::sleep(std::time::Duration::from_micros(100));
            }
            x
        });
        let expected: Vec<u64> = (0..50).collect();
        assert_eq!(results, expected);
    }
}
