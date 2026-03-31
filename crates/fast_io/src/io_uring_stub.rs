//! Portable io_uring fallback for non-Linux platforms or when the feature is disabled.
//!
//! Provides the same public API as the real `io_uring` module but always falls
//! back to standard buffered I/O. The `is_io_uring_available()` function always
//! returns `false`. This module is compiled when either:
//!
//! - The target OS is not Linux, or
//! - The `io_uring` cargo feature is not enabled
//!
//! All factory types ([`IoUringReaderFactory`], [`IoUringWriterFactory`]) produce
//! `Std` variants directly. The stub types ([`IoUringReader`], [`IoUringWriter`])
//! cannot be constructed and exist only for enum variant completeness.

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
    /// Whether to register fixed buffers (no-op on non-Linux).
    pub register_buffers: bool,
    /// Number of fixed buffers to register (no-op on non-Linux).
    pub registered_buffer_count: usize,
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
            register_buffers: true,
            registered_buffer_count: 8,
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
            register_buffers: true,
            registered_buffer_count: 16,
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
            register_buffers: true,
            registered_buffer_count: 8,
        }
    }
}

/// Stub module for registered buffer types (not available on this platform).
pub mod registered_buffers {
    use std::io;

    /// Stub registered buffer group (not available on this platform).
    ///
    /// On non-Linux platforms, buffer registration always returns `None` from
    /// `try_new` and `Unsupported` from `new`.
    #[derive(Debug)]
    pub struct RegisteredBufferGroup {
        _private: (),
    }

    impl RegisteredBufferGroup {
        /// Always returns an `Unsupported` error on this platform.
        pub fn new(_ring: &(), _buffer_size: usize, _count: usize) -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "io_uring buffer registration is not available on this platform",
            ))
        }

        /// Always returns `None` on this platform.
        #[must_use]
        pub fn try_new(_ring: &(), _buffer_size: usize, _count: usize) -> Option<Self> {
            None
        }

        /// Returns the number of buffers (always 0).
        #[must_use]
        pub fn count(&self) -> usize {
            0
        }

        /// Returns the buffer size (always 0).
        #[must_use]
        pub fn buffer_size(&self) -> usize {
            0
        }

        /// Returns the number of available slots (always 0).
        #[must_use]
        pub fn available(&self) -> usize {
            0
        }

        /// Always returns `None` on this platform.
        #[must_use]
        pub fn checkout(&self) -> Option<RegisteredBufferSlot<'_>> {
            None
        }

        /// No-op on this platform.
        pub fn unregister(&self, _ring: &()) -> io::Result<()> {
            Ok(())
        }
    }

    /// Stub registered buffer slot (not available on this platform).
    pub struct RegisteredBufferSlot<'a> {
        _phantom: std::marker::PhantomData<&'a ()>,
    }

    impl RegisteredBufferSlot<'_> {
        /// Returns the buffer index (always 0).
        #[must_use]
        pub fn buf_index(&self) -> u16 {
            0
        }

        /// Returns a null mutable pointer.
        #[must_use]
        pub fn as_mut_ptr(&self) -> *mut u8 {
            std::ptr::null_mut()
        }

        /// Returns a null pointer.
        #[must_use]
        pub fn as_ptr(&self) -> *const u8 {
            std::ptr::null()
        }

        /// Returns the buffer size (always 0).
        #[must_use]
        pub fn buffer_size(&self) -> usize {
            0
        }
    }
}

pub use registered_buffers::{RegisteredBufferGroup, RegisteredBufferSlot};

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

impl std::fmt::Debug for IoUringOrStdReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IoUring(_) => f.debug_tuple("IoUring").field(&"<io_uring>").finish(),
            Self::Std(_) => f.debug_tuple("Std").field(&"<buffered>").finish(),
        }
    }
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

impl std::fmt::Debug for IoUringOrStdWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IoUring(_) => f.debug_tuple("IoUring").field(&"<io_uring>").finish(),
            Self::Std(_) => f.debug_tuple("Std").field(&"<buffered>").finish(),
        }
    }
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

