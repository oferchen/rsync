//! Batched io_uring disk writer for multi-file commit operations.
//!
//! Provides [`IoUringDiskBatch`] which shares a single io_uring ring across
//! multiple file write operations, amortizing ring setup cost and enabling
//! batched submission of writes to different files.
//!
//! # Usage
//!
//! The disk commit thread opens files, writes chunks, then commits. Instead
//! of creating a separate io_uring ring per file (as [`super::IoUringWriter`]
//! does), `IoUringDiskBatch` reuses one ring across the entire commit phase,
//! re-registering file descriptors as files rotate.
//!
//! # Upstream Reference
//!
//! - `fileio.c:write_file()` - upstream writes use a single static buffer
//!   (`wf_writeBuf`) and write(2). We batch these into io_uring SQEs.

use std::fs::File;
use std::io::{self, Write};
use std::os::unix::io::AsRawFd;

use io_uring::{IoUring as RawIoUring, opcode};

use super::batching::{NO_FIXED_FD, maybe_fixed_file, sqe_fd, submit_write_batch, try_register_fd};
use super::config::{IoUringConfig, is_io_uring_available};

/// Default write buffer capacity for the batched disk writer (256 KB).
///
/// Matches upstream rsync's `wf_writeBufSize = WRITE_SIZE * 8`
/// (fileio.c:161).
const DEFAULT_BUFFER_CAPACITY: usize = 256 * 1024;

/// Batched io_uring disk writer for the disk commit phase.
///
/// Owns a single io_uring ring and reuses it across multiple file operations.
/// When a new file is started via [`begin_file`](Self::begin_file), the
/// previous file's data is flushed, the fd registration is updated, and the
/// internal write position resets.
///
/// # Thread Safety
///
/// This type is not `Send` or `Sync` - it is designed for single-threaded use
/// on the dedicated disk commit thread.
pub struct IoUringDiskBatch {
    ring: RawIoUring,
    config: IoUringConfig,
    /// Currently active file, if any.
    current_file: Option<ActiveFile>,
    /// Reusable write buffer.
    buffer: Vec<u8>,
    /// Current write position in the buffer.
    buffer_pos: usize,
}

/// Tracks the currently active file in the batch writer.
struct ActiveFile {
    file: File,
    /// Cumulative bytes written (flushed) to this file.
    bytes_written: u64,
    /// Fixed-file slot index, or `NO_FIXED_FD` when not registered.
    fixed_fd_slot: i32,
}

impl IoUringDiskBatch {
    /// Creates a new batched disk writer with the given configuration.
    ///
    /// Allocates a single io_uring ring that will be reused across all file
    /// operations. Returns `Err` if ring creation fails.
    pub fn new(config: &IoUringConfig) -> io::Result<Self> {
        let ring = config.build_ring()?;
        Ok(Self {
            ring,
            config: config.clone(),
            current_file: None,
            buffer: vec![0u8; config.buffer_size.max(DEFAULT_BUFFER_CAPACITY)],
            buffer_pos: 0,
        })
    }

    /// Attempts to create a batched disk writer, returning `None` if io_uring
    /// is unavailable or ring creation fails.
    ///
    /// This is the recommended constructor for production use - callers should
    /// fall back to standard buffered I/O when `None` is returned.
    #[must_use]
    pub fn try_new(config: &IoUringConfig) -> Option<Self> {
        if !is_io_uring_available() {
            return None;
        }
        Self::new(config).ok()
    }

    /// Begins a new file for writing.
    ///
    /// Flushes any buffered data for the previous file, unregisters the old fd,
    /// and registers the new file with the ring. The caller is responsible for
    /// opening the file with appropriate flags.
    ///
    /// # Errors
    ///
    /// Returns an error if flushing the previous file fails.
    pub fn begin_file(&mut self, file: File) -> io::Result<()> {
        // Flush and finalize the previous file.
        self.flush_current()?;
        self.finalize_current_file();

        let fixed_fd_slot =
            try_register_fd(&self.ring, file.as_raw_fd(), self.config.register_files);
        self.current_file = Some(ActiveFile {
            file,
            bytes_written: 0,
            fixed_fd_slot,
        });
        Ok(())
    }

