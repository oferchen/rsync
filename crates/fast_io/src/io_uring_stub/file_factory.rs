//! Stub file factories mirroring [`crate::io_uring::file_factory`].
//!
//! Always falls back to standard buffered I/O.

use super::file_reader::IoUringReader;
use super::file_writer::IoUringWriter;
use crate::io_uring_common::IoUringConfig;
use crate::traits::{
    FileReader, FileReaderFactory, FileWriter, FileWriterFactory, StdFileReader, StdFileWriter,
};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

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
    writer_from_file_with_depth(file, buffer_capacity, policy, None)
}

/// Like [`writer_from_file`] but accepts an explicit submission queue depth.
pub fn writer_from_file_with_depth(
    file: std::fs::File,
    buffer_capacity: usize,
    policy: crate::IoUringPolicy,
    _depth: Option<u32>,
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
    reader_from_path_with_depth(path, policy, None)
}

/// Like [`reader_from_path`] but accepts an explicit submission queue depth.
pub fn reader_from_path_with_depth<P: AsRef<Path>>(
    path: P,
    policy: crate::IoUringPolicy,
    _depth: Option<u32>,
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
