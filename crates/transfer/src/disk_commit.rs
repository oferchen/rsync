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
use std::io::{self, IoSlice, Seek, Write};
use std::thread::{self, JoinHandle};

use crate::pipeline::spsc;

use metadata::MetadataOptions;

use crate::delta_apply::{ChecksumVerifier, SparseWriteState};
use crate::pipeline::messages::{BeginMessage, CommitResult, ComputedChecksum, FileMessage};
use crate::temp_guard::{TempFileGuard, open_tmpfile};

/// Default bounded-channel capacity.
///
/// 32 slots × ~32 KB average chunk ≈ 1 MB peak memory from buffered messages.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 32;

/// Fixed write buffer size matching upstream's `wf_writeBufSize = WRITE_SIZE * 8`
/// (fileio.c:161). Upstream always uses 256 KB regardless of file size.
const WRITE_BUF_SIZE: usize = 256 * 1024;

/// Minimum chunk size for direct-to-file writes, bypassing the buffer.
///
/// Chunks at or above this size are written directly to the file descriptor,
/// eliminating one `memcpy` from the hot path. Smaller chunks are still
/// buffered to amortize syscall overhead for tiny delta tokens.
///
/// 8 KB balances syscall cost (~100-200 ns) against copy cost (~200-400 ns
/// for 8 KB in L1/L2 cache). Most rsync literal tokens are 32 KB+, so this
/// threshold catches the common case.
const DIRECT_WRITE_THRESHOLD: usize = 8 * 1024;

/// Minimum file size to pre-allocate with `set_len()`.
/// Matches upstream rsync's `preallocate_files` threshold — small files are not
/// worth the extra syscall.
const PREALLOC_THRESHOLD: u64 = 64 * 1024;

/// Writes two buffers as a single `writev` syscall, falling back to
/// sequential `write_all` if vectored I/O is unsupported.
fn write_all_vectored(file: &mut fs::File, first: &[u8], second: &[u8]) -> io::Result<()> {
    let total = first.len() + second.len();
    let mut written = 0usize;

    // First attempt: vectored write combining both slices.
    while written < first.len() {
        let bufs = [IoSlice::new(&first[written..]), IoSlice::new(second)];
        match file.write_vectored(&bufs) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "write_vectored returned 0",
                ));
            }
            Ok(n) => written += n,
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }

    // Second part: remaining bytes from `second`.
    let second_offset = written - first.len();
    if second_offset < second.len() {
        file.write_all(&second[second_offset..])?;
    }

    debug_assert_eq!(
        first.len() + second.len(),
        total,
        "write_all_vectored size mismatch"
    );
    Ok(())
}

/// Buffered writer that reuses an externally-owned `Vec<u8>`, avoiding
/// per-file allocation. The buffer is allocated once in `disk_thread_main`
/// and cleared between files — matching upstream rsync's static `wf_writeBuf`
/// (fileio.c:161).
struct ReusableBufWriter<'a> {
    file: fs::File,
    buf: &'a mut Vec<u8>,
}

impl<'a> ReusableBufWriter<'a> {
    fn new(file: fs::File, buf: &'a mut Vec<u8>) -> Self {
        buf.clear();
        Self { file, buf }
    }

    fn sync(&mut self) -> io::Result<()> {
        self.flush()?;
        self.file.sync_all()
    }
}

impl Write for ReusableBufWriter<'_> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.len() >= DIRECT_WRITE_THRESHOLD {
            if !self.buf.is_empty() {
                // Combine buffered data and new chunk in a single writev
                // syscall, halving the write count for the common case of
                // small buffered data followed by a large literal token.
                write_all_vectored(&mut self.file, self.buf, data)?;
                self.buf.clear();
            } else {
                self.file.write_all(data)?;
            }
            return Ok(data.len());
        }

        if self.buf.len() + data.len() <= self.buf.capacity() {
            self.buf.extend_from_slice(data);
        } else {
            self.file.write_all(self.buf)?;
            self.buf.clear();
            self.buf.extend_from_slice(data);
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            self.file.write_all(self.buf)?;
            self.buf.clear();
        }
        Ok(())
    }
}