    /// Writes data to the current file.
    ///
    /// Data is buffered internally and flushed in batched SQEs when the buffer
    /// fills or [`flush`](Self::flush) is called.
    ///
    /// # Errors
    ///
    /// Returns an error if no file is active or if a flush fails.
    pub fn write_data(&mut self, data: &[u8]) -> io::Result<()> {
        if self.current_file.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "no file active in batched disk writer",
            ));
        }

        let mut offset = 0;
        while offset < data.len() {
            let available = self.buffer.len() - self.buffer_pos;
            let to_copy = available.min(data.len() - offset);

            if to_copy > 0 {
                self.buffer[self.buffer_pos..self.buffer_pos + to_copy]
                    .copy_from_slice(&data[offset..offset + to_copy]);
                self.buffer_pos += to_copy;
                offset += to_copy;
            }

            if self.buffer_pos == self.buffer.len() {
                self.flush_current()?;
            }
        }
        Ok(())
    }

    /// Flushes buffered data to the current file using batched io_uring writes.
    ///
    /// # Errors
    ///
    /// Returns an error if no file is active or the io_uring submission fails.
    pub fn flush(&mut self) -> io::Result<()> {
        self.flush_current()
    }

    /// Commits the current file: flushes data and optionally calls fsync.
    ///
    /// After this call, the file is no longer active. The caller can retrieve
    /// the file handle via the returned `File` for rename/metadata operations.
    ///
    /// # Errors
    ///
    /// Returns an error if flushing or fsync fails.
    pub fn commit_file(&mut self, do_fsync: bool) -> io::Result<(File, u64)> {
        self.flush_current()?;

        let active = self.current_file.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "no file active in batched disk writer",
            )
        })?;

        let bytes_written = active.bytes_written;

        if do_fsync {
            self.submit_fsync(&active)?;
        }

        // Unregister the fd from the ring.
        self.unregister_fd(&active);

        Ok((active.file, bytes_written))
    }

    /// Returns the number of bytes written to the current file (flushed only).
    #[must_use]
    pub fn bytes_written(&self) -> u64 {
        self.current_file.as_ref().map_or(0, |f| f.bytes_written)
    }

    /// Returns the total bytes including unflushed buffer content.
    #[must_use]
    pub fn bytes_written_with_pending(&self) -> u64 {
        self.current_file
            .as_ref()
            .map_or(0, |f| f.bytes_written + self.buffer_pos as u64)
    }

    /// Flushes the internal buffer to the current file via batched writes.
    fn flush_current(&mut self) -> io::Result<()> {
        if self.buffer_pos == 0 {
            return Ok(());
        }

        let active = self.current_file.as_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "no file active in batched disk writer",
            )
        })?;

        let fd = sqe_fd(active.file.as_raw_fd(), active.fixed_fd_slot);
        let len = self.buffer_pos;
        let offset = active.bytes_written;

        let written = submit_write_batch(
            &mut self.ring,
            fd,
            &self.buffer[..len],
            offset,
            self.config.buffer_size,
            self.config.sq_entries as usize,
            active.fixed_fd_slot,
        )?;

        active.bytes_written += written as u64;
        self.buffer_pos = 0;
        Ok(())
    }

    /// Submits an fsync SQE for the active file and waits for completion.
    fn submit_fsync(&mut self, active: &ActiveFile) -> io::Result<()> {
        let fd = sqe_fd(active.file.as_raw_fd(), active.fixed_fd_slot);
        let entry = opcode::Fsync::new(fd).build().user_data(0);
        let entry = maybe_fixed_file(entry, active.fixed_fd_slot);

        // SAFETY: The SQE references a valid fd that outlives the submission.
        // The fsync opcode does not reference any user buffers.
        unsafe {
            self.ring
                .submission()
                .push(&entry)
                .map_err(|_| io::Error::other("submission queue full"))?;
        }

        self.ring.submit_and_wait(1)?;

        let cqe = self
            .ring
            .completion()
            .next()
            .ok_or_else(|| io::Error::other("no completion for fsync"))?;

        let result = cqe.result();
        if result < 0 {
            return Err(io::Error::from_raw_os_error(-result));
        }
        Ok(())
    }

    /// Unregisters the fd from the ring's fixed file table.
    fn unregister_fd(&self, active: &ActiveFile) {
        if active.fixed_fd_slot != NO_FIXED_FD {
            // Best-effort unregister - ignore errors since the ring may
            // not have the fd registered (e.g., after a failed registration).
            let _ = self.ring.submitter().unregister_files();
        }
    }

    /// Flushes and drops the current file without returning it.
    fn finalize_current_file(&mut self) {
        if let Some(active) = self.current_file.take() {
            self.unregister_fd(&active);
            // File is dropped here, closing the fd.
        }
    }
}

