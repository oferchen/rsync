//! Portable io_uring fallback for non-Linux platforms or when the feature is disabled.
//!
//! Provides the same public API as `io_uring` but always falls back to standard
//! buffered I/O. The `is_io_uring_available()` function always returns `false`.

#![allow(dead_code)]

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::traits::{
    FileReader, FileReaderFactory, FileWriter, FileWriterFactory, StdFileReader, StdFileWriter,
};

/// Check whether io_uring is available (always `false` on this platform).
#[must_use]
pub fn is_io_uring_available() -> bool {
    false
}

/// Configuration for io_uring instances (informational only on this platform).
#[derive(Debug, Clone)]
pub struct IoUringConfig {
    /// Number of submission queue entries.
    pub sq_entries: u32,
    /// Size of read/write buffers.
    pub buffer_size: usize,
    /// Whether to use direct I/O.
    pub direct_io: bool,
    /// Whether to register file descriptors (no-op on non-Linux).
    pub register_files: bool,
    /// Whether to enable SQPOLL (no-op on non-Linux).
    pub sqpoll: bool,
    /// SQPOLL idle timeout in ms (no-op on non-Linux).
    pub sqpoll_idle_ms: u32,
}

impl Default for IoUringConfig {
    fn default() -> Self {
        Self {
            sq_entries: 64,
            buffer_size: 64 * 1024,
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
            buffer_size: 256 * 1024,
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
            buffer_size: 16 * 1024,
            direct_io: false,
            register_files: true,
            sqpoll: false,
            sqpoll_idle_ms: 1000,
        }
    }
}

/// Stub io_uring reader (not available on this platform).
///
/// Opening always fails with `Unsupported`.
pub struct IoUringReader {
    _private: (),
}

impl IoUringReader {
    /// Always returns an `Unsupported` error on this platform.
    pub fn open<P: AsRef<Path>>(_path: P, _config: &IoUringConfig) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    /// Reads data at the specified offset.
    pub fn read_at(&mut self, _offset: u64, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    /// Reads the entire file into a vector.
    pub fn read_all_batched(&mut self) -> io::Result<Vec<u8>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }
}

impl Read for IoUringReader {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }
}

impl FileReader for IoUringReader {
    fn size(&self) -> u64 {
        0
    }

    fn position(&self) -> u64 {
        0
    }

    fn seek_to(&mut self, _pos: u64) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    fn read_all(&mut self) -> io::Result<Vec<u8>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }
}

/// Stub io_uring writer (not available on this platform).
///
/// Creating always fails with `Unsupported`.
pub struct IoUringWriter {
    _private: (),
}

impl IoUringWriter {
    /// Always returns an `Unsupported` error on this platform.
    pub fn create<P: AsRef<Path>>(_path: P, _config: &IoUringConfig) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    /// Creates a file with preallocated space (always fails on this platform).
    pub fn create_with_size<P: AsRef<Path>>(
        _path: P,
        _size: u64,
        _config: &IoUringConfig,
    ) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    /// Writes data at the specified offset.
    pub fn write_at(&mut self, _offset: u64, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }
}

impl Write for IoUringWriter {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }
}

impl Seek for IoUringWriter {
    fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }
}

impl FileWriter for IoUringWriter {
    fn bytes_written(&self) -> u64 {
        0
    }

    fn sync(&mut self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    fn preallocate(&mut self, _size: u64) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }
}

/// Factory that creates io_uring readers (always falls back to standard I/O).
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

    /// Forces fallback to standard I/O (no-op on this platform, always falls back).
    #[must_use]
    pub fn force_fallback(mut self, force: bool) -> Self {
        self.force_fallback = force;
        self
    }

    /// Returns whether io_uring will be used (always `false`).
    #[must_use]
    pub fn will_use_io_uring(&self) -> bool {
        false
    }
}

/// Reader that can be either io_uring-based or standard I/O.
///
/// On this platform, always uses standard I/O.
pub enum IoUringOrStdReader {
    /// io_uring-based reader (never constructed on this platform).
    IoUring(IoUringReader),
    /// Standard buffered reader.
    Std(StdFileReader),
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
        Ok(IoUringOrStdReader::Std(StdFileReader::open(path)?))
    }
}

