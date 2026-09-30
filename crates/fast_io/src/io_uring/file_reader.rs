//! io_uring-based file reader with batched read support.
//!
//! Submissions go through the per-thread io_uring ring established by IUR-3.a
//! ([`super::per_thread_ring::with_ring`]). The reader no longer owns a
//! `RawIoUring` instance; one ring per OS thread is shared across every
//! [`IoUringReader`] that thread holds, dissolving the cross-thread
//! contention the shared-ring layout imposed on rayon-parallel readers (IUR-2
//! design doc section 1.1).
//!
//! Registered buffers live on the per-thread ring too: one
//! [`RegisteredBufferGroup`](super::registered_buffers::RegisteredBufferGroup)
//! per thread, shared by every reader and writer on it. Reads go out as
//! `IORING_OP_READ_FIXED` into those buffers and are copied to the caller;
//! when registration is disabled or the kernel rejected it, reads use plain
//! `IORING_OP_READ` straight into the caller's buffer. Fixed-file
//! registration stays off on the thread-shared ring.

use std::fs::File;
use std::io::{self, Read};
use std::os::unix::io::AsRawFd;
use std::path::Path;

use io_uring::opcode;

use super::batching::{NO_FIXED_FD, maybe_fixed_file, sqe_fd};
use super::config::IoUringConfig;
use super::per_thread_ring::{FixedBuffers, with_ring, with_ring_and_buffers};
use super::registered_buffers::{
    FixedReadLease, RegisteredBufferStatus, checkout_all, read_fixed_lease, submit_read_fixed_batch,
};
use crate::traits::FileReader;

/// A file reader using io_uring for async I/O.
///
/// Provides both single-operation (`read_at`) and batched (`read_all_batched`)
/// interfaces. The batched path submits up to `sq_entries` concurrent reads per
/// `submit_and_wait` call, dramatically reducing syscall count for large files.
///
/// Submissions are issued against the calling thread's per-thread ring (see
/// [`super::per_thread_ring`]). Reads use `IORING_OP_READ_FIXED` through the
/// thread's registered buffers when [`IoUringConfig::register_buffers`] is
/// set and the kernel accepted the registration, and plain `IORING_OP_READ`
/// otherwise.
pub struct IoUringReader {
    file: File,
    size: u64,
    position: u64,
    buffer_size: usize,
    sq_entries: u32,
    fixed: FixedBuffers,
}

