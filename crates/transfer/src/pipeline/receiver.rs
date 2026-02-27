//! `PipelinedReceiver` — mediator between network and disk threads.
//!
//! Owns the channels and the disk commit thread, coordinating lifecycle,
//! error collection, and graceful shutdown. Supports upstream rsync's
//! redo mechanism: when a file's whole-file checksum fails verification,
//! it is queued for retransmission in phase 2 instead of aborting the
//! entire transfer.
//!
//! # Upstream Reference
//!
//! - `receiver.c:970-976` — `send_msg_int(MSG_REDO, ndx)` on checksum failure
//! - `generator.c:2160-2199` — `check_for_finished_files()` processes redo queue
//! - `receiver.c:580-587` — phase transition on `NDX_DONE`

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::thread::JoinHandle;

use crate::pipeline::spsc::{self, TryRecvError};

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
    /// File list index for this file, used to identify which file to redo.
    file_index: usize,
}

/// Mediator that coordinates the network ingest thread with the disk
/// commit thread via bounded channels, including deferred checksum
/// verification.
///
/// When `redo_enabled` is `true`, checksum mismatches in phase 1 are
/// collected in `redo_indices` instead of returning errors. The caller
/// retrieves the list via [`Self::take_redo_indices`] and retransmits
/// those files in phase 2 with empty basis (whole-file transfer).
pub struct PipelinedReceiver {
    file_tx: spsc::Sender<FileMessage>,
    result_rx: spsc::Receiver<io::Result<CommitResult>>,
    /// Return channel for buffer recycling from the disk thread.
    buf_return_rx: spsc::Receiver<Vec<u8>>,
    disk_thread: Option<JoinHandle<()>>,
    /// Number of commits sent but not yet collected.
    pending_commits: usize,
    /// Queue of expected checksums for deferred verification.
    /// Consumed FIFO when collecting `CommitResult`s, since the disk thread
    /// processes files in the same order they are submitted.
    expected_checksums: VecDeque<PendingChecksum>,
    /// File indices that failed checksum verification and should be retried.
    /// Mirrors upstream `redo_list` in `io.c:158`.
    redo_indices: Vec<usize>,
    /// Whether the redo mechanism is active (phase 1). When false (phase 2),
    /// checksum mismatches are hard errors.
    redo_enabled: bool,
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
            redo_indices: Vec::new(),
            redo_enabled: true,
        }
    }

    /// Returns a reference to the channel sender for `FileMessage` items.
    ///
    /// Pass this to [`crate::transfer_ops::process_file_response_streaming`].
    #[inline]
    pub fn file_sender(&self) -> &spsc::Sender<FileMessage> {
        &self.file_tx
    }

    /// Returns a reference to the buffer return receiver.
    ///
    /// Pass this to [`crate::transfer_ops::process_file_response_streaming`]
    /// so it can reuse buffers returned by the disk thread.
    #[inline]
    pub fn buf_return_rx(&self) -> &spsc::Receiver<Vec<u8>> {
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
        file_index: usize,
    ) {
        self.pending_commits += 1;
        self.expected_checksums.push_back(PendingChecksum {
            expected: expected_checksum,
            len: checksum_len,
            file_path,
            file_index,
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
    /// in submission order).
    ///
    /// When `redo_enabled` is true (phase 1), checksum mismatches queue the file
    /// index into `redo_indices` and log a warning — mirroring upstream
    /// `receiver.c:960-973` which sends `MSG_REDO` and continues.
    ///
    /// When `redo_enabled` is false (phase 2), checksum mismatches are logged
    /// as errors but do not abort the transfer — mirroring upstream
    /// `receiver.c:948-957` where `redoing=1` uses `FERROR_XFER`.
    fn verify_checksum(&mut self, result: &CommitResult) -> io::Result<()> {
        let pending = match self.expected_checksums.pop_front() {
            Some(p) => p,
            None => return Ok(()), // No expected checksum (legacy/test path).
        };

        if let Some(ref computed) = result.computed_checksum {
            if computed.len != pending.len
                || computed.bytes[..computed.len] != pending.expected[..pending.len]
            {
                if self.redo_enabled {
                    // upstream: receiver.c:960-968 — WARNING, will try again
                    eprintln!(
                        "WARNING: {:?} failed verification -- update discarded (will try again).",
                        pending.file_path,
                    );
                    self.redo_indices.push(pending.file_index);
                    return Ok(());
                }
                // upstream: receiver.c:957-959 — ERROR in phase 2 (redoing)
                eprintln!(
                    "ERROR: {:?} failed verification -- update discarded.",
                    pending.file_path,
                );
                // In phase 2, upstream logs the error but continues the transfer.
                return Ok(());
            }
        }

        Ok(())
    }

    /// Drains newly detected redo indices without disabling redo mode.
    ///
    /// Call this after `drain_ready_results` or `drain_all_results` to
    /// retrieve file indices that failed checksum verification since the
    /// last drain. The caller should send `MSG_REDO` for each index over
    /// the multiplexed writer to signal the generator.
    ///
    /// Unlike [`Self::take_redo_indices`], this does NOT disable redo mode.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:970-974`: `send_msg_int(MSG_REDO, ndx)` sent immediately
    ///   when a checksum mismatch is detected during phase 1.
    pub fn drain_new_redo_indices(&mut self) -> Vec<usize> {
        std::mem::take(&mut self.redo_indices)
    }

    /// Returns the list of file indices that need redo (failed checksum in phase 1).
    ///
    /// After calling this, the redo list is empty and `redo_enabled` is set to
    /// `false` so that subsequent checksum failures in phase 2 are hard-logged.
    pub fn take_redo_indices(&mut self) -> Vec<usize> {
        self.redo_enabled = false;
        std::mem::take(&mut self.redo_indices)
    }

    /// Returns the number of files queued for redo.
    pub fn redo_count(&self) -> usize {
        self.redo_indices.len()
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
                checksum_verifier: None,
                file_entry: None,
                is_device_target: false,
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
            0,
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

    #[test]
    fn redo_initially_empty() {
        let pr = PipelinedReceiver::new(DiskCommitConfig::default());
        assert_eq!(pr.redo_count(), 0);
        drop(pr);
    }

    #[test]
    fn take_redo_indices_disables_redo() {
        let mut pr = PipelinedReceiver::new(DiskCommitConfig::default());
        assert!(pr.redo_enabled);
        let indices = pr.take_redo_indices();
        assert!(indices.is_empty());
        assert!(!pr.redo_enabled);
        drop(pr);
    }

    #[test]
    fn checksum_mismatch_queues_redo_in_phase1() {
        use crate::pipeline::messages::ComputedChecksum;

        let mut pr = PipelinedReceiver::new(DiskCommitConfig::default());

        // Simulate a committed file with a mismatching checksum.
        let mut expected = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        expected[0] = 0xAA;

        pr.expected_checksums.push_back(PendingChecksum {
            expected,
            len: 4,
            file_path: PathBuf::from("/dest/file.txt"),
            file_index: 7,
        });

        // Create a CommitResult with a different computed checksum.
        let mut computed_bytes = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        computed_bytes[0] = 0xBB;
        let result = CommitResult {
            bytes_written: 100,
            file_entry_index: 0,
            metadata_error: None,
            computed_checksum: Some(ComputedChecksum {
                bytes: computed_bytes,
                len: 4,
            }),
        };

        // In phase 1 (redo_enabled=true), this should NOT return an error.
        pr.verify_checksum(&result).unwrap();

        // The file should be queued for redo.
        assert_eq!(pr.redo_count(), 1);
        let indices = pr.take_redo_indices();
        assert_eq!(indices, vec![7]);

        // After take_redo_indices, redo is disabled (phase 2).
        assert!(!pr.redo_enabled);
        assert_eq!(pr.redo_count(), 0);

        drop(pr);
    }

    #[test]
    fn checksum_mismatch_logs_error_in_phase2() {
        use crate::pipeline::messages::ComputedChecksum;

        let mut pr = PipelinedReceiver::new(DiskCommitConfig::default());

        // Move to phase 2.
        let _ = pr.take_redo_indices();
        assert!(!pr.redo_enabled);

        let mut expected = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        expected[0] = 0xAA;

        pr.expected_checksums.push_back(PendingChecksum {
            expected,
            len: 4,
            file_path: PathBuf::from("/dest/file2.txt"),
            file_index: 3,
        });

        let mut computed_bytes = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        computed_bytes[0] = 0xCC;
        let result = CommitResult {
            bytes_written: 200,
            file_entry_index: 0,
            metadata_error: None,
            computed_checksum: Some(ComputedChecksum {
                bytes: computed_bytes,
                len: 4,
            }),
        };

        // In phase 2, mismatch should still return Ok (error is logged, not fatal).
        pr.verify_checksum(&result).unwrap();

        // No redo queued in phase 2.
        assert_eq!(pr.redo_count(), 0);

        drop(pr);
    }

    #[test]
    fn checksum_match_does_not_queue_redo() {
        use crate::pipeline::messages::ComputedChecksum;

        let mut pr = PipelinedReceiver::new(DiskCommitConfig::default());

        let mut expected = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        expected[0] = 0xAA;
        expected[1] = 0xBB;

        pr.expected_checksums.push_back(PendingChecksum {
            expected,
            len: 4,
            file_path: PathBuf::from("/dest/ok.txt"),
            file_index: 5,
        });

        // Same checksum — should succeed without redo.
        let result = CommitResult {
            bytes_written: 50,
            file_entry_index: 0,
            metadata_error: None,
            computed_checksum: Some(ComputedChecksum {
                bytes: expected,
                len: 4,
            }),
        };

        pr.verify_checksum(&result).unwrap();
        assert_eq!(pr.redo_count(), 0);

        drop(pr);
    }
}
