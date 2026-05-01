//! io_uring-based async file and socket I/O for Linux 5.6+.
//!
//! This module provides high-performance I/O using Linux's io_uring interface,
//! which batches syscalls and enables true async I/O without thread pools.
//!
//! # Kernel requirements
//!
//! - **Linux 5.6 or later** - Minimum for `io_uring_setup(2)` and all opcodes
//!   used by this module (`IORING_OP_READ`, `IORING_OP_WRITE`, `IORING_OP_SEND`).
//! - **`io_uring` cargo feature** must be enabled at compile time.
//! - **No seccomp blocking** - Some container runtimes (Docker with default
//!   seccomp profile before v20.10.2, gVisor) block io_uring syscalls.
//!   The runtime probe in [`is_io_uring_available`] detects this.
//!
//! # Runtime detection and fallback
//!
//! Availability is checked once per process via [`is_io_uring_available`] and
//! cached in a process-wide atomic. The check:
//!
//! 1. Parses `uname().release` for major.minor >= 5.6
//! 2. Attempts `IoUring::new(4)` to verify the syscall is not blocked
//!
//! Factory types ([`IoUringReaderFactory`], [`IoUringWriterFactory`]) and the
//! top-level helpers ([`reader_from_path`], [`writer_from_file`]) automatically
//! fall back to standard buffered I/O (`BufReader`/`BufWriter`) when io_uring
//! is unavailable or ring creation fails.
//!
//! On non-Linux platforms or when the `io_uring` feature is disabled, the stub
//! module (`io_uring_stub.rs`) provides the same public API with
//! `is_io_uring_available()` always returning `false`.
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
//! # Optional features
//!
//! - **`IORING_REGISTER_FILES`** (fd registration) - Enabled by default via
//!   [`IoUringConfig::register_files`]. Eliminates per-SQE kernel file table
//!   lookups, saving ~50ns per operation on high-fd-count processes.
//! - **`IORING_SETUP_SQPOLL`** - Opt-in via [`IoUringConfig::sqpoll`]. A kernel
//!   thread polls the submission queue, eliminating `io_uring_enter` syscalls.
//!   Requires `CAP_SYS_NICE` or root; falls back to normal submission on
//!   `EPERM`.
//!
//! # Privilege requirements
//!
//! | Feature | Privilege | Notes |
//! |---------|-----------|-------|
//! | Base io_uring | None (Linux 5.6+) | Blocked by seccomp in some container runtimes (Docker < 20.10.2, gVisor) |
//! | SQPOLL (`IORING_SETUP_SQPOLL`) | `CAP_SYS_NICE` or root | Falls back transparently to regular submission on `EPERM` |
//! | Registered buffers (`IORING_REGISTER_BUFFERS`) | None | Pins pages in kernel; falls back to regular `Read`/`Write` opcodes on failure |
//! | File registration (`IORING_REGISTER_FILES`) | None | Eliminates per-SQE file table lookups |
//! | Direct I/O (`O_DIRECT`) | None | Requires filesystem support (not tmpfs); alignment constraints apply |
//! | Container / seccomp | N/A | `io_uring_setup(2)` may be blocked entirely; detected once at startup by [`is_io_uring_available`] |
//!
//! # Fallback chain
//!
//! Each layer degrades independently so that io_uring features are best-effort:
//!
//! - **Ring creation**: SQPOLL ring -> regular io_uring ring -> standard buffered I/O.
//!   Factory types handle the final fallback to `BufReader`/`BufWriter`.
//! - **Buffer registration**: registered (`READ_FIXED`/`WRITE_FIXED`) -> regular
//!   (`Read`/`Write`) opcodes. Silent fallback on registration failure.

mod batching;
pub mod buffer_ring;
mod config;
mod disk_batch;
mod file_factory;
mod file_reader;
mod file_writer;
pub mod registered_buffers;
mod socket_factory;
mod socket_reader;
mod socket_writer;

#[cfg(test)]
mod tests;

