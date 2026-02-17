//! `PipelinedReceiver` — mediator between network and disk threads.
//!
//! Owns the channels and the disk commit thread, coordinating lifecycle,
//! error collection, and graceful shutdown.

use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};
use std::thread::JoinHandle;

use crate::disk_commit::{DiskCommitConfig, spawn_disk_thread};
use crate::pipeline::messages::{CommitResult, FileMessage};

/// Mediator that coordinates the network ingest thread with the disk
/// commit thread via bounded channels.
///
/// # Lifecycle
///
/// 1. `PipelinedReceiver::new()` spawns the disk thread.
/// 2. The network thread calls `file_sender()` to get the channel sender
///    and passes it to `process_file_response_streaming()`.
/// 3. After each response, `drain_ready_results()` non-blockingly checks
///    for completed disk writes and early errors.
/// 4. After all files are processed, `drain_all_results()` blocks until
///    every committed file has been written.
/// 5. `shutdown()` (or `Drop`) sends `Shutdown` and joins the thread.
pub struct PipelinedReceiver {
    file_tx: SyncSender<FileMessage>,
    result_rx: Receiver<io::Result<CommitResult>>,
    disk_thread: Option<JoinHandle<()>>,
    /// Number of commits sent but not yet collected.
    pending_commits: usize,
}

impl PipelinedReceiver {
    /// Spawns the disk commit thread and returns a new mediator.
    pub fn new(config: DiskCommitConfig) -> Self {
        let (file_tx, result_rx, handle) = spawn_disk_thread(config);
        Self {
            file_tx,
            result_rx,
            disk_thread: Some(handle),
            pending_commits: 0,
        }
    }

    /// Returns a reference to the channel sender for `FileMessage` items.
    ///
    /// Pass this to [`crate::transfer_ops::process_file_response_streaming`].
    #[inline]
    pub fn file_sender(&self) -> &SyncSender<FileMessage> {
        &self.file_tx
    }

    /// Increments the pending-commit counter.
    ///
    /// Call this after `process_file_response_streaming` successfully returns
    /// (meaning it sent `Commit` through the channel).
    #[inline]
    pub fn note_commit_sent(&mut self) {
        self.pending_commits += 1;
    }

    /// Non-blockingly drains all available commit results.
    ///
    /// Returns accumulated (bytes_written, metadata_errors).
    /// Propagates the first disk error encountered.
    pub fn drain_ready_results(&mut self) -> io::Result<(u64, Vec<(PathBuf, String)>)> {
        let mut bytes = 0u64;
        let mut meta_errors = Vec::new();

        loop {
            match self.result_rx.try_recv() {
                Ok(Ok(result)) => {
                    bytes += result.bytes_written;
                    if let Some(err) = result.metadata_error {
                        meta_errors.push(err);
                    }
                    self.pending_commits = self.pending_commits.saturating_sub(1);
                }
                Ok(Err(e)) => {
                    self.pending_commits = self.pending_commits.saturating_sub(1);
                    return Err(e);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.pending_commits > 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "disk commit thread disconnected with pending commits",
                        ));
                    }
                    break;
                }
            }
        }

        Ok((bytes, meta_errors))
    }

    /// Blocks until all pending commits have been collected.
    ///
    /// Returns accumulated (bytes_written, metadata_errors).
    /// Propagates the first disk error encountered.
    pub fn drain_all_results(&mut self) -> io::Result<(u64, Vec<(PathBuf, String)>)> {
        let mut bytes = 0u64;
        let mut meta_errors = Vec::new();

        while self.pending_commits > 0 {
            match self.result_rx.recv() {
                Ok(Ok(result)) => {
                    bytes += result.bytes_written;
                    if let Some(err) = result.metadata_error {
                        meta_errors.push(err);
                    }
                    self.pending_commits -= 1;
                }
                Ok(Err(e)) => {
                    self.pending_commits -= 1;
                    return Err(e);
                }
                Err(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "disk commit thread disconnected with pending commits",
                    ));
                }
            }
        }

        Ok((bytes, meta_errors))
    }

    /// Sends `Shutdown` and joins the disk thread.
    ///
    /// Implicitly drains remaining results. Returns the final accumulated
    /// (bytes_written, metadata_errors).
    pub fn shutdown(mut self) -> io::Result<(u64, Vec<(PathBuf, String)>)> {
        let result = self.drain_all_results();

        // Send shutdown — ignore error (thread may have already exited).
        let _ = self.file_tx.send(FileMessage::Shutdown);

        if let Some(handle) = self.disk_thread.take() {
            let _ = handle.join();
        }

        result
    }
}

impl Drop for PipelinedReceiver {
    fn drop(&mut self) {
        // Best-effort shutdown: send Shutdown and join.
        let _ = self.file_tx.send(FileMessage::Shutdown);
        if let Some(handle) = self.disk_thread.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::messages::BeginMessage;

    #[test]
    fn spawn_and_shutdown() {
        let pr = PipelinedReceiver::new(DiskCommitConfig::default());
        let (bytes, errors) = pr.shutdown().unwrap();
        assert_eq!(bytes, 0);
        assert!(errors.is_empty());
    }

    #[test]
    fn write_file_through_mediator() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.dat");

        let mut pr = PipelinedReceiver::new(DiskCommitConfig::default());

        pr.file_sender()
            .send(FileMessage::Begin(BeginMessage {
                file_path: file_path.clone(),
                target_size: 100,
                file_entry_index: 0,
                use_sparse: false,
                direct_write: true,
            }))
            .unwrap();

        pr.file_sender()
            .send(FileMessage::Chunk(b"test data".to_vec()))
            .unwrap();

        pr.file_sender().send(FileMessage::Commit).unwrap();
        pr.note_commit_sent();

        let (bytes, errors) = pr.drain_all_results().unwrap();
        assert_eq!(bytes, 9);
        assert!(errors.is_empty());

        assert_eq!(std::fs::read(&file_path).unwrap(), b"test data");

        let (extra_bytes, _) = pr.shutdown().unwrap();
        assert_eq!(extra_bytes, 0);
    }

    #[test]
    fn drain_ready_returns_empty_when_nothing_pending() {
        let mut pr = PipelinedReceiver::new(DiskCommitConfig::default());
        let (bytes, errors) = pr.drain_ready_results().unwrap();
        assert_eq!(bytes, 0);
        assert!(errors.is_empty());
        drop(pr);
    }

    #[test]
    fn drop_cleans_up_thread() {
        let pr = PipelinedReceiver::new(DiskCommitConfig::default());
        drop(pr); // Should not hang or panic.
    }
}
