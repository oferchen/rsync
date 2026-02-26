//! io_uring-based async file I/O for Linux 5.6+.
//!
//! This module provides high-performance file I/O using Linux's io_uring interface,
//! which batches syscalls and enables true async I/O without thread pools.
//!
//! # Batching strategy
//!
//! The core advantage of io_uring is amortizing syscall overhead by submitting
//! multiple I/O operations in a single `submit_and_wait()` call. This module
//! implements two batched methods:
//!
//! - [`IoUringReader::read_all_batched`]: Submits up to `sq_entries` concurrent
//!   reads at different file offsets, processes all completions, then repeats
//!   until the entire file is read. A single large file read may need only
//!   `ceil(file_size / (buffer_size * sq_entries))` syscalls instead of
//!   `ceil(file_size / buffer_size)`.
//!
//! - [`IoUringWriter::write_all_batched`]: Splits a contiguous buffer into
//!   chunk-sized SQEs, submits them all at once, and processes completions.
//!   The `flush()` implementation uses this for the internal write buffer.
//!
//! Single-operation methods (`read_at`, `write_at`) are retained as convenience
//! wrappers for callers that need one-off positioned I/O.
//!
//! # Requirements
//!
//! - Linux kernel 5.6 or later
//! - The `io_uring` feature must be enabled

use std::ffi::CStr;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use io_uring::{IoUring as RawIoUring, opcode, types};

use crate::traits::{FileReader, FileReaderFactory, FileWriter, FileWriterFactory};

// ─────────────────────────────────────────────────────────────────────────────
// Kernel version detection
// ─────────────────────────────────────────────────────────────────────────────

/// Minimum kernel version required for io_uring (5.6.0).
const MIN_KERNEL_VERSION: (u32, u32) = (5, 6);

/// Cached result of io_uring availability check.
static IO_URING_AVAILABLE: AtomicBool = AtomicBool::new(false);
static IO_URING_CHECKED: AtomicBool = AtomicBool::new(false);

/// Parses kernel version from uname release string (e.g., "5.15.0-generic").
fn parse_kernel_version(release: &str) -> Option<(u32, u32)> {
    let mut parts = release.split(|c: char| !c.is_ascii_digit());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// Gets the kernel release string using libc uname.
fn get_kernel_release() -> Option<String> {
    unsafe {
        let mut utsname: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut utsname) != 0 {
            return None;
        }
        let release = CStr::from_ptr(utsname.release.as_ptr());
        release.to_str().ok().map(String::from)
    }
}

/// Checks if the current kernel supports io_uring.
///
/// Returns `true` if:
/// 1. Running on Linux
/// 2. Kernel version is 5.6 or later
/// 3. io_uring syscalls are available (not blocked by seccomp)
#[must_use]
pub fn is_io_uring_available() -> bool {
    // Fast path: use cached result
    if IO_URING_CHECKED.load(Ordering::Relaxed) {
        return IO_URING_AVAILABLE.load(Ordering::Relaxed);
    }

    let available = check_io_uring_available();
    IO_URING_AVAILABLE.store(available, Ordering::Relaxed);
    IO_URING_CHECKED.store(true, Ordering::Relaxed);
    available
}

fn check_io_uring_available() -> bool {
    // Check kernel version
    let release = match get_kernel_release() {
        Some(r) => r,
        None => return false,
    };

    let version = match parse_kernel_version(&release) {
        Some(v) => v,
        None => return false,
    };

    if version < MIN_KERNEL_VERSION {
        return false;
    }

    // Try to create a small io_uring instance to verify it's not blocked
    RawIoUring::new(4).is_ok()
}

// ─────────────────────────────────────────────────────────────────────────────
// Configuration
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration for io_uring instances.
#[derive(Debug, Clone)]
pub struct IoUringConfig {
    /// Number of submission queue entries (must be power of 2).
    pub sq_entries: u32,
    /// Size of read/write buffers.
    pub buffer_size: usize,
    /// Whether to use direct I/O (O_DIRECT).
    pub direct_io: bool,
    /// Whether to register the file descriptor with io_uring.
    ///
    /// When enabled, the fd is registered via `IORING_REGISTER_FILES` at open
    /// time, eliminating per-op file table lookups in the kernel. This saves
    /// ~50ns per SQE on high-fd-count processes.
    pub register_files: bool,
    /// Whether to enable kernel-side SQ polling (`IORING_SETUP_SQPOLL`).
    ///
    /// When enabled, a kernel thread continuously polls the submission queue,
    /// eliminating the `io_uring_enter` syscall on submit. Requires elevated
    /// privileges or `CAP_SYS_NICE` on most kernels. Falls back to normal
    /// submission if setup fails.
    pub sqpoll: bool,
    /// Idle timeout (ms) for the SQPOLL kernel thread before it goes to sleep.
    /// Only relevant when `sqpoll` is true. Default: 1000ms.
    pub sqpoll_idle_ms: u32,
}

impl Default for IoUringConfig {
    fn default() -> Self {
        Self {
            sq_entries: 64,
            buffer_size: 64 * 1024, // 64 KB
            direct_io: false,
            register_files: true,
            sqpoll: false,
            sqpoll_idle_ms: 1000,
        }
    }
}