impl IoUringReader {
    /// Opens a file for reading with io_uring.
    ///
    /// Submissions route through the calling thread's per-thread ring (see
    /// [`super::per_thread_ring`]); the reader no longer owns a `RawIoUring`
    /// instance. `config.register_buffers` registers the thread's fixed
    /// buffers on first use; a rejected registration falls back to plain
    /// reads and is reported by [`Self::registered_buffer_status`].
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The file cannot be opened
    /// - io_uring initialization fails on the calling thread
    pub fn open<P: AsRef<Path>>(path: P, config: &IoUringConfig) -> io::Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        // Probe the per-thread ring once at construction so callers observe
        // io_uring unavailability synchronously, matching the old behaviour
        // where `config.build_ring()?` surfaced setup errors here.
        with_ring(|_| Ok(()))?;
        Ok(Self {
            file,
            size,
            position: 0,
            buffer_size: config.buffer_size,
            sq_entries: config.sq_entries,
            fixed: FixedBuffers::register(
                config.register_buffers,
                config.buffer_size,
                config.registered_buffer_count,
            ),
        })
    }

    /// Returns the number of fixed buffers registered on the per-thread ring
    /// this reader submits to, or `None` if registration is not active.
    #[must_use]
    pub fn registered_buffer_count(&self) -> Option<usize> {
        self.fixed.count()
    }

    /// Returns the provenance of fixed-buffer registration on this reader:
    /// `Enabled` when reads use `IORING_OP_READ_FIXED`, `Disabled` when the
    /// config opted out, `RegistrationFailed` when the kernel rejected the
    /// registration and reads fell back to `IORING_OP_READ`.
    #[must_use]
    pub fn registered_buffer_status(&self) -> &RegisteredBufferStatus {
        self.fixed.status()
    }

    /// Reads into `out` at `offset` through the thread's registered buffers.
    ///
    /// Returns `None` without submitting anything when no fixed-buffer group
    /// is active, so the caller takes the plain `IORING_OP_READ` path. A
    /// short count means EOF was reached.
    fn read_fixed(&self, offset: u64, out: &mut [u8]) -> io::Result<Option<usize>> {
        let fd = sqe_fd(self.file.as_raw_fd(), NO_FIXED_FD);
        with_ring_and_buffers(self.fixed.request(), |ring, group| {
            let Some(group) = group else {
                return Ok(None);
            };
            let (_slots, infos) = checkout_all(group);
            if infos.is_empty() {
                return Ok(None);
            }
            submit_read_fixed_batch(ring, fd, out, offset, &infos, NO_FIXED_FD).map(Some)
        })
    }

    /// Reads up to `max_len` bytes at the current position into the thread's
    /// registered buffers and lends them to the caller without a copy.
    ///
    /// Returns `None` when registered buffers are not active or every slot is
    /// already leased; the caller then uses [`Read`]. Advances the position
    /// by the leased length, so an empty lease means EOF. One lease covers at
    /// most `registered_buffer_count * buffer_size` bytes.
    ///
    /// # Errors
    ///
    /// Propagates ring and `READ_FIXED` completion errors.
    pub fn read_lease(&mut self, max_len: usize) -> io::Result<Option<FixedReadLease>> {
        let offset = self.position;
        let len =
            max_len.min(usize::try_from(self.size.saturating_sub(offset)).unwrap_or(usize::MAX));
        let fd = sqe_fd(self.file.as_raw_fd(), NO_FIXED_FD);
        let lease = with_ring_and_buffers(self.fixed.request(), |ring, group| match group {
            Some(group) if group.available() > 0 => {
                read_fixed_lease(ring, group, fd, offset, len).map(Some)
            }
            _ => Ok(None),
        })?;
        if let Some(lease) = &lease {
            self.position += lease.len() as u64;
        }
        Ok(lease)
    }

    /// Reads data at the specified offset without advancing the position.
    ///
    /// Submits a single SQE on the per-thread ring and waits for completion.
    /// For bulk reads, prefer `read_all_batched` which amortizes syscall
    /// overhead across many SQEs.
    pub fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        if offset >= self.size {
            return Ok(0);
        }

        let to_read = buf.len().min((self.size - offset) as usize);
        if to_read == 0 {
            return Ok(0);
        }

        if let Some(n) = self.read_fixed(offset, &mut buf[..to_read])? {
            return Ok(n);
        }

        let raw_fd = self.file.as_raw_fd();
        let fd = sqe_fd(raw_fd, NO_FIXED_FD);

        with_ring(|ring| {
            let entry = opcode::Read::new(fd, buf.as_mut_ptr(), to_read as u32)
                .offset(offset)
                .build()
                .user_data(0);
            let entry = maybe_fixed_file(entry, NO_FIXED_FD);

            // SAFETY: `entry` references `buf` and the file fd; both outlive
            // `submit_and_wait` below, so the kernel can safely fill the buffer
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

    /// Reads the entire file into a vector using batched io_uring submissions.
    ///
    /// Divides the file into `buffer_size` chunks and submits up to
    /// `sq_entries` reads per `submit_and_wait` call against the per-thread
    /// ring. For a 1 MB file with 64 KB buffers and 64 SQ entries this
    /// completes in a single syscall instead of 16. With registered buffers
    /// active the file is read through them with `IORING_OP_READ_FIXED`
    /// instead, one SQE per registered buffer per round.
    pub fn read_all_batched(&mut self) -> io::Result<Vec<u8>> {
        let size = self.size as usize;
        if size == 0 {
            return Ok(Vec::new());
        }

        let mut output = vec![0u8; size];

        if let Some(n) = self.read_fixed(0, &mut output)? {
            output.truncate(n);
            return Ok(output);
        }

        let chunk_size = self.buffer_size;
        let max_batch = self.sq_entries as usize;
        let total_chunks = size.div_ceil(chunk_size);
        let raw_fd = self.file.as_raw_fd();
        let fd = sqe_fd(raw_fd, NO_FIXED_FD);
        let file_size = self.size;

        let mut chunks_done = 0usize;

        while chunks_done < total_chunks {
            let batch_count = (total_chunks - chunks_done).min(max_batch);
            let base_offset = (chunks_done * chunk_size) as u64;

            // Track (offset_in_output, len, bytes_done) per slot. Each slot
            // borrows a disjoint region of `output` through raw pointers to
            // avoid multiple mutable borrows.
            let mut slots: Vec<(usize, usize, usize)> = Vec::with_capacity(batch_count);

            for i in 0..batch_count {
                let out_start = (chunks_done + i) * chunk_size;
                let out_end = (out_start + chunk_size).min(size);
                let len = out_end - out_start;
                slots.push((out_start, len, 0));
            }

            let mut all_done = false;
            while !all_done {
                with_ring(|ring| -> io::Result<()> {
                    let mut submitted = 0u32;

                    for (idx, &(out_start, len, done)) in slots.iter().enumerate() {
                        let want = len - done;
                        if want == 0 {
                            continue;
                        }
                        let file_off = base_offset + (idx * chunk_size) as u64 + done as u64;
                        if file_off >= file_size {
                            continue;
                        }
                        let clamped = want.min((file_size - file_off) as usize);
                        if clamped == 0 {
                            continue;
                        }

                        let ptr = output[out_start + done..].as_mut_ptr();
                        let entry = opcode::Read::new(fd, ptr, clamped as u32)
                            .offset(file_off)
                            .build()
                            .user_data(idx as u64);
                        let entry = maybe_fixed_file(entry, NO_FIXED_FD);

                        // SAFETY: `entry` references `output` (held across the
                        // whole batched read) and the file fd; the pointer
                        // remains valid until `submit_and_wait` returns.
                        unsafe {
                            ring.submission()
                                .push(&entry)
                                .map_err(|_| io::Error::other("submission queue full"))?;
                        }
                        submitted += 1;
                    }

                    if submitted == 0 {
                        return Ok(());
                    }

                    ring.submit_and_wait(submitted as usize)?;

                    let mut completed = 0u32;
                    while completed < submitted {
                        let cqe = ring
                            .completion()
                            .next()
                            .ok_or_else(|| io::Error::other("missing CQE"))?;

                        let idx = cqe.user_data() as usize;
                        let result = cqe.result();

                        if result < 0 {
                            return Err(io::Error::from_raw_os_error(-result));
                        }

                        slots[idx].2 += result as usize;
                        completed += 1;
                    }

                    Ok(())
                })?;

                all_done = slots.iter().all(|&(_, len, done)| done >= len);
            }

            chunks_done += batch_count;
        }

        Ok(output)
    }
}

impl Read for IoUringReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.read_at(self.position, buf)?;
        self.position += n as u64;
        Ok(n)
    }
}

