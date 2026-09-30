//! io_uring-based file writer with buffered batched writes.
//!
//! Submissions go through the per-thread io_uring ring established by IUR-3.a
//! ([`super::per_thread_ring::with_ring`]). The writer no longer owns a
//! `RawIoUring` instance; one ring per OS thread is shared across every
//! [`IoUringWriter`] that thread holds, dissolving the cross-thread
//! contention the shared-ring layout imposed on rayon-parallel writers (IUR-2
//! design doc section 1.1).
//!
//! Registered buffers live on the per-thread ring too: one
//! [`RegisteredBufferGroup`](super::registered_buffers::RegisteredBufferGroup)
//! per thread, shared by every writer and reader on it. A batch large enough
//! to use the ring stages its chunks in those buffers and submits
//! `IORING_OP_WRITE_FIXED`; when registration is disabled or the kernel
//! rejected it, the same batch uses plain `IORING_OP_WRITE`. Fixed-file
//! registration stays off on the thread-shared ring.

use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::os::unix::io::AsRawFd;
use std::path::Path;

use io_uring::opcode;

use super::batching::{
    NO_FIXED_FD, batch_bypasses_ring, maybe_fixed_file, sqe_fd, submit_write_batch,
};
use super::config::IoUringConfig;
use super::per_thread_ring::{FixedBuffers, with_ring, with_ring_and_buffers};
use super::registered_buffers::{RegisteredBufferStatus, checkout_all, submit_write_fixed_batch};
use crate::traits::FileWriter;

/// A file writer using io_uring for async I/O.
///
/// Incoming writes are buffered internally. On `flush()` (or when the buffer
/// fills), the buffered data is submitted as a batch of write SQEs -- up to
/// `sq_entries` of them, which the kernel may run concurrently *within* that
/// one `submit_and_wait` call. Batches do not overlap: the ring is drained to
/// empty before the next one is built, so a batch of a single chunk gains no
/// concurrency at all. Those go to a positional write instead; see
/// `batching::MIN_RING_BATCH_CHUNKS`.
///
/// Submissions are issued against the calling thread's per-thread ring (see
/// [`super::per_thread_ring`]). Ring batches use `IORING_OP_WRITE_FIXED`
/// through the thread's registered buffers when
/// [`IoUringConfig::register_buffers`] is set and the kernel accepted the
/// registration, and plain `IORING_OP_WRITE` otherwise.
pub struct IoUringWriter {
    file: File,
    bytes_written: u64,
    buffer: Vec<u8>,
    buffer_pos: usize,
    buffer_size: usize,
    sq_entries: u32,
    fixed: FixedBuffers,
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
    /// Used by [`super::writer_from_file`], which probes the per-thread ring
    /// first so it can fall back to standard I/O without consuming the
    /// `File`. `buffer_capacity` sizes the writer's staging buffer; the SQ
    /// depth and the fixed-buffer knobs come from `config`.
    pub(super) fn with_ring(file: File, buffer_capacity: usize, config: &IoUringConfig) -> Self {
        Self::new_with_capacity(file, buffer_capacity, config)
    }

    /// Returns the number of fixed buffers registered on the per-thread ring
    /// this writer submits to, or `None` if registration is not active.
    #[must_use]
    pub fn registered_buffer_count(&self) -> Option<usize> {
        self.fixed.count()
    }

    /// Returns the provenance of fixed-buffer registration on this writer:
    /// `Enabled` when ring batches use `IORING_OP_WRITE_FIXED`, `Disabled`
    /// when the config opted out, `RegistrationFailed` when the kernel
    /// rejected the registration and batches fell back to `IORING_OP_WRITE`.
    #[must_use]
    pub fn registered_buffer_status(&self) -> &RegisteredBufferStatus {
        self.fixed.status()
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
        let written = self.submit_batch(data, offset)?;
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
        Ok(Self::new_with_capacity(file, config.buffer_size, config))
    }

