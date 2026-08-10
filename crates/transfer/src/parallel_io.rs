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

/// Per-operation cost classification driving threshold selection.
///
/// Each variant gates one rayon dual-path call site. The variant identity
/// encodes the expected per-item cost profile - cheap syscalls dispatch
/// at a higher item count than expensive CPU-bound work - so call sites
/// look up the appropriate threshold by operation rather than reading
/// a single global constant.
///
/// Cost estimates are documented per variant and reflect warm-cache local
/// filesystem behaviour; networked filesystems shift the crossover upward
/// (see `docs/audits/parallel-stat-batch-size-profile.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParallelOp {
    /// `lstat` / `stat` / quick-check probes. Cheap (~1 us warm, hundreds of
    /// microseconds on NFS). Default crossover: [`DEFAULT_STAT_THRESHOLD`].
    Stat,
    /// Rolling + strong checksum signature generation over basis files.
    /// Expensive (milliseconds per file). Default crossover:
    /// [`DEFAULT_SIGNATURE_THRESHOLD`].
    Signature,
    /// `chmod` / `chown` / `utimes` / xattr / ACL application. Medium
    /// (a few syscalls per directory). Default crossover:
    /// [`DEFAULT_METADATA_THRESHOLD`].
    Metadata,
    /// `read_dir` + per-entry filter evaluation during `--delete` scans.
    /// Medium (one `read_dir` plus per-entry stat). Default crossover:
    /// [`DEFAULT_DELETION_THRESHOLD`].
    Deletion,
}

/// Per-operation thresholds for switching between sequential and parallel execution.
///
/// Different operations have different overhead profiles: CPU-bound signature
/// computation benefits from parallelism at lower counts than I/O-bound stat calls.
/// This struct allows each operation to use an appropriate threshold rather than
/// a single global constant.
///
/// All thresholds represent the minimum item count required to justify rayon
/// thread-pool dispatch. Below the threshold, sequential iteration is used.
///
/// Call sites should prefer [`ParallelThresholds::for_op`] over direct field
/// access so that adding a new operation only requires extending [`ParallelOp`]
/// and this struct, not editing every dispatch site.
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
    /// Returns the threshold configured for `op`.
    ///
    /// This is the preferred accessor for dispatch sites: it keeps the
    /// per-operation mapping in one place, so call sites read as
    /// `thresholds.for_op(ParallelOp::Stat)` rather than coupling to the
    /// struct's field layout.
    #[must_use]
    pub const fn for_op(&self, op: ParallelOp) -> usize {
        match op {
            ParallelOp::Stat => self.stat,
            ParallelOp::Signature => self.signature,
            ParallelOp::Metadata => self.metadata,
            ParallelOp::Deletion => self.deletion,
        }
    }

    /// Returns a copy with `op`'s threshold replaced.
    ///
    /// Mirrors [`ParallelThresholds::for_op`] for the override side: tests
    /// and call-site overrides set a single operation's value without
    /// having to know which struct field backs it.
    #[must_use]
    pub const fn with_op(mut self, op: ParallelOp, threshold: usize) -> Self {
        match op {
            ParallelOp::Stat => self.stat = threshold,
            ParallelOp::Signature => self.signature = threshold,
            ParallelOp::Metadata => self.metadata = threshold,
            ParallelOp::Deletion => self.deletion = threshold,
        }
        self
    }

    /// Sets the stat threshold.
    #[must_use]
    pub const fn with_stat(self, threshold: usize) -> Self {
        self.with_op(ParallelOp::Stat, threshold)
    }

    /// Sets the signature computation threshold.
    #[must_use]
    pub const fn with_signature(self, threshold: usize) -> Self {
        self.with_op(ParallelOp::Signature, threshold)
    }

    /// Sets the metadata application threshold.
    #[must_use]
    pub const fn with_metadata(self, threshold: usize) -> Self {
        self.with_op(ParallelOp::Metadata, threshold)
    }

    /// Sets the deletion scanning threshold.
    #[must_use]
    pub const fn with_deletion(self, threshold: usize) -> Self {
        self.with_op(ParallelOp::Deletion, threshold)
    }
}

