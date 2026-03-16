#![deny(unsafe_code)]
//! Parallel I/O using rayon for bounded-concurrency metadata operations.
//!
//! Provides a generic `map_blocking` helper that runs I/O-bound closures
//! (stat, chmod, chown) on rayon's work-stealing thread pool, which is
//! lighter than tokio `spawn_blocking` for synchronous I/O operations.
//!
//! For lists below `min_parallel`, falls back to sequential `Iterator::map`
//! to avoid thread-pool dispatch overhead.

use rayon::prelude::*;

/// Runs `f` on each item in parallel using rayon's work-stealing pool.
///
/// Returns results in the same order as the input. For lists smaller
/// than `min_parallel`, falls back to sequential `Iterator::map` to
/// avoid dispatch overhead.
///
/// Unlike the previous tokio `spawn_blocking` implementation, rayon's
/// approach avoids per-item task creation, semaphore management, and
/// runtime construction overhead. For 10K stat() calls, this eliminates
/// ~10K task spawns and semaphore acquire/release cycles.
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

    items.into_par_iter().map(f).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
