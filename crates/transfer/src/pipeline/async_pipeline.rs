//! Async pipeline orchestrator for tokio-based file transfers.
//!
//! Spawns a producer task (iterates [`FileList`] → [`FileJob`]) and runs a
//! consumer loop (processes each job), connected by a bounded `tokio::sync::mpsc`
//! channel. Retryable failures are queued locally and processed after the
//! initial pass completes.
//!
//! # Cancellation
//!
//! The [`PipelineHandle`] returned by [`run_pipeline`] provides a
//! `CancellationToken` for cooperative shutdown. When cancelled, the consumer
//! drains gracefully and the pipeline returns partial statistics.

use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::async_dispatch::produce_file_jobs;
use super::job::{FileJob, FileList};
use super::{AsyncPipelineConfig, DEFAULT_JOB_CHANNEL_CAPACITY};

/// Result of processing a single file transfer.
#[derive(Debug)]
pub enum TransferOutcome {
    /// Transfer completed successfully.
    Success {
        /// Protocol NDX of the transferred file.
        ndx: u32,
        /// Bytes written to disk.
        bytes_transferred: u64,
    },
    /// Transfer failed but is eligible for retry.
    RetryableError {
        /// The job to retry (with incremented retry count).
        job: FileJob,
        /// Description of the failure.
        reason: String,
    },
    /// Transfer failed permanently (max retries exceeded or non-retryable).
    PermanentError {
        /// Protocol NDX of the failed file.
        ndx: u32,
        /// The underlying error.
        error: io::Error,
    },
    /// File was skipped (up-to-date, excluded, etc.).
    Skipped {
        /// Protocol NDX of the skipped file.
        ndx: u32,
    },
}

/// Aggregate statistics from a completed pipeline run.
#[derive(Debug, Clone, Default)]
pub struct PipelineRunStats {
    /// Files successfully transferred.
    pub files_transferred: u64,
    /// Files skipped (up-to-date).
    pub files_skipped: u64,
    /// Files that failed permanently.
    pub files_failed: u64,
    /// Total bytes written to disk.
    pub bytes_transferred: u64,
    /// Jobs dispatched by the producer.
    pub jobs_dispatched: u64,
    /// Retry attempts made.
    pub retries_attempted: u64,
}

/// Handle for monitoring and cancelling a running pipeline.
///
/// Returned by [`run_pipeline`]. The pipeline runs as spawned tokio tasks;
/// this handle provides cooperative cancellation and live progress counters.
pub struct PipelineHandle {
    cancel: CancellationToken,
    files_done: Arc<AtomicU64>,
    bytes_done: Arc<AtomicU64>,
}

impl PipelineHandle {
    /// Requests cooperative cancellation of the pipeline.
    ///
    /// Both producer and consumer tasks will drain gracefully. The `run_pipeline`
    /// future resolves shortly after cancellation with partial statistics.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Returns `true` if cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Returns the number of files completed so far (atomically).
    #[must_use]
    pub fn files_completed(&self) -> u64 {
        self.files_done.load(Ordering::Relaxed)
    }

    /// Returns the total bytes transferred so far (atomically).
    #[must_use]
    pub fn bytes_transferred(&self) -> u64 {
        self.bytes_done.load(Ordering::Relaxed)
    }
}