impl Seek for ReusableBufWriter<'_> {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        self.flush()?;
        self.file.seek(pos)
    }
}

/// Channels and handle returned by [`spawn_disk_thread`].
pub struct DiskThreadHandle {
    /// Send [`FileMessage`] items to the disk thread.
    pub file_tx: spsc::Sender<FileMessage>,
    /// Receive [`CommitResult`] (one per committed file).
    pub result_rx: spsc::Receiver<io::Result<CommitResult>>,
    /// Receive recycled `Vec<u8>` buffers from the disk thread.
    pub buf_return_rx: spsc::Receiver<Vec<u8>>,
    /// Join handle for the disk commit thread.
    pub join_handle: JoinHandle<()>,
}

/// Configuration for the disk commit thread.
#[derive(Debug, Clone)]
pub struct DiskCommitConfig {
    /// Whether to fsync files after writing.
    pub do_fsync: bool,
    /// Bounded channel capacity (back-pressure threshold).
    pub channel_capacity: usize,
    /// Metadata options for applying file attributes after commit.
    /// When `Some`, the disk thread applies metadata (mtime, perms, ownership)
    /// immediately after rename — mirroring upstream `finish_transfer()` →
    /// `set_file_attrs()` in receiver.c.
    pub metadata_opts: Option<MetadataOptions>,
}

impl Default for DiskCommitConfig {
    fn default() -> Self {
        Self {
            do_fsync: false,
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
            metadata_opts: None,
        }
    }
}

