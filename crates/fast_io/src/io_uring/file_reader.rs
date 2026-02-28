//! io_uring-based file reader with batched read support.

use std::fs::File;
use std::io::{self, Read};
use std::os::unix::io::AsRawFd;
use std::path::Path;

use io_uring::{IoUring as RawIoUring, opcode, types};

use super::batching::{NO_FIXED_FD, maybe_fixed_file, sqe_fd};
use super::config::IoUringConfig;
use crate::traits::FileReader;

/// A file reader using io_uring for async I/O.
///
/// Provides both single-operation (`read_at`) and batched (`read_all_batched`)
/// interfaces. The batched path submits up to `sq_entries` concurrent reads per
/// `submit_and_wait` call, dramatically reducing syscall count for large files.
pub struct IoUringReader {
    ring: RawIoUring,
    file: File,
    size: u64,
    position: u64,
    buffer_size: usize,
    sq_entries: u32,
    /// Fixed-file slot index, or `NO_FIXED_FD` when not registered.
    fixed_fd_slot: i32,
}

impl IoUringReader {
    /// Opens a file for reading with io_uring.
    ///
    /// When `config.register_files` is true, the fd is registered with the
    /// ring via `IORING_REGISTER_FILES`, eliminating per-op file-table
    /// lookups in the kernel.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The file cannot be opened
    /// - io_uring initialization fails
    pub fn open<P: AsRef<Path>>(path: P, config: &IoUringConfig) -> io::Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();

        let ring = config.build_ring()?;

        let raw_fd = file.as_raw_fd();
        let fixed_fd_slot = if config.register_files {
            let fds = [raw_fd];
            match ring.submitter().register_files(&fds) {
                Ok(()) => 0,           // registered at slot 0
                Err(_) => NO_FIXED_FD, // silent fallback
            }
        } else {
            NO_FIXED_FD
        };

        Ok(Self {
            ring,
            file,
            size,
            position: 0,
            buffer_size: config.buffer_size,
            sq_entries: config.sq_entries,
            fixed_fd_slot,
        })
    }

    /// Reads data at the specified offset without advancing the position.
    ///
    /// Submits a single SQE and waits for completion. For bulk reads, prefer
    /// `read_all_batched` which amortizes syscall overhead across many SQEs.
    pub fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        if offset >= self.size {
            return Ok(0);
        }

        let to_read = buf.len().min((self.size - offset) as usize);
        if to_read == 0 {
            return Ok(0);
        }

        let fd = sqe_fd(self.file.as_raw_fd(), self.fixed_fd_slot);

        let entry = opcode::Read::new(fd, buf.as_mut_ptr(), to_read as u32)
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

    /// Reads the entire file into a vector using batched io_uring submissions.
    ///
    /// Divides the file into `buffer_size` chunks and submits up to `sq_entries`
    /// reads per `submit_and_wait` call. For a 1 MB file with 64 KB buffers and
    /// 64 SQ entries, this completes in a single syscall instead of 16.
    pub fn read_all_batched(&mut self) -> io::Result<Vec<u8>> {
        let size = self.size as usize;
        if size == 0 {
            return Ok(Vec::new());
        }

        let mut output = vec![0u8; size];
        let chunk_size = self.buffer_size;
        let max_batch = self.sq_entries as usize;
        let total_chunks = size.div_ceil(chunk_size);
        let fd = sqe_fd(self.file.as_raw_fd(), self.fixed_fd_slot);

        let mut chunks_done = 0usize;

        while chunks_done < total_chunks {
            let batch_count = (total_chunks - chunks_done).min(max_batch);
            let base_offset = (chunks_done * chunk_size) as u64;

            // Build per-slot buffer slices. Each slot borrows a region of `output`
            // through raw pointers to avoid multiple mutable borrows.
            // Track (offset_in_output, len, bytes_done) per slot.
            let mut slots: Vec<(usize, usize, usize)> = Vec::with_capacity(batch_count);

            for i in 0..batch_count {
                let out_start = (chunks_done + i) * chunk_size;
                let out_end = (out_start + chunk_size).min(size);
                let len = out_end - out_start;
                slots.push((out_start, len, 0));
            }

            let mut all_done = false;
            while !all_done {
                let mut submitted = 0u32;

                for (idx, &(out_start, len, done)) in slots.iter().enumerate() {
                    let want = len - done;
                    if want == 0 {
                        continue;
                    }
                    let file_off = base_offset + (idx * chunk_size) as u64 + done as u64;
                    if file_off >= self.size {
                        continue;
                    }
                    let clamped = want.min((self.size - file_off) as usize);
                    if clamped == 0 {
                        continue;
                    }

                    let ptr = output[out_start + done..].as_mut_ptr();
                    let entry = opcode::Read::new(fd, ptr, clamped as u32)
                        .offset(file_off)
                        .build()
                        .user_data(idx as u64);
                    let entry = maybe_fixed_file(entry, self.fixed_fd_slot);

                    unsafe {
                        self.ring
                            .submission()
                            .push(&entry)
                            .map_err(|_| io::Error::other("submission queue full"))?;
                    }
                    submitted += 1;
                }

                if submitted == 0 {
                    break;
                }

                self.ring.submit_and_wait(submitted as usize)?;

                let mut completed = 0u32;
                while completed < submitted {
                    let cqe = self
                        .ring
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