/// Runs the async transfer pipeline.
///
/// Spawns a producer task that iterates `file_list` creating [`FileJob`] values,
/// then consumes each job using the caller-provided `process_fn`. A bounded
/// channel with capacity from `config.job_channel_capacity` provides backpressure.
///
/// Retryable failures are queued locally and processed after all initial jobs
/// complete. This avoids circular channel dependencies between producer and
/// consumer.
///
/// # Arguments
///
/// * `config` - Pipeline configuration (window size, channel capacity, retry settings).
/// * `file_list` - Immutable sorted file list shared via `Arc`.
/// * `dest_dir` - Base destination directory for file writes.
/// * `process_fn` - Async function that processes a single `FileJob` and returns
///   a `TransferOutcome`. Called once per job (including retries).
///
/// # Returns
///
/// A tuple of `(PipelineHandle, impl Future)`. The handle enables cancellation
/// and progress monitoring. The future resolves to `PipelineRunStats` when all
/// files have been processed (or the pipeline is cancelled).
pub fn run_pipeline<F, Fut>(
    config: AsyncPipelineConfig,
    file_list: FileList,
    dest_dir: PathBuf,
    process_fn: F,
) -> (PipelineHandle, impl Future<Output = PipelineRunStats>)
where
    F: Fn(FileJob) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = TransferOutcome> + Send,
{
    let cancel = CancellationToken::new();
    let files_done = Arc::new(AtomicU64::new(0));
    let bytes_done = Arc::new(AtomicU64::new(0));

    let handle = PipelineHandle {
        cancel: cancel.clone(),
        files_done: Arc::clone(&files_done),
        bytes_done: Arc::clone(&bytes_done),
    };

    let capacity = config
        .job_channel_capacity
        .clamp(1, DEFAULT_JOB_CHANNEL_CAPACITY * 8);

    let retry_enabled = config.retry_enabled;

    let pipeline_future = async move {
        let (job_tx, job_rx) = mpsc::channel(capacity);

        // Producer: iterate file list → FileJob → channel.
        // Dropping job_tx after spawn ensures the consumer's rx closes
        // when the producer finishes.
        let producer_cancel = cancel.clone();
        let producer = tokio::spawn(async move {
            tokio::select! {
                count = produce_file_jobs(&file_list, &dest_dir, job_tx) => count,
                () = producer_cancel.cancelled() => 0,
            }
        });

        // Consumer: process jobs from channel, queue retries locally.
        let consumer_stats = consume_jobs(
            job_rx,
            retry_enabled,
            process_fn,
            cancel,
            Arc::clone(&files_done),
            Arc::clone(&bytes_done),
        )
        .await;

        let jobs_dispatched = producer.await.unwrap_or_default();

        PipelineRunStats {
            jobs_dispatched,
            ..consumer_stats
        }
    };

    (handle, pipeline_future)
}

/// Consumer loop: receives `FileJob` values and processes them.
///
/// Retryable failures are queued in a local `VecDeque` and processed after
/// the channel closes (all initial jobs dispatched). This avoids the deadlock
/// that would occur if retries were fed back through the same channel.
async fn consume_jobs<F, Fut>(
    mut rx: mpsc::Receiver<FileJob>,
    retry_enabled: bool,
    process_fn: F,
    cancel: CancellationToken,
    files_done: Arc<AtomicU64>,
    bytes_done: Arc<AtomicU64>,
) -> PipelineRunStats
where
    F: Fn(FileJob) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = TransferOutcome> + Send,
{
    let mut stats = PipelineRunStats::default();
    let mut retry_queue: VecDeque<FileJob> = VecDeque::new();

    // Phase 1: Process initial jobs from the producer channel.
    loop {
        let job = tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            maybe_job = rx.recv() => {
                match maybe_job {
                    Some(job) => job,
                    None => break,
                }
            }
        };

        process_outcome(
            &process_fn,
            job,
            retry_enabled,
            &mut retry_queue,
            &mut stats,
            &files_done,
            &bytes_done,
        )
        .await;
    }

    // Phase 2: Process retries from the local queue.
    while let Some(job) = retry_queue.pop_front() {
        if cancel.is_cancelled() {
            break;
        }

        stats.retries_attempted += 1;
        process_outcome(
            &process_fn,
            job,
            retry_enabled,
            &mut retry_queue,
            &mut stats,
            &files_done,
            &bytes_done,
        )
        .await;
    }

    stats
}