impl IoUringConfig {
    /// Creates a config optimized for large file transfers.
    #[must_use]
    pub fn for_large_files() -> Self {
        Self {
            sq_entries: 256,
            buffer_size: 256 * 1024, // 256 KB
            direct_io: false,
            register_files: true,
            sqpoll: false,
            sqpoll_idle_ms: 1000,
        }
    }

    /// Creates a config optimized for many small files.
    #[must_use]
    pub fn for_small_files() -> Self {
        Self {
            sq_entries: 128,
            buffer_size: 16 * 1024, // 16 KB
            direct_io: false,
            register_files: true,
            sqpoll: false,
            sqpoll_idle_ms: 1000,
        }
    }

    /// Builds an `IoUring` instance from this config.
    ///
    /// Tries SQPOLL first if requested; falls back to a plain ring on
    /// `EPERM` / `ENOMEM`.
    fn build_ring(&self) -> io::Result<RawIoUring> {
        if self.sqpoll {
            let mut builder = io_uring::IoUring::builder();
            builder.setup_sqpoll(self.sqpoll_idle_ms);
            match builder.build(self.sq_entries) {
                Ok(ring) => return Ok(ring),
                Err(_) => {
                    // SQPOLL requires privileges — fall through to normal ring
                }
            }
        }
        RawIoUring::new(self.sq_entries)
            .map_err(|e| io::Error::other(format!("io_uring init failed: {e}")))
    }
}

/// Sentinel for "no fixed fd"; use raw fd path.
const NO_FIXED_FD: i32 = -1;

/// Returns the fd `types::Fd` for an SQE, using the fixed-file slot when
/// registered, or the raw fd otherwise.
fn sqe_fd(raw_fd: i32, fixed_fd_slot: i32) -> types::Fd {
    if fixed_fd_slot != NO_FIXED_FD {
        types::Fd(fixed_fd_slot)
    } else {
        types::Fd(raw_fd)
    }
}

/// Sets the `IOSQE_FIXED_FILE` flag on an SQE when using registered files.
fn maybe_fixed_file(entry: io_uring::squeue::Entry, fixed_fd_slot: i32) -> io_uring::squeue::Entry {
    if fixed_fd_slot != NO_FIXED_FD {
        entry.flags(io_uring::squeue::Flags::FIXED_FILE)
    } else {
        entry
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Batched I/O helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Submits a batch of write SQEs from contiguous `data` and collects completions.
///
/// Splits `data` into `chunk_size`-sized pieces, submitting up to `max_sqes` at
/// a time. Handles short writes by resubmitting the remainder.
///
/// When `fixed_fd_slot` is not `NO_FIXED_FD`, SQEs use the registered fixed-file
/// index and set `IOSQE_FIXED_FILE`.
fn submit_write_batch(
    ring: &mut RawIoUring,
    fd: types::Fd,
    data: &[u8],
    base_offset: u64,
    chunk_size: usize,
    max_sqes: usize,
    fixed_fd_slot: i32,
) -> io::Result<usize> {
    if data.is_empty() {
        return Ok(0);
    }

    let total = data.len();
    let mut global_done = 0usize;

    while global_done < total {
        let remaining = total - global_done;

        // Build a batch of chunks from the remaining data.
        let n_chunks = remaining.div_ceil(chunk_size).min(max_sqes);
        // Per-chunk tracking: (chunk_start_in_data, chunk_len, bytes_written_so_far).
        let mut slots: Vec<(usize, usize, usize)> = Vec::with_capacity(n_chunks);
        for i in 0..n_chunks {
            let start = global_done + i * chunk_size;
            let len = chunk_size.min(total - start);
            slots.push((start, len, 0));
        }

        let mut batch_complete = false;
        while !batch_complete {
            let mut submitted = 0u32;
            for (idx, &(start, len, done)) in slots.iter().enumerate() {
                let want = len - done;
                if want == 0 {
                    continue;
                }
                let file_off = base_offset + (start + done) as u64;
                let entry = opcode::Write::new(fd, data[start + done..].as_ptr(), want as u32)
                    .offset(file_off)
                    .build()
                    .user_data(idx as u64);
                let entry = maybe_fixed_file(entry, fixed_fd_slot);

                unsafe {
                    ring.submission()
                        .push(&entry)
                        .map_err(|_| io::Error::other("submission queue full"))?;
                }
                submitted += 1;
            }

            if submitted == 0 {
                break;
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
                if result == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "write returned 0 bytes",
                    ));
                }

                slots[idx].2 += result as usize;
                completed += 1;
            }

            batch_complete = slots.iter().all(|&(_, len, done)| done >= len);
        }

        let batch_written: usize = slots.iter().map(|&(_, _, done)| done).sum();
        global_done += batch_written;
    }

    Ok(global_done)
}

// ─────────────────────────────────────────────────────────────────────────────
// io_uring File Reader
// ─────────────────────────────────────────────────────────────────────────────

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

