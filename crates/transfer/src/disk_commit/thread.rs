//! Disk commit thread spawning and main loop.
//!
//! The disk thread consumes `FileMessage` items from a bounded SPSC channel,
//! performing all disk I/O on a dedicated thread so the network thread never
//! blocks on disk latency.

use std::io;
use std::thread::{self, JoinHandle};

use crate::pipeline::messages::{CommitResult, FileMessage};
use crate::pipeline::spsc;

use super::config::DiskCommitConfig;
use super::process::{process_file, process_whole_file};
use super::writer::WRITE_BUF_SIZE;

/// Bounded SPSC channel capacity for the disk commit thread.
///
/// 128 slots x ~32 KB average chunk = 4 MB peak memory from buffered messages.
/// Larger capacity reduces spin-wait contention when the disk thread falls
/// behind, keeping the network thread productive instead of busy-spinning.
const CHANNEL_CAPACITY: usize = 128;

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
    let (file_tx, file_rx) = spsc::channel::<FileMessage>(CHANNEL_CAPACITY);
    let (result_tx, result_rx) =
        spsc::channel::<io::Result<CommitResult>>(CHANNEL_CAPACITY * 2);
    let (buf_return_tx, buf_return_rx) = spsc::channel::<Vec<u8>>(CHANNEL_CAPACITY * 2);

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

/// Main loop of the disk commit thread.
///
/// Allocates a single 256KB write buffer reused across all files, matching
/// upstream rsync's static `wf_writeBuf` (fileio.c:161).
fn disk_thread_main(
    file_rx: spsc::Receiver<FileMessage>,
    result_tx: spsc::Sender<io::Result<CommitResult>>,
    buf_return_tx: spsc::Sender<Vec<u8>>,
    config: DiskCommitConfig,
) {
    let mut write_buf = Vec::with_capacity(WRITE_BUF_SIZE);

    while let Ok(msg) = file_rx.recv() {
        match msg {
            FileMessage::Shutdown => break,
            FileMessage::Begin(begin) => {
                let result =
                    process_file(&file_rx, &buf_return_tx, &config, *begin, &mut write_buf);
                if result_tx.send(result).is_err() {
                    break;
                }
            }
            FileMessage::WholeFile { begin, data } => {
                let result =
                    process_whole_file(&buf_return_tx, &config, *begin, data, &mut write_buf);
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