/// Runs `f` on each item in parallel using rayon's work-stealing pool.
///
/// Returns results in the same order as the input. For lists smaller
/// than `min_parallel`, falls back to sequential `Iterator::map` to
/// avoid dispatch overhead.
///
/// Rayon's work-stealing scheduler avoids per-item task creation, semaphore
/// management, and runtime construction overhead. For 10K stat() calls, this
/// keeps dispatch bounded to the rayon pool size rather than spawning one
/// task per item.
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
            // Variable per-item delay forces worker reordering if ordering is not preserved.
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
        let items: Vec<i32> = (0..5).collect();
        let results = map_blocking(items, t.stat, |x| x * 2);
        assert_eq!(results, vec![0, 2, 4, 6, 8]);
    }

    #[test]
    fn for_op_returns_documented_defaults() {
        let t = ParallelThresholds::default();
        assert_eq!(t.for_op(ParallelOp::Stat), DEFAULT_STAT_THRESHOLD);
        assert_eq!(t.for_op(ParallelOp::Signature), DEFAULT_SIGNATURE_THRESHOLD);
        assert_eq!(t.for_op(ParallelOp::Metadata), DEFAULT_METADATA_THRESHOLD);
        assert_eq!(t.for_op(ParallelOp::Deletion), DEFAULT_DELETION_THRESHOLD);
    }

    #[test]
    fn for_op_reflects_field_overrides() {
        let t = ParallelThresholds::default()
            .with_stat(128)
            .with_signature(16)
            .with_metadata(256)
            .with_deletion(48);
        assert_eq!(t.for_op(ParallelOp::Stat), 128);
        assert_eq!(t.for_op(ParallelOp::Signature), 16);
        assert_eq!(t.for_op(ParallelOp::Metadata), 256);
        assert_eq!(t.for_op(ParallelOp::Deletion), 48);
    }

    #[test]
    fn with_op_overrides_only_targeted_operation() {
        let base = ParallelThresholds::default();
        let stat_only = base.with_op(ParallelOp::Stat, 7);
        assert_eq!(stat_only.for_op(ParallelOp::Stat), 7);
        assert_eq!(
            stat_only.for_op(ParallelOp::Signature),
            DEFAULT_SIGNATURE_THRESHOLD
        );
        assert_eq!(
            stat_only.for_op(ParallelOp::Metadata),
            DEFAULT_METADATA_THRESHOLD
        );
        assert_eq!(
            stat_only.for_op(ParallelOp::Deletion),
            DEFAULT_DELETION_THRESHOLD
        );

        let chained = stat_only
            .with_op(ParallelOp::Signature, 1)
            .with_op(ParallelOp::Metadata, 2)
            .with_op(ParallelOp::Deletion, 3);
        assert_eq!(chained.for_op(ParallelOp::Stat), 7);
        assert_eq!(chained.for_op(ParallelOp::Signature), 1);
        assert_eq!(chained.for_op(ParallelOp::Metadata), 2);
        assert_eq!(chained.for_op(ParallelOp::Deletion), 3);
    }

    #[test]
    fn with_op_and_with_field_helpers_agree() {
        let via_op = ParallelThresholds::default()
            .with_op(ParallelOp::Stat, 11)
            .with_op(ParallelOp::Signature, 12)
            .with_op(ParallelOp::Metadata, 13)
            .with_op(ParallelOp::Deletion, 14);
        let via_field = ParallelThresholds::default()
            .with_stat(11)
            .with_signature(12)
            .with_metadata(13)
            .with_deletion(14);
        assert_eq!(via_op, via_field);
    }

    #[test]
    fn for_op_drives_map_blocking_dispatch() {
        // Test override path: confirm `for_op` is a drop-in replacement for
        // direct field access in the dispatch site contract.
        let t = ParallelThresholds::default().with_op(ParallelOp::Stat, 0);
        let items: Vec<i32> = (0..5).collect();
        let results = map_blocking(items, t.for_op(ParallelOp::Stat), |x| x + 1);
        assert_eq!(results, vec![1, 2, 3, 4, 5]);
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
