//! io_uring-based file writer with buffered batched writes.
//!
//! When registered buffers are available, flushes use `IORING_OP_WRITE_FIXED`
//! which avoids per-SQE `get_user_pages()` overhead. Falls back to regular
//! `IORING_OP_WRITE` when registration is unavailable or all slots are busy.

use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::os::unix::io::AsRawFd;
use std::path::Path;

use io_uring::{IoUring as RawIoUring, opcode};

use super::batching::{maybe_fixed_file, sqe_fd, submit_write_batch, try_register_fd};
use super::config::IoUringConfig;
use super::registered_buffers::{
    RegisteredBufferGroup, RegisteredBufferSlotInfo, RegisteredBufferStatus,
    submit_write_fixed_batch,
};
use crate::traits::FileWriter;

/// A file writer using io_uring for async I/O.
///
/// Incoming writes are buffered internally. On `flush()` (or when the buffer
/// fills), the buffered data is submitted as a batch of write SQEs -- up to
/// `sq_entries` concurrent writes per `submit_and_wait` call.
///
/// When [`RegisteredBufferGroup`] is available, the writer uses
/// `IORING_OP_WRITE_FIXED` for flushes, eliminating kernel-side page pinning
/// overhead on each operation.
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
    /// Optional registered buffer group for `WRITE_FIXED` operations.
    registered_buffers: Option<RegisteredBufferGroup>,
    /// Provenance of `registered_buffers`: distinguishes "disabled by config"
    /// from "kernel rejected registration" so operators can diagnose why a
    /// transfer is using the unfixed `IORING_OP_WRITE` fallback.
    registered_buffer_status: RegisteredBufferStatus,
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
        let (registered_buffers, registered_buffer_status) =
            RegisteredBufferGroup::try_new_with_status(
                &ring,
                config.buffer_size,
                config.registered_buffer_count,
                config.register_buffers,
            );

        Ok(Self {
            ring,
            file,
            bytes_written: 0,
            buffer: vec![0u8; config.buffer_size],
            buffer_pos: 0,
            buffer_size: config.buffer_size,
            sq_entries: config.sq_entries,
            fixed_fd_slot,
            registered_buffers,
            registered_buffer_status,
        })
    }

    /// Wraps an existing file handle for writing with io_uring.
    pub fn from_file(file: File, config: &IoUringConfig) -> io::Result<Self> {
        let ring = config.build_ring()?;
        let fixed_fd_slot = try_register_fd(&ring, file.as_raw_fd(), config.register_files);
        let (registered_buffers, registered_buffer_status) =
            RegisteredBufferGroup::try_new_with_status(
                &ring,
                config.buffer_size,
                config.registered_buffer_count,
                config.register_buffers,
            );

        Ok(Self {
            ring,
            file,
            bytes_written: 0,
            buffer: vec![0u8; config.buffer_size],
            buffer_pos: 0,
            buffer_size: config.buffer_size,
            sq_entries: config.sq_entries,
            fixed_fd_slot,
            registered_buffers,
            registered_buffer_status,
        })
    }

    /// Wraps an existing file handle, io_uring ring, and fixed-fd slot.
    ///
    /// Used by [`super::writer_from_file`] which builds the ring separately
    /// so it can fall back to standard I/O without consuming the `File`.
    ///
    /// Honors `register_buffers`: when `false`, skips
    /// [`RegisteredBufferGroup`] construction entirely so flushes use the
    /// regular `IORING_OP_WRITE` path. When `true`, allocates
    /// `registered_buffer_count` fixed buffers (matching the pattern in
    /// [`IoUringConfig`](super::IoUringConfig)).
    pub(super) fn with_ring(
        file: File,
        ring: RawIoUring,
        buffer_capacity: usize,
        sq_entries: u32,
        fixed_fd_slot: i32,
        register_buffers: bool,
        registered_buffer_count: usize,
    ) -> Self {
        let (registered_buffers, registered_buffer_status) =
            RegisteredBufferGroup::try_new_with_status(
                &ring,
                buffer_capacity,
                registered_buffer_count,
                register_buffers,
            );

        Self {
            ring,
            file,
            bytes_written: 0,
            buffer: vec![0u8; buffer_capacity],
            buffer_pos: 0,
            buffer_size: buffer_capacity,
            sq_entries,
            fixed_fd_slot,
            registered_buffers,
            registered_buffer_status,
        }
    }

    /// Returns the count of currently-registered fixed buffers, or `None` if
    /// buffer registration is not active on this writer.
    ///
    /// Returns `None` when the caller disabled `register_buffers` or when
    /// kernel registration failed and the writer is using the regular
    /// `IORING_OP_WRITE` fallback path. To tell the two cases apart, call
    /// [`registered_buffer_status`](Self::registered_buffer_status).
    #[must_use]
    pub fn registered_buffer_count(&self) -> Option<usize> {
        self.registered_buffers.as_ref().map(|g| g.count())
    }

    /// Returns the provenance of fixed-buffer registration on this writer.
    ///
    /// Use this to distinguish the two `registered_buffer_count() == None`
    /// cases: caller disabled registration vs the kernel rejected it.
    #[must_use]
    pub fn registered_buffer_status(&self) -> &RegisteredBufferStatus {
        &self.registered_buffer_status
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
        let (registered_buffers, registered_buffer_status) =
            RegisteredBufferGroup::try_new_with_status(
                &ring,
                config.buffer_size,
                config.registered_buffer_count,
                config.register_buffers,
            );

        Ok(Self {
            ring,
            file,
            bytes_written: 0,
            buffer: vec![0u8; config.buffer_size],
            buffer_pos: 0,
            buffer_size: config.buffer_size,
            sq_entries: config.sq_entries,
            fixed_fd_slot,
            registered_buffers,
            registered_buffer_status,
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

        // SAFETY: `entry` references `buf` and the file fd; both outlive
        // `submit_and_wait` below, so the kernel can read from the buffer
        // before we observe completion.
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
    /// Uses `WRITE_FIXED` when registered buffers are available, falling back
    /// to regular `IORING_OP_WRITE` otherwise. Splits `data` into chunks and
    /// submits up to `sq_entries` writes per `submit_and_wait` call.
    pub fn write_all_batched(&mut self, data: &[u8], offset: u64) -> io::Result<()> {
        let fd = sqe_fd(self.file.as_raw_fd(), self.fixed_fd_slot);

        if let Some(ref reg) = self.registered_buffers {
            let slot_count = reg.available().min(self.sq_entries as usize);
            if slot_count > 0 {
                let mut slots: Vec<_> = (0..slot_count).filter_map(|_| reg.checkout()).collect();
                if !slots.is_empty() {
                    let slot_infos: Vec<RegisteredBufferSlotInfo> = slots
                        .iter_mut()
                        .map(|s| RegisteredBufferSlotInfo {
                            ptr: s.as_mut_ptr(),
                            buf_index: s.buf_index(),
                            buffer_size: s.buffer_size(),
                        })
                        .collect();

                    let written = submit_write_fixed_batch(
                        &mut self.ring,
                        fd,
                        data,
                        offset,
                        &slot_infos,
                        self.fixed_fd_slot,
                    )?;
                    if written != data.len() {
                        return Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "batched write_fixed incomplete",
                        ));
                    }
                    return Ok(());
                }
            }
        }

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
    /// When registered buffers are available, uses `IORING_OP_WRITE_FIXED` to
    /// avoid per-SQE page pinning. Falls back to regular `IORING_OP_WRITE` when
    /// registration is unavailable or all slots are busy.
    fn flush_buffer(&mut self) -> io::Result<()> {
        if self.buffer_pos == 0 {
            return Ok(());
        }

        let fd = sqe_fd(self.file.as_raw_fd(), self.fixed_fd_slot);
        let len = self.buffer_pos;
        let offset = self.bytes_written;

        if let Some(ref reg) = self.registered_buffers {
            let slot_count = reg.available().min(self.sq_entries as usize);
            if slot_count > 0 {
                let mut slots: Vec<_> = (0..slot_count).filter_map(|_| reg.checkout()).collect();
                if !slots.is_empty() {
                    let slot_infos: Vec<RegisteredBufferSlotInfo> = slots
                        .iter_mut()
                        .map(|s| RegisteredBufferSlotInfo {
                            ptr: s.as_mut_ptr(),
                            buf_index: s.buf_index(),
                            buffer_size: s.buffer_size(),
                        })
                        .collect();

                    let written = submit_write_fixed_batch(
                        &mut self.ring,
                        fd,
                        &self.buffer[..len],
                        offset,
                        &slot_infos,
                        self.fixed_fd_slot,
                    )?;
                    self.bytes_written += written as u64;
                    self.buffer_pos = 0;
                    return Ok(());
                }
            }
        }

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

        let fd = sqe_fd(self.file.as_raw_fd(), self.fixed_fd_slot);

        let entry = opcode::Fsync::new(fd).build().user_data(0);
        let fsync_op = maybe_fixed_file(entry, self.fixed_fd_slot);

        // SAFETY: `Fsync` carries only the file fd which remains valid for
        // the duration of `submit_and_wait`; no user-space buffer is shared
        // with the kernel.
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
        let _ = self.flush_buffer();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Builds a writer via `with_ring` for testing. Returns `None` when the
    /// kernel rejects `io_uring_setup(2)` (e.g., container, seccomp, or non
    /// 5.6+ kernel) so the test skips cleanly.
    fn make_writer(
        register_buffers: bool,
        registered_buffer_count: usize,
    ) -> Option<IoUringWriter> {
        let dir = tempdir().ok()?;
        let file = File::create(dir.path().join("out.bin")).ok()?;
        let ring = RawIoUring::new(4).ok()?;
        // Keep `dir` alive for the duration of the writer by leaking it; the
        // OS reclaims the temp file at process exit. Tests are short-lived
        // and this avoids ordering the drop with the writer.
        std::mem::forget(dir);
        Some(IoUringWriter::with_ring(
            file,
            ring,
            4096,
            4,
            -1,
            register_buffers,
            registered_buffer_count,
        ))
    }

    #[test]
    fn with_ring_skips_registration_when_disabled() {
        let writer = match make_writer(false, 8) {
            Some(w) => w,
            None => return,
        };
        assert_eq!(
            writer.registered_buffer_count(),
            None,
            "register_buffers=false must skip RegisteredBufferGroup construction"
        );
        assert_eq!(
            writer.registered_buffer_status(),
            &RegisteredBufferStatus::Disabled,
            "status must be Disabled when register_buffers=false, not RegistrationFailed"
        );
    }

    #[test]
    fn registered_buffer_status_failed_when_kernel_rejects() {
        // Force registration failure by asking for more buffers than the
        // wrapper allows. This is rejected before reaching the kernel, so the
        // test is portable to environments without io_uring permissions.
        let writer = match make_writer(
            true,
            super::super::registered_buffers::MAX_REGISTERED_BUFFERS + 1,
        ) {
            Some(w) => w,
            None => return,
        };
        assert_eq!(writer.registered_buffer_count(), None);
        assert!(
            writer.registered_buffer_status().is_registration_failed(),
            "kernel-side rejection must surface as RegistrationFailed, not Disabled"
        );
    }

    #[test]
    fn with_ring_respects_configured_count() {
        let writer = match make_writer(true, 16) {
            Some(w) => w,
            None => return,
        };
        // Buffer registration may fail silently inside RegisteredBufferGroup
        // (e.g., kernel without IORING_REGISTER_BUFFERS) - skip in that case.
        let count = match writer.registered_buffer_count() {
            Some(c) => c,
            None => return,
        };
        assert_eq!(
            count, 16,
            "with_ring must honor registered_buffer_count, not hard-code 8"
        );
    }

    #[test]
    fn with_ring_uses_default_count_when_unset() {
        // `IoUringConfig::default()` sets `registered_buffer_count = 8`.
        // Verify that wiring the config default through `with_ring` produces
        // a writer with exactly 8 registered buffers.
        let config = IoUringConfig::default();
        assert!(config.register_buffers, "default must enable registration");
        assert_eq!(config.registered_buffer_count, 8, "default count must be 8");

        let writer = match make_writer(config.register_buffers, config.registered_buffer_count) {
            Some(w) => w,
            None => return,
        };
        let count = match writer.registered_buffer_count() {
            Some(c) => c,
            None => return,
        };
        assert_eq!(count, 8, "default config must produce 8 registered buffers");
    }
}
