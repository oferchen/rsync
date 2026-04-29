//! Factory types and enum wrappers for IOCP reader/writer.
//!
//! Mirrors the io_uring factory pattern: each factory checks availability
//! and returns either an IOCP or Std variant. The enum wrappers dispatch
//! trait methods to the underlying implementation.

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use super::config::{IOCP_MIN_FILE_SIZE, IocpConfig, is_iocp_available};
use super::file_reader::IocpReader;
use super::file_writer::IocpWriter;
use crate::traits::{
    FileReader, FileReaderFactory, FileWriter, FileWriterFactory, StdFileReader, StdFileWriter,
};

/// Reader that is either IOCP-based or standard buffered I/O.
pub enum IocpOrStdReader {
    /// IOCP-based reader using overlapped I/O.
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
            Self::Iocp(r) => r.read(buf),
            Self::Std(r) => r.read(buf),
        }
    }
}

impl FileReader for IocpOrStdReader {
    fn size(&self) -> u64 {
        match self {
            Self::Iocp(r) => r.size(),
            Self::Std(r) => r.size(),
        }
    }

    fn position(&self) -> u64 {
        match self {
            Self::Iocp(r) => r.position(),
            Self::Std(r) => r.position(),
        }
    }

    fn seek_to(&mut self, pos: u64) -> io::Result<()> {
        match self {
            Self::Iocp(r) => r.seek_to(pos),
            Self::Std(r) => r.seek_to(pos),
        }
    }

    fn read_all(&mut self) -> io::Result<Vec<u8>> {
        match self {
            Self::Iocp(r) => r.read_all(),
            Self::Std(r) => r.read_all(),
        }
    }
}

/// Writer that is either IOCP-based or standard buffered I/O.
pub enum IocpOrStdWriter {
    /// IOCP-based writer using overlapped I/O.
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
            Self::Iocp(w) => w.write(buf),
            Self::Std(w) => w.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Iocp(w) => w.flush(),
            Self::Std(w) => w.flush(),
        }
    }
}

impl Seek for IocpOrStdWriter {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match self {
            Self::Iocp(w) => w.seek(pos),
            Self::Std(w) => w.seek(pos),
        }
    }
}

impl FileWriter for IocpOrStdWriter {
    fn bytes_written(&self) -> u64 {
        match self {
            Self::Iocp(w) => w.bytes_written(),
            Self::Std(w) => w.bytes_written(),
        }
    }

    fn sync(&mut self) -> io::Result<()> {
        match self {
            Self::Iocp(w) => w.sync(),
            Self::Std(w) => w.sync(),
        }
    }

    fn preallocate(&mut self, size: u64) -> io::Result<()> {
        match self {
            Self::Iocp(w) => w.preallocate(size),
            Self::Std(w) => w.preallocate(size),
        }
    }
}

/// Factory that creates IOCP readers with automatic fallback.
///
/// When IOCP is available and the file is large enough to benefit from
/// async I/O, returns an IOCP reader. Otherwise, returns a standard
/// buffered reader.
#[derive(Debug, Clone)]
pub struct IocpReaderFactory {
    config: IocpConfig,
    force_fallback: bool,
}

impl Default for IocpReaderFactory {
    fn default() -> Self {
        Self {
            config: IocpConfig::default(),
            force_fallback: false,
        }
    }
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

    /// Forces fallback to standard I/O regardless of IOCP availability.
    #[must_use]
    pub fn force_fallback(mut self, force: bool) -> Self {
        self.force_fallback = force;
        self
    }

    /// Returns whether IOCP will be used for reads.
    #[must_use]
    pub fn will_use_iocp(&self) -> bool {
        !self.force_fallback && is_iocp_available()
    }
}

impl FileReaderFactory for IocpReaderFactory {
    type Reader = IocpOrStdReader;

    fn open(&self, path: &Path) -> io::Result<Self::Reader> {
        if self.will_use_iocp() {
            // Files below IOCP_MIN_FILE_SIZE are read synchronously because the
            // overlapped-I/O setup overhead exceeds the async benefit at that size.
            let metadata = std::fs::metadata(path)?;
            if metadata.len() >= IOCP_MIN_FILE_SIZE {
                if let Ok(reader) = IocpReader::open(path, &self.config) {
                    return Ok(IocpOrStdReader::Iocp(reader));
                }
            }
        }
        Ok(IocpOrStdReader::Std(StdFileReader::open(path)?))
    }
}

/// Factory that creates IOCP writers with automatic fallback.
#[derive(Debug, Clone)]
pub struct IocpWriterFactory {
    config: IocpConfig,
    force_fallback: bool,
}

impl Default for IocpWriterFactory {
    fn default() -> Self {
        Self {
            config: IocpConfig::default(),
            force_fallback: false,
        }
    }
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

    /// Forces fallback to standard I/O regardless of IOCP availability.
    #[must_use]
    pub fn force_fallback(mut self, force: bool) -> Self {
        self.force_fallback = force;
        self
    }

    /// Returns whether IOCP will be used for writes.
    #[must_use]
    pub fn will_use_iocp(&self) -> bool {
        !self.force_fallback && is_iocp_available()
    }
}

impl FileWriterFactory for IocpWriterFactory {
    type Writer = IocpOrStdWriter;

    fn create(&self, path: &Path) -> io::Result<Self::Writer> {
        if self.will_use_iocp() {
            if let Ok(writer) = IocpWriter::create(path, &self.config) {
                return Ok(IocpOrStdWriter::Iocp(writer));
            }
        }
        Ok(IocpOrStdWriter::Std(StdFileWriter::create(path)?))
    }

