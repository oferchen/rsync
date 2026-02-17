//! Disk commit thread for the decoupled receiver architecture.
//!
//! Consumes [`FileMessage`] items from a bounded channel and performs all
//! disk I/O: opening files, writing chunks, flushing, renaming, and metadata
//! application.  Runs on a dedicated [`std::thread`] so the network thread
//! never blocks on disk.
//!
//! # Thread Protocol
//!
//! ```text
//! Network thread                      Disk thread
//! ──────────────                      ───────────
//! Begin(msg)   ──────────────────▶    open file
//! Chunk(data)  ──────────────────▶    write data
//! ...          ──────────────────▶    ...
//! Commit       ──────────────────▶    flush / rename
//!              ◀──────────────────    Ok(CommitResult)
//! ```
//!
//! A `Shutdown` message terminates the thread after draining in-progress work.

use std::fs;
use std::io::{self, Write};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::{self, JoinHandle};

use fast_io::FileWriter;

use crate::adaptive_buffer::adaptive_writer_capacity;
use crate::delta_apply::SparseWriteState;
use crate::pipeline::messages::{BeginMessage, CommitResult, FileMessage};
use crate::temp_guard::{TempFileGuard, open_tmpfile};

/// Default bounded-channel capacity.
///
/// 32 slots × ~32 KB average chunk ≈ 1 MB peak memory from buffered messages.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 32;

/// Configuration for the disk commit thread.
#[derive(Debug, Clone)]
pub struct DiskCommitConfig {
    /// Whether to fsync files after writing.
    pub do_fsync: bool,
    /// Bounded channel capacity (back-pressure threshold).
    pub channel_capacity: usize,
}

impl Default for DiskCommitConfig {
    fn default() -> Self {
        Self {
            do_fsync: false,
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
        }
    }
}

/// Spawns the disk commit thread and returns the communication channels.
///
/// - `file_tx`: send [`FileMessage`] items to the disk thread.
/// - `result_rx`: receive [`CommitResult`] (one per committed file).
/// - `JoinHandle`: join the thread after sending `Shutdown`.
pub fn spawn_disk_thread(
    config: DiskCommitConfig,
) -> (
    SyncSender<FileMessage>,
    Receiver<io::Result<CommitResult>>,
    JoinHandle<()>,
) {
    let (file_tx, file_rx) = sync_channel::<FileMessage>(config.channel_capacity);
    let (result_tx, result_rx) = std::sync::mpsc::channel::<io::Result<CommitResult>>();

    let handle = thread::Builder::new()
        .name("disk-commit".into())
        .spawn(move || disk_thread_main(file_rx, result_tx, config))
        .expect("failed to spawn disk-commit thread");

    (file_tx, result_rx, handle)
}

/// Main loop of the disk commit thread.
fn disk_thread_main(
    file_rx: Receiver<FileMessage>,
    result_tx: std::sync::mpsc::Sender<io::Result<CommitResult>>,
    config: DiskCommitConfig,
) {
    while let Ok(msg) = file_rx.recv() {
        match msg {
            FileMessage::Shutdown => break,
            FileMessage::Begin(begin) => {
                let result = process_file(&file_rx, &config, begin);
                // If the result channel is disconnected, exit silently —
                // the network thread has already dropped its receiver.
                if result_tx.send(result).is_err() {
                    break;
                }
            }
            FileMessage::Chunk(_) | FileMessage::Commit | FileMessage::Abort { .. } => {
                // Orphaned message without a preceding Begin — protocol error.
                let err = io::Error::new(
                    io::ErrorKind::InvalidData,
                    "disk thread received message without preceding Begin",
                );
                if result_tx.send(Err(err)).is_err() {
                    break;
                }
            }
        }
    }
}

/// Processes a single file: open, write chunks, commit or abort.
fn process_file(
    file_rx: &Receiver<FileMessage>,
    config: &DiskCommitConfig,
    begin: BeginMessage,
) -> io::Result<CommitResult> {
    let (file, mut cleanup_guard, needs_rename) = open_output_file(&begin)?;

    let writer_capacity = adaptive_writer_capacity(begin.target_size);
    let mut output = fast_io::writer_from_file(file, writer_capacity);

    let mut sparse_state = if begin.use_sparse {
        Some(SparseWriteState::default())
    } else {
        None
    };

    let mut bytes_written: u64 = 0;

    // Consume Chunk / Commit / Abort messages for this file.
    loop {
        let msg = file_rx.recv().map_err(|_| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "disk thread: channel disconnected while processing file",
            )
        })?;

        match msg {
            FileMessage::Chunk(data) => {
                if let Some(ref mut sparse) = sparse_state {
                    sparse.write(&mut output, &data)?;
                } else {
                    output.write_all(&data)?;
                }
                bytes_written += data.len() as u64;
            }
            FileMessage::Commit => {
                // Finalize sparse writing.
                if let Some(ref mut sparse) = sparse_state {
                    let _final_pos = sparse.finish(&mut output)?;
                }

                // Flush and optionally fsync.
                if config.do_fsync {
                    output.sync().map_err(|e| {
                        io::Error::new(
                            e.kind(),
                            format!("fsync failed for {:?}: {e}", begin.file_path),
                        )
                    })?;
                } else {
                    output.flush().map_err(|e| {
                        io::Error::other(format!("flush failed for {:?}: {e}", begin.file_path))
                    })?;
                }
                drop(output);

                // Atomic rename (only for temp+rename path).
                if needs_rename {
                    fs::rename(cleanup_guard.path(), &begin.file_path)?;
                }
                cleanup_guard.keep();

                return Ok(CommitResult {
                    bytes_written,
                    file_entry_index: begin.file_entry_index,
                    metadata_error: None,
                });
            }
            FileMessage::Abort { reason } => {
                // Drop output and guard — guard's Drop removes the temp file.
                drop(output);
                drop(cleanup_guard);
                return Err(io::Error::other(reason));
            }
            FileMessage::Shutdown => {
                // Unexpected shutdown mid-file — clean up and exit.
                drop(output);
                drop(cleanup_guard);
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "disk thread: shutdown received while processing file",
                ));
            }
            FileMessage::Begin(_) => {
                // Nested Begin without a preceding Commit/Abort — protocol violation.
                drop(output);
                drop(cleanup_guard);
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "disk thread: received Begin while processing another file",
                ));
            }
        }
    }
}

