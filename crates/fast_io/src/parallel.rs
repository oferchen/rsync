//! Parallel file I/O operations using rayon.
//!
//! This module provides utilities for parallel file processing that maximize
//! throughput on multi-core systems.

use rayon::prelude::*;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering as AtomicOrdering};

/// Result of a parallel operation on multiple items.
#[derive(Debug)]
pub struct ParallelResult<T> {
    /// Successfully processed items.
    pub successes: Vec<T>,
    /// Errors encountered during processing.
    pub errors: Vec<(usize, io::Error)>,
    /// Total bytes processed.
    pub bytes_processed: u64,
}

impl<T> Default for ParallelResult<T> {
    fn default() -> Self {
        Self {
            successes: Vec::new(),
            errors: Vec::new(),
            bytes_processed: 0,
        }
    }
}

impl<T> ParallelResult<T> {
    /// Returns true if all operations succeeded.
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.errors.is_empty()
    }

    /// Returns the number of successful operations.
    #[must_use]
    pub fn success_count(&self) -> usize {
        self.successes.len()
    }

    /// Returns the number of failed operations.
    #[must_use]
    pub fn error_count(&self) -> usize {
        self.errors.len()
    }
}

/// Executor for parallel file operations.
///
/// Provides a builder-style API for configuring and executing parallel I/O.
///
/// # Example
///
/// ```ignore
/// use fast_io::ParallelExecutor;
///
/// let executor = ParallelExecutor::new()
///     .with_thread_count(4)
///     .with_buffer_size(64 * 1024);
///
/// let result = executor.process_files(&paths, |path| {
///     // Process each file...
///     Ok(())
/// });
/// ```
#[derive(Debug, Clone)]
pub struct ParallelExecutor {
    /// Number of threads to use (0 = rayon default).
    thread_count: usize,
    /// Buffer size for I/O operations.
    buffer_size: usize,
    /// Whether to continue on errors.
    continue_on_error: bool,
}

impl Default for ParallelExecutor {
    fn default() -> Self {
        Self {
            thread_count: 0,
            buffer_size: 128 * 1024,
            continue_on_error: true,
        }
    }
}

impl ParallelExecutor {
    /// Creates a new parallel executor with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the number of threads to use.
    ///
    /// Pass 0 to use rayon's default (typically number of CPU cores).
    #[must_use]
    pub fn with_thread_count(mut self, count: usize) -> Self {
        self.thread_count = count;
        self
    }

    /// Sets the buffer size for I/O operations.
    #[must_use]
    pub fn with_buffer_size(mut self, size: usize) -> Self {
        self.buffer_size = size;
        self
    }

    /// Sets whether to continue processing on errors.
    #[must_use]
    pub fn continue_on_error(mut self, continue_on_error: bool) -> Self {
        self.continue_on_error = continue_on_error;
        self
    }

    /// Processes items in parallel, returning results for each.
    ///
    /// # Arguments
    ///
    /// * `items` - Items to process
    /// * `process_fn` - Function to apply to each item
    ///
    /// # Returns
    ///
    /// A `ParallelResult` containing successes and errors.
    pub fn process<T, U, F>(&self, items: &[T], process_fn: F) -> ParallelResult<U>
    where
        T: Sync,
        U: Send,
        F: Fn(&T) -> io::Result<U> + Sync,
    {
        let errors = Arc::new(std::sync::Mutex::new(Vec::new()));
        let bytes = Arc::new(AtomicU64::new(0));

        let successes: Vec<U> = items
            .par_iter()
            .enumerate()
            .filter_map(|(idx, item)| match process_fn(item) {
                Ok(result) => Some(result),
                Err(e) => {
                    errors.lock().unwrap().push((idx, e));
                    None
                }
            })
            .collect();

        ParallelResult {
            successes,
            errors: Arc::try_unwrap(errors).unwrap().into_inner().unwrap(),
            bytes_processed: bytes.load(AtomicOrdering::Relaxed),
        }
    }

    /// Processes file paths in parallel.
    ///
    /// Specialized version for file operations that tracks bytes processed.
    pub fn process_files<P, U, F>(&self, paths: &[P], process_fn: F) -> ParallelResult<U>
    where
        P: AsRef<Path> + Sync,
        U: Send,
        F: Fn(&Path) -> io::Result<(U, u64)> + Sync,
    {
        let errors = Arc::new(std::sync::Mutex::new(Vec::new()));
        let bytes = Arc::new(AtomicU64::new(0));

        let successes: Vec<U> = paths
            .par_iter()
            .enumerate()
            .filter_map(|(idx, path)| match process_fn(path.as_ref()) {
                Ok((result, file_bytes)) => {
                    bytes.fetch_add(file_bytes, AtomicOrdering::Relaxed);
                    Some(result)
                }
                Err(e) => {
                    errors.lock().unwrap().push((idx, e));
                    None
                }
            })
            .collect();

        ParallelResult {
            successes,
            errors: Arc::try_unwrap(errors).unwrap().into_inner().unwrap(),
            bytes_processed: bytes.load(AtomicOrdering::Relaxed),
        }
    }

