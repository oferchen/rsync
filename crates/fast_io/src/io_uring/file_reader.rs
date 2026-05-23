//! io_uring-based file reader with batched read support.
//!
//! Submissions go through the per-thread io_uring ring established by IUR-3.a
//! ([`super::per_thread_ring::with_ring`]). The reader no longer owns a
//! `RawIoUring` instance; one ring per OS thread is shared across every
//! [`IoUringReader`] that thread holds, dissolving the cross-thread
//! contention the shared-ring layout imposed on rayon-parallel readers (IUR-2
//! design doc section 1.1).
//!
//! Per-ring kernel state - fixed-file registration and registered buffers -
//! is tied to a specific ring fd and does not survive the move to a
//! thread-shared ring. Both are recorded as unavailable on every reader
//! constructed through this module; IUR-3.e re-introduces them on the
//! per-thread topology via the bgid-lease primitive. Until then the batched
//! read path uses plain `IORING_OP_READ` SQEs.

use std::fs::File;
use std::io::{self, Read};
use std::os::unix::io::AsRawFd;
use std::path::Path;

use io_uring::opcode;

use super::batching::{NO_FIXED_FD, maybe_fixed_file, sqe_fd};
use super::config::IoUringConfig;
use super::per_thread_ring::with_ring;
use super::registered_buffers::RegisteredBufferStatus;
use crate::traits::FileReader;

/// A file reader using io_uring for async I/O.
///
/// Provides both single-operation (`read_at`) and batched (`read_all_batched`)
/// interfaces. The batched path submits up to `sq_entries` concurrent reads per
/// `submit_and_wait` call, dramatically reducing syscall count for large files.
///
/// Submissions are issued against the calling thread's per-thread ring (see
/// [`super::per_thread_ring`]). Fixed-file registration and registered-buffer
/// fast paths are disabled on this reader until IUR-3.e wires the per-thread
/// bgid lease; batched reads always use the regular `IORING_OP_READ` opcode.
pub struct IoUringReader {
    file: File,
    size: u64,
    position: u64,
    buffer_size: usize,
    sq_entries: u32,
}

impl IoUringReader {
    /// Opens a file for reading with io_uring.
    ///
    /// Submissions route through the calling thread's per-thread ring (see
    /// [`super::per_thread_ring`]); the reader no longer owns a `RawIoUring`
    /// instance. The fixed-file and registered-buffer knobs on `config` are
    /// honoured for sizing only - per-writer/reader registration is disabled
    /// on the per-thread topology and IUR-3.e re-introduces it via the
    /// bgid-lease primitive.
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
        })
    }

    /// Returns the count of currently-registered fixed buffers, or `None` if
    /// buffer registration is not active on this reader.
    ///
    /// Always returns `None` on the per-thread topology: the per-thread ring
    /// is shared across every reader on the same thread, so per-reader
    /// buffer registration cannot be expressed safely. IUR-3.e re-introduces
    /// shared per-thread buffer registration via the bgid lease; call
    /// [`registered_buffer_status`](Self::registered_buffer_status) to tell
    /// the post-migration state apart from a kernel-side rejection.
    #[must_use]
    pub fn registered_buffer_count(&self) -> Option<usize> {
        None
    }

    /// Returns the provenance of fixed-buffer registration on this reader.
    ///
    /// Always returns [`RegisteredBufferStatus::Disabled`] on the per-thread
    /// topology; see [`Self::registered_buffer_count`].
    #[must_use]
    pub fn registered_buffer_status(&self) -> &RegisteredBufferStatus {
        &RegisteredBufferStatus::Disabled
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
    /// completes in a single syscall instead of 16. Always uses plain
    /// `IORING_OP_READ`; the `READ_FIXED` fast path returns with IUR-3.e via
    /// the per-thread bgid lease.
    pub fn read_all_batched(&mut self) -> io::Result<Vec<u8>> {
        let size = self.size as usize;
        if size == 0 {
            return Ok(Vec::new());
        }

        let mut output = vec![0u8; size];

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
    use tempfile::tempdir;

    /// Builds a reader for testing via the per-thread ring. Returns `None`
    /// when the kernel rejects `io_uring_setup(2)` (e.g., container,
    /// seccomp, or non-5.6+ kernel) so the test skips cleanly.
    fn make_reader() -> Option<IoUringReader> {
        let dir = tempdir().ok()?;
        let path = dir.path().join("in.bin");
        std::fs::write(&path, b"hello").ok()?;
        // Probe the per-thread ring; on hosts without io_uring we skip the
        // test rather than constructing a reader that would later fail.
        with_ring(|_| Ok(())).ok()?;
        // Keep `dir` alive for the duration of the reader by leaking it; the
        // OS reclaims the temp file at process exit. Tests are short-lived
        // and this avoids ordering the drop with the reader.
        let reader = IoUringReader::open(&path, &IoUringConfig::default()).ok()?;
        std::mem::forget(dir);
        Some(reader)
    }

    #[test]
    fn registered_buffers_always_disabled_on_per_thread_ring() {
        let reader = match make_reader() {
            Some(r) => r,
            None => return,
        };
        assert_eq!(
            reader.registered_buffer_count(),
            None,
            "per-thread ring readers do not own per-reader registered buffers"
        );
        assert_eq!(
            reader.registered_buffer_status(),
            &RegisteredBufferStatus::Disabled,
            "status must report Disabled on the per-thread topology until IUR-3.e wires bgid lease"
        );
    }

    #[test]
    fn open_returns_reader_when_io_uring_available() {
        let reader = match make_reader() {
            Some(r) => r,
            None => return,
        };
        assert_eq!(reader.size(), 5);
        assert_eq!(reader.position(), 0);
    }

    #[test]
    fn default_config_constructs_reader() {
        let reader = match make_reader() {
            Some(r) => r,
            None => return,
        };
        assert_eq!(reader.registered_buffer_count(), None);
    }
}
