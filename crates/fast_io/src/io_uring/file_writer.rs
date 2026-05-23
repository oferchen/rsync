//! io_uring-based file writer with buffered batched writes.
//!
//! Submissions go through the per-thread io_uring ring established by IUR-3.a
//! ([`super::per_thread_ring::with_ring`]). The writer no longer owns a
//! `RawIoUring` instance; one ring per OS thread is shared across every
//! [`IoUringWriter`] that thread holds, dissolving the cross-thread
//! contention the shared-ring layout imposed on rayon-parallel writers (IUR-2
//! design doc section 1.1).
//!
//! Per-ring kernel state - fixed-file registration and registered buffers -
//! is tied to a specific ring fd and does not survive the move to a
//! thread-shared ring. Both are recorded as unavailable on every writer
//! constructed through this module; IUR-3.e re-introduces them on the
//! per-thread topology via the bgid-lease primitive.

use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::os::unix::io::AsRawFd;
use std::path::Path;

use io_uring::opcode;

use super::batching::{NO_FIXED_FD, maybe_fixed_file, sqe_fd, submit_write_batch};
use super::config::IoUringConfig;
use super::per_thread_ring::with_ring;
use super::registered_buffers::RegisteredBufferStatus;
use crate::traits::FileWriter;

/// A file writer using io_uring for async I/O.
///
/// Incoming writes are buffered internally. On `flush()` (or when the buffer
/// fills), the buffered data is submitted as a batch of write SQEs -- up to
/// `sq_entries` concurrent writes per `submit_and_wait` call.
///
/// Submissions are issued against the calling thread's per-thread ring (see
/// [`super::per_thread_ring`]). Fixed-file registration and registered-buffer
/// fast paths are disabled on this writer until IUR-3.e wires the per-thread
/// bgid lease; flushes always use the regular `IORING_OP_WRITE` opcode.
pub struct IoUringWriter {
    file: File,
    bytes_written: u64,
    buffer: Vec<u8>,
    buffer_pos: usize,
    buffer_size: usize,
    sq_entries: u32,
}

impl IoUringWriter {
    /// Creates a file for writing with io_uring.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The file cannot be created
    /// - io_uring initialization fails on the calling thread
    pub fn create<P: AsRef<Path>>(path: P, config: &IoUringConfig) -> io::Result<Self> {
        let file = File::create(path)?;
        Self::new_from_file(file, config)
    }

    /// Wraps an existing file handle for writing with io_uring.
    pub fn from_file(file: File, config: &IoUringConfig) -> io::Result<Self> {
        Self::new_from_file(file, config)
    }

    /// Wraps an existing file handle for writing with the per-thread ring.
    ///
    /// Used by [`super::writer_from_file`] which previously built a ring
    /// per call so it could fall back to standard I/O without consuming the
    /// `File`. With the per-thread topology the writer no longer owns the
    /// ring; this constructor is kept for API parity but is now a thin
    /// wrapper over [`Self::new_from_file`] that ignores the
    /// `register_buffers` / `registered_buffer_count` knobs (IUR-3.e
    /// re-introduces buffer registration on the per-thread ring).
    pub(super) fn with_ring(
        file: File,
        buffer_capacity: usize,
        sq_entries: u32,
        _fixed_fd_slot: i32,
        _register_buffers: bool,
        _registered_buffer_count: usize,
    ) -> Self {
        Self::new_with_capacity(file, buffer_capacity, sq_entries)
    }

    /// Returns the count of currently-registered fixed buffers, or `None` if
    /// buffer registration is not active on this writer.
    ///
    /// Always returns `None` on the per-thread topology: the per-thread ring
    /// is shared across every writer on the same thread, so per-writer
    /// buffer registration cannot be expressed safely. IUR-3.e re-introduces
    /// shared per-thread buffer registration via the bgid lease; call
    /// [`registered_buffer_status`](Self::registered_buffer_status) to tell
    /// the post-migration state apart from a kernel-side rejection.
    #[must_use]
    pub fn registered_buffer_count(&self) -> Option<usize> {
        None
    }

    /// Returns the provenance of fixed-buffer registration on this writer.
    ///
    /// Always returns [`RegisteredBufferStatus::Disabled`] on the per-thread
    /// topology; see [`Self::registered_buffer_count`].
    #[must_use]
    pub fn registered_buffer_status(&self) -> &RegisteredBufferStatus {
        &RegisteredBufferStatus::Disabled
    }