// ─────────────────────────────────────────────────────────────────────────────
// io_uring File Writer
// ─────────────────────────────────────────────────────────────────────────────

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

/// Registers `raw_fd` with `ring` if `register` is true. Returns the
/// fixed-file slot (0) on success, or `NO_FIXED_FD` on failure / opt-out.
fn try_register_fd(ring: &RawIoUring, raw_fd: i32, register: bool) -> i32 {
    if register {
        let fds = [raw_fd];
        match ring.submitter().register_files(&fds) {
            Ok(()) => 0,
            Err(_) => NO_FIXED_FD,
        }
    } else {
        NO_FIXED_FD
    }
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

// ─────────────────────────────────────────────────────────────────────────────
// Factories with automatic fallback
// ─────────────────────────────────────────────────────────────────────────────

/// Factory that creates io_uring readers when available, with fallback to standard I/O.
#[derive(Debug, Clone, Default)]
pub struct IoUringReaderFactory {
    config: IoUringConfig,
    force_fallback: bool,
}

impl IoUringReaderFactory {
    /// Creates a factory with custom configuration.
    #[must_use]
    pub fn with_config(config: IoUringConfig) -> Self {
        Self {
            config,
            force_fallback: false,
        }
    }

    /// Forces fallback to standard I/O even if io_uring is available.
    #[must_use]
    pub fn force_fallback(mut self, force: bool) -> Self {
        self.force_fallback = force;
        self
    }

    /// Returns whether io_uring will be used.
    #[must_use]
    pub fn will_use_io_uring(&self) -> bool {
        !self.force_fallback && is_io_uring_available()
    }
}

/// Reader that can be either io_uring-based or standard I/O.
#[allow(clippy::large_enum_variant)]
pub enum IoUringOrStdReader {
    /// io_uring-based reader.
    IoUring(IoUringReader),
    /// Standard buffered reader (fallback).
    Std(crate::traits::StdFileReader),
}

impl Read for IoUringOrStdReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            IoUringOrStdReader::IoUring(r) => r.read(buf),
            IoUringOrStdReader::Std(r) => r.read(buf),
        }
    }
}

impl FileReader for IoUringOrStdReader {
    fn size(&self) -> u64 {
        match self {
            IoUringOrStdReader::IoUring(r) => r.size(),
            IoUringOrStdReader::Std(r) => r.size(),
        }
    }

    fn position(&self) -> u64 {
        match self {
            IoUringOrStdReader::IoUring(r) => r.position(),
            IoUringOrStdReader::Std(r) => r.position(),
        }
    }

    fn seek_to(&mut self, pos: u64) -> io::Result<()> {
        match self {
            IoUringOrStdReader::IoUring(r) => r.seek_to(pos),
            IoUringOrStdReader::Std(r) => r.seek_to(pos),
        }
    }

    fn read_all(&mut self) -> io::Result<Vec<u8>> {
        match self {
            IoUringOrStdReader::IoUring(r) => r.read_all(),
            IoUringOrStdReader::Std(r) => r.read_all(),
        }
    }
}

impl FileReaderFactory for IoUringReaderFactory {
    type Reader = IoUringOrStdReader;

    fn open(&self, path: &Path) -> io::Result<Self::Reader> {
        if self.will_use_io_uring() {
            match IoUringReader::open(path, &self.config) {
                Ok(r) => return Ok(IoUringOrStdReader::IoUring(r)),
                Err(_) => {
                    // Fall through to standard I/O
                }
            }
        }

        Ok(IoUringOrStdReader::Std(crate::traits::StdFileReader::open(
            path,
        )?))
    }
}

/// Factory that creates io_uring writers when available, with fallback to standard I/O.
#[derive(Debug, Clone, Default)]
pub struct IoUringWriterFactory {
    config: IoUringConfig,
    force_fallback: bool,
}

impl IoUringWriterFactory {
    /// Creates a factory with custom configuration.
    #[must_use]
    pub fn with_config(config: IoUringConfig) -> Self {
        Self {
            config,
            force_fallback: false,
        }
    }

    /// Forces fallback to standard I/O even if io_uring is available.
    #[must_use]
    pub fn force_fallback(mut self, force: bool) -> Self {
        self.force_fallback = force;
        self
    }

    /// Returns whether io_uring will be used.
    #[must_use]
    pub fn will_use_io_uring(&self) -> bool {
        !self.force_fallback && is_io_uring_available()
    }
}

/// Writer that can be either io_uring-based or standard I/O.
#[allow(clippy::large_enum_variant)]
pub enum IoUringOrStdWriter {
    /// io_uring-based writer.
    IoUring(IoUringWriter),
    /// Standard buffered writer (fallback).
    Std(crate::traits::StdFileWriter),
}

impl Write for IoUringOrStdWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            IoUringOrStdWriter::IoUring(w) => w.write(buf),
            IoUringOrStdWriter::Std(w) => w.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            IoUringOrStdWriter::IoUring(w) => w.flush(),
            IoUringOrStdWriter::Std(w) => w.flush(),
        }
    }
}

impl Seek for IoUringOrStdWriter {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match self {
            IoUringOrStdWriter::IoUring(w) => w.seek(pos),
            IoUringOrStdWriter::Std(w) => w.seek(pos),
        }
    }
}

