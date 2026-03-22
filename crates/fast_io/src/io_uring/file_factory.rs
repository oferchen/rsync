//! Factory types and fallback enums for io_uring file I/O.
//!
//! The factories check [`is_io_uring_available`] before each open/create call.
//! When io_uring is unavailable or ring construction fails, they silently return
//! a `Std` variant wrapping standard buffered I/O. Callers interact only with
//! the [`FileReader`] / [`FileWriter`] traits, unaware of the backend.

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use super::config::{IoUringConfig, is_io_uring_available};
use super::file_reader::IoUringReader;
use super::file_writer::IoUringWriter;
use crate::traits::{FileReader, FileReaderFactory, FileWriter, FileWriterFactory};

/// Factory that creates io_uring readers when available, with fallback to standard I/O.
///
/// On each `open()` call, checks [`is_io_uring_available`] (cached atomic) and
/// `force_fallback`. If io_uring is eligible, attempts to open an
/// [`IoUringReader`]; on any failure, returns a [`StdFileReader`] instead.
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
///
/// Created by [`IoUringReaderFactory`] or [`reader_from_path`](super::reader_from_path).
/// The variant is chosen at construction time based on io_uring availability;
/// callers use the [`FileReader`] trait and never branch on the variant.
#[allow(clippy::large_enum_variant)]
pub enum IoUringOrStdReader {
    /// io_uring-based reader (Linux 5.6+ with `io_uring` feature).
    IoUring(IoUringReader),
    /// Standard buffered reader (fallback on all platforms).
    Std(crate::traits::StdFileReader),
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
///
/// On each `create()` call, checks [`is_io_uring_available`] (cached atomic) and
/// `force_fallback`. If io_uring is eligible, attempts to create an
/// [`IoUringWriter`]; on any failure, returns a [`StdFileWriter`] instead.
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
///
/// Created by [`IoUringWriterFactory`] or [`writer_from_file`](super::writer_from_file).
/// The variant is chosen at construction time based on io_uring availability;
/// callers use the [`FileWriter`] trait and never branch on the variant.
#[allow(clippy::large_enum_variant)]
pub enum IoUringOrStdWriter {
    /// io_uring-based writer (Linux 5.6+ with `io_uring` feature).
    IoUring(IoUringWriter),
    /// Standard buffered writer (fallback on all platforms).
    Std(crate::traits::StdFileWriter),
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
