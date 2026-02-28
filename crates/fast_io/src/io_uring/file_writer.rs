//! io_uring-based file writer with buffered batched writes.

use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::os::unix::io::AsRawFd;
use std::path::Path;

use io_uring::{IoUring as RawIoUring, opcode, types};

use super::batching::{maybe_fixed_file, sqe_fd, submit_write_batch, try_register_fd};
use super::config::IoUringConfig;
use crate::traits::FileWriter;

/// A file writer using io_uring for async I/O.
///
/// Incoming writes are buffered internally. On `flush()` (or when the buffer
/// fills), the buffered data is submitted as a batch of write SQEs -- up to
/// `sq_entries` concurrent writes per `submit_and_wait` call.
pub struct IoUringWriter {
    ring: RawIoUring,
    file: File,
    bytes_written: u64,
    buffer: Vec<u8>,
    buffer_pos: usize,
    buffer_size: usize,
    sq_entries: u32,
    /// Fixed-file slot index, or `NO_FIXED_FD` when not registered.
    fixed_fd_slot: i32,
}

impl IoUringWriter {
    /// Creates a file for writing with io_uring.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The file cannot be created
    /// - io_uring initialization fails
    pub fn create<P: AsRef<Path>>(path: P, config: &IoUringConfig) -> io::Result<Self> {
        let file = File::create(path)?;
        let ring = config.build_ring()?;
        let fixed_fd_slot = try_register_fd(&ring, file.as_raw_fd(), config.register_files);

        Ok(Self {
            ring,
            file,
            bytes_written: 0,
            buffer: vec![0u8; config.buffer_size],
            buffer_pos: 0,
            buffer_size: config.buffer_size,
            sq_entries: config.sq_entries,
            fixed_fd_slot,
        })
    }

    /// Wraps an existing file handle for writing with io_uring.
    pub fn from_file(file: File, config: &IoUringConfig) -> io::Result<Self> {
        let ring = config.build_ring()?;
        let fixed_fd_slot = try_register_fd(&ring, file.as_raw_fd(), config.register_files);

        Ok(Self {
            ring,
            file,
            bytes_written: 0,
            buffer: vec![0u8; config.buffer_size],
            buffer_pos: 0,
            buffer_size: config.buffer_size,
            sq_entries: config.sq_entries,
            fixed_fd_slot,
        })
    }

    /// Wraps an existing file handle, io_uring ring, and fixed-fd slot.
    ///
    /// Used by [`super::writer_from_file`] which builds the ring separately
    /// so it can fall back to standard I/O without consuming the `File`.
    pub(super) fn with_ring(
        file: File,
        ring: RawIoUring,
        buffer_capacity: usize,
        sq_entries: u32,
        fixed_fd_slot: i32,
    ) -> Self {
        Self {
            ring,
            file,
            bytes_written: 0,
            buffer: vec![0u8; buffer_capacity],
            buffer_pos: 0,
            buffer_size: buffer_capacity,
            sq_entries,
            fixed_fd_slot,
        }
    }

    /// Creates a file with preallocated space.
    pub fn create_with_size<P: AsRef<Path>>(
        path: P,
        size: u64,
        config: &IoUringConfig,
    ) -> io::Result<Self> {
        let file = File::create(path)?;
        file.set_len(size)?;
        let ring = config.build_ring()?;
        let fixed_fd_slot = try_register_fd(&ring, file.as_raw_fd(), config.register_files);

        Ok(Self {
            ring,
            file,
            bytes_written: 0,
            buffer: vec![0u8; config.buffer_size],
            buffer_pos: 0,
            buffer_size: config.buffer_size,
            sq_entries: config.sq_entries,
            fixed_fd_slot,
        })
    }

    /// Writes data at the specified offset without advancing the internal position.
    ///
    /// Submits a single SQE and waits for completion. For bulk writes, prefer
    /// buffered `write()` + `flush()` which batches SQEs automatically.
    pub fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let fd = sqe_fd(self.file.as_raw_fd(), self.fixed_fd_slot);

        let entry = opcode::Write::new(fd, buf.as_ptr(), buf.len() as u32)
            .offset(offset)
            .build()
            .user_data(0);
        let entry = maybe_fixed_file(entry, self.fixed_fd_slot);

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
            .ok_or_else(|| io::Error::other("no completion"))?;