/// Processes a single job and records the outcome in stats.
async fn process_outcome<F, Fut>(
    process_fn: &F,
    job: FileJob,
    retry_enabled: bool,
    retry_queue: &mut VecDeque<FileJob>,
    stats: &mut PipelineRunStats,
    files_done: &AtomicU64,
    bytes_done: &AtomicU64,
) where
    F: Fn(FileJob) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = TransferOutcome> + Send,
{
    let outcome = process_fn(job).await;

    match outcome {
        TransferOutcome::Success {
            bytes_transferred, ..
        } => {
            stats.files_transferred += 1;
            stats.bytes_transferred += bytes_transferred;
            files_done.fetch_add(1, Ordering::Relaxed);
            bytes_done.fetch_add(bytes_transferred, Ordering::Relaxed);
        }
        TransferOutcome::RetryableError { job, .. } => {
            if retry_enabled {
                if let Some(retry_job) = job.retry() {
                    retry_queue.push_back(retry_job);
                } else {
                    stats.files_failed += 1;
                }
            } else {
                stats.files_failed += 1;
            }
        }
        TransferOutcome::PermanentError { .. } => {
            stats.files_failed += 1;
        }
        TransferOutcome::Skipped { .. } => {
            stats.files_skipped += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::flist::FileEntry;
    use std::sync::Arc as StdArc;

    fn make_file_list(count: usize) -> FileList {
        let entries: Vec<_> = (0..count)
            .map(|i| {
                FileEntry::new_file(format!("file_{i}.txt").into(), (i as u64 + 1) * 100, 0o644)
            })
            .collect();
        FileList::new(entries)
    }

    #[tokio::test]
    async fn pipeline_empty_file_list() {
        let config = AsyncPipelineConfig::default();
        let list = FileList::new(Vec::new());

        let (handle, fut) = run_pipeline(config, list, PathBuf::from("/dst"), |_job| async {
            panic!("should not be called for empty list");
        });

        let stats = fut.await;
        assert_eq!(stats.jobs_dispatched, 0);
        assert_eq!(stats.files_transferred, 0);
        assert!(!handle.is_cancelled());
    }

    #[tokio::test]
    async fn pipeline_processes_all_files() {
        let config = AsyncPipelineConfig::default();
        let list = make_file_list(5);

        let (_handle, fut) = run_pipeline(config, list, PathBuf::from("/dst"), |job| async move {
            TransferOutcome::Success {
                ndx: job.ndx(),
                bytes_transferred: 100,
            }
        });

        let stats = fut.await;
        assert_eq!(stats.jobs_dispatched, 5);
        assert_eq!(stats.files_transferred, 5);
        assert_eq!(stats.bytes_transferred, 500);
        assert_eq!(stats.files_failed, 0);
    }

    #[tokio::test]
    async fn pipeline_counts_skipped_files() {
        let config = AsyncPipelineConfig::default();
        let list = make_file_list(3);

        let (_handle, fut) = run_pipeline(config, list, PathBuf::from("/dst"), |job| async move {
            TransferOutcome::Skipped { ndx: job.ndx() }
        });

        let stats = fut.await;
        assert_eq!(stats.files_skipped, 3);
        assert_eq!(stats.files_transferred, 0);
    }

    #[tokio::test]
    async fn pipeline_counts_permanent_errors() {
        let config = AsyncPipelineConfig::default();
        let list = make_file_list(2);

        let (_handle, fut) = run_pipeline(config, list, PathBuf::from("/dst"), |job| async move {
            TransferOutcome::PermanentError {
                ndx: job.ndx(),
                error: io::Error::new(io::ErrorKind::PermissionDenied, "access denied"),
            }
        });

        let stats = fut.await;
        assert_eq!(stats.files_failed, 2);
        assert_eq!(stats.files_transferred, 0);
    }

    #[tokio::test]
    async fn pipeline_retries_on_retryable_error() {
        let config = AsyncPipelineConfig::default();
        let list = make_file_list(1);

        let attempt = StdArc::new(std::sync::atomic::AtomicU32::new(0));
        let attempt_clone = StdArc::clone(&attempt);

        let (_handle, fut) = run_pipeline(config, list, PathBuf::from("/dst"), move |job| {
            let attempt = StdArc::clone(&attempt_clone);
            async move {
                let n = attempt.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    TransferOutcome::RetryableError {
                        job,
                        reason: "transient".into(),
                    }
                } else {
                    TransferOutcome::Success {
                        ndx: job.ndx(),
                        bytes_transferred: 100,
                    }
                }
            }
        });

        let stats = fut.await;
        assert_eq!(stats.files_transferred, 1);
        assert_eq!(stats.files_failed, 0);
        assert_eq!(stats.retries_attempted, 1);
    }

    #[tokio::test]
    async fn pipeline_retries_exhaust_max() {
        let config = AsyncPipelineConfig {
            retry_enabled: true,
            ..AsyncPipelineConfig::default()
        };
        let list = make_file_list(1);

        let (_handle, fut) = run_pipeline(config, list, PathBuf::from("/dst"), |job| async move {
            TransferOutcome::RetryableError {
                job,
                reason: "always fails".into(),
            }
        });

        let stats = fut.await;
        assert_eq!(stats.files_failed, 1);
        assert_eq!(stats.files_transferred, 0);
    }

    #[tokio::test]
    async fn pipeline_no_retry_when_disabled() {
        let config = AsyncPipelineConfig {
            retry_enabled: false,
            ..AsyncPipelineConfig::default()
        };
        let list = make_file_list(1);

        let (_handle, fut) = run_pipeline(config, list, PathBuf::from("/dst"), |job| async move {
            TransferOutcome::RetryableError {
                job,
                reason: "fail".into(),
            }
        });

        let stats = fut.await;
        assert_eq!(stats.files_failed, 1);
        assert_eq!(stats.retries_attempted, 0);
    }

    #[tokio::test]
    async fn pipeline_cancellation() {
        let config = AsyncPipelineConfig::default();
        let list = make_file_list(10_000);

        let (handle, fut) = run_pipeline(config, list, PathBuf::from("/dst"), |job| async move {
            tokio::task::yield_now().await;
            TransferOutcome::Success {
                ndx: job.ndx(),
                bytes_transferred: 1,
            }
        });

        handle.cancel();
        let stats = fut.await;

        assert!(stats.files_transferred < 10_000);
        assert!(handle.is_cancelled());
    }

    #[tokio::test]
    async fn pipeline_handle_progress_counters() {
        let config = AsyncPipelineConfig::default();
        let list = make_file_list(3);

        let (handle, fut) = run_pipeline(config, list, PathBuf::from("/dst"), |job| async move {
            TransferOutcome::Success {
                ndx: job.ndx(),
                bytes_transferred: 50,
            }
        });

        let stats = fut.await;

        assert_eq!(handle.files_completed(), stats.files_transferred);
        assert_eq!(handle.bytes_transferred(), stats.bytes_transferred);
    }

    #[tokio::test]
    async fn pipeline_mixed_outcomes() {
        let config = AsyncPipelineConfig::default();
        let list = make_file_list(4);

        let (_handle, fut) = run_pipeline(config, list, PathBuf::from("/dst"), |job| async move {
            match job.ndx() {
                0 => TransferOutcome::Success {
                    ndx: 0,
                    bytes_transferred: 100,
                },
                1 => TransferOutcome::Skipped { ndx: 1 },
                2 => TransferOutcome::PermanentError {
                    ndx: 2,
                    error: io::Error::other("bad"),
                },
                _ => TransferOutcome::Success {
                    ndx: job.ndx(),
                    bytes_transferred: 200,
                },
            }
        });

        let stats = fut.await;
        assert_eq!(stats.files_transferred, 2);
        assert_eq!(stats.files_skipped, 1);
        assert_eq!(stats.files_failed, 1);
        assert_eq!(stats.bytes_transferred, 300);
    }
}
