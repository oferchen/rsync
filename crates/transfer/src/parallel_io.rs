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

/// Default threshold for parallel stat operations (filesystem metadata lookups).
///
/// Below this count, sequential iteration avoids rayon thread-pool dispatch overhead.
pub const DEFAULT_STAT_THRESHOLD: usize = 64;

/// Default threshold for parallel signature computation.
///
/// Signatures are CPU-bound (rolling + strong checksums), so parallelism
/// pays off at lower counts than I/O-bound operations.
pub const DEFAULT_SIGNATURE_THRESHOLD: usize = 32;

/// Default threshold for parallel metadata application (chmod/chown/utimes).
///
/// Mixed I/O and CPU - similar overhead profile to stat operations.
pub const DEFAULT_METADATA_THRESHOLD: usize = 64;

/// Default threshold for parallel deletion scanning.
///
/// Each directory scan involves `read_dir` + per-entry checks, so the
/// per-item cost is higher than a single stat call.
pub const DEFAULT_DELETION_THRESHOLD: usize = 64;

/// Per-operation thresholds for switching between sequential and parallel execution.
///
/// Different operations have different overhead profiles: CPU-bound signature
/// computation benefits from parallelism at lower counts than I/O-bound stat calls.
/// This struct allows each operation to use an appropriate threshold rather than
/// a single global constant.
///
/// All thresholds represent the minimum item count required to justify rayon
/// thread-pool dispatch. Below the threshold, sequential iteration is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParallelThresholds {
    /// Minimum file count for parallel `stat()` / `lstat()` calls.
    pub stat: usize,
    /// Minimum file count for parallel signature (basis file + checksum) computation.
    pub signature: usize,
    /// Minimum directory count for parallel metadata application (`chmod`/`chown`/`utimes`).
    pub metadata: usize,
    /// Minimum directory count for parallel deletion scanning.
    pub deletion: usize,
}

impl Default for ParallelThresholds {
    fn default() -> Self {
        Self {
            stat: DEFAULT_STAT_THRESHOLD,
            signature: DEFAULT_SIGNATURE_THRESHOLD,
            metadata: DEFAULT_METADATA_THRESHOLD,
            deletion: DEFAULT_DELETION_THRESHOLD,
        }
    }
}

impl ParallelThresholds {
    /// Sets the stat threshold.
    #[must_use]
    pub const fn with_stat(mut self, threshold: usize) -> Self {
        self.stat = threshold;
        self
    }

    /// Sets the signature computation threshold.
    #[must_use]
    pub const fn with_signature(mut self, threshold: usize) -> Self {
        self.signature = threshold;
        self
    }

    /// Sets the metadata application threshold.
    #[must_use]
    pub const fn with_metadata(mut self, threshold: usize) -> Self {
        self.metadata = threshold;
        self
    }

    /// Sets the deletion scanning threshold.
    #[must_use]
    pub const fn with_deletion(mut self, threshold: usize) -> Self {
        self.deletion = threshold;
        self
    }
}

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

    // Ordering: callers zip results with input by position.
    // Preserved by `into_par_iter().map().collect()` (rayon preserves index order).
    // Violation breaks result-to-file correspondence, applying wrong metadata.
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

    #[test]
    fn test_parallel_thresholds_default() {
        let t = ParallelThresholds::default();
        assert_eq!(t.stat, DEFAULT_STAT_THRESHOLD);
        assert_eq!(t.signature, DEFAULT_SIGNATURE_THRESHOLD);
        assert_eq!(t.metadata, DEFAULT_METADATA_THRESHOLD);
        assert_eq!(t.deletion, DEFAULT_DELETION_THRESHOLD);
    }

    #[test]
    fn test_parallel_thresholds_default_values() {
        let t = ParallelThresholds::default();
        assert_eq!(t.stat, 64);
        assert_eq!(t.signature, 32);
        assert_eq!(t.metadata, 64);
        assert_eq!(t.deletion, 64);
    }

    #[test]
    fn test_parallel_thresholds_builder() {
        let t = ParallelThresholds::default()
            .with_stat(128)
            .with_signature(16)
            .with_metadata(256)
            .with_deletion(48);
        assert_eq!(t.stat, 128);
        assert_eq!(t.signature, 16);
        assert_eq!(t.metadata, 256);
        assert_eq!(t.deletion, 48);
    }

    #[test]
    fn test_parallel_thresholds_copy() {
        let t1 = ParallelThresholds::default().with_stat(100);
        let t2 = t1;
        assert_eq!(t1, t2);
    }

    #[test]
    fn test_parallel_thresholds_zero_thresholds() {
        let t = ParallelThresholds::default().with_stat(0).with_signature(0);
        assert_eq!(t.stat, 0);
        assert_eq!(t.signature, 0);
        // Always uses parallel path when threshold is 0
        let items: Vec<i32> = (0..5).collect();
        let results = map_blocking(items, t.stat, |x| x * 2);
        assert_eq!(results, vec![0, 2, 4, 6, 8]);
    }
}