    /// Constructs the writer state and registers the calling thread's fixed
    /// buffers when `config` asks for them.
    fn new_with_capacity(file: File, buffer_capacity: usize, config: &IoUringConfig) -> Self {
        Self {
            file,
            bytes_written: 0,
            buffer: vec![0u8; buffer_capacity],
            buffer_pos: 0,
            buffer_size: buffer_capacity,
            sq_entries: config.sq_entries,
            fixed: FixedBuffers::register(
                config.register_buffers,
                config.buffer_size,
                config.registered_buffer_count,
            ),
        }
    }

    /// Writes all of `data` at `offset` on the per-thread ring.
    ///
    /// A batch that fills at least `MIN_RING_BATCH_CHUNKS` registered
    /// buffers goes out as `IORING_OP_WRITE_FIXED`; everything else takes
    /// [`submit_write_batch`], which also owns the small-batch `pwrite(2)`
    /// bypass.
    fn submit_batch(&self, data: &[u8], offset: u64) -> io::Result<usize> {
        let buffer_size = self.buffer_size;
        let sq_entries = self.sq_entries as usize;
        with_ring_and_buffers(self.fixed.request(), |ring, group| {
            if let Some(group) =
                group.filter(|g| !batch_bypasses_ring(data.len(), g.buffer_size(), g.count()))
            {
                let (_slots, infos) = checkout_all(group);
                if !infos.is_empty() {
                    let fd = sqe_fd(self.file.as_raw_fd(), NO_FIXED_FD);
                    return submit_write_fixed_batch(ring, fd, data, offset, &infos, NO_FIXED_FD);
                }
            }
            submit_write_batch(
                ring,
                &self.file,
                data,
                offset,
                buffer_size,
                sq_entries,
                NO_FIXED_FD,
            )
        })
    }