/// Opens the output file using direct write or temp+rename strategy.
///
/// Mirrors the logic in [`crate::transfer_ops::process_file_response`].
fn open_output_file(begin: &BeginMessage) -> io::Result<(fs::File, TempFileGuard, bool)> {
    if begin.direct_write {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&begin.file_path)
        {
            Ok(file) => {
                // Direct write: guard cleans up file_path on failure, no rename needed.
                Ok((file, TempFileGuard::new(begin.file_path.clone()), false))
            }
            Err(ref e) if e.kind() == io::ErrorKind::AlreadyExists => {
                // Race: file appeared between basis check and create. Use temp+rename.
                let (file, guard) = open_tmpfile(&begin.file_path, None)?;
                Ok((file, guard, true))
            }
            Err(e) => Err(e),
        }
    } else {
        let (file, guard) = open_tmpfile(&begin.file_path, None)?;
        Ok((file, guard, true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::TryRecvError;

    #[test]
    fn default_config() {
        let config = DiskCommitConfig::default();
        assert!(!config.do_fsync);
        assert_eq!(config.channel_capacity, DEFAULT_CHANNEL_CAPACITY);
    }

    #[test]
    fn spawn_and_shutdown() {
        let (tx, _rx, handle) = spawn_disk_thread(DiskCommitConfig::default());
        tx.send(FileMessage::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn write_and_commit_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("output.dat");

        let (tx, rx, handle) = spawn_disk_thread(DiskCommitConfig::default());

        tx.send(FileMessage::Begin(BeginMessage {
            file_path: file_path.clone(),
            target_size: 1024,
            file_entry_index: 0,
            use_sparse: false,
            direct_write: true,
        }))
        .unwrap();

        tx.send(FileMessage::Chunk(b"hello world".to_vec()))
            .unwrap();
        tx.send(FileMessage::Commit).unwrap();

        let result = rx.recv().unwrap().unwrap();
        assert_eq!(result.bytes_written, 11);
        assert_eq!(result.file_entry_index, 0);
        assert!(result.metadata_error.is_none());

        assert_eq!(fs::read(&file_path).unwrap(), b"hello world");

        tx.send(FileMessage::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn abort_cleans_up_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("aborted.dat");

        let (tx, rx, handle) = spawn_disk_thread(DiskCommitConfig::default());

        // Use non-direct write so a temp file is created.
        tx.send(FileMessage::Begin(BeginMessage {
            file_path: file_path.clone(),
            target_size: 100,
            file_entry_index: 1,
            use_sparse: false,
            direct_write: false,
        }))
        .unwrap();

        tx.send(FileMessage::Chunk(b"partial".to_vec())).unwrap();
        tx.send(FileMessage::Abort {
            reason: "test abort".into(),
        })
        .unwrap();

        let result = rx.recv().unwrap();
        assert!(result.is_err());

        // Destination should not exist.
        assert!(!file_path.exists());

        tx.send(FileMessage::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn multiple_files_sequential() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx, handle) = spawn_disk_thread(DiskCommitConfig::default());

        for i in 0..3 {
            let path = dir.path().join(format!("file{i}.dat"));
            let data = format!("content-{i}");

            tx.send(FileMessage::Begin(BeginMessage {
                file_path: path.clone(),
                target_size: data.len() as u64,
                file_entry_index: i,
                use_sparse: false,
                direct_write: true,
            }))
            .unwrap();

            tx.send(FileMessage::Chunk(data.into_bytes())).unwrap();
            tx.send(FileMessage::Commit).unwrap();

            let result = rx.recv().unwrap().unwrap();
            assert_eq!(result.file_entry_index, i);
            assert_eq!(fs::read_to_string(&path).unwrap(), format!("content-{i}"));
        }

        tx.send(FileMessage::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn channel_disconnect_stops_thread() {
        let (tx, rx, handle) = spawn_disk_thread(DiskCommitConfig::default());
        drop(tx);

        // Thread exits because channel disconnected.
        handle.join().unwrap();

        // No results should be pending.
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Disconnected)));
    }

    #[test]
    fn multi_chunk_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("multi_chunk.dat");

        let (tx, rx, handle) = spawn_disk_thread(DiskCommitConfig::default());

        tx.send(FileMessage::Begin(BeginMessage {
            file_path: file_path.clone(),
            target_size: 300,
            file_entry_index: 0,
            use_sparse: false,
            direct_write: true,
        }))
        .unwrap();

        tx.send(FileMessage::Chunk(b"aaa".to_vec())).unwrap();
        tx.send(FileMessage::Chunk(b"bbb".to_vec())).unwrap();
        tx.send(FileMessage::Chunk(b"ccc".to_vec())).unwrap();
        tx.send(FileMessage::Commit).unwrap();

        let result = rx.recv().unwrap().unwrap();
        assert_eq!(result.bytes_written, 9);
        assert_eq!(fs::read(&file_path).unwrap(), b"aaabbbccc");

        tx.send(FileMessage::Shutdown).unwrap();
        handle.join().unwrap();
    }
}