impl FileWriter for IoUringOrStdWriter {
    fn bytes_written(&self) -> u64 {
        match self {
            IoUringOrStdWriter::IoUring(w) => w.bytes_written(),
            IoUringOrStdWriter::Std(w) => w.bytes_written(),
        }
    }

    fn sync(&mut self) -> io::Result<()> {
        match self {
            IoUringOrStdWriter::IoUring(w) => w.sync(),
            IoUringOrStdWriter::Std(w) => w.sync(),
        }
    }

    fn preallocate(&mut self, size: u64) -> io::Result<()> {
        match self {
            IoUringOrStdWriter::IoUring(w) => w.preallocate(size),
            IoUringOrStdWriter::Std(w) => w.preallocate(size),
        }
    }
}

impl FileWriterFactory for IoUringWriterFactory {
    type Writer = IoUringOrStdWriter;

    fn create(&self, path: &Path) -> io::Result<Self::Writer> {
        if self.will_use_io_uring() {
            match IoUringWriter::create(path, &self.config) {
                Ok(w) => return Ok(IoUringOrStdWriter::IoUring(w)),
                Err(_) => {
                    // Fall through to standard I/O
                }
            }
        }

        Ok(IoUringOrStdWriter::Std(
            crate::traits::StdFileWriter::create(path)?,
        ))
    }

    fn create_with_size(&self, path: &Path, size: u64) -> io::Result<Self::Writer> {
        if self.will_use_io_uring() {
            match IoUringWriter::create_with_size(path, size, &self.config) {
                Ok(w) => return Ok(IoUringOrStdWriter::IoUring(w)),
                Err(_) => {
                    // Fall through to standard I/O
                }
            }
        }

        Ok(IoUringOrStdWriter::Std(
            crate::traits::StdFileWriter::create_with_size(path, size)?,
        ))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Convenience functions
// ─────────────────────────────────────────────────────────────────────────────

/// Reads an entire file using io_uring if available, falling back to standard I/O.
///
/// This is a convenience function for one-off file reads.
pub fn read_file<P: AsRef<Path>>(path: P) -> io::Result<Vec<u8>> {
    let factory = IoUringReaderFactory::default();
    let mut reader = factory.open(path.as_ref())?;
    reader.read_all()
}

/// Creates a writer from an existing file handle, respecting the io_uring policy.
///
/// This is the primary integration point for hot paths that open files
/// themselves (e.g., with `create_new` for atomic creation) but want to
/// leverage io_uring for the actual writes.
///
/// The `policy` parameter controls io_uring usage:
/// - `Auto`: use io_uring when available, fall back to standard I/O
/// - `Enabled`: require io_uring, return error if unavailable
/// - `Disabled`: always use standard buffered I/O
pub fn writer_from_file(
    file: File,
    buffer_capacity: usize,
    policy: crate::IoUringPolicy,
) -> io::Result<IoUringOrStdWriter> {
    let config = IoUringConfig::default();

    match policy {
        crate::IoUringPolicy::Auto => {
            if is_io_uring_available() {
                // Build ring first — if this fails, `file` is still ours.
                if let Ok(ring) = config.build_ring() {
                    let fixed_fd_slot =
                        try_register_fd(&ring, file.as_raw_fd(), config.register_files);
                    return Ok(IoUringOrStdWriter::IoUring(IoUringWriter {
                        ring,
                        file,
                        bytes_written: 0,
                        buffer: vec![0u8; buffer_capacity],
                        buffer_pos: 0,
                        buffer_size: buffer_capacity,
                        sq_entries: config.sq_entries,
                        fixed_fd_slot,
                    }));
                }
            }
            Ok(IoUringOrStdWriter::Std(
                crate::traits::StdFileWriter::from_file_with_capacity(file, buffer_capacity),
            ))
        }
        crate::IoUringPolicy::Enabled => {
            if !is_io_uring_available() {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "io_uring requested via --io-uring but not available on this system",
                ));
            }
            let ring = config.build_ring()?;
            let fixed_fd_slot = try_register_fd(&ring, file.as_raw_fd(), config.register_files);
            Ok(IoUringOrStdWriter::IoUring(IoUringWriter {
                ring,
                file,
                bytes_written: 0,
                buffer: vec![0u8; buffer_capacity],
                buffer_pos: 0,
                buffer_size: buffer_capacity,
                sq_entries: config.sq_entries,
                fixed_fd_slot,
            }))
        }
        crate::IoUringPolicy::Disabled => Ok(IoUringOrStdWriter::Std(
            crate::traits::StdFileWriter::from_file_with_capacity(file, buffer_capacity),
        )),
    }
}