        let result = cqe.result();
        if result < 0 {
            return Err(io::Error::from_raw_os_error(-result));
        }

        Ok(result as usize)
    }

    /// Writes all of `data` starting at `offset` using batched SQEs.
    ///
    /// Splits `data` into `buffer_size` chunks and submits up to `sq_entries`
    /// writes per `submit_and_wait` call, handling short writes via resubmission.
    pub fn write_all_batched(&mut self, data: &[u8], offset: u64) -> io::Result<()> {
        let fd = sqe_fd(self.file.as_raw_fd(), self.fixed_fd_slot);
        let written = submit_write_batch(
            &mut self.ring,
            fd,
            data,
            offset,
            self.buffer_size,
            self.sq_entries as usize,
            self.fixed_fd_slot,
        )?;
        if written != data.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "batched write incomplete",
            ));
        }
        Ok(())
    }

    /// Flushes the internal buffer to disk using batched writes.
    ///
    /// Passes the buffer slice directly to the batched writer without
    /// allocating a copy — the SQEs reference the buffer in place and
    /// the kernel completes all writes before we reset `buffer_pos`.
    fn flush_buffer(&mut self) -> io::Result<()> {
        if self.buffer_pos == 0 {
            return Ok(());
        }

        let fd = sqe_fd(self.file.as_raw_fd(), self.fixed_fd_slot);
        let len = self.buffer_pos;
        let offset = self.bytes_written;

        // Submit directly from the internal buffer — no allocation.
        // Safety: the buffer is not modified until submit_write_batch returns,
        // and the kernel only reads from these pointers during submit_and_wait.
        let written = submit_write_batch(
            &mut self.ring,
            fd,
            &self.buffer[..len],
            offset,
            self.buffer_size,
            self.sq_entries as usize,
            self.fixed_fd_slot,
        )?;
        self.bytes_written += written as u64;
        self.buffer_pos = 0;
        Ok(())
    }
}

impl Write for IoUringWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        // If data fits in buffer, just copy it
        if self.buffer_pos + buf.len() <= self.buffer_size {
            self.buffer[self.buffer_pos..self.buffer_pos + buf.len()].copy_from_slice(buf);
            self.buffer_pos += buf.len();
            return Ok(buf.len());
        }

        // Flush current buffer
        self.flush_buffer()?;

        // If data is larger than buffer, write directly using batched path
        if buf.len() >= self.buffer_size {
            self.write_all_batched(buf, self.bytes_written)?;
            self.bytes_written += buf.len() as u64;
            return Ok(buf.len());
        }

        // Otherwise, buffer the data
        self.buffer[..buf.len()].copy_from_slice(buf);
        self.buffer_pos = buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_buffer()
    }
}

impl FileWriter for IoUringWriter {
    fn bytes_written(&self) -> u64 {
        self.bytes_written + self.buffer_pos as u64
    }

    fn sync(&mut self) -> io::Result<()> {
        self.flush_buffer()?;

        let fd = sqe_fd(self.file.as_raw_fd(), self.fixed_fd_slot);

        let entry = opcode::Fsync::new(fd).build().user_data(0);
        let fsync_op = maybe_fixed_file(entry, self.fixed_fd_slot);

        unsafe {
            self.ring
                .submission()
                .push(&fsync_op)
                .map_err(|_| io::Error::other("submission queue full"))?;
        }

        self.ring.submit_and_wait(1)?;

        let cqe = self
            .ring
            .completion()
            .next()
            .ok_or_else(|| io::Error::other("no completion"))?;

        let result = cqe.result();
        if result < 0 {
            return Err(io::Error::from_raw_os_error(-result));
        }

        Ok(())
    }

    fn preallocate(&mut self, size: u64) -> io::Result<()> {
        self.file.set_len(size)
    }
}

impl Seek for IoUringWriter {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.flush_buffer()?;
        let new_pos = self.file.seek(pos)?;
        self.bytes_written = new_pos;
        Ok(new_pos)
    }
}

impl Drop for IoUringWriter {
    fn drop(&mut self) {
        // Best-effort flush on drop
        let _ = self.flush_buffer();
    }
}
