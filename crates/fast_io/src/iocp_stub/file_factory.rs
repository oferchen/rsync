//! Stub IOCP reader/writer factories and policy-aware constructors.
//!
//! Mirrors the public surface of [`crate::iocp::file_factory`] so cross-platform
//! callers can name [`IocpReaderFactory`], [`IocpWriterFactory`],
//! [`IocpOrStdReader`], and [`IocpOrStdWriter`] without `#[cfg]` branching.
//! On this platform the factories always fall back to standard buffered I/O
//! and the policy-aware helpers reject [`crate::IocpPolicy::Enabled`] with
//! [`io::ErrorKind::Unsupported`].

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::traits::{
    FileReader, FileReaderFactory, FileWriter, FileWriterFactory, StdFileReader, StdFileWriter,
};

use super::{IocpConfig, IocpReader, IocpWriter};

/// Factory that creates IOCP readers (always falls back to standard I/O).
#[derive(Debug, Clone, Default)]
pub struct IocpReaderFactory {
    config: IocpConfig,
    force_fallback: bool,
}

impl IocpReaderFactory {
    /// Creates a factory with custom configuration.
    #[must_use]
    pub fn with_config(config: IocpConfig) -> Self {
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

    /// Returns whether IOCP will be used (always `false`).
    #[must_use]
    pub fn will_use_iocp(&self) -> bool {
        false
    }
}

/// Reader that can be either IOCP-based or standard I/O.
///
/// On this platform, always uses standard I/O.
pub enum IocpOrStdReader {
    /// IOCP-based reader (never constructed on this platform).
    Iocp(IocpReader),
    /// Standard buffered reader.
    Std(StdFileReader),
}

impl std::fmt::Debug for IocpOrStdReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Iocp(_) => f.debug_tuple("Iocp").field(&"<iocp>").finish(),
            Self::Std(_) => f.debug_tuple("Std").field(&"<buffered>").finish(),
        }
    }
}

impl Read for IocpOrStdReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            IocpOrStdReader::Iocp(r) => r.read(buf),
            IocpOrStdReader::Std(r) => r.read(buf),
        }
    }
}

impl FileReader for IocpOrStdReader {
    fn size(&self) -> u64 {
        match self {
            IocpOrStdReader::Iocp(r) => r.size(),
            IocpOrStdReader::Std(r) => r.size(),
        }
    }

    fn position(&self) -> u64 {
        match self {
            IocpOrStdReader::Iocp(r) => r.position(),
            IocpOrStdReader::Std(r) => r.position(),
        }
    }

    fn seek_to(&mut self, pos: u64) -> io::Result<()> {
        match self {
            IocpOrStdReader::Iocp(r) => r.seek_to(pos),
            IocpOrStdReader::Std(r) => r.seek_to(pos),
        }
    }

    fn read_all(&mut self) -> io::Result<Vec<u8>> {
        match self {
            IocpOrStdReader::Iocp(r) => r.read_all(),
            IocpOrStdReader::Std(r) => r.read_all(),
        }
    }
}

impl FileReaderFactory for IocpReaderFactory {
    type Reader = IocpOrStdReader;

    fn open(&self, path: &Path) -> io::Result<Self::Reader> {
        Ok(IocpOrStdReader::Std(StdFileReader::open(path)?))
    }
}

/// Factory that creates IOCP writers (always falls back to standard I/O).
#[derive(Debug, Clone, Default)]
pub struct IocpWriterFactory {
    config: IocpConfig,
    force_fallback: bool,
}

impl IocpWriterFactory {
    /// Creates a factory with custom configuration.
    #[must_use]
    pub fn with_config(config: IocpConfig) -> Self {
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

    /// Returns whether IOCP will be used (always `false`).
    #[must_use]
    pub fn will_use_iocp(&self) -> bool {
        false
    }
}

/// Writer that can be either IOCP-based or standard I/O.
///
/// On this platform, always uses standard I/O.
pub enum IocpOrStdWriter {
    /// IOCP-based writer (never constructed on this platform).
    Iocp(IocpWriter),
    /// Standard buffered writer.
    Std(StdFileWriter),
}

impl std::fmt::Debug for IocpOrStdWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Iocp(_) => f.debug_tuple("Iocp").field(&"<iocp>").finish(),
            Self::Std(_) => f.debug_tuple("Std").field(&"<buffered>").finish(),
        }
    }
}

impl Write for IocpOrStdWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            IocpOrStdWriter::Iocp(w) => w.write(buf),
            IocpOrStdWriter::Std(w) => w.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            IocpOrStdWriter::Iocp(w) => w.flush(),
            IocpOrStdWriter::Std(w) => w.flush(),
        }
    }
}

impl Seek for IocpOrStdWriter {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match self {
            IocpOrStdWriter::Iocp(w) => w.seek(pos),
            IocpOrStdWriter::Std(w) => w.seek(pos),
        }
    }
}

impl FileWriter for IocpOrStdWriter {
    fn bytes_written(&self) -> u64 {
        match self {
            IocpOrStdWriter::Iocp(w) => w.bytes_written(),
            IocpOrStdWriter::Std(w) => w.bytes_written(),
        }
    }

    fn sync(&mut self) -> io::Result<()> {
        match self {
            IocpOrStdWriter::Iocp(w) => w.sync(),
            IocpOrStdWriter::Std(w) => w.sync(),
        }
    }

    fn preallocate(&mut self, size: u64) -> io::Result<()> {
        match self {
            IocpOrStdWriter::Iocp(w) => w.preallocate(size),
            IocpOrStdWriter::Std(w) => w.preallocate(size),
        }
    }
}

impl FileWriterFactory for IocpWriterFactory {
    type Writer = IocpOrStdWriter;

    fn create(&self, path: &Path) -> io::Result<Self::Writer> {
        Ok(IocpOrStdWriter::Std(StdFileWriter::create(path)?))
    }

    fn create_with_size(&self, path: &Path, size: u64) -> io::Result<Self::Writer> {
        Ok(IocpOrStdWriter::Std(StdFileWriter::create_with_size(
            path, size,
        )?))
    }
}

/// Creates a writer from an existing file handle, respecting the IOCP policy.
///
/// On non-Windows platforms, `Enabled` returns an error since IOCP is unavailable.
/// `Auto` and `Disabled` both use standard buffered I/O.
pub fn writer_from_file(
    file: std::fs::File,
    buffer_capacity: usize,
    policy: crate::IocpPolicy,
) -> io::Result<IocpOrStdWriter> {
    if matches!(policy, crate::IocpPolicy::Enabled) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP requested but not available on this platform",
        ));
    }
    Ok(IocpOrStdWriter::Std(
        StdFileWriter::from_file_with_capacity(file, buffer_capacity),
    ))
}

/// Creates a reader from a file path, respecting the IOCP policy.
///
/// On non-Windows platforms, `Enabled` returns an error since IOCP is unavailable.
/// `Auto` and `Disabled` both use standard buffered I/O.
pub fn reader_from_path<P: AsRef<Path>>(
    path: P,
    policy: crate::IocpPolicy,
) -> io::Result<IocpOrStdReader> {
    if matches!(policy, crate::IocpPolicy::Enabled) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP requested but not available on this platform",
        ));
    }
    Ok(IocpOrStdReader::Std(StdFileReader::open(path.as_ref())?))
}
