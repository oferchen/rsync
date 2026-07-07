//! Stub socket factory helpers mirroring [`crate::io_uring::socket_factory`].
//!
//! Always falls back to standard buffered I/O over a raw fd.

use super::socket_reader::IoUringSocketReader;
use super::socket_writer::IoUringSocketWriter;
use std::io::{self, BufReader, Read, Write};
use std::os::unix::io::RawFd;

/// Socket reader that falls back to `BufReader` (io_uring unavailable).
pub enum IoUringOrStdSocketReader {
    /// io_uring variant (never constructed on this platform).
    IoUring(IoUringSocketReader),
    /// Standard buffered reader.
    Std(BufReader<Box<dyn Read + Send>>),
}

impl Read for IoUringOrStdSocketReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::IoUring(r) => r.read(buf),
            Self::Std(r) => r.read(buf),
        }
    }
}

/// Socket writer that falls back to standard `Write` (io_uring unavailable).
pub enum IoUringOrStdSocketWriter {
    /// io_uring variant (never constructed on this platform).
    IoUring(IoUringSocketWriter),
    /// Standard writer.
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

/// Thin Read adapter over a raw fd (does not take ownership).
struct FdReader(RawFd);

impl Read for FdReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // SAFETY: `self.0` is a fd whose validity is guaranteed by the owner
        // (see struct docs); `buf` provides a `buf.len()`-byte writable
        // region matching `read(2)`'s contract.
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

/// Thin Write adapter over a raw fd (does not take ownership).
struct FdWriter(RawFd);

impl Write for FdWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // SAFETY: `self.0` is a fd whose validity is guaranteed by the owner
        // (see struct docs); `buf` provides a `buf.len()`-byte readable
        // region matching `write(2)`'s contract.
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

/// Creates a socket reader, always using standard buffered I/O.
///
/// On non-Linux platforms, `Enabled` returns an error. `Auto` and `Disabled`
/// both return a `BufReader` wrapping the fd.
pub fn socket_reader_from_fd(
    fd: RawFd,
    buffer_capacity: usize,
    policy: crate::IoUringPolicy,
) -> io::Result<IoUringOrStdSocketReader> {
    if matches!(policy, crate::IoUringPolicy::Enabled) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring requested via --io-uring but not available on this platform",
        ));
    }
    let reader = FdReader(fd);
    Ok(IoUringOrStdSocketReader::Std(BufReader::with_capacity(
        buffer_capacity,
        Box::new(reader),
    )))
}

/// Creates a socket writer, always using standard I/O.
///
/// On non-Linux platforms, `Enabled` returns an error. `Auto` and `Disabled`
/// both return a standard writer wrapping the fd.
pub fn socket_writer_from_fd(
    fd: RawFd,
    buffer_capacity: usize,
    policy: crate::IoUringPolicy,
) -> io::Result<IoUringOrStdSocketWriter> {
    let _ = buffer_capacity;
    if matches!(policy, crate::IoUringPolicy::Enabled) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring requested via --io-uring but not available on this platform",
        ));
    }
    let writer = FdWriter(fd);
    Ok(IoUringOrStdSocketWriter::Std(Box::new(writer)))
}

/// Creates a socket writer keyed on [`ZeroCopyPolicy`](crate::ZeroCopyPolicy).
///
/// SEND_ZC is unavailable in this build (non-Linux or the `io_uring` cargo
/// feature is off), so every policy - including
/// [`ZeroCopyPolicy::Enabled`](crate::ZeroCopyPolicy::Enabled) - gracefully
/// degrades to a standard `Write` wrapper over the raw fd. This never errors:
/// the daemon-sender treats `--zero-copy` as a best-effort opt-in that falls
/// back to the plain socket write path with byte-identical framing when the
/// zero-copy transport is not present.
///
/// The caller retains ownership of `fd`; this wrapper does not close it.
///
/// # Errors
///
/// Never returns an error on this platform.
pub fn socket_writer_from_fd_zero_copy(
    fd: RawFd,
    buffer_capacity: usize,
    policy: crate::ZeroCopyPolicy,
) -> io::Result<IoUringOrStdSocketWriter> {
    let _ = buffer_capacity;
    let _ = policy;
    let writer = FdWriter(fd);
    Ok(IoUringOrStdSocketWriter::Std(Box::new(writer)))
}