impl FileReader for IoUringReader {
    fn size(&self) -> u64 {
        self.size
    }

    fn position(&self) -> u64 {
        self.position
    }

    fn seek_to(&mut self, pos: u64) -> io::Result<()> {
        if pos > self.size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek position beyond end of file",
            ));
        }
        self.position = pos;
        Ok(())
    }

    fn read_all(&mut self) -> io::Result<Vec<u8>> {
        self.seek_to(0)?;
        self.read_all_batched()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io_uring::per_thread_ring::{expect_fixed_buffers, thread_buffer_stats};
    use crate::io_uring::registered_buffers::MAX_REGISTERED_BUFFERS;
    use tempfile::tempdir;

    /// Several MiB plus a ragged tail, so reads span many registered
    /// buffers and end in a partial one.
    fn payload() -> Vec<u8> {
        (0..(4u32 << 20) + 4099)
            .map(|i| (i.wrapping_mul(37) % 253) as u8)
            .collect()
    }

    /// The constructor behind the generator's `reader_from_path` must, when
    /// `register_buffers` is opted into, register buffers and serve both the
    /// streaming `Read` path and `read_all` through `READ_FIXED`.
    #[test]
    fn production_reader_reads_through_registered_buffers() {
        if with_ring(|_| Ok(())).is_err() {
            eprintln!("skipping registered-buffer reader test: io_uring unavailable");
            return;
        }

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("in.bin");
        let data = payload();
        std::fs::write(&path, &data).expect("write fixture");

        let config = IoUringConfig {
            register_buffers: true,
            ..IoUringConfig::default()
        };
        let mut reader = IoUringReader::open(&path, &config).expect("open");
        if !expect_fixed_buffers(reader.registered_buffer_status(), "IoUringReader::open") {
            return;
        }
        assert_eq!(
            reader.registered_buffer_count(),
            Some(IoUringConfig::default().registered_buffer_count)
        );

        let before = thread_buffer_stats()
            .expect("group registered")
            .total_acquires;
        let mut streamed = Vec::new();
        reader.read_to_end(&mut streamed).expect("stream");
        let mid = thread_buffer_stats()
            .expect("group registered")
            .total_acquires;
        let whole = reader.read_all().expect("read_all");
        let after = thread_buffer_stats()
            .expect("group registered")
            .total_acquires;

        assert!(mid > before, "streaming reads must use READ_FIXED");
        assert!(after > mid, "read_all must use READ_FIXED");
        assert_eq!(streamed, data);
        assert_eq!(whole, data);
    }

    /// A rejected registration must fall back to plain `READ` and return the
    /// same bytes. The oversized count forces the rejection on any host.
    #[test]
    fn rejected_registration_falls_back_with_identical_bytes() {
        if with_ring(|_| Ok(())).is_err() {
            eprintln!("skipping registration-fallback reader test: io_uring unavailable");
            return;
        }
        // A fresh thread owns a fresh per-thread ring, so no earlier
        // successful registration can mask the forced failure.
        std::thread::spawn(|| {
            let dir = tempdir().expect("tempdir");
            let path = dir.path().join("in.bin");
            let data = payload();
            std::fs::write(&path, &data).expect("write fixture");
            let config = IoUringConfig {
                register_buffers: true,
                registered_buffer_count: MAX_REGISTERED_BUFFERS + 1,
                ..IoUringConfig::default()
            };
            let mut reader = IoUringReader::open(&path, &config).expect("open");
            assert!(
                matches!(
                    reader.registered_buffer_status(),
                    RegisteredBufferStatus::RegistrationFailed { .. }
                ),
                "status must surface the rejection, got {:?}",
                reader.registered_buffer_status()
            );
            assert_eq!(reader.registered_buffer_count(), None);

            let mut streamed = Vec::new();
            reader.read_to_end(&mut streamed).expect("stream");
            assert_eq!(streamed, data);
            assert_eq!(reader.read_all().expect("read_all"), data);
            assert!(thread_buffer_stats().is_none());
        })
        .join()
        .expect("fallback thread");
    }

    /// A lease must hand back exactly the file's bytes, straight from the
    /// registered buffers, and return its slots on drop: the loop takes a
    /// fresh lease per step, so a leaked slot would exhaust the group.
    #[test]
    fn read_lease_lends_file_bytes_and_returns_slots() {
        if with_ring(|_| Ok(())).is_err() {
            eprintln!("skipping read lease test: io_uring unavailable");
            return;
        }
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("in.bin");
        let data = payload();
        std::fs::write(&path, &data).expect("write fixture");
        let config = IoUringConfig {
            register_buffers: true,
            ..IoUringConfig::default()
        };
        let mut reader = IoUringReader::open(&path, &config).expect("open");
        if !expect_fixed_buffers(reader.registered_buffer_status(), "read_lease") {
            return;
        }

        let before = thread_buffer_stats()
            .expect("group registered")
            .total_acquires;
        let mut got: Vec<u8> = Vec::with_capacity(data.len());
        loop {
            let lease = reader
                .read_lease(256 * 1024)
                .expect("lease")
                .expect("slots must be back in the group after each drop");
            if lease.is_empty() {
                break;
            }
            got.extend(lease.chunks().flatten().copied());
        }
        let stats = thread_buffer_stats().expect("group registered");

        assert_eq!(got, data);
        assert_eq!(reader.position(), data.len() as u64);
        assert!(stats.total_acquires > before, "leases must use READ_FIXED");
        assert_eq!(stats.total_misses, 0, "no lease may find the group drained");
    }

    /// Without registered buffers there is nothing to lend.
    #[test]
    fn read_lease_is_none_without_registered_buffers() {
        if with_ring(|_| Ok(())).is_err() {
            return;
        }
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("in.bin");
        std::fs::write(&path, b"hello").expect("write fixture");
        let mut reader = IoUringReader::open(&path, &IoUringConfig::default()).expect("open");
        assert!(reader.read_lease(4096).expect("lease").is_none());
        assert_eq!(reader.position(), 0);
    }

    #[test]
    fn open_reports_size_and_zero_position() {
        if with_ring(|_| Ok(())).is_err() {
            return;
        }
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("in.bin");
        std::fs::write(&path, b"hello").expect("write fixture");
        let reader = IoUringReader::open(&path, &IoUringConfig::default()).expect("open");
        assert_eq!(reader.size(), 5);
        assert_eq!(reader.position(), 0);
    }
}