    /// Copies multiple files in parallel.
    ///
    /// # Arguments
    ///
    /// * `operations` - List of (source, destination) path pairs
    ///
    /// # Returns
    ///
    /// A `ParallelResult` with bytes copied for each successful operation.
    pub fn copy_files<P: AsRef<Path> + Sync>(&self, operations: &[(P, P)]) -> ParallelResult<u64> {
        self.process(operations, |(src, dst)| {
            std::fs::copy(src.as_ref(), dst.as_ref())
        })
    }
}

/// Statistics for tracking parallel operation progress.
#[derive(Debug, Default)]
pub struct ParallelStats {
    /// Number of items processed.
    pub items_processed: AtomicUsize,
    /// Number of items that failed.
    pub items_failed: AtomicUsize,
    /// Total bytes processed.
    pub bytes_processed: AtomicU64,
}

impl ParallelStats {
    /// Creates new stats tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a successful operation.
    pub fn record_success(&self, bytes: u64) {
        self.items_processed.fetch_add(1, AtomicOrdering::Relaxed);
        self.bytes_processed
            .fetch_add(bytes, AtomicOrdering::Relaxed);
    }

    /// Records a failed operation.
    pub fn record_failure(&self) {
        self.items_failed.fetch_add(1, AtomicOrdering::Relaxed);
    }

    /// Returns the total items processed (success + failure).
    #[must_use]
    pub fn total_items(&self) -> usize {
        self.items_processed.load(AtomicOrdering::Relaxed)
            + self.items_failed.load(AtomicOrdering::Relaxed)
    }

    /// Returns a snapshot of current statistics.
    #[must_use]
    pub fn snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            items_processed: self.items_processed.load(AtomicOrdering::Relaxed),
            items_failed: self.items_failed.load(AtomicOrdering::Relaxed),
            bytes_processed: self.bytes_processed.load(AtomicOrdering::Relaxed),
        }
    }
}

/// A point-in-time snapshot of parallel operation statistics.
#[derive(Debug, Clone, Copy)]
pub struct StatsSnapshot {
    /// Number of items processed.
    pub items_processed: usize,
    /// Number of items that failed.
    pub items_failed: usize,
    /// Total bytes processed.
    pub bytes_processed: u64,
}

/// Parallel iterator extension for file operations.
pub trait ParallelFileOps: ParallelIterator {
    /// Maps items to results, collecting errors separately.
    fn try_map_collect<T, F>(self, f: F) -> ParallelResult<T>
    where
        T: Send,
        F: Fn(Self::Item) -> io::Result<T> + Sync + Send,
        Self: Sized,
        Self::Item: Send,
    {
        let errors = Arc::new(std::sync::Mutex::new(Vec::new()));
        let counter = Arc::new(AtomicUsize::new(0));

        let successes: Vec<T> = self
            .map(|item| {
                let idx = counter.fetch_add(1, AtomicOrdering::Relaxed);
                match f(item) {
                    Ok(result) => Some(result),
                    Err(e) => {
                        errors.lock().unwrap().push((idx, e));
                        None
                    }
                }
            })
            .flatten()
            .collect();

        ParallelResult {
            successes,
            errors: Arc::try_unwrap(errors).unwrap().into_inner().unwrap(),
            bytes_processed: 0,
        }
    }
}

impl<I: ParallelIterator> ParallelFileOps for I {}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn executor_process_basic() {
        let executor = ParallelExecutor::new();
        let items = vec![1, 2, 3, 4, 5];

        let result = executor.process(&items, |&x| Ok::<_, io::Error>(x * 2));

        assert!(result.is_success());
        assert_eq!(result.success_count(), 5);
        let mut values = result.successes;
        values.sort();
        assert_eq!(values, vec![2, 4, 6, 8, 10]);
    }

    #[test]
    fn executor_handles_errors() {
        let executor = ParallelExecutor::new();
        let items = vec![1, 2, 3, 4, 5];

        let result = executor.process(&items, |&x| {
            if x == 3 {
                Err(io::Error::other("test error"))
            } else {
                Ok(x * 2)
            }
        });

        assert!(!result.is_success());
        assert_eq!(result.success_count(), 4);
        assert_eq!(result.error_count(), 1);
    }

    #[test]
    fn executor_copy_files() {
        let dir = tempdir().unwrap();
        let src1 = dir.path().join("src1.txt");
        let src2 = dir.path().join("src2.txt");
        let dst1 = dir.path().join("dst1.txt");
        let dst2 = dir.path().join("dst2.txt");

        std::fs::write(&src1, b"hello").unwrap();
        std::fs::write(&src2, b"world").unwrap();

        let executor = ParallelExecutor::new();
        let result = executor.copy_files(&[(src1, dst1.clone()), (src2, dst2.clone())]);

        assert!(result.is_success());
        assert_eq!(std::fs::read_to_string(&dst1).unwrap(), "hello");
        assert_eq!(std::fs::read_to_string(&dst2).unwrap(), "world");
    }

    #[test]
    fn stats_tracking() {
        let stats = ParallelStats::new();

        stats.record_success(100);
        stats.record_success(200);
        stats.record_failure();

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.items_processed, 2);
        assert_eq!(snapshot.items_failed, 1);
        assert_eq!(snapshot.bytes_processed, 300);
        assert_eq!(stats.total_items(), 3);
    }
}
