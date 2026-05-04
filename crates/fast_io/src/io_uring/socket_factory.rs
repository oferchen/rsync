//! Fallback enums and factory functions for io_uring socket I/O.

use std::io::{self, Read, Write};
use std::os::unix::io::RawFd;

use super::config::{IoUringConfig, is_io_uring_available};
use super::socket_reader::IoUringSocketReader;
use super::socket_writer::IoUringSocketWriter;

/// Socket reader that uses io_uring when available, falling back to `BufReader`.
#[allow(clippy::large_enum_variant)]
pub enum IoUringOrStdSocketReader {
    /// io_uring-based socket reader.
    IoUring(IoUringSocketReader),
    /// Standard buffered reader (fallback).
    Std(io::BufReader<Box<dyn Read + Send>>),
}

impl Read for IoUringOrStdSocketReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::IoUring(r) => r.read(buf),
            Self::Std(r) => r.read(buf),
        }
    }
}

/// Socket writer that uses io_uring when available, falling back to standard `Write`.
#[allow(clippy::large_enum_variant)]
pub enum IoUringOrStdSocketWriter {
    /// io_uring-based socket writer.
    IoUring(IoUringSocketWriter),
    /// Standard writer (fallback).
    Std(Box<dyn Write + Send>),
}

impl Write for IoUringOrStdSocketWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::IoUring(w) => w.write(buf),
            Self::Std(w) => w.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::IoUring(w) => w.flush(),
            Self::Std(w) => w.flush(),
        }
    }
}

/// Creates a socket reader backed by io_uring `RECV` when available.
///
/// On Linux 5.6+ with `io_uring` feature enabled and `policy` permitting,
/// returns an `IoUring` variant that reads via `IORING_OP_RECV`. Otherwise
/// falls back to `BufReader` wrapping a standard `Read`.
///
/// The `fd` must be a valid socket file descriptor. The caller retains
/// ownership — this function does not close the fd.
pub fn socket_reader_from_fd(
    fd: RawFd,
    buffer_capacity: usize,
    policy: crate::IoUringPolicy,
) -> io::Result<IoUringOrStdSocketReader> {
    let config = IoUringConfig {
        buffer_size: buffer_capacity,
        ..IoUringConfig::default()
    };

    match policy {
        crate::IoUringPolicy::Auto => {
            if is_io_uring_available() {
                if let Ok(reader) = IoUringSocketReader::from_raw_fd(fd, &config) {
                    return Ok(IoUringOrStdSocketReader::IoUring(reader));
                }
            }
            let reader = FdReader(fd);
            Ok(IoUringOrStdSocketReader::Std(io::BufReader::with_capacity(
                buffer_capacity,
                Box::new(reader),
            )))
        }
        crate::IoUringPolicy::Enabled => {
            if !is_io_uring_available() {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "io_uring requested but not available",
                ));
            }
            let reader = IoUringSocketReader::from_raw_fd(fd, &config)?;
            Ok(IoUringOrStdSocketReader::IoUring(reader))
        }
        crate::IoUringPolicy::Disabled => {
            let reader = FdReader(fd);
            Ok(IoUringOrStdSocketReader::Std(io::BufReader::with_capacity(
                buffer_capacity,
                Box::new(reader),
            )))
        }
    }
}

/// Creates a socket writer backed by io_uring `SEND` when available.
///
/// On Linux 5.6+ with `io_uring` feature enabled and `policy` permitting,
/// returns an `IoUring` variant that writes via `IORING_OP_SEND`. Otherwise
/// falls back to a standard `Write` wrapper.
///
/// The `fd` must be a valid socket file descriptor. The caller retains
/// ownership — this function does not close the fd.
pub fn socket_writer_from_fd(
    fd: RawFd,
    buffer_capacity: usize,
    policy: crate::IoUringPolicy,
) -> io::Result<IoUringOrStdSocketWriter> {
    let config = IoUringConfig {
        buffer_size: buffer_capacity,
        ..IoUringConfig::default()
    };

    match policy {
        crate::IoUringPolicy::Auto => {
            if is_io_uring_available() {
                if let Ok(writer) = IoUringSocketWriter::from_raw_fd(fd, &config) {
                    return Ok(IoUringOrStdSocketWriter::IoUring(writer));
                }
            }
            let writer = FdWriter(fd);
            Ok(IoUringOrStdSocketWriter::Std(Box::new(writer)))
        }
        crate::IoUringPolicy::Enabled => {
            if !is_io_uring_available() {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "io_uring requested but not available",
                ));
            }
            let writer = IoUringSocketWriter::from_raw_fd(fd, &config)?;
            Ok(IoUringOrStdSocketWriter::IoUring(writer))
        }
        crate::IoUringPolicy::Disabled => {
            let writer = FdWriter(fd);
            Ok(IoUringOrStdSocketWriter::Std(Box::new(writer)))
        }
    }
}

/// Thin Read adapter over a raw fd that does NOT take ownership.
///
/// Used as the fallback reader when io_uring is unavailable. The caller must
/// ensure the fd remains valid for the lifetime of this struct.
pub(super) struct FdReader(pub(super) RawFd);

impl Read for FdReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let ret = unsafe { libc::read(self.0, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len()) };
        if ret < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(ret as usize)
        }
    }
}

// SAFETY: The fd is just an integer; the caller guarantees validity.
unsafe impl Send for FdReader {}

/// Thin Write adapter over a raw fd that does NOT take ownership.
pub(super) struct FdWriter(pub(super) RawFd);

impl Write for FdWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let ret = unsafe { libc::write(self.0, buf.as_ptr().cast::<libc::c_void>(), buf.len()) };
        if ret < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(ret as usize)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// SAFETY: The fd is just an integer; the caller guarantees validity.
unsafe impl Send for FdWriter {}