impl Write for IoUringDiskBatch {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_data(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_current()
    }
}

impl Drop for IoUringDiskBatch {
    fn drop(&mut self) {
        // Best-effort flush on drop.
        let _ = self.flush_current();
        self.finalize_current_file();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn config_defaults_are_valid() {
        let config = IoUringConfig::default();
        assert!(config.sq_entries > 0);
        assert!(config.buffer_size > 0);
    }

    #[test]
    fn try_new_returns_none_or_some() {
        // On CI Linux this may return Some; on macOS/non-Linux it returns None.
        // Either way, this should not panic.
        let config = IoUringConfig::default();
        let _result = IoUringDiskBatch::try_new(&config);
    }

    #[test]
    fn write_without_active_file_returns_error() {
        let config = IoUringConfig::default();
        if let Some(mut batch) = IoUringDiskBatch::try_new(&config) {
            let result = batch.write_data(b"hello");
            assert!(result.is_err());
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn commit_without_active_file_returns_error() {
        let config = IoUringConfig::default();
        if let Some(mut batch) = IoUringDiskBatch::try_new(&config) {
            let result = batch.commit_file(false);
            assert!(result.is_err());
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn bytes_written_with_no_active_file() {
        let config = IoUringConfig::default();
        if let Some(batch) = IoUringDiskBatch::try_new(&config) {
            assert_eq!(batch.bytes_written(), 0);
            assert_eq!(batch.bytes_written_with_pending(), 0);
        }
    }

    #[test]
    fn single_file_write_and_commit() {
        let config = IoUringConfig::default();
        let Some(mut batch) = IoUringDiskBatch::try_new(&config) else {
            return; // io_uring not available
        };

        let dir = tempdir().unwrap();
        let path = dir.path().join("single.bin");
        let file = File::create(&path).unwrap();
        let data = b"hello io_uring disk batch";

        batch.begin_file(file).unwrap();
        batch.write_data(data).unwrap();

        let (_, bytes_written) = batch.commit_file(false).unwrap();
        assert_eq!(bytes_written, data.len() as u64);

        let content = std::fs::read(&path).unwrap();
        assert_eq!(content, data);
    }

    #[test]
    fn multi_file_sequential_writes() {
        let config = IoUringConfig::default();
        let Some(mut batch) = IoUringDiskBatch::try_new(&config) else {
            return;
        };

        let dir = tempdir().unwrap();
        let test_data: Vec<(&str, Vec<u8>)> = vec![
            ("file_a.bin", vec![0xAA; 1024]),
            ("file_b.bin", vec![0xBB; 4096]),
            ("file_c.bin", vec![0xCC; 128]),
        ];

        for (name, data) in &test_data {
            let path = dir.path().join(name);
            let file = File::create(&path).unwrap();
            batch.begin_file(file).unwrap();
            batch.write_data(data).unwrap();
            let (_, written) = batch.commit_file(false).unwrap();
            assert_eq!(written, data.len() as u64);
        }

        // Verify all files.
        for (name, data) in &test_data {
            let content = std::fs::read(dir.path().join(name)).unwrap();
            assert_eq!(content, *data, "content mismatch for {name}");
        }
    }

    #[test]
    fn large_write_exceeds_buffer() {
        let config = IoUringConfig {
            buffer_size: 4096,
            ..IoUringConfig::default()
        };
        let Some(mut batch) = IoUringDiskBatch::try_new(&config) else {
            return;
        };

        let dir = tempdir().unwrap();
        let path = dir.path().join("large.bin");
        let file = File::create(&path).unwrap();
        let data: Vec<u8> = (0..32768).map(|i| (i % 256) as u8).collect();

        batch.begin_file(file).unwrap();
        batch.write_data(&data).unwrap();
        let (_, written) = batch.commit_file(false).unwrap();
        assert_eq!(written, data.len() as u64);

        let content = std::fs::read(&path).unwrap();
        assert_eq!(content, data);
    }

    #[test]
    fn write_trait_implementation() {
        let config = IoUringConfig::default();
        let Some(mut batch) = IoUringDiskBatch::try_new(&config) else {
            return;
        };

        let dir = tempdir().unwrap();
        let path = dir.path().join("write_trait.bin");
        let file = File::create(&path).unwrap();

        batch.begin_file(file).unwrap();

        // Use the Write trait directly.
        let n = Write::write(&mut batch, b"hello ").unwrap();
        assert_eq!(n, 6);
        Write::write_all(&mut batch, b"world").unwrap();
        Write::flush(&mut batch).unwrap();

        let (_, written) = batch.commit_file(false).unwrap();
        assert_eq!(written, 11);

        let content = std::fs::read(&path).unwrap();
        assert_eq!(content, b"hello world");
    }

    #[test]
    fn commit_with_fsync() {
        let config = IoUringConfig::default();
        let Some(mut batch) = IoUringDiskBatch::try_new(&config) else {
            return;
        };

        let dir = tempdir().unwrap();
        let path = dir.path().join("fsync.bin");
        let file = File::create(&path).unwrap();

        batch.begin_file(file).unwrap();
        batch.write_data(b"durable data").unwrap();
        let (_, written) = batch.commit_file(true).unwrap();
        assert_eq!(written, 12);

        let content = std::fs::read(&path).unwrap();
        assert_eq!(content, b"durable data");
    }

    #[test]
    fn begin_file_flushes_previous() {
        let config = IoUringConfig::default();
        let Some(mut batch) = IoUringDiskBatch::try_new(&config) else {
            return;
        };

        let dir = tempdir().unwrap();

        // Write to first file without explicit commit.
        let path1 = dir.path().join("first.bin");
        let file1 = File::create(&path1).unwrap();
        batch.begin_file(file1).unwrap();
        batch.write_data(b"first file data").unwrap();

        // Begin second file - should flush first.
        let path2 = dir.path().join("second.bin");
        let file2 = File::create(&path2).unwrap();
        batch.begin_file(file2).unwrap();

        // First file should have been flushed (but not committed via commit_file).
        let content1 = std::fs::read(&path1).unwrap();
        assert_eq!(content1, b"first file data");

        batch.write_data(b"second").unwrap();
        let (_, written) = batch.commit_file(false).unwrap();
        assert_eq!(written, 6);
    }

    #[test]
    fn drop_flushes_pending_data() {
        let config = IoUringConfig::default();
        let dir = tempdir().unwrap();
        let path = dir.path().join("drop_flush.bin");

        {
            let Some(mut batch) = IoUringDiskBatch::try_new(&config) else {
                return;
            };
            let file = File::create(&path).unwrap();
            batch.begin_file(file).unwrap();
            batch.write_data(b"drop test").unwrap();
            // Drop without explicit flush/commit.
        }

        let content = std::fs::read(&path).unwrap();
        assert_eq!(content, b"drop test");
    }
}
