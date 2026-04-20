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

    items.into_par_iter().map(f).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

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

    /// Simulates a stat result paired with its original file list index.
    /// The `delay_hint` field introduces variable per-item work to stress
    /// rayon's work-stealing scheduler and expose any reordering bugs.
    #[derive(Debug, Clone)]
    struct FakeFileEntry {
        index: usize,
        path: String,
        size: u64,
        delay_hint: u8,
    }

    /// Simulates a stat result carrying its original index for ordering checks.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct StatResult {
        index: usize,
        path: String,
        size: u64,
    }

    /// Generates a vector of fake file entries with random paths and sizes.
    fn arb_file_entries() -> impl Strategy<Value = Vec<FakeFileEntry>> {
        prop::collection::vec(
            (
                "[a-z]{1,8}/[a-z]{1,8}\\.[a-z]{1,3}",
                0..10_000_000u64,
                0..255u8,
            ),
            0..512,
        )
        .prop_map(|items| {
            items
                .into_iter()
                .enumerate()
                .map(|(i, (path, size, delay))| FakeFileEntry {
                    index: i,
                    path,
                    size,
                    delay_hint: delay,
                })
                .collect()
        })
    }

    proptest! {
        /// Verifies that `map_blocking` preserves input ordering regardless
        /// of list size, threshold, or per-item work variance.
        ///
        /// This property must hold for the receiver's parallel quick-check:
        /// file list indices drive protocol exchange, so any reordering would
        /// cause the wrong file to be matched with its delta data.
        #[test]
        fn parallel_stat_preserves_ordering(
            entries in arb_file_entries(),
            threshold in 0..128usize,
        ) {
            let expected: Vec<StatResult> = entries
                .iter()
                .map(|e| StatResult {
                    index: e.index,
                    path: e.path.clone(),
                    size: e.size,
                })
                .collect();

            let results = map_blocking(entries, threshold, |entry| {
                // Simulate variable-cost stat work via busy-spin proportional
                // to delay_hint. This stresses rayon's scheduler without
                // relying on thread::sleep (which is too coarse).
                let mut acc = 0u64;
                for _ in 0..(entry.delay_hint as u64 * 10) {
                    acc = acc.wrapping_add(entry.size);
                }
                // Prevent the optimizer from eliding the loop
                let _ = std::hint::black_box(acc);

                StatResult {
                    index: entry.index,
                    path: entry.path,
                    size: entry.size,
                }
            });

            prop_assert_eq!(results.len(), expected.len());
            for (i, (got, want)) in results.iter().zip(expected.iter()).enumerate() {
                prop_assert_eq!(
                    got, want,
                    "ordering diverged at position {}: got index {}, want index {}",
                    i, got.index, want.index,
                );
            }
        }

        /// Verifies that both the sequential and parallel code paths produce
        /// identical results for the same input, regardless of where the
        /// threshold falls relative to the list length.
        #[test]
        fn sequential_and_parallel_paths_agree(
            items in prop::collection::vec(0..10_000i64, 0..256),
        ) {
            let sequential = map_blocking(items.clone(), usize::MAX, |x| x * 3 + 1);
            let parallel = map_blocking(items, 1, |x| x * 3 + 1);
            prop_assert_eq!(sequential, parallel);
        }
    }
}