    fn create_with_size(&self, path: &Path, size: u64) -> io::Result<Self::Writer> {
        if self.will_use_iocp() {
            if let Ok(writer) = IocpWriter::create_with_size(path, size, &self.config) {
                return Ok(IocpOrStdWriter::Iocp(writer));
            }
        }
        Ok(IocpOrStdWriter::Std(StdFileWriter::create_with_size(
            path, size,
        )?))
    }
}

/// Creates a writer from an existing std::fs::File, respecting the IOCP policy.
///
/// `Enabled` forces IOCP (error if unavailable). `Auto` uses IOCP if available.
/// `Disabled` always uses standard I/O.
pub fn writer_from_file(
    file: std::fs::File,
    buffer_capacity: usize,
    policy: crate::IocpPolicy,
) -> io::Result<IocpOrStdWriter> {
    match policy {
        crate::IocpPolicy::Enabled => {
            if !is_iocp_available() {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "IOCP requested but not available on this system",
                ));
            }
            // An existing std::fs::File was not opened with FILE_FLAG_OVERLAPPED,
            // so it cannot be associated with a completion port. Fall back to
            // standard buffered I/O even under the Enabled policy.
            Ok(IocpOrStdWriter::Std(
                StdFileWriter::from_file_with_capacity(file, buffer_capacity),
            ))
        }
        crate::IocpPolicy::Auto | crate::IocpPolicy::Disabled => Ok(IocpOrStdWriter::Std(
            StdFileWriter::from_file_with_capacity(file, buffer_capacity),
        )),
    }
}

/// Creates a reader from a file path, respecting the IOCP policy.
///
/// `Enabled` forces IOCP (error if unavailable). `Auto` uses IOCP for
/// large files if available. `Disabled` always uses standard I/O.
pub fn reader_from_path<P: AsRef<Path>>(
    path: P,
    policy: crate::IocpPolicy,
) -> io::Result<IocpOrStdReader> {
    match policy {
        crate::IocpPolicy::Enabled => {
            if !is_iocp_available() {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "IOCP requested but not available on this system",
                ));
            }
            let config = IocpConfig::default();
            Ok(IocpOrStdReader::Iocp(IocpReader::open(
                path.as_ref(),
                &config,
            )?))
        }
        crate::IocpPolicy::Auto => {
            if is_iocp_available() {
                let metadata = std::fs::metadata(path.as_ref())?;
                if metadata.len() >= IOCP_MIN_FILE_SIZE {
                    let config = IocpConfig::default();
                    if let Ok(reader) = IocpReader::open(path.as_ref(), &config) {
                        return Ok(IocpOrStdReader::Iocp(reader));
                    }
                }
            }
            Ok(IocpOrStdReader::Std(StdFileReader::open(path.as_ref())?))
        }
        crate::IocpPolicy::Disabled => {
            Ok(IocpOrStdReader::Std(StdFileReader::open(path.as_ref())?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IocpPolicy;
    use tempfile::tempdir;

    #[test]
    fn factory_reader_opens_std_for_small_files() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("small.txt");
        std::fs::write(&path, b"tiny").unwrap();

        let factory = IocpReaderFactory::default();
        let reader = factory.open(&path).unwrap();
        assert!(matches!(reader, IocpOrStdReader::Std(_)));
    }

    #[test]
    fn factory_reader_forced_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("forced.bin");
        let data = vec![0u8; 128 * 1024]; // > IOCP_MIN_FILE_SIZE
        std::fs::write(&path, &data).unwrap();

        let factory = IocpReaderFactory::default().force_fallback(true);
        assert!(!factory.will_use_iocp());
        let reader = factory.open(&path).unwrap();
        assert!(matches!(reader, IocpOrStdReader::Std(_)));
    }

    #[test]
    fn factory_writer_creates_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("factory_write.txt");

        let factory = IocpWriterFactory::default();
        let mut writer = factory.create(&path).unwrap();
        writer.write_all(b"factory test").unwrap();
        writer.flush().unwrap();
        drop(writer);

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "factory test");
    }

    #[test]
    fn factory_writer_forced_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("forced_write.txt");

        let factory = IocpWriterFactory::default().force_fallback(true);
        assert!(!factory.will_use_iocp());
        let writer = factory.create(&path).unwrap();
        assert!(matches!(writer, IocpOrStdWriter::Std(_)));
    }

    #[test]
    fn reader_from_path_disabled_uses_std() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("disabled.txt");
        std::fs::write(&path, b"disabled test").unwrap();

        let reader = reader_from_path(&path, IocpPolicy::Disabled).unwrap();
        assert!(matches!(reader, IocpOrStdReader::Std(_)));
    }

    #[test]
    fn writer_from_file_disabled_uses_std() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("writer_disabled.txt");
        let file = std::fs::File::create(&path).unwrap();

        let writer = writer_from_file(file, 8192, IocpPolicy::Disabled).unwrap();
        assert!(matches!(writer, IocpOrStdWriter::Std(_)));
    }

    #[test]
    fn reader_writer_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("roundtrip.bin");
        let test_data: Vec<u8> = (0..65536).map(|i| ((i * 17 + 5) % 256) as u8).collect();

        {
            let factory = IocpWriterFactory::default();
            let mut writer = factory.create(&path).unwrap();
            writer.write_all(&test_data).unwrap();
            writer.flush().unwrap();
        }

        let factory = IocpReaderFactory::default();
        let mut reader = factory.open(&path).unwrap();
        let read_back = reader.read_all().unwrap();

        assert_eq!(read_back, test_data);
    }
}