/// Writes data to a file using io_uring if available, falling back to standard I/O.
///
/// This is a convenience function for one-off file writes.
pub fn write_file<P: AsRef<Path>>(path: P, data: &[u8]) -> io::Result<()> {
    let factory = IoUringWriterFactory::default();
    let mut writer = factory.create(path.as_ref())?;
    writer.write_all(data)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_kernel_version_parsing() {
        assert_eq!(parse_kernel_version("5.15.0-generic"), Some((5, 15)));
        assert_eq!(parse_kernel_version("6.1.0"), Some((6, 1)));
        assert_eq!(parse_kernel_version("4.19.123-aws"), Some((4, 19)));
        assert_eq!(parse_kernel_version("invalid"), None);
    }

    #[test]
    fn test_io_uring_availability_check() {
        let available = is_io_uring_available();
        println!("io_uring available: {available}");
    }

    #[test]
    fn test_io_uring_config_defaults() {
        let config = IoUringConfig::default();
        assert_eq!(config.sq_entries, 64);
        assert_eq!(config.buffer_size, 64 * 1024);
        assert!(!config.direct_io);
    }

    #[test]
    fn test_io_uring_config_presets() {
        let large = IoUringConfig::for_large_files();
        assert_eq!(large.sq_entries, 256);
        assert_eq!(large.buffer_size, 256 * 1024);

        let small = IoUringConfig::for_small_files();
        assert_eq!(small.sq_entries, 128);
        assert_eq!(small.buffer_size, 16 * 1024);
    }

    #[test]
    fn test_reader_factory_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, b"hello world").unwrap();

        let factory = IoUringReaderFactory::default().force_fallback(true);
        assert!(!factory.will_use_io_uring());

        let mut reader = factory.open(&path).unwrap();
        assert!(matches!(reader, IoUringOrStdReader::Std(_)));

        let data = reader.read_all().unwrap();
        assert_eq!(data, b"hello world");
    }

    #[test]
    fn test_writer_factory_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");

        let factory = IoUringWriterFactory::default().force_fallback(true);
        assert!(!factory.will_use_io_uring());

        let mut writer = factory.create(&path).unwrap();
        assert!(matches!(writer, IoUringOrStdWriter::Std(_)));

        writer.write_all(b"hello world").unwrap();
        writer.flush().unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world");
    }

    #[test]
    fn test_convenience_functions_with_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");

        write_file(&path, b"test data").unwrap();
        let data = read_file(&path).unwrap();
        assert_eq!(data, b"test data");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Tests that run only when io_uring is actually available
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_io_uring_reader_if_available() {
        if !is_io_uring_available() {
            println!("Skipping io_uring reader test: not available");
            return;
        }

        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, b"hello from io_uring").unwrap();

        let config = IoUringConfig::default();
        let mut reader = IoUringReader::open(&path, &config).unwrap();

        assert_eq!(reader.size(), 19);
        assert_eq!(reader.position(), 0);

        let data = reader.read_all().unwrap();
        assert_eq!(data, b"hello from io_uring");
    }

    #[test]
    fn test_io_uring_writer_if_available() {
        if !is_io_uring_available() {
            println!("Skipping io_uring writer test: not available");
            return;
        }

        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");

        let config = IoUringConfig::default();
        let mut writer = IoUringWriter::create(&path, &config).unwrap();

        writer.write_all(b"hello from io_uring").unwrap();
        writer.sync().unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "hello from io_uring"
        );
    }

    #[test]
    fn test_io_uring_factory_uses_io_uring_when_available() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, b"test").unwrap();

        let factory = IoUringReaderFactory::default();
        let reader = factory.open(&path).unwrap();

        if is_io_uring_available() {
            assert!(matches!(reader, IoUringOrStdReader::IoUring(_)));
        } else {
            assert!(matches!(reader, IoUringOrStdReader::Std(_)));
        }
    }

    #[test]
    fn test_io_uring_read_at() {
        if !is_io_uring_available() {
            println!("Skipping io_uring read_at test: not available");
            return;
        }

        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, b"hello world").unwrap();

        let config = IoUringConfig::default();
        let mut reader = IoUringReader::open(&path, &config).unwrap();

        let mut buf = [0u8; 5];
        let n = reader.read_at(6, &mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf, b"world");

        assert_eq!(reader.position(), 0);
    }

    #[test]
    fn test_io_uring_write_at() {
        if !is_io_uring_available() {
            println!("Skipping io_uring write_at test: not available");
            return;
        }

        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");

        let config = IoUringConfig::default();
        let mut writer = IoUringWriter::create(&path, &config).unwrap();

        writer.write_at(0, b"hello").unwrap();
        writer.write_at(6, b"world").unwrap();
        writer.flush().unwrap();

        let content = std::fs::read(&path).unwrap();
        assert_eq!(&content[0..5], b"hello");
        assert_eq!(&content[6..11], b"world");
    }

    #[test]
    fn test_reader_seek() {
        if !is_io_uring_available() {
            println!("Skipping io_uring seek test: not available");
            return;
        }

        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, b"hello world").unwrap();

        let config = IoUringConfig::default();
        let mut reader = IoUringReader::open(&path, &config).unwrap();

        reader.seek_to(6).unwrap();
        assert_eq!(reader.position(), 6);

        let mut buf = [0u8; 5];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"world");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Comprehensive io_uring tests with graceful fallback
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_basic_read_with_io_uring_or_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("read_test.txt");
        let test_data = b"The quick brown fox jumps over the lazy dog";
        std::fs::write(&path, test_data).unwrap();

        let factory = IoUringReaderFactory::default();
        let mut reader = factory.open(&path).unwrap();

        let data = reader.read_all().unwrap();
        assert_eq!(data, test_data);
        assert_eq!(reader.size(), test_data.len() as u64);
    }

    #[test]
    fn test_basic_write_with_io_uring_or_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("write_test.txt");
        let test_data = b"Hello, io_uring world!";

        let factory = IoUringWriterFactory::default();
        let mut writer = factory.create(&path).unwrap();

        writer.write_all(test_data).unwrap();
        writer.flush().unwrap();

        let written = std::fs::read(&path).unwrap();
        assert_eq!(written, test_data);
        assert_eq!(writer.bytes_written(), test_data.len() as u64);
    }

    #[test]
    fn test_large_file_read_with_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("large_read.bin");

        let chunk_size = 1024;
        let num_chunks = 1024;
        let mut expected_data = Vec::with_capacity(chunk_size * num_chunks);
        for i in 0..num_chunks {
            let pattern = (i % 256) as u8;
            expected_data.extend(std::iter::repeat_n(pattern, chunk_size));
        }
        std::fs::write(&path, &expected_data).unwrap();

        let factory = IoUringReaderFactory::default();
        let mut reader = factory.open(&path).unwrap();

        let data = reader.read_all().unwrap();
        assert_eq!(data.len(), expected_data.len());
        assert_eq!(data, expected_data);
    }

    #[test]
    fn test_large_file_write_with_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("large_write.bin");

        let chunk_size = 1024;
        let num_chunks = 512;
        let mut test_data = Vec::with_capacity(chunk_size * num_chunks);
        for i in 0..num_chunks {
            let pattern = (i % 256) as u8;
            test_data.extend(std::iter::repeat_n(pattern, chunk_size));
        }

        let factory = IoUringWriterFactory::default();
        let mut writer = factory.create(&path).unwrap();

        for chunk in test_data.chunks(chunk_size) {
            writer.write_all(chunk).unwrap();
        }
        writer.sync().unwrap();

        let written = std::fs::read(&path).unwrap();
        assert_eq!(written.len(), test_data.len());
        assert_eq!(written, test_data);
    }

    #[test]
    fn test_forced_fallback_to_standard_io() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("fallback_test.txt");
        let test_data = b"Testing forced fallback";
        std::fs::write(&path, test_data).unwrap();

        let factory = IoUringReaderFactory::default().force_fallback(true);
        assert!(!factory.will_use_io_uring());

        let mut reader = factory.open(&path).unwrap();
        assert!(matches!(reader, IoUringOrStdReader::Std(_)));

        let data = reader.read_all().unwrap();
        assert_eq!(data, test_data);
    }

    #[test]
    fn test_writer_forced_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("fallback_write.txt");
        let test_data = b"Forced fallback write";

        let factory = IoUringWriterFactory::default().force_fallback(true);
        assert!(!factory.will_use_io_uring());

        let mut writer = factory.create(&path).unwrap();
        assert!(matches!(writer, IoUringOrStdWriter::Std(_)));

        writer.write_all(test_data).unwrap();
        writer.flush().unwrap();

        let written = std::fs::read(&path).unwrap();
        assert_eq!(written, test_data);
    }

    #[test]
    fn test_reader_partial_reads() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("partial_read.txt");
        std::fs::write(&path, b"0123456789ABCDEF").unwrap();

        let factory = IoUringReaderFactory::default();
        let mut reader = factory.open(&path).unwrap();

        let mut buf = [0u8; 3];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 3);
        assert_eq!(&buf, b"012");

        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 3);
        assert_eq!(&buf, b"345");

        reader.seek_to(10).unwrap();
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 3);
        assert_eq!(&buf, b"ABC");
    }

    #[test]
    fn test_writer_buffering() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("buffering_test.txt");

        let _config = IoUringConfig {
            sq_entries: 32,
            buffer_size: 128,
            direct_io: false,
            register_files: true,
            sqpoll: false,
            sqpoll_idle_ms: 1000,
        };

        let factory = IoUringWriterFactory::default().force_fallback(true);
        let mut writer = factory.create(&path).unwrap();

        let data = b"x".repeat(256);
        writer.write_all(&data).unwrap();

        assert_eq!(writer.bytes_written(), 256);

        writer.flush().unwrap();

        let written = std::fs::read(&path).unwrap();
        assert_eq!(written.len(), 256);
    }

    #[test]
    fn test_writer_sync() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sync_test.txt");

        let factory = IoUringWriterFactory::default();
        let mut writer = factory.create(&path).unwrap();

        writer.write_all(b"sync test").unwrap();
        writer.sync().unwrap();

        let written = std::fs::read(&path).unwrap();
        assert_eq!(written, b"sync test");
    }

    #[test]
    fn test_writer_preallocate() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("preallocate_test.txt");

        let factory = IoUringWriterFactory::default();
        let mut writer = factory.create_with_size(&path, 1024).unwrap();

        writer.write_all(b"prealloc").unwrap();
        writer.flush().unwrap();

        let metadata = std::fs::metadata(&path).unwrap();
        assert_eq!(metadata.len(), 1024);
    }

    #[test]
    fn test_read_empty_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty.txt");
        std::fs::write(&path, b"").unwrap();

        let factory = IoUringReaderFactory::default();
        let mut reader = factory.open(&path).unwrap();

        assert_eq!(reader.size(), 0);
        let data = reader.read_all().unwrap();
        assert_eq!(data.len(), 0);
    }

    #[test]
    fn test_read_at_eof() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("eof_test.txt");
        std::fs::write(&path, b"short").unwrap();

        let factory = IoUringReaderFactory::default();
        let mut reader = factory.open(&path).unwrap();

        reader.seek_to(5).unwrap();
        assert_eq!(reader.position(), 5);

        let mut buf = [0u8; 10];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn test_seek_beyond_eof_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("seek_error.txt");
        std::fs::write(&path, b"data").unwrap();

        let factory = IoUringReaderFactory::default();
        let mut reader = factory.open(&path).unwrap();

        let result = reader.seek_to(100);
        assert!(result.is_err());
    }

    #[test]
    fn test_concurrent_operations_with_fallback() {
        use std::sync::Arc;
        use std::thread;

        let dir = Arc::new(tempdir().unwrap());
        let test_data = b"concurrent test data";

        let handles: Vec<_> = (0..4)
            .map(|i| {
                let dir = Arc::clone(&dir);
                let data = test_data.to_vec();
                thread::spawn(move || {
                    let path = dir.path().join(format!("thread_{i}.txt"));

                    let factory = IoUringWriterFactory::default();
                    let mut writer = factory.create(&path).unwrap();
                    writer.write_all(&data).unwrap();
                    writer.sync().unwrap();

                    let factory = IoUringReaderFactory::default();
                    let mut reader = factory.open(&path).unwrap();
                    let read_data = reader.read_all().unwrap();

                    assert_eq!(read_data, data);
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }
    }

    #[test]
    fn test_convenience_functions() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("convenience.txt");
        let test_data = b"convenience function test";

        write_file(&path, test_data).unwrap();

        let data = read_file(&path).unwrap();
        assert_eq!(data, test_data);
    }

    #[test]
    fn test_multiple_sequential_operations() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sequential.txt");

        let factory = IoUringWriterFactory::default();

        {
            let mut writer = factory.create(&path).unwrap();
            writer.write_all(b"first").unwrap();
            writer.flush().unwrap();
        }

        let factory_read = IoUringReaderFactory::default();
        {
            let mut reader = factory_read.open(&path).unwrap();
            let data = reader.read_all().unwrap();
            assert_eq!(data, b"first");
        }

        {
            let mut writer = factory.create(&path).unwrap();
            writer.write_all(b"second write").unwrap();
            writer.flush().unwrap();
        }

        {
            let mut reader = factory_read.open(&path).unwrap();
            let data = reader.read_all().unwrap();
            assert_eq!(data, b"second write");
        }
    }

    #[test]
    fn test_config_presets() {
        let large = IoUringConfig::for_large_files();
        assert!(large.sq_entries >= 128);
        assert!(large.buffer_size >= 128 * 1024);

        let small = IoUringConfig::for_small_files();
        assert!(small.buffer_size <= 32 * 1024);
    }

    #[test]
    fn test_factory_with_custom_config() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("custom_config.txt");
        std::fs::write(&path, b"custom").unwrap();

        let config = IoUringConfig {
            sq_entries: 32,
            buffer_size: 4096,
            direct_io: false,
        };

        let factory = IoUringReaderFactory::with_config(config);
        let mut reader = factory.open(&path).unwrap();
        let data = reader.read_all().unwrap();
        assert_eq!(data, b"custom");
    }

    #[test]
    fn test_error_handling_nonexistent_file() {
        let factory = IoUringReaderFactory::default();
        let result = factory.open(Path::new("/nonexistent/path/file.txt"));
        assert!(result.is_err());
    }

    #[test]
    fn test_error_handling_permission_denied() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let path = dir.path().join("readonly.txt");
        std::fs::write(&path, b"data").unwrap();

        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o200);
        fs::set_permissions(&path, perms).unwrap();

        let factory = IoUringReaderFactory::default();
        let result = factory.open(&path);
        assert!(result.is_err());

        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o644);
        fs::set_permissions(&path, perms).unwrap();
    }

    #[test]
    fn test_queue_depth_limits() {
        if !is_io_uring_available() {
            println!("Skipping queue depth test: io_uring not available");
            return;
        }

        let dir = tempdir().unwrap();
        let path = dir.path().join("queue_test.txt");

        let config = IoUringConfig {
            sq_entries: 4,
            buffer_size: 1024,
            direct_io: false,
        };

        let mut writer = IoUringWriter::create(&path, &config).unwrap();
        let data = b"x".repeat(8192);
        writer.write_all(&data).unwrap();
        writer.flush().unwrap();

        let written = std::fs::read(&path).unwrap();
        assert_eq!(written.len(), data.len());
    }

    #[test]
    fn test_reader_remaining() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("remaining.txt");
        std::fs::write(&path, b"0123456789").unwrap();

        let factory = IoUringReaderFactory::default();
        let mut reader = factory.open(&path).unwrap();

        assert_eq!(reader.remaining(), 10);

        let mut buf = [0u8; 3];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(reader.remaining(), 7);

        reader.seek_to(8).unwrap();
        assert_eq!(reader.remaining(), 2);
    }

    #[test]
    fn test_write_zero_bytes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("zero_write.txt");

        let factory = IoUringWriterFactory::default();
        let mut writer = factory.create(&path).unwrap();

        let n = writer.write(b"").unwrap();
        assert_eq!(n, 0);
        assert_eq!(writer.bytes_written(), 0);

        writer.flush().unwrap();
        let written = std::fs::read(&path).unwrap();
        assert_eq!(written.len(), 0);
    }

    #[test]
    fn test_io_uring_reader_read_all_batched() {
        if !is_io_uring_available() {
            println!("Skipping batched read test: io_uring not available");
            return;
        }

        let dir = tempdir().unwrap();
        let path = dir.path().join("batched.txt");

        let size = 256 * 1024;
        let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        std::fs::write(&path, &data).unwrap();

        let config = IoUringConfig {
            sq_entries: 64,
            buffer_size: 64 * 1024,
            direct_io: false,
        };

        let mut reader = IoUringReader::open(&path, &config).unwrap();
        let read_data = reader.read_all_batched().unwrap();

        assert_eq!(read_data.len(), data.len());
        assert_eq!(read_data, data);
    }

    #[test]
    fn test_io_uring_batched_read_small_sq() {
        if !is_io_uring_available() {
            println!("Skipping batched read small-sq test: io_uring not available");
            return;
        }

        let dir = tempdir().unwrap();
        let path = dir.path().join("batched_small_sq.bin");

        // 128 KB file with 4 SQ entries and 8 KB buffers = 4 batches of 4 reads
        let size = 128 * 1024;
        let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();

        let config = IoUringConfig {
            sq_entries: 4,
            buffer_size: 8 * 1024,
            direct_io: false,
        };

        let mut reader = IoUringReader::open(&path, &config).unwrap();
        let read_data = reader.read_all_batched().unwrap();

        assert_eq!(read_data.len(), data.len());
        assert_eq!(read_data, data);
    }

    #[test]
    fn test_io_uring_batched_write() {
        if !is_io_uring_available() {
            println!("Skipping batched write test: io_uring not available");
            return;
        }

        let dir = tempdir().unwrap();
        let path = dir.path().join("batched_write.bin");

        // Write 512 KB in one shot via write_all_batched
        let size = 512 * 1024;
        let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();

        let config = IoUringConfig {
            sq_entries: 32,
            buffer_size: 64 * 1024,
            direct_io: false,
        };

        let mut writer = IoUringWriter::create(&path, &config).unwrap();
        writer.write_all_batched(&data, 0).unwrap();
        writer.flush().unwrap();

        let written = std::fs::read(&path).unwrap();
        assert_eq!(written.len(), data.len());
        assert_eq!(written, data);
    }

    #[test]
    fn test_io_uring_large_file_batched_roundtrip() {
        if !is_io_uring_available() {
            println!("Skipping large batched roundtrip: io_uring not available");
            return;
        }

        let dir = tempdir().unwrap();
        let path = dir.path().join("roundtrip.bin");

        // 2 MB file
        let size = 2 * 1024 * 1024;
        let data: Vec<u8> = (0..size).map(|i| ((i * 7 + 3) % 256) as u8).collect();

        let config = IoUringConfig {
            sq_entries: 64,
            buffer_size: 64 * 1024,
            direct_io: false,
        };

        {
            let mut writer = IoUringWriter::create(&path, &config).unwrap();
            writer.write_all(&data).unwrap();
            writer.sync().unwrap();
        }

        {
            let mut reader = IoUringReader::open(&path, &config).unwrap();
            let read_data = reader.read_all_batched().unwrap();
            assert_eq!(read_data.len(), data.len());
            assert_eq!(read_data, data);
        }
    }

    #[test]
    fn test_binary_data_integrity() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("binary.bin");

        let data: Vec<u8> = (0..=255).cycle().take(4096).collect();

        let factory = IoUringWriterFactory::default();
        let mut writer = factory.create(&path).unwrap();
        writer.write_all(&data).unwrap();
        writer.flush().unwrap();
        drop(writer);

        let factory_read = IoUringReaderFactory::default();
        let mut reader = factory_read.open(&path).unwrap();
        let read_data = reader.read_all().unwrap();

        assert_eq!(read_data.len(), data.len());
        assert_eq!(read_data, data);
    }

    #[test]
    fn test_drop_flushes_writer() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("drop_flush.txt");

        {
            let factory = IoUringWriterFactory::default();
            let mut writer = factory.create(&path).unwrap();
            writer.write_all(b"data to flush on drop").unwrap();
        }

        let written = std::fs::read(&path).unwrap();
        assert_eq!(written, b"data to flush on drop");
    }
}
