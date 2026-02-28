//! Batch async file copying with bounded concurrency.
//!
//! Uses a semaphore-gated spawning loop to ensure that at most
//! `max_concurrent` file copy operations are in flight at any time,
//! regardless of the total number of files.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Semaphore;

use super::copier::AsyncFileCopier;
use super::error::AsyncIoError;
use super::progress::CopyResult;

/// Maximum sane concurrency to prevent file descriptor exhaustion.
///
/// Each copy holds 2 fds; 64 workers = 128 fds, well within typical `ulimit -n`.
const MAX_CONCURRENT_UPPER_BOUND: usize = 64;

/// Minimum concurrency — always at least one worker.
const MAX_CONCURRENT_LOWER_BOUND: usize = 1;

/// Resolves the effective `max_concurrent` value.
///
/// Priority: explicit value > `RSYNC_MAX_CONCURRENT` env var > CPU-derived default.
/// Result is clamped to [`MAX_CONCURRENT_LOWER_BOUND`, `MAX_CONCURRENT_UPPER_BOUND`].
fn resolve_max_concurrent(explicit: Option<usize>) -> usize {
    let cpu_default = || {
        std::thread::available_parallelism()
            .map(|n| n.get() * 2)
            .unwrap_or(4)
    };
    let raw = if let Some(v) = explicit {
        v
    } else if let Ok(env_val) = std::env::var("RSYNC_MAX_CONCURRENT") {
        env_val.parse::<usize>().unwrap_or_else(|_| cpu_default())
    } else {
        cpu_default()
    };
    raw.clamp(MAX_CONCURRENT_LOWER_BOUND, MAX_CONCURRENT_UPPER_BOUND)
}

/// Batch async file copier with bounded concurrency.
///
/// Uses a semaphore to ensure that at most `max_concurrent` file copy
/// operations are in flight at any time. Each file pair spawns its own
/// task, but the semaphore blocks the spawning loop until a permit is
/// available, providing natural backpressure without shared-mutex contention.
///
/// # Example
///
/// ```ignore
/// use engine::async_io::AsyncBatchCopier;
///
/// let copier = AsyncBatchCopier::new().max_concurrent(8);
/// let results = copier.copy_files(vec![
///     ("src/a.txt", "dst/a.txt"),
///     ("src/b.txt", "dst/b.txt"),
/// ]).await;
/// ```
#[derive(Debug)]
pub struct AsyncBatchCopier {
    copier: AsyncFileCopier,
    max_concurrent: usize,
}

impl Default for AsyncBatchCopier {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncBatchCopier {
    /// Creates a new batch copier with CPU-derived concurrency.
    ///
    /// Default `max_concurrent` is `available_parallelism() * 2`, clamped to
    /// `[1, 64]`. Override with [`Self::max_concurrent`] or the
    /// `RSYNC_MAX_CONCURRENT` environment variable.
    #[must_use]
    pub fn new() -> Self {
        Self {
            copier: AsyncFileCopier::new(),
            max_concurrent: resolve_max_concurrent(None),
        }
    }

    /// Sets the maximum number of concurrent copy operations.
    ///
    /// Clamped to `[1, 64]`. For I/O-bound local disk workloads,
    /// `num_cpus * 2` is a reasonable default; for high-latency
    /// network filesystems, `num_cpus * 4` may be appropriate.
    #[must_use]
    pub fn max_concurrent(mut self, max: usize) -> Self {
        self.max_concurrent = max.clamp(MAX_CONCURRENT_LOWER_BOUND, MAX_CONCURRENT_UPPER_BOUND);
        self
    }

    /// Sets the underlying file copier configuration.
    #[must_use]
    pub const fn with_copier(mut self, copier: AsyncFileCopier) -> Self {
        self.copier = copier;
        self
    }

