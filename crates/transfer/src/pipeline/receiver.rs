//! `PipelinedReceiver` — mediator between network and disk threads.
//!
//! Owns the channels and the disk commit thread, coordinating lifecycle,
//! error collection, and graceful shutdown.

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};
use std::thread::JoinHandle;

use crate::delta_apply::ChecksumVerifier;
use crate::disk_commit::{DiskCommitConfig, spawn_disk_thread};
use crate::pipeline::messages::{CommitResult, FileMessage};

/// Expected checksum for a pending file, used for deferred verification.
///
/// Stored in a FIFO queue by `PipelinedReceiver`. When the disk thread
/// returns a `CommitResult` with a computed checksum, it is compared
/// against the next `PendingChecksum` in the queue.
struct PendingChecksum {
    expected: [u8; ChecksumVerifier::MAX_DIGEST_LEN],
    len: usize,
    file_path: PathBuf,
}

/// Mediator that coordinates the network ingest thread with the disk
/// commit thread via bounded channels, including deferred checksum
/// verification.
pub struct PipelinedReceiver {
    file_tx: SyncSender<FileMessage>,
    result_rx: Receiver<io::Result<CommitResult>>,
    /// Return channel for buffer recycling from the disk thread.
    buf_return_rx: Receiver<Vec<u8>>,
    disk_thread: Option<JoinHandle<()>>,
    /// Number of commits sent but not yet collected.
    pending_commits: usize,
    /// Queue of expected checksums for deferred verification.
    /// Consumed FIFO when collecting `CommitResult`s, since the disk thread
    /// processes files in the same order they are submitted.
    expected_checksums: VecDeque<PendingChecksum>,
}

impl PipelinedReceiver {
    /// Spawns the disk commit thread and returns a new mediator.
    pub fn new(config: DiskCommitConfig) -> Self {
        let h = spawn_disk_thread(config);
        Self {
            file_tx: h.file_tx,
            result_rx: h.result_rx,
            buf_return_rx: h.buf_return_rx,
            disk_thread: Some(h.join_handle),
            pending_commits: 0,
            expected_checksums: VecDeque::new(),
        }
    }

    /// Returns a reference to the channel sender for `FileMessage` items.
    ///
    /// Pass this to [`crate::transfer_ops::process_file_response_streaming`].
    #[inline]
    pub fn file_sender(&self) -> &SyncSender<FileMessage> {
        &self.file_tx
    }

    /// Returns a reference to the buffer return receiver.
    ///
    /// Pass this to [`crate::transfer_ops::process_file_response_streaming`]
    /// so it can reuse buffers returned by the disk thread.
    #[inline]
    pub fn buf_return_rx(&self) -> &Receiver<Vec<u8>> {
        &self.buf_return_rx
    }

    /// Increments the pending-commit counter and records the expected checksum
    /// for deferred verification.
    ///
    /// Call this after `process_file_response_streaming` successfully returns
    /// (meaning it sent `Commit` through the channel).
    pub fn note_commit_sent(
        &mut self,
        expected_checksum: [u8; ChecksumVerifier::MAX_DIGEST_LEN],
        checksum_len: usize,
        file_path: PathBuf,
    ) {
        self.pending_commits += 1;
        self.expected_checksums.push_back(PendingChecksum {
            expected: expected_checksum,
            len: checksum_len,
            file_path,
        });
    }

    /// Non-blockingly drains all available commit results.
    ///
    /// Returns accumulated (bytes_written, metadata_errors).
    /// Propagates the first disk error encountered. Verifies per-file
    /// checksums when the disk thread returns a computed digest.
    pub fn drain_ready_results(&mut self) -> io::Result<(u64, Vec<(PathBuf, String)>)> {
        let mut bytes = 0u64;
        let mut meta_errors = Vec::new();

        loop {
            match self.result_rx.try_recv() {
                Ok(Ok(result)) => {
                    self.verify_checksum(&result)?;
                    bytes += result.bytes_written;
                    if let Some(err) = result.metadata_error {
                        meta_errors.push(err);
                    }
                    self.pending_commits = self.pending_commits.saturating_sub(1);
                }
                Ok(Err(e)) => {
                    self.pending_commits = self.pending_commits.saturating_sub(1);
                    // Consume the corresponding expected checksum on error.
                    let _ = self.expected_checksums.pop_front();
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
    /// Propagates the first disk error encountered. Verifies per-file
    /// checksums when the disk thread returns a computed digest.
    pub fn drain_all_results(&mut self) -> io::Result<(u64, Vec<(PathBuf, String)>)> {
        let mut bytes = 0u64;
        let mut meta_errors = Vec::new();

        while self.pending_commits > 0 {
            match self.result_rx.recv() {
                Ok(Ok(result)) => {
                    self.verify_checksum(&result)?;
                    bytes += result.bytes_written;
                    if let Some(err) = result.metadata_error {
                        meta_errors.push(err);
                    }
                    self.pending_commits -= 1;
                }
                Ok(Err(e)) => {
                    self.pending_commits -= 1;
                    let _ = self.expected_checksums.pop_front();
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

    /// Verifies a commit result's computed checksum against the expected value.
    ///
    /// Pops the next expected checksum from the FIFO queue (files are processed
    /// in submission order). Returns `Err` on mismatch — mirrors upstream
    /// `receiver.c:315` which aborts on checksum failure.
    fn verify_checksum(&mut self, result: &CommitResult) -> io::Result<()> {
        let pending = match self.expected_checksums.pop_front() {
            Some(p) => p,
            None => return Ok(()), // No expected checksum (legacy/test path).
        };

        if let Some(ref computed) = result.computed_checksum {
            if computed.len != pending.len
                || computed.bytes[..computed.len] != pending.expected[..pending.len]
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "checksum verification failed for {:?}: expected {:02x?}, got {:02x?}",
                        pending.file_path,
                        &pending.expected[..pending.len],
                        &computed.bytes[..computed.len]
                    ),
                ));
            }
        }

        Ok(())
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
            .send(FileMessage::Begin(Box::new(BeginMessage {
                file_path: file_path.clone(),
                target_size: 100,
                file_entry_index: 0,
                use_sparse: false,
                direct_write: true,
                checksum_verifier: None,
            })))
            .unwrap();

        pr.file_sender()
            .send(FileMessage::Chunk(b"test data".to_vec()))
            .unwrap();

        pr.file_sender().send(FileMessage::Commit).unwrap();
        pr.note_commit_sent(
            [0u8; ChecksumVerifier::MAX_DIGEST_LEN],
            0,
            file_path.clone(),
        );

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