/// Spawns the disk commit thread and returns channels + join handle.
///
/// Buffer recycling mirrors upstream rsync's `simple_recv_token` (token.c:284)
/// which uses a single static buffer that is never freed. Here, the disk thread
/// sends used `Vec<u8>` buffers back through `buf_return_rx` for reuse by the
/// network thread, eliminating per-chunk malloc/free overhead.
pub fn spawn_disk_thread(config: DiskCommitConfig) -> DiskThreadHandle {
    let (file_tx, file_rx) = spsc::channel::<FileMessage>(config.channel_capacity);
    let (result_tx, result_rx) =
        spsc::channel::<io::Result<CommitResult>>(config.channel_capacity * 2);
    let (buf_return_tx, buf_return_rx) = spsc::channel::<Vec<u8>>(config.channel_capacity * 2);

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

/// Processes a single file: open, write chunks, commit or abort.
///
/// After writing each chunk, the owned `Vec<u8>` is returned through
/// `buf_return_tx` for reuse by the network thread.
fn process_file(
    file_rx: &spsc::Receiver<FileMessage>,
    buf_return_tx: &spsc::Sender<Vec<u8>>,
    config: &DiskCommitConfig,
    begin: BeginMessage,
    write_buf: &mut Vec<u8>,
) -> io::Result<CommitResult> {
    let (file, mut cleanup_guard, needs_rename) = open_output_file(&begin)?;

    // Pre-allocate disk space for new files above the threshold.
    // Mirrors upstream receiver.c:254-258 `do_fallocate(fd, 0, total_size)`.
    if begin.direct_write && begin.target_size >= PREALLOC_THRESHOLD {
        // set_len pre-allocates on most filesystems. On failure (e.g. ENOSPC),
        // we continue — writes will fail later with a clearer error.
        let _ = file.set_len(begin.target_size);
    }

    let mut output = ReusableBufWriter::new(file, write_buf);

    let mut sparse_state = if begin.use_sparse {
        Some(SparseWriteState::default())
    } else {
        None
    };

    // Per-file checksum verifier, moved from the network thread.
    // Computing the checksum here overlaps hashing with disk I/O and
    // removes ~42% of instructions from the network-critical path.
    let mut checksum_verifier = begin.checksum_verifier;

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
                // Update per-file checksum before writing (mirrors upstream
                // receiver.c:315 which hashes each token before writing).
                if let Some(ref mut verifier) = checksum_verifier {
                    verifier.update(&data);
                }

                if let Some(ref mut sparse) = sparse_state {
                    sparse.write(&mut output, &data)?;
                } else {
                    output.write_all(&data)?;
                }
                bytes_written += data.len() as u64;
                // Return the buffer for reuse. Ignore errors — the network
                // thread may have moved on (e.g. after an error).
                let _ = buf_return_tx.send(data);
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

                // Apply metadata immediately after rename — mirrors upstream
                // finish_transfer() → set_file_attrs() in receiver.c.
                let metadata_error = match (&config.metadata_opts, &begin.file_entry) {
                    (Some(opts), Some(entry)) => {
                        match metadata::apply_metadata_from_file_entry(
                            &begin.file_path,
                            entry,
                            opts,
                        ) {
                            Ok(()) => None,
                            Err(e) => Some((begin.file_path.clone(), e.to_string())),
                        }
                    }
                    _ => None,
                };

                // Finalize per-file checksum and return to network thread.
                let computed_checksum = checksum_verifier.map(|verifier| {
                    let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
                    let len = verifier.finalize_into(&mut buf);
                    ComputedChecksum { bytes: buf, len }
                });

                return Ok(CommitResult {
                    bytes_written,
                    file_entry_index: begin.file_entry_index,
                    metadata_error,
                    computed_checksum,
                });
            }
            FileMessage::Abort { reason } => {
                drop(output);
                drop(cleanup_guard);
                return Err(io::Error::other(reason));
            }
            FileMessage::Shutdown => {
                drop(output);
                drop(cleanup_guard);
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "disk thread: shutdown received while processing file",
                ));
            }
            FileMessage::Begin(_) | FileMessage::WholeFile { .. } => {
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

/// Processes a single-chunk file in one shot (coalesced Begin+Chunk+Commit).
///
/// Avoids the per-message channel recv loop of [`process_file`], reducing
/// futex overhead from 3+ sends/recvs to 1 for small files.
fn process_whole_file(
    buf_return_tx: &spsc::Sender<Vec<u8>>,
    config: &DiskCommitConfig,
    begin: BeginMessage,
    data: Vec<u8>,
    write_buf: &mut Vec<u8>,
) -> io::Result<CommitResult> {
    let (file, mut cleanup_guard, needs_rename) = open_output_file(&begin)?;

    if begin.direct_write && begin.target_size >= PREALLOC_THRESHOLD {
        let _ = file.set_len(begin.target_size);
    }

    let mut output = ReusableBufWriter::new(file, write_buf);
    let bytes_written = data.len() as u64;

    // Update checksum and write data.
    let mut checksum_verifier = begin.checksum_verifier;
    if let Some(ref mut verifier) = checksum_verifier {
        verifier.update(&data);
    }

    if begin.use_sparse {
        let mut sparse = SparseWriteState::default();
        sparse.write(&mut output, &data)?;
        let _final_pos = sparse.finish(&mut output)?;
    } else {
        output.write_all(&data)?;
    }

    // Return buffer for reuse.
    let _ = buf_return_tx.send(data);

    // Flush / fsync.
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

    if needs_rename {
        fs::rename(cleanup_guard.path(), &begin.file_path)?;
    }
    cleanup_guard.keep();

    // Apply metadata — mirrors upstream finish_transfer() → set_file_attrs().
    let metadata_error = match (&config.metadata_opts, &begin.file_entry) {
        (Some(opts), Some(entry)) => {
            match metadata::apply_metadata_from_file_entry(&begin.file_path, entry, opts) {
                Ok(()) => None,
                Err(e) => Some((begin.file_path.clone(), e.to_string())),
            }
        }
        _ => None,
    };

    let computed_checksum = checksum_verifier.map(|verifier| {
        let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        let len = verifier.finalize_into(&mut buf);
        ComputedChecksum { bytes: buf, len }
    });

    Ok(CommitResult {
        bytes_written,
        file_entry_index: begin.file_entry_index,
        metadata_error,
        computed_checksum,
    })
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
    use crate::pipeline::spsc::TryRecvError;

    #[test]
    fn default_config() {
        let config = DiskCommitConfig::default();
        assert!(!config.do_fsync);
        assert_eq!(config.channel_capacity, DEFAULT_CHANNEL_CAPACITY);
    }

    #[test]
    fn spawn_and_shutdown() {
        let h = spawn_disk_thread(DiskCommitConfig::default());
        h.file_tx.send(FileMessage::Shutdown).unwrap();
        h.join_handle.join().unwrap();
    }

    #[test]
    fn write_and_commit_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("output.dat");

        let h = spawn_disk_thread(DiskCommitConfig::default());

        h.file_tx
            .send(FileMessage::Begin(Box::new(BeginMessage {
                file_path: file_path.clone(),
                target_size: 1024,
                file_entry_index: 0,
                use_sparse: false,
                direct_write: true,
                checksum_verifier: None,
                file_entry: None,
            })))
            .unwrap();

        h.file_tx
            .send(FileMessage::Chunk(b"hello world".to_vec()))
            .unwrap();
        h.file_tx.send(FileMessage::Commit).unwrap();

        let result = h.result_rx.recv().unwrap().unwrap();
        assert_eq!(result.bytes_written, 11);
        assert_eq!(result.file_entry_index, 0);
        assert!(result.metadata_error.is_none());

        assert_eq!(fs::read(&file_path).unwrap(), b"hello world");

        h.file_tx.send(FileMessage::Shutdown).unwrap();
        h.join_handle.join().unwrap();
    }

    #[test]
    fn abort_cleans_up_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("aborted.dat");

        let h = spawn_disk_thread(DiskCommitConfig::default());

        h.file_tx
            .send(FileMessage::Begin(Box::new(BeginMessage {
                file_path: file_path.clone(),
                target_size: 100,
                file_entry_index: 1,
                use_sparse: false,
                direct_write: false,
                checksum_verifier: None,
                file_entry: None,
            })))
            .unwrap();

        h.file_tx
            .send(FileMessage::Chunk(b"partial".to_vec()))
            .unwrap();
        h.file_tx
            .send(FileMessage::Abort {
                reason: "test abort".into(),
            })
            .unwrap();

        let result = h.result_rx.recv().unwrap();
        assert!(result.is_err());

        assert!(!file_path.exists());

        h.file_tx.send(FileMessage::Shutdown).unwrap();
        h.join_handle.join().unwrap();
    }

    #[test]
    fn multiple_files_sequential() {
        let dir = tempfile::tempdir().unwrap();
        let h = spawn_disk_thread(DiskCommitConfig::default());

        for i in 0..3 {
            let path = dir.path().join(format!("file{i}.dat"));
            let data = format!("content-{i}");

            h.file_tx
                .send(FileMessage::Begin(Box::new(BeginMessage {
                    file_path: path.clone(),
                    target_size: data.len() as u64,
                    file_entry_index: i,
                    use_sparse: false,
                    direct_write: true,
                    checksum_verifier: None,
                    file_entry: None,
                })))
                .unwrap();

            h.file_tx
                .send(FileMessage::Chunk(data.into_bytes()))
                .unwrap();
            h.file_tx.send(FileMessage::Commit).unwrap();

            let result = h.result_rx.recv().unwrap().unwrap();
            assert_eq!(result.file_entry_index, i);
            assert_eq!(fs::read_to_string(&path).unwrap(), format!("content-{i}"));
        }

        h.file_tx.send(FileMessage::Shutdown).unwrap();
        h.join_handle.join().unwrap();
    }

    #[test]
    fn channel_disconnect_stops_thread() {
        let h = spawn_disk_thread(DiskCommitConfig::default());
        drop(h.file_tx);

        h.join_handle.join().unwrap();

        assert!(matches!(
            h.result_rx.try_recv(),
            Err(TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn multi_chunk_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("multi_chunk.dat");

        let h = spawn_disk_thread(DiskCommitConfig::default());

        h.file_tx
            .send(FileMessage::Begin(Box::new(BeginMessage {
                file_path: file_path.clone(),
                target_size: 300,
                file_entry_index: 0,
                use_sparse: false,
                direct_write: true,
                checksum_verifier: None,
                file_entry: None,
            })))
            .unwrap();

        h.file_tx.send(FileMessage::Chunk(b"aaa".to_vec())).unwrap();
        h.file_tx.send(FileMessage::Chunk(b"bbb".to_vec())).unwrap();
        h.file_tx.send(FileMessage::Chunk(b"ccc".to_vec())).unwrap();
        h.file_tx.send(FileMessage::Commit).unwrap();

        let result = h.result_rx.recv().unwrap().unwrap();
        assert_eq!(result.bytes_written, 9);
        assert_eq!(fs::read(&file_path).unwrap(), b"aaabbbccc");

        h.file_tx.send(FileMessage::Shutdown).unwrap();
        h.join_handle.join().unwrap();
    }

    #[test]
    fn buffer_recycling() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("recycle.dat");

        let h = spawn_disk_thread(DiskCommitConfig::default());

        h.file_tx
            .send(FileMessage::Begin(Box::new(BeginMessage {
                file_path: file_path.clone(),
                target_size: 100,
                file_entry_index: 0,
                use_sparse: false,
                direct_write: true,
                checksum_verifier: None,
                file_entry: None,
            })))
            .unwrap();

        h.file_tx
            .send(FileMessage::Chunk(b"hello".to_vec()))
            .unwrap();
        h.file_tx
            .send(FileMessage::Chunk(b" world".to_vec()))
            .unwrap();
        h.file_tx.send(FileMessage::Commit).unwrap();

        let result = h.result_rx.recv().unwrap().unwrap();
        assert_eq!(result.bytes_written, 11);

        // Disk thread should have returned 2 buffers for recycling.
        let recycled1 = h.buf_return_rx.recv().unwrap();
        let recycled2 = h.buf_return_rx.recv().unwrap();
        assert!(recycled1.capacity() >= 5);
        assert!(recycled2.capacity() >= 6);

        h.file_tx.send(FileMessage::Shutdown).unwrap();
        h.join_handle.join().unwrap();
    }

    #[test]
    fn whole_file_coalesced() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("whole.dat");

        let h = spawn_disk_thread(DiskCommitConfig::default());

        h.file_tx
            .send(FileMessage::WholeFile {
                begin: Box::new(BeginMessage {
                    file_path: file_path.clone(),
                    target_size: 9,
                    file_entry_index: 0,
                    use_sparse: false,
                    direct_write: true,
                    checksum_verifier: None,
                    file_entry: None,
                }),
                data: b"whole dat".to_vec(),
            })
            .unwrap();

        let result = h.result_rx.recv().unwrap().unwrap();
        assert_eq!(result.bytes_written, 9);
        assert_eq!(result.file_entry_index, 0);
        assert!(result.metadata_error.is_none());
        assert_eq!(fs::read(&file_path).unwrap(), b"whole dat");

        // Buffer should be returned for recycling.
        let recycled = h.buf_return_rx.recv().unwrap();
        assert!(recycled.capacity() >= 9);

        h.file_tx.send(FileMessage::Shutdown).unwrap();
        h.join_handle.join().unwrap();
    }
}