    /// Copies multiple files concurrently using a bounded semaphore.
    ///
    /// At most `max_concurrent` copy operations execute simultaneously.
    /// A semaphore gates task spawning so that backpressure is applied
    /// naturally without shared-mutex contention on a channel receiver.
    ///
    /// Results are returned in the same order as the input iterator.
    pub async fn copy_files<I, P, Q>(&self, files: I) -> Vec<Result<CopyResult, AsyncIoError>>
    where
        I: IntoIterator<Item = (P, Q)>,
        P: AsRef<Path> + Send + 'static,
        Q: AsRef<Path> + Send + 'static,
    {
        let copier = Arc::new(self.copier.clone());
        let semaphore = Arc::new(Semaphore::new(self.max_concurrent));

        let pairs: Vec<(PathBuf, PathBuf)> = files
            .into_iter()
            .map(|(src, dst)| (src.as_ref().to_path_buf(), dst.as_ref().to_path_buf()))
            .collect();

        if pairs.is_empty() {
            return Vec::new();
        }

        let mut handles = Vec::with_capacity(pairs.len());

        for (src, dst) in pairs {
            let permit = semaphore.clone().acquire_owned().await;
            let copier = copier.clone();
            let src_path = src.clone();

            handles.push(tokio::spawn(async move {
                let _permit = match permit {
                    Ok(p) => p,
                    Err(_) => {
                        return Err(AsyncIoError::Cancelled(src_path));
                    }
                };
                copier.copy_file(&src, &dst).await
            }));
        }

        let mut results = Vec::with_capacity(handles.len());
        for handle in handles {
            results.push(match handle.await {
                Ok(result) => result,
                Err(e) => Err(AsyncIoError::JoinError(e)),
            });
        }

        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_batch_copy() {
        let temp = TempDir::new().unwrap();

        let files: Vec<_> = (0..5)
            .map(|i| {
                let src = temp.path().join(format!("src_{i}.txt"));
                let dst = temp.path().join(format!("dst_{i}.txt"));
                std::fs::write(&src, format!("Content {i}")).unwrap();
                (src, dst)
            })
            .collect();

        let batch_copier = AsyncBatchCopier::new().max_concurrent(2);
        let results = batch_copier.copy_files(files.clone()).await;

        assert_eq!(results.len(), 5);
        for (i, result) in results.iter().enumerate() {
            assert!(result.is_ok(), "File {i} should copy successfully");
        }

        for i in 0..5 {
            let content =
                std::fs::read_to_string(temp.path().join(format!("dst_{i}.txt"))).unwrap();
            assert_eq!(content, format!("Content {i}"));
        }
    }

    #[test]
    fn test_async_batch_copier_default() {
        let copier = AsyncBatchCopier::default();
        assert!(copier.max_concurrent >= MAX_CONCURRENT_LOWER_BOUND);
        assert!(copier.max_concurrent <= MAX_CONCURRENT_UPPER_BOUND);
    }

    #[test]
    fn test_async_batch_copier_max_concurrent() {
        let copier = AsyncBatchCopier::new().max_concurrent(8);
        assert_eq!(copier.max_concurrent, 8);
    }

    #[test]
    fn test_async_batch_copier_max_concurrent_clamped() {
        let copier = AsyncBatchCopier::new().max_concurrent(0);
        assert_eq!(copier.max_concurrent, MAX_CONCURRENT_LOWER_BOUND);

        let copier = AsyncBatchCopier::new().max_concurrent(1000);
        assert_eq!(copier.max_concurrent, MAX_CONCURRENT_UPPER_BOUND);

        let copier = AsyncBatchCopier::new().max_concurrent(16);
        assert_eq!(copier.max_concurrent, 16);
    }

    #[test]
    fn test_async_batch_copier_with_copier() {
        let file_copier = AsyncFileCopier::new().with_buffer_size(16384);
        let batch = AsyncBatchCopier::new().with_copier(file_copier);
        assert_eq!(batch.copier.buffer_size(), 16384);
    }

    #[test]
    fn test_async_batch_copier_debug() {
        let copier = AsyncBatchCopier::new();
        let debug = format!("{copier:?}");
        assert!(debug.contains("AsyncBatchCopier"));
    }

    #[tokio::test]
    async fn test_batch_copy_ordered_results() {
        let temp = TempDir::new().unwrap();
        let files: Vec<_> = (0..10)
            .map(|i| {
                let src = temp.path().join(format!("src_{i}.txt"));
                let dst = temp.path().join(format!("dst_{i}.txt"));
                std::fs::write(&src, vec![b'x'; (i + 1) * 100]).unwrap();
                (src, dst)
            })
            .collect();

        let copier = AsyncBatchCopier::new().max_concurrent(2);
        let results = copier.copy_files(files).await;

        assert_eq!(results.len(), 10);
        for (i, result) in results.iter().enumerate() {
            let r = result.as_ref().unwrap();
            assert_eq!(r.bytes_copied, ((i + 1) * 100) as u64);
        }
    }

    #[tokio::test]
    async fn test_batch_copy_empty() {
        let copier = AsyncBatchCopier::new();
        let results = copier.copy_files(Vec::<(PathBuf, PathBuf)>::new()).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_batch_copy_partial_failure() {
        let temp = TempDir::new().unwrap();
        let good_src = temp.path().join("good.txt");
        std::fs::write(&good_src, "ok").unwrap();

        let files = vec![
            (good_src, temp.path().join("good_dst.txt")),
            (
                PathBuf::from("/nonexistent/source.txt"),
                temp.path().join("fail_dst.txt"),
            ),
        ];

        let copier = AsyncBatchCopier::new().max_concurrent(2);
        let results = copier.copy_files(files).await;

        assert_eq!(results.len(), 2);
        assert!(results[0].is_ok());
        assert!(results[1].is_err());
    }

    #[test]
    fn test_resolve_max_concurrent_explicit() {
        assert_eq!(resolve_max_concurrent(Some(8)), 8);
        assert_eq!(resolve_max_concurrent(Some(0)), MAX_CONCURRENT_LOWER_BOUND);
        assert_eq!(
            resolve_max_concurrent(Some(200)),
            MAX_CONCURRENT_UPPER_BOUND
        );
    }

    #[test]
    fn test_resolve_max_concurrent_default() {
        let val = resolve_max_concurrent(None);
        assert!(val >= MAX_CONCURRENT_LOWER_BOUND);
        assert!(val <= MAX_CONCURRENT_UPPER_BOUND);
    }
}