/// Factory that creates io_uring writers (always falls back to standard I/O).
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

    /// Forces fallback to standard I/O (no-op on this platform, always falls back).
    #[must_use]
    pub fn force_fallback(mut self, force: bool) -> Self {
        self.force_fallback = force;
        self
    }

    /// Returns whether io_uring will be used (always `false`).
    #[must_use]
    pub fn will_use_io_uring(&self) -> bool {
        false
    }
}

/// Writer that can be either io_uring-based or standard I/O.
///
/// On this platform, always uses standard I/O.
pub enum IoUringOrStdWriter {
    /// io_uring-based writer (never constructed on this platform).
    IoUring(IoUringWriter),
    /// Standard buffered writer.
    Std(StdFileWriter),
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
        Ok(IoUringOrStdWriter::Std(StdFileWriter::create(path)?))
    }

    fn create_with_size(&self, path: &Path, size: u64) -> io::Result<Self::Writer> {
        Ok(IoUringOrStdWriter::Std(StdFileWriter::create_with_size(
            path, size,
        )?))
    }
}

/// Creates a writer from an existing file handle, respecting the io_uring policy.
///
/// On non-Linux platforms, `Enabled` returns an error since io_uring is unavailable.
/// `Auto` and `Disabled` both use standard buffered I/O.
pub fn writer_from_file(
    file: std::fs::File,
    buffer_capacity: usize,
    policy: crate::IoUringPolicy,
) -> io::Result<IoUringOrStdWriter> {
    if matches!(policy, crate::IoUringPolicy::Enabled) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring requested via --io-uring but not available on this platform",
        ));
    }
    Ok(IoUringOrStdWriter::Std(
        StdFileWriter::from_file_with_capacity(file, buffer_capacity),
    ))
}

/// Reads an entire file using standard I/O (io_uring not available).
pub fn read_file<P: AsRef<Path>>(path: P) -> io::Result<Vec<u8>> {
    let factory = IoUringReaderFactory::default();
    let mut reader = factory.open(path.as_ref())?;
    reader.read_all()
}

/// Writes data to a file using standard I/O (io_uring not available).
pub fn write_file<P: AsRef<Path>>(path: P, data: &[u8]) -> io::Result<()> {
    let factory = IoUringWriterFactory::default();
    let mut writer = factory.create(path.as_ref())?;
    writer.write_all(data)?;
    writer.flush()?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Socket I/O stubs (Unix only — requires RawFd)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(unix)]
mod socket_stub {
    use std::io::{self, BufReader, Read, Write};
    use std::os::unix::io::RawFd;

    /// Stub io_uring socket reader (not available on this platform).
    pub struct IoUringSocketReader {
        _private: (),
    }

    impl IoUringSocketReader {
        /// Always returns an `Unsupported` error on this platform.
        pub fn from_raw_fd(_fd: RawFd, _config: &super::IoUringConfig) -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "io_uring is not available on this platform",
            ))
        }
    }

    impl Read for IoUringSocketReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "io_uring is not available on this platform",
            ))
        }
    }

    /// Stub io_uring socket writer (not available on this platform).
    pub struct IoUringSocketWriter {
        _private: (),
    }

    impl IoUringSocketWriter {
        /// Always returns an `Unsupported` error on this platform.
        pub fn from_raw_fd(_fd: RawFd, _config: &super::IoUringConfig) -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "io_uring is not available on this platform",
            ))
        }
    }

    impl Write for IoUringSocketWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "io_uring is not available on this platform",
            ))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "io_uring is not available on this platform",
            ))
        }
    }

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
            let ret =
                unsafe { libc::read(self.0, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len()) };
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
            let ret =
                unsafe { libc::write(self.0, buf.as_ptr().cast::<libc::c_void>(), buf.len()) };
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
}

#[cfg(unix)]
pub use socket_stub::*;