    /// Flushes the internal buffer to disk using batched writes.
    ///
    /// Submits the buffered region through [`Self::submit_batch`].
    fn flush_buffer(&mut self) -> io::Result<()> {
        if self.buffer_pos == 0 {
            return Ok(());
        }

        let len = self.buffer_pos;
        let written = self.submit_batch(&self.buffer[..len], self.bytes_written)?;
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
        // Positional io_uring writes never advance the fd offset, so a
        // relative seek must resolve against the tracked logical position.
        let pos =
            match pos {
                SeekFrom::Current(delta) => {
                    SeekFrom::Start(self.bytes_written.checked_add_signed(delta).ok_or_else(
                        || io::Error::new(io::ErrorKind::InvalidInput, "seek before start of file"),
                    )?)
                }
                other => other,
            };
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
    use crate::io_uring::per_thread_ring::{expect_fixed_buffers, thread_buffer_stats};
    use crate::io_uring::registered_buffers::MAX_REGISTERED_BUFFERS;
    use tempfile::tempdir;

    /// Several MiB plus a ragged tail: enough 64 KiB chunks to clear the
    /// ring-batch threshold, ending in a partial registered buffer.
    fn payload() -> Vec<u8> {
        (0..(4u32 << 20) + 4099)
            .map(|i| (i.wrapping_mul(31) % 251) as u8)
            .collect()
    }

    /// The receiver's writer constructor (`writer_from_file`, used by
    /// `transfer_ops/response.rs`) must actually register buffers and route
    /// ring batches through `WRITE_FIXED`. Registered buffers were silently
    /// dead in production once already; this pins the wiring, not a helper.
    #[test]
    fn production_writer_writes_through_registered_buffers() {
        if with_ring(|_| Ok(())).is_err() {
            eprintln!("skipping registered-buffer writer test: io_uring unavailable");
            return;
        }
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("fixed.bin");
        let file = File::create(&path).expect("create");
        let writer =
            crate::io_uring::writer_from_file(file, 256 * 1024, crate::IoUringPolicy::Auto)
                .expect("writer_from_file");
        let crate::io_uring::IoUringOrStdWriter::IoUring(mut writer) = writer else {
            panic!("Auto policy on an io_uring host must build the io_uring writer");
        };
        if !expect_fixed_buffers(writer.registered_buffer_status(), "writer_from_file") {
            return;
        }
        assert_eq!(
            writer.registered_buffer_count(),
            Some(IoUringConfig::default().registered_buffer_count)
        );

        let before = thread_buffer_stats()
            .expect("group registered")
            .total_acquires;
        let data = payload();
        writer.write_all(&data).expect("write");
        writer.flush().expect("flush");
        drop(writer);
        let after = thread_buffer_stats()
            .expect("group registered")
            .total_acquires;

        assert!(
            after > before,
            "a multi-MiB batch must check out registered buffers for WRITE_FIXED"
        );
        assert_eq!(std::fs::read(&path).expect("read back"), data);
    }

    /// A kernel that rejects registration (ENOMEM under `RLIMIT_MEMLOCK`,
    /// EPERM under seccomp) must cost nothing but speed: the writer reports
    /// the failure, never registers a group, and lands the same bytes via
    /// plain `WRITE`. The oversized count forces the rejection on any host.
    #[test]
    fn rejected_registration_falls_back_with_identical_bytes() {
        if with_ring(|_| Ok(())).is_err() {
            eprintln!("skipping registration-fallback writer test: io_uring unavailable");
            return;
        }
        // A fresh thread owns a fresh per-thread ring, so no earlier
        // successful registration can mask the forced failure.
        std::thread::spawn(|| {
            let dir = tempdir().expect("tempdir");
            let path = dir.path().join("fallback.bin");
            let config = IoUringConfig {
                registered_buffer_count: MAX_REGISTERED_BUFFERS + 1,
                ..IoUringConfig::default()
            };
            let mut writer = IoUringWriter::create(&path, &config).expect("create");
            assert!(
                matches!(
                    writer.registered_buffer_status(),
                    RegisteredBufferStatus::RegistrationFailed { .. }
                ),
                "status must surface the rejection, got {:?}",
                writer.registered_buffer_status()
            );
            assert_eq!(writer.registered_buffer_count(), None);

            let data = payload();
            writer
                .write_all(&data)
                .expect("write must not fail on fallback");
            writer.flush().expect("flush");
            drop(writer);

            assert!(
                thread_buffer_stats().is_none(),
                "a rejected registration must leave no group behind"
            );
            assert_eq!(std::fs::read(&path).expect("read back"), data);
        })
        .join()
        .expect("fallback thread");
    }

    /// `register_buffers = false` must keep the kernel out of it entirely.
    #[test]
    fn disabled_by_config_registers_nothing() {
        if with_ring(|_| Ok(())).is_err() {
            eprintln!("skipping disabled-registration writer test: io_uring unavailable");
            return;
        }
        std::thread::spawn(|| {
            let dir = tempdir().expect("tempdir");
            let config = IoUringConfig {
                register_buffers: false,
                ..IoUringConfig::default()
            };
            let writer =
                IoUringWriter::create(dir.path().join("off.bin"), &config).expect("create");
            assert_eq!(
                writer.registered_buffer_status(),
                &RegisteredBufferStatus::Disabled
            );
            assert_eq!(writer.registered_buffer_count(), None);
            assert!(thread_buffer_stats().is_none());
        })
        .join()
        .expect("disabled thread");
    }

    /// A relative seek must advance from the logical write position. The
    /// sparse writer leaves holes with `seek(Current(n))`; resolving it
    /// against the fd offset, which positional writes never move, would land
    /// the next data span on top of the bytes already written.
    #[test]
    fn relative_seek_advances_from_logical_position() {
        if with_ring(|_| Ok(())).is_err() {
            return;
        }
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("hole.bin");
        let file = File::create(&path).expect("create");
        let config = IoUringConfig {
            sq_entries: 4,
            register_buffers: false,
            ..IoUringConfig::default()
        };
        let mut writer = IoUringWriter::with_ring(file, 4096, &config);

        writer.write_all(b"abc").expect("write head");
        assert_eq!(writer.seek(SeekFrom::Current(2)).expect("seek"), 5);
        writer.write_all(b"d").expect("write tail");
        assert_eq!(writer.stream_position().expect("position"), 6);
        writer.flush().expect("flush");
        drop(writer);

        assert_eq!(std::fs::read(&path).expect("read"), b"abc\0\0d");
    }
}