use std::fs::File;
use std::io::{self, Write};
use std::os::unix::io::AsRawFd;

pub use buffer_ring::{BufferRing, BufferRingConfig, BufferRingError, buffer_id_from_cqe_flags};
pub use config::{
    IoUringConfig, IoUringKernelInfo, config_detail, is_io_uring_available, sqpoll_fell_back,
};
pub use disk_batch::IoUringDiskBatch;
pub use file_factory::{
    IoUringOrStdReader, IoUringOrStdWriter, IoUringReaderFactory, IoUringWriterFactory,
};
pub use file_reader::IoUringReader;
pub use file_writer::IoUringWriter;
pub use registered_buffers::{RegisteredBufferGroup, RegisteredBufferSlot, RegisteredBufferStats};
pub use socket_factory::{
    IoUringOrStdSocketReader, IoUringOrStdSocketWriter, socket_reader_from_fd,
    socket_writer_from_fd,
};
pub use socket_reader::IoUringSocketReader;
pub use socket_writer::IoUringSocketWriter;

use crate::traits::{FileReader, FileReaderFactory, FileWriterFactory};
use batching::try_register_fd;

/// Reads an entire file using io_uring if available, falling back to standard I/O.
///
/// This is a convenience function for one-off file reads.
pub fn read_file<P: AsRef<std::path::Path>>(path: P) -> io::Result<Vec<u8>> {
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
                    return Ok(IoUringOrStdWriter::IoUring(IoUringWriter::with_ring(
                        file,
                        ring,
                        buffer_capacity,
                        config.sq_entries,
                        fixed_fd_slot,
                    )));
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
            Ok(IoUringOrStdWriter::IoUring(IoUringWriter::with_ring(
                file,
                ring,
                buffer_capacity,
                config.sq_entries,
                fixed_fd_slot,
            )))
        }
        crate::IoUringPolicy::Disabled => Ok(IoUringOrStdWriter::Std(
            crate::traits::StdFileWriter::from_file_with_capacity(file, buffer_capacity),
        )),
    }
}

/// Creates a reader from a file path, respecting the io_uring policy.
///
/// This is the read-side counterpart to [`writer_from_file`]. Used by the
/// sender/generator to read source files with io_uring when available.
///
/// The `policy` parameter controls io_uring usage:
/// - `Auto`: use io_uring when available, fall back to standard buffered I/O
/// - `Enabled`: require io_uring, return error if unavailable
/// - `Disabled`: always use standard buffered I/O
pub fn reader_from_path<P: AsRef<std::path::Path>>(
    path: P,
    policy: crate::IoUringPolicy,
) -> io::Result<IoUringOrStdReader> {
    let config = IoUringConfig::default();

    match policy {
        crate::IoUringPolicy::Auto => {
            if is_io_uring_available() {
                match IoUringReader::open(path.as_ref(), &config) {
                    Ok(r) => return Ok(IoUringOrStdReader::IoUring(r)),
                    Err(_) => { /* fall through to standard I/O */ }
                }
            }
            Ok(IoUringOrStdReader::Std(crate::traits::StdFileReader::open(
                path.as_ref(),
            )?))
        }
        crate::IoUringPolicy::Enabled => {
            if !is_io_uring_available() {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "io_uring requested via --io-uring but not available on this system",
                ));
            }
            Ok(IoUringOrStdReader::IoUring(IoUringReader::open(
                path.as_ref(),
                &config,
            )?))
        }
        crate::IoUringPolicy::Disabled => Ok(IoUringOrStdReader::Std(
            crate::traits::StdFileReader::open(path.as_ref())?,
        )),
    }
}

/// Writes data to a file using io_uring if available, falling back to standard I/O.
///
/// This is a convenience function for one-off file writes.
pub fn write_file<P: AsRef<std::path::Path>>(path: P, data: &[u8]) -> io::Result<()> {
    let factory = IoUringWriterFactory::default();
    let mut writer = factory.create(path.as_ref())?;
    writer.write_all(data)?;
    writer.flush()?;
    Ok(())
}
