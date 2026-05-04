//! Disk commit thread spawning and main loop.
//!
//! The disk thread consumes `FileMessage` items from a bounded SPSC channel,
//! performing all disk I/O on a dedicated thread so the network thread never
//! blocks on disk latency.
//!
//! When io_uring is available (Linux 5.6+ with the `io_uring` feature), the
//! thread creates a single [`fast_io::IoUringDiskBatch`] and threads it into
//! every per-file [`process_file`]/[`process_whole_file`] call so writes are
//! submitted as batched io_uring SQEs. When the batch is unavailable or sparse
//! mode is requested, the thread falls back to the buffered writer using a
//! reusable 256 KB scratch buffer that mirrors upstream's static
//! `wf_writeBuf` (fileio.c:161).

use std::io;
use std::thread::{self, JoinHandle};

use logging::debug_log;

use crate::pipeline::messages::{CommitResult, FileMessage};
use crate::pipeline::spsc;

use super::config::DiskCommitConfig;
use super::process::{process_file, process_whole_file};
use super::writer::WRITE_BUF_SIZE;

/// Channels and handle returned by [`spawn_disk_thread`].
pub struct DiskThreadHandle {
    /// Send `FileMessage` items to the disk thread.
    pub file_tx: spsc::Sender<FileMessage>,
    /// Receive [`CommitResult`] (one per committed file).
    pub result_rx: spsc::Receiver<io::Result<CommitResult>>,
    /// Receive recycled `Vec<u8>` buffers from the disk thread.
    pub buf_return_rx: spsc::Receiver<Vec<u8>>,
    /// Join handle for the disk commit thread.
    pub join_handle: JoinHandle<()>,
}

/// Spawns the disk commit thread and returns channels + join handle.
///
/// Buffer recycling mirrors upstream rsync's `simple_recv_token` (token.c:284)
/// which uses a single static buffer that is never freed. Here, the disk thread
/// sends used `Vec<u8>` buffers back through `buf_return_rx` for reuse by the
/// network thread, eliminating per-chunk malloc/free overhead.
pub fn spawn_disk_thread(config: DiskCommitConfig) -> DiskThreadHandle {
    let capacity = config.effective_channel_capacity();
    let (file_tx, file_rx) = spsc::channel::<FileMessage>(capacity);
    let (result_tx, result_rx) = spsc::channel::<io::Result<CommitResult>>(capacity * 2);
    let (buf_return_tx, buf_return_rx) = spsc::channel::<Vec<u8>>(capacity * 2);

    let join_handle = thread::Builder::new()
        .name("disk-commit".into())
        .spawn(move || disk_thread_main(file_rx, result_tx, buf_return_tx, config))
        .expect("failed to spawn disk-commit thread");

    DiskThreadHandle {
        file_tx,
        result_rx,
        buf_return_rx,
        join_handle,
    }
}

/// Attempts to create an io_uring batch writer based on the configured policy.
///
/// Returns `Some` on Linux 5.6+ when the `io_uring` feature is enabled and
/// the policy is `Auto` or `Enabled`. Returns `None` when io_uring is
/// unavailable or the policy is `Disabled`.
fn try_create_disk_batch(policy: fast_io::IoUringPolicy) -> Option<fast_io::IoUringDiskBatch> {
    match policy {
        fast_io::IoUringPolicy::Disabled => None,
        fast_io::IoUringPolicy::Auto => {
            fast_io::IoUringDiskBatch::try_new(&fast_io::IoUringConfig::default())
        }
        fast_io::IoUringPolicy::Enabled => {
            // Enabled policy: try to create, but log and proceed if it fails.
            // The caller explicitly requested io_uring, so we attempt it but
            // do not fail the entire transfer if the ring cannot be created.
            fast_io::IoUringDiskBatch::try_new(&fast_io::IoUringConfig::default())
        }
    }
}

/// Logs io_uring availability at debug I/O level 1 (activated at `-vv`).
///
/// Reports whether io_uring is being used for disk writes and, if not, why.
/// This gives users visibility into which I/O path is active without
/// cluttering default output.
fn log_io_uring_status(policy: fast_io::IoUringPolicy, batch_created: bool) {
    match policy {
        fast_io::IoUringPolicy::Disabled => {
            debug_log!(
                Io,
                1,
                "io_uring disabled by --no-io-uring, using standard I/O"
            );
        }
        fast_io::IoUringPolicy::Auto | fast_io::IoUringPolicy::Enabled => {
            if batch_created {
                debug_log!(
                    Io,
                    1,
                    "disk I/O: {}",
                    fast_io::io_uring_availability_reason()
                );
            } else {
                debug_log!(
                    Io,
                    1,
                    "disk I/O: {}, using standard I/O fallback",
                    fast_io::io_uring_availability_reason()
                );
            }
        }
    }
}

/// Main loop of the disk commit thread.
///
/// Allocates a single 256KB write buffer reused across all files, matching
/// upstream rsync's static `wf_writeBuf` (fileio.c:161). On Linux 5.6+
/// with io_uring support, a batched ring writer is created once and reused
/// across all files for reduced syscall overhead.
fn disk_thread_main(
    file_rx: spsc::Receiver<FileMessage>,
    result_tx: spsc::Sender<io::Result<CommitResult>>,
    buf_return_tx: spsc::Sender<Vec<u8>>,
    config: DiskCommitConfig,
) {
    let mut write_buf = Vec::with_capacity(WRITE_BUF_SIZE);
    let mut disk_batch = try_create_disk_batch(config.io_uring_policy);

    log_io_uring_status(config.io_uring_policy, disk_batch.is_some());

    while let Ok(msg) = file_rx.recv() {
        match msg {
            FileMessage::Shutdown => break,
            FileMessage::Begin(begin) => {
                let result = process_file(
                    &file_rx,
                    &buf_return_tx,
                    &config,
                    *begin,
                    &mut write_buf,
                    disk_batch.as_mut(),
                );
                if result_tx.send(result).is_err() {
                    break;
                }
            }
            FileMessage::WholeFile { begin, data } => {
                let result = process_whole_file(
                    &buf_return_tx,
                    &config,
                    *begin,
                    data,
                    &mut write_buf,
                    disk_batch.as_mut(),
                );
                if result_tx.send(result).is_err() {
                    break;
                }
            }
            FileMessage::Chunk(_) | FileMessage::Commit | FileMessage::Abort { .. } => {
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