/// Creates a reader from a file path, respecting the io_uring policy.
///
/// On non-Linux platforms, `Enabled` returns an error since io_uring is unavailable.
/// `Auto` and `Disabled` both use standard buffered I/O.
pub fn reader_from_path<P: AsRef<Path>>(
    path: P,
    policy: crate::IoUringPolicy,
) -> io::Result<IoUringOrStdReader> {
    if matches!(policy, crate::IoUringPolicy::Enabled) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring requested via --io-uring but not available on this platform",
        ));
    }
    Ok(IoUringOrStdReader::Std(StdFileReader::open(path.as_ref())?))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IoUringPolicy;
    use crate::traits::{FileReader, FileWriter};
    use std::io::{Read, Write};
    use tempfile::{NamedTempFile, tempdir};

    // ─────────────────────────────────────────────────────────────────────
    // IoUringPolicy fallback tests (non-Linux / feature-disabled platform)
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn io_uring_unavailable_on_stub_platform() {
        assert!(!is_io_uring_available());
    }

    // ─────────────────────────────────────────────────────────────────────
    // Registered buffer stubs
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn registered_buffer_group_try_new_returns_none() {
        let result = RegisteredBufferGroup::try_new(&(), 4096, 4);
        assert!(result.is_none());
    }

    #[test]
    fn registered_buffer_group_new_returns_unsupported() {
        let result = RegisteredBufferGroup::new(&(), 4096, 4);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn config_has_register_buffers_fields() {
        let config = IoUringConfig::default();
        assert!(config.register_buffers);
        assert_eq!(config.registered_buffer_count, 8);

        let large = IoUringConfig::for_large_files();
        assert!(large.register_buffers);
        assert_eq!(large.registered_buffer_count, 16);

        let small = IoUringConfig::for_small_files();
        assert!(small.register_buffers);
        assert_eq!(small.registered_buffer_count, 8);
    }

    #[test]
    fn policy_disabled_writer_uses_std() {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(b"").unwrap();
        let file = tmp.reopen().unwrap();

        let writer = writer_from_file(file, 8192, IoUringPolicy::Disabled).unwrap();
        assert!(matches!(writer, IoUringOrStdWriter::Std(_)));
    }

    #[test]
    fn policy_disabled_reader_uses_std() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("disabled_reader.txt");
        std::fs::write(&path, b"hello").unwrap();

        let reader = reader_from_path(&path, IoUringPolicy::Disabled).unwrap();
        assert!(matches!(reader, IoUringOrStdReader::Std(_)));
    }

    #[test]
    fn policy_auto_falls_back_to_std_writer() {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(b"").unwrap();
        let file = tmp.reopen().unwrap();

        let writer = writer_from_file(file, 8192, IoUringPolicy::Auto).unwrap();
        // On non-Linux, Auto always falls back to Std
        assert!(matches!(writer, IoUringOrStdWriter::Std(_)));
    }

    #[test]
    fn policy_auto_falls_back_to_std_reader() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("auto_reader.txt");
        std::fs::write(&path, b"world").unwrap();

        let reader = reader_from_path(&path, IoUringPolicy::Auto).unwrap();
        assert!(matches!(reader, IoUringOrStdReader::Std(_)));
    }

    #[test]
    fn policy_enabled_writer_returns_error() {
        let tmp = NamedTempFile::new().unwrap();
        let file = tmp.reopen().unwrap();

        let result = writer_from_file(file, 8192, IoUringPolicy::Enabled);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        assert!(err.to_string().contains("io_uring"));
    }

    #[test]
    fn policy_enabled_reader_returns_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("enabled_reader.txt");
        std::fs::write(&path, b"data").unwrap();

        let result = reader_from_path(&path, IoUringPolicy::Enabled);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        assert!(err.to_string().contains("io_uring"));
    }

    // ─────────────────────────────────────────────────────────────────────
    // Parity tests - output must be identical regardless of policy
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn writer_parity_disabled_vs_auto() {
        let test_data: Vec<u8> = (0..4096).map(|i| ((i * 7 + 13) % 256) as u8).collect();

        // Write via Disabled policy
        let dir = tempdir().unwrap();
        let path_disabled = dir.path().join("parity_disabled.bin");
        {
            let file = std::fs::File::create(&path_disabled).unwrap();
            let mut writer = writer_from_file(file, 8192, IoUringPolicy::Disabled).unwrap();
            writer.write_all(&test_data).unwrap();
            writer.flush().unwrap();
        }

        // Write via Auto policy (also falls back to Std on non-Linux)
        let path_auto = dir.path().join("parity_auto.bin");
        {
            let file = std::fs::File::create(&path_auto).unwrap();
            let mut writer = writer_from_file(file, 8192, IoUringPolicy::Auto).unwrap();
            writer.write_all(&test_data).unwrap();
            writer.flush().unwrap();
        }

        let content_disabled = std::fs::read(&path_disabled).unwrap();
        let content_auto = std::fs::read(&path_auto).unwrap();

        assert_eq!(content_disabled.len(), test_data.len());
        assert_eq!(content_disabled, content_auto);
        assert_eq!(content_disabled, test_data);
    }

    #[test]
    fn reader_parity_disabled_vs_auto() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("parity_read.bin");
        let test_data: Vec<u8> = (0..8192).map(|i| ((i * 11 + 3) % 256) as u8).collect();
        std::fs::write(&path, &test_data).unwrap();

        // Read via Disabled policy
        let mut reader_disabled = reader_from_path(&path, IoUringPolicy::Disabled).unwrap();
        let data_disabled = reader_disabled.read_all().unwrap();

        // Read via Auto policy
        let mut reader_auto = reader_from_path(&path, IoUringPolicy::Auto).unwrap();
        let data_auto = reader_auto.read_all().unwrap();

        assert_eq!(data_disabled.len(), test_data.len());
        assert_eq!(data_disabled, data_auto);
        assert_eq!(data_disabled, test_data);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Partial write and multi-chunk tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn writer_handles_partial_writes_correctly() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("partial_write.bin");
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = writer_from_file(file, 8192, IoUringPolicy::Disabled).unwrap();

        // Write in small chunks to exercise buffering
        for i in 0u8..100 {
            writer.write_all(&[i]).unwrap();
        }
        writer.flush().unwrap();

        let content = std::fs::read(&path).unwrap();
        let expected: Vec<u8> = (0u8..100).collect();
        assert_eq!(content, expected);
    }

    #[test]
    fn writer_large_payload_multi_flush() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("multi_flush.bin");
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = writer_from_file(file, 4096, IoUringPolicy::Disabled).unwrap();

        // Write in chunks larger than buffer capacity to trigger multiple flushes
        let chunk: Vec<u8> = (0..8192).map(|i| (i % 256) as u8).collect();
        for _ in 0..4 {
            writer.write_all(&chunk).unwrap();
        }
        writer.flush().unwrap();

        let content = std::fs::read(&path).unwrap();
        assert_eq!(content.len(), 8192 * 4);
        // Verify each chunk is correct
        for (i, actual_chunk) in content.chunks(8192).enumerate() {
            assert_eq!(actual_chunk, &chunk[..], "chunk {i} mismatch");
        }
    }

    #[test]
    fn reader_partial_reads_via_policy() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("partial_read.bin");
        std::fs::write(&path, b"0123456789ABCDEF").unwrap();

        let mut reader = reader_from_path(&path, IoUringPolicy::Disabled).unwrap();

        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"0123");

        reader.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"4567");

        // Read remaining
        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).unwrap();
        assert_eq!(&rest, b"89ABCDEF");
    }

    #[test]
    fn writer_bytes_written_tracking() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bytes_tracking.bin");
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = writer_from_file(file, 8192, IoUringPolicy::Disabled).unwrap();

        assert_eq!(writer.bytes_written(), 0);
        writer.write_all(b"hello").unwrap();
        assert_eq!(writer.bytes_written(), 5);
        writer.write_all(b" world").unwrap();
        assert_eq!(writer.bytes_written(), 11);
        writer.flush().unwrap();
        assert_eq!(writer.bytes_written(), 11);
    }

    #[test]
    fn reader_size_and_position_tracking() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("position_tracking.bin");
        let data = b"abcdefghijklmnop";
        std::fs::write(&path, data).unwrap();

        let mut reader = reader_from_path(&path, IoUringPolicy::Disabled).unwrap();
        assert_eq!(reader.size(), 16);
        assert_eq!(reader.position(), 0);

        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(reader.position(), 4);
        assert_eq!(reader.remaining(), 12);
    }

    #[test]
    fn write_then_read_roundtrip_via_policy() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("roundtrip.bin");
        let test_data: Vec<u8> = (0..65536).map(|i| ((i * 17 + 5) % 256) as u8).collect();

        // Write
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut writer = writer_from_file(file, 16384, IoUringPolicy::Disabled).unwrap();
            writer.write_all(&test_data).unwrap();
            writer.flush().unwrap();
        }

        // Read back
        let mut reader = reader_from_path(&path, IoUringPolicy::Disabled).unwrap();
        let read_back = reader.read_all().unwrap();

        assert_eq!(read_back.len(), test_data.len());
        assert_eq!(read_back, test_data);
    }

    #[test]
    fn factory_reader_forced_fallback_produces_std() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("factory_fallback.txt");
        std::fs::write(&path, b"factory test").unwrap();

        let factory = IoUringReaderFactory::default().force_fallback(true);
        assert!(!factory.will_use_io_uring());

        let reader = factory.open(&path).unwrap();
        assert!(matches!(reader, IoUringOrStdReader::Std(_)));
    }

    #[test]
    fn factory_writer_forced_fallback_produces_std() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("factory_fallback_write.txt");

        let factory = IoUringWriterFactory::default().force_fallback(true);
        assert!(!factory.will_use_io_uring());

        let writer = factory.create(&path).unwrap();
        assert!(matches!(writer, IoUringOrStdWriter::Std(_)));
    }

    #[test]
    fn empty_file_roundtrip_via_policy() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty_roundtrip.bin");

        {
            let file = std::fs::File::create(&path).unwrap();
            let mut writer = writer_from_file(file, 8192, IoUringPolicy::Disabled).unwrap();
            writer.flush().unwrap();
            assert_eq!(writer.bytes_written(), 0);
        }

        let mut reader = reader_from_path(&path, IoUringPolicy::Disabled).unwrap();
        assert_eq!(reader.size(), 0);
        let data = reader.read_all().unwrap();
        assert!(data.is_empty());
    }

    #[test]
    fn policy_default_is_auto() {
        assert_eq!(IoUringPolicy::default(), IoUringPolicy::Auto);
    }

    #[cfg(unix)]
    #[test]
    fn socket_reader_disabled_policy_uses_std() {
        let (fd_a, fd_b) = {
            let mut fds = [0i32; 2];
            let ret =
                unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
            assert_eq!(ret, 0);
            (fds[0], fds[1])
        };

        let reader = socket_reader_from_fd(fd_b, 8192, IoUringPolicy::Disabled).unwrap();
        assert!(matches!(reader, IoUringOrStdSocketReader::Std(_)));

        unsafe {
            libc::close(fd_a);
            libc::close(fd_b);
        }
    }

    #[cfg(unix)]
    #[test]
    fn socket_writer_disabled_policy_uses_std() {
        let (fd_a, fd_b) = {
            let mut fds = [0i32; 2];
            let ret =
                unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
            assert_eq!(ret, 0);
            (fds[0], fds[1])
        };

        let writer = socket_writer_from_fd(fd_a, 8192, IoUringPolicy::Disabled).unwrap();
        assert!(matches!(writer, IoUringOrStdSocketWriter::Std(_)));

        unsafe {
            libc::close(fd_a);
            libc::close(fd_b);
        }
    }

    #[cfg(unix)]
    #[test]
    fn socket_enabled_policy_returns_error() {
        let (fd_a, fd_b) = {
            let mut fds = [0i32; 2];
            let ret =
                unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
            assert_eq!(ret, 0);
            (fds[0], fds[1])
        };

        let reader_result = socket_reader_from_fd(fd_b, 8192, IoUringPolicy::Enabled);
        match reader_result {
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::Unsupported),
            Ok(_) => panic!("expected Unsupported error for reader"),
        }

        let writer_result = socket_writer_from_fd(fd_a, 8192, IoUringPolicy::Enabled);
        match writer_result {
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::Unsupported),
            Ok(_) => panic!("expected Unsupported error for writer"),
        }

        unsafe {
            libc::close(fd_a);
            libc::close(fd_b);
        }
    }
}