    /// Creates a file with preallocated space.
    pub fn create_with_size<P: AsRef<Path>>(
        path: P,
        size: u64,
        config: &IoUringConfig,
    ) -> io::Result<Self> {
        let file = File::create(path)?;
        file.set_len(size)?;
        Self::new_from_file(file, config)
    }

    /// Writes data at the specified offset without advancing the internal position.
    ///
    /// Submits a single SQE on the per-thread ring and waits for completion.
    /// For bulk writes, prefer buffered `write()` + `flush()` which batches
    /// SQEs automatically.
    pub fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let raw_fd = self.file.as_raw_fd();
        let fd = sqe_fd(raw_fd, NO_FIXED_FD);

        with_ring(|ring| {
            let entry = opcode::Write::new(fd, buf.as_ptr(), buf.len() as u32)
                .offset(offset)
                .build()
                .user_data(0);
            let entry = maybe_fixed_file(entry, NO_FIXED_FD);

            // SAFETY: `entry` references `buf` and the file fd; both outlive
            // `submit_and_wait` below, so the kernel can read from the buffer
            // before we observe completion.
            unsafe {
                ring.submission()
                    .push(&entry)
                    .map_err(|_| io::Error::other("submission queue full"))?;
            }

            ring.submit_and_wait(1)?;

            let cqe = ring
                .completion()
                .next()
                .ok_or_else(|| io::Error::other("no completion"))?;

            let result = cqe.result();
            if result < 0 {
                return Err(io::Error::from_raw_os_error(-result));
            }

            Ok(result as usize)
        })
    }

    /// Writes all of `data` starting at `offset` using batched SQEs.
    ///
    /// Submits up to `sq_entries` writes per `submit_and_wait` call on the
    /// per-thread ring.
    pub fn write_all_batched(&mut self, data: &[u8], offset: u64) -> io::Result<()> {
        let raw_fd = self.file.as_raw_fd();
        let fd = sqe_fd(raw_fd, NO_FIXED_FD);
        let buffer_size = self.buffer_size;
        let sq_entries = self.sq_entries as usize;

        let written = with_ring(|ring| {
            submit_write_batch(ring, fd, data, offset, buffer_size, sq_entries, NO_FIXED_FD)
        })?;
        if written != data.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "batched write incomplete",
            ));
        }
        Ok(())
    }

    /// Builds a writer from a file handle using the configured buffer size.
    fn new_from_file(file: File, config: &IoUringConfig) -> io::Result<Self> {
        // Probe the per-thread ring once at construction so callers observe
        // io_uring unavailability synchronously, matching the old behaviour
        // where `config.build_ring()?` surfaced setup errors here.
        with_ring(|_| Ok(()))?;
        Ok(Self::new_with_capacity(
            file,
            config.buffer_size,
            config.sq_entries,
        ))
    }

    /// Constructs the writer state from a file, buffer capacity, and SQ depth.
    fn new_with_capacity(file: File, buffer_capacity: usize, sq_entries: u32) -> Self {
        Self {
            file,
            bytes_written: 0,
            buffer: vec![0u8; buffer_capacity],
            buffer_pos: 0,
            buffer_size: buffer_capacity,
            sq_entries,
        }
    }

    /// Flushes the internal buffer to disk using batched writes.
    ///
    /// Submits the buffered region as a batch of `IORING_OP_WRITE` SQEs on
    /// the per-thread ring.
    fn flush_buffer(&mut self) -> io::Result<()> {
        if self.buffer_pos == 0 {
            return Ok(());
        }

        let raw_fd = self.file.as_raw_fd();
        let fd = sqe_fd(raw_fd, NO_FIXED_FD);
        let len = self.buffer_pos;
        let offset = self.bytes_written;
        let buffer_size = self.buffer_size;
        let sq_entries = self.sq_entries as usize;

        let written = with_ring(|ring| {
            submit_write_batch(
                ring,
                fd,
                &self.buffer[..len],
                offset,
                buffer_size,
                sq_entries,
                NO_FIXED_FD,
            )
        })?;
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

        if self.buffer_pos + buf.len() <= self.buffer_size {
            self.buffer[self.buffer_pos..self.buffer_pos + buf.len()].copy_from_slice(buf);
            self.buffer_pos += buf.len();
            return Ok(buf.len());
        }

        self.flush_buffer()?;

        // Bypass internal buffer when data is at least one full chunk: a single
        // batched submission is cheaper than copy-then-flush.
        if buf.len() >= self.buffer_size {
            self.write_all_batched(buf, self.bytes_written)?;
            self.bytes_written += buf.len() as u64;
            return Ok(buf.len());
        }

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

        let raw_fd = self.file.as_raw_fd();
        let fd = sqe_fd(raw_fd, NO_FIXED_FD);

        with_ring(|ring| {
            let entry = opcode::Fsync::new(fd).build().user_data(0);
            let fsync_op = maybe_fixed_file(entry, NO_FIXED_FD);

            // SAFETY: `Fsync` carries only the file fd which remains valid for
            // the duration of `submit_and_wait`; no user-space buffer is shared
            // with the kernel.
            unsafe {
                ring.submission()
                    .push(&fsync_op)
                    .map_err(|_| io::Error::other("submission queue full"))?;
            }

            ring.submit_and_wait(1)?;

            let cqe = ring
                .completion()
                .next()
                .ok_or_else(|| io::Error::other("no completion"))?;

            let result = cqe.result();
            if result < 0 {
                return Err(io::Error::from_raw_os_error(-result));
            }

            Ok(())
        })
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
        let _ = self.flush_buffer();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Builds a writer for testing via the per-thread ring. Returns `None`
    /// when the kernel rejects `io_uring_setup(2)` (e.g., container,
    /// seccomp, or non-5.6+ kernel) so the test skips cleanly.
    fn make_writer(
        register_buffers: bool,
        registered_buffer_count: usize,
    ) -> Option<IoUringWriter> {
        let dir = tempdir().ok()?;
        let file = File::create(dir.path().join("out.bin")).ok()?;
        // Probe the per-thread ring; on hosts without io_uring we skip the
        // test rather than constructing a writer that would later fail.
        with_ring(|_| Ok(())).ok()?;
        // Keep `dir` alive for the duration of the writer by leaking it; the
        // OS reclaims the temp file at process exit. Tests are short-lived
        // and this avoids ordering the drop with the writer.
        std::mem::forget(dir);
        Some(IoUringWriter::with_ring(
            file,
            4096,
            4,
            -1,
            register_buffers,
            registered_buffer_count,
        ))
    }

    #[test]
    fn registered_buffers_always_disabled_on_per_thread_ring() {
        let writer = match make_writer(false, 8) {
            Some(w) => w,
            None => return,
        };
        assert_eq!(
            writer.registered_buffer_count(),
            None,
            "per-thread ring writers do not own per-writer registered buffers"
        );
        assert_eq!(
            writer.registered_buffer_status(),
            &RegisteredBufferStatus::Disabled,
            "status must report Disabled on the per-thread topology until IUR-3.e wires bgid lease"
        );
    }

    #[test]
    fn registered_buffer_count_ignored_on_per_thread_ring() {
        // Passing a high count must not crash and must not surface as
        // RegistrationFailed: the per-thread topology simply ignores the
        // per-writer registration knobs.
        let writer = match make_writer(
            true,
            super::super::registered_buffers::MAX_REGISTERED_BUFFERS + 1,
        ) {
            Some(w) => w,
            None => return,
        };
        assert_eq!(writer.registered_buffer_count(), None);
        assert_eq!(
            writer.registered_buffer_status(),
            &RegisteredBufferStatus::Disabled,
            "per-thread topology never reports RegistrationFailed for per-writer requests"
        );
    }

    #[test]
    fn with_ring_returns_writer_when_io_uring_available() {
        let writer = match make_writer(true, 16) {
            Some(w) => w,
            None => return,
        };
        // The writer is usable; bytes_written starts at zero.
        assert_eq!(writer.bytes_written, 0);
    }

    #[test]
    fn default_config_constructs_writer() {
        // `IoUringConfig::default()` sets `registered_buffer_count = 8`.
        // The per-thread topology ignores it; the writer must still
        // construct and report Disabled.
        let config = IoUringConfig::default();
        let writer = match make_writer(config.register_buffers, config.registered_buffer_count) {
            Some(w) => w,
            None => return,
        };
        assert_eq!(writer.registered_buffer_count(), None);
    }
}
