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

mod batching;
mod config;
mod file_factory;
mod file_reader;
mod file_writer;
mod socket_factory;
mod socket_reader;
mod socket_writer;

#[cfg(test)]
mod tests;

use std::fs::File;
use std::io::{self, Write};
use std::os::unix::io::AsRawFd;

pub use config::{IoUringConfig, is_io_uring_available};
pub use file_factory::{
    IoUringOrStdReader, IoUringOrStdWriter, IoUringReaderFactory, IoUringWriterFactory,
};
pub use file_reader::IoUringReader;
pub use file_writer::IoUringWriter;
pub use socket_factory::{
    IoUringOrStdSocketReader, IoUringOrStdSocketWriter, socket_reader_from_fd,
    socket_writer_from_fd,
};
pub use socket_reader::IoUringSocketReader;
pub use socket_writer::IoUringSocketWriter;

use crate::traits::FileReader;
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
