//! Factory types and fallback enums for io_uring file I/O.

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use super::config::{IoUringConfig, is_io_uring_available};
use super::file_reader::IoUringReader;
use super::file_writer::IoUringWriter;
use crate::traits::{FileReader, FileReaderFactory, FileWriter, FileWriterFactory};

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
