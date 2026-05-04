//! Portable IOCP fallback for non-Windows platforms or when the feature is disabled.
//!
//! Provides the same public API as the real `iocp` module but always falls
//! back to standard buffered I/O. The `is_iocp_available()` function always
//! returns `false`. This module is compiled when either:
//!
//! - The target OS is not Windows, or
//! - The `iocp` cargo feature is not enabled
//!
//! All factory types ([`IocpReaderFactory`], [`IocpWriterFactory`]) produce
//! `Std` variants directly. The stub types ([`IocpReader`], [`IocpWriter`])
//! cannot be constructed and exist only for enum variant completeness.

#![allow(dead_code)]

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::traits::{
    FileReader, FileReaderFactory, FileWriter, FileWriterFactory, StdFileReader, StdFileWriter,
};

/// Minimum file size threshold (informational only on this platform).
pub const IOCP_MIN_FILE_SIZE: u64 = 64 * 1024;

/// Typed IOCP error variants.
///
/// On non-Windows platforms the IOCP backend is never constructed, so this
/// type exists purely to keep cross-platform callers compiling. Both variants
/// implement `From<IocpError> for io::Error` to match the Windows surface.
#[derive(Debug, thiserror::Error)]
pub enum IocpError {
    /// Mirrors the Windows `ERROR_INVALID_PARAMETER` mapping.
    #[error("IOCP overlapped operation rejected with ERROR_INVALID_PARAMETER: {context}")]
    InvalidOperation {
        /// Free-form context describing the call site.
        context: &'static str,
    },
    /// Mirrors the Windows `ERROR_INSUFFICIENT_BUFFER` mapping.
    #[error(
        "IOCP completion drain ran out of buffer space ({requested} entries requested, capacity {capacity})"
    )]
    InsufficientBuffer {
        /// Number of completion entries the kernel wanted to deliver.
        requested: u32,
        /// Number of entries the buffer could hold.
        capacity: u32,
    },
}

impl From<IocpError> for io::Error {
    fn from(err: IocpError) -> Self {
        match err {
            IocpError::InvalidOperation { .. } => io::Error::new(io::ErrorKind::InvalidInput, err),
            IocpError::InsufficientBuffer { .. } => io::Error::new(io::ErrorKind::OutOfMemory, err),
        }
    }
}

/// Check whether IOCP is available (always `false` on this platform).
#[must_use]
pub fn is_iocp_available() -> bool {
    false
}

/// Returns whether FILE_SKIP_SET_EVENT_ON_HANDLE is available (always `false`).
#[must_use]
pub fn skip_event_optimization_available() -> bool {
    false
}

/// Returns a human-readable string describing IOCP availability.
#[must_use]
pub fn iocp_availability_reason() -> String {
    "IOCP unavailable: platform is not Windows".to_string()
}

/// Configuration for IOCP instances (informational only on this platform).
#[derive(Debug, Clone)]
pub struct IocpConfig {
    /// Number of concurrent I/O operations.
    pub concurrent_ops: u32,
    /// Size of each I/O buffer.
    pub buffer_size: usize,
    /// Whether to use unbuffered I/O (no-op on non-Windows).
    pub unbuffered: bool,
    /// Whether to use write-through (no-op on non-Windows).
    pub write_through: bool,
}

impl Default for IocpConfig {
    fn default() -> Self {
        Self {
            concurrent_ops: 4,
            buffer_size: 64 * 1024,
            unbuffered: false,
            write_through: false,
        }
    }
}

impl IocpConfig {
    /// Creates a config optimized for large file transfers.
    #[must_use]
    pub fn for_large_files() -> Self {
        Self {
            concurrent_ops: 8,
            buffer_size: 256 * 1024,
            unbuffered: false,
            write_through: false,
        }
    }

    /// Creates a config optimized for many small files.
    #[must_use]
    pub fn for_small_files() -> Self {
        Self {
            concurrent_ops: 4,
            buffer_size: 16 * 1024,
            unbuffered: false,
            write_through: false,
        }
    }
}

/// Stub IOCP reader (not available on this platform).
///
/// Opening always fails with `Unsupported`.
pub struct IocpReader {
    _private: (),
}

impl IocpReader {
    /// Always returns an `Unsupported` error on this platform.
    pub fn open<P: AsRef<Path>>(_path: P, _config: &IocpConfig) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }

    /// Reads data at the specified offset.
    pub fn read_at(&mut self, _offset: u64, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }

    /// Reads the entire file into a vector.
    pub fn read_all_batched(&mut self) -> io::Result<Vec<u8>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }
}

impl Read for IocpReader {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }
}

impl FileReader for IocpReader {
    fn size(&self) -> u64 {
        0
    }

    fn position(&self) -> u64 {
        0
    }

    fn seek_to(&mut self, _pos: u64) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }

    fn read_all(&mut self) -> io::Result<Vec<u8>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }
}

/// Stub IOCP writer (not available on this platform).
///
/// Creating always fails with `Unsupported`.
pub struct IocpWriter {
    _private: (),
}

impl IocpWriter {
    /// Always returns an `Unsupported` error on this platform.
    pub fn create<P: AsRef<Path>>(_path: P, _config: &IocpConfig) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }

    /// Creates a file with preallocated space (always fails on this platform).
    pub fn create_with_size<P: AsRef<Path>>(
        _path: P,
        _size: u64,
        _config: &IocpConfig,
    ) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }
}

impl Write for IocpWriter {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }
}

impl Seek for IocpWriter {
    fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }
}

impl FileWriter for IocpWriter {
    fn bytes_written(&self) -> u64 {
        0
    }

    fn sync(&mut self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }

    fn preallocate(&mut self, _size: u64) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }
}

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

/// Boxed completion-handler type mirroring the Windows pump API.
///
/// On non-Windows platforms the pump is never constructed, so the alias
/// exists only to keep downstream code that names the type compiling.
pub type CompletionHandler = Box<dyn FnOnce(io::Result<u32>) + Send + 'static>;

/// Configuration mirror for [`CompletionPump`] on non-Windows platforms.
#[derive(Debug, Clone, Default)]
pub struct IocpPumpConfig {
    /// Maximum concurrent worker threads (informational only on this platform).
    pub max_concurrent_threads: u32,
    /// Drain batch size (informational only on this platform).
    pub batch_size: usize,
}

/// Stub IOCP completion-port pump.
///
/// Construction always fails with [`io::ErrorKind::Unsupported`]. The type
/// exists so downstream callers can reference it from cross-platform code
/// behind a runtime check on [`is_iocp_available`].
#[derive(Debug)]
pub struct CompletionPump {
    _private: (),
}

impl CompletionPump {
    /// Returns `Unsupported` on this platform.
    pub fn new() -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP completion pump is not available on this platform",
        ))
    }

    /// Returns `Unsupported` on this platform.
    pub fn with_config(_config: IocpPumpConfig) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP completion pump is not available on this platform",
        ))
    }

    /// Returns `false` on this platform; the pump is never running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        false
    }

    /// Always reports zero pending operations on this platform.
    #[must_use]
    pub fn pending_ops(&self) -> usize {
        0
    }

    /// Always returns `Ok(())` on this platform.
    pub fn shutdown(self) -> io::Result<()> {
        Ok(())
    }
}

/// Stub `oneshot_handler` matching the Windows API.
///
/// Returns a no-op handler and an empty receiver because the pump cannot be
/// constructed on this platform; the receiver will never produce a value.
#[must_use]
pub fn oneshot_handler() -> (
    CompletionHandler,
    std::sync::mpsc::Receiver<io::Result<u32>>,
) {
    let (_tx, rx) = std::sync::mpsc::channel::<io::Result<u32>>();
    let handler: CompletionHandler = Box::new(|_| {});
    (handler, rx)
}

/// Cross-platform stub for the Windows-only `iocp::socket` module (issue #1928).
///
/// Mirrors the public surface of [`crate::iocp::socket`] on Windows so code
/// that names `IocpSocketReader` / `IocpSocketWriter` behind a runtime check
/// against [`is_iocp_available`] still compiles on Linux and macOS. All
/// constructors and methods return [`io::ErrorKind::Unsupported`].
pub mod socket {
    use super::CompletionPump;
    use std::io;
    use std::sync::Arc;

    /// Shared completion-pump reference - matches the Windows alias so
    /// downstream APIs keep their signatures unchanged across platforms.
    pub type SharedPump = Arc<CompletionPump>;

    /// Stub IOCP socket reader. Construction always fails with
    /// [`io::ErrorKind::Unsupported`].
    pub struct IocpSocketReader {
        _private: (),
    }

    impl IocpSocketReader {
        /// Returns a stub instance for type compatibility - never used because
        /// the pump cannot be constructed on this platform. Consumers behind
        /// a runtime IOCP check never reach here.
        #[must_use]
        pub fn from_raw_socket(_socket: u64, _pump: SharedPump) -> Self {
            Self { _private: () }
        }

        /// Returns `Unsupported` on this platform.
        pub fn associate(_socket: u64, _pump: SharedPump) -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "IOCP socket reader is not available on this platform",
            ))
        }

        /// Returns `Unsupported` on this platform.
        pub fn recv_async(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "IOCP recv_async is not available on this platform",
            ))
        }

        /// Override the per-socket completion key reported by the pump.
        #[must_use]
        pub fn with_completion_key(self, _key: usize) -> Self {
            self
        }

        /// Returns the completion key (always `0` on this platform).
        #[must_use]
        pub fn completion_key(&self) -> usize {
            0
        }
    }

    /// Stub IOCP socket writer. Construction always fails with
    /// [`io::ErrorKind::Unsupported`].
    pub struct IocpSocketWriter {
        _private: (),
    }

    impl IocpSocketWriter {
        /// Returns a stub instance for type compatibility.
        #[must_use]
        pub fn from_raw_socket(_socket: u64, _pump: SharedPump) -> Self {
            Self { _private: () }
        }

        /// Returns `Unsupported` on this platform.
        pub fn associate(_socket: u64, _pump: SharedPump) -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "IOCP socket writer is not available on this platform",
            ))
        }

        /// Returns `Unsupported` on this platform.
        pub fn send_async(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "IOCP send_async is not available on this platform",
            ))
        }

        /// Override the per-socket completion key reported by the pump.
        #[must_use]
        pub fn with_completion_key(self, _key: usize) -> Self {
            self
        }

        /// Returns the completion key (always `0` on this platform).
        #[must_use]
        pub fn completion_key(&self) -> usize {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IocpPolicy;
    use crate::traits::{FileReader, FileWriter};
    use std::io::Write;
    use tempfile::{NamedTempFile, tempdir};

    #[test]
    fn iocp_unavailable_on_stub_platform() {
        assert!(!is_iocp_available());
    }

    #[test]
    fn skip_event_unavailable_on_stub_platform() {
        assert!(!skip_event_optimization_available());
    }

    #[test]
    fn availability_reason_mentions_platform() {
        let reason = iocp_availability_reason();
        assert!(reason.contains("not Windows"));
    }

    #[test]
    fn config_default_values() {
        let config = IocpConfig::default();
        assert_eq!(config.concurrent_ops, 4);
        assert_eq!(config.buffer_size, 64 * 1024);
    }

    #[test]
    fn policy_disabled_writer_uses_std() {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(b"").unwrap();
        let file = tmp.reopen().unwrap();

        let writer = writer_from_file(file, 8192, IocpPolicy::Disabled).unwrap();
        assert!(matches!(writer, IocpOrStdWriter::Std(_)));
    }

    #[test]
    fn policy_disabled_reader_uses_std() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("disabled_reader.txt");
        std::fs::write(&path, b"hello").unwrap();

        let reader = reader_from_path(&path, IocpPolicy::Disabled).unwrap();
        assert!(matches!(reader, IocpOrStdReader::Std(_)));
    }

    #[test]
    fn policy_auto_falls_back_to_std_writer() {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(b"").unwrap();
        let file = tmp.reopen().unwrap();

        let writer = writer_from_file(file, 8192, IocpPolicy::Auto).unwrap();
        assert!(matches!(writer, IocpOrStdWriter::Std(_)));
    }

    #[test]
    fn policy_auto_falls_back_to_std_reader() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("auto_reader.txt");
        std::fs::write(&path, b"world").unwrap();

        let reader = reader_from_path(&path, IocpPolicy::Auto).unwrap();
        assert!(matches!(reader, IocpOrStdReader::Std(_)));
    }

    #[test]
    fn policy_enabled_writer_returns_error() {
        let tmp = NamedTempFile::new().unwrap();
        let file = tmp.reopen().unwrap();

        let result = writer_from_file(file, 8192, IocpPolicy::Enabled);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        assert!(err.to_string().contains("IOCP"));
    }

    #[test]
    fn policy_enabled_reader_returns_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("enabled_reader.txt");
        std::fs::write(&path, b"data").unwrap();

        let result = reader_from_path(&path, IocpPolicy::Enabled);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        assert!(err.to_string().contains("IOCP"));
    }

    #[test]
    fn writer_parity_disabled_vs_auto() {
        let test_data: Vec<u8> = (0..4096).map(|i| ((i * 7 + 13) % 256) as u8).collect();

        let dir = tempdir().unwrap();
        let path_disabled = dir.path().join("parity_disabled.bin");
        {
            let file = std::fs::File::create(&path_disabled).unwrap();
            let mut writer = writer_from_file(file, 8192, IocpPolicy::Disabled).unwrap();
            writer.write_all(&test_data).unwrap();
            writer.flush().unwrap();
        }

        let path_auto = dir.path().join("parity_auto.bin");
        {
            let file = std::fs::File::create(&path_auto).unwrap();
            let mut writer = writer_from_file(file, 8192, IocpPolicy::Auto).unwrap();
            writer.write_all(&test_data).unwrap();
            writer.flush().unwrap();
        }

        let content_disabled = std::fs::read(&path_disabled).unwrap();
        let content_auto = std::fs::read(&path_auto).unwrap();
        assert_eq!(content_disabled, content_auto);
        assert_eq!(content_disabled, test_data);
    }

    #[test]
    fn reader_parity_disabled_vs_auto() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("parity_read.bin");
        let test_data: Vec<u8> = (0..8192).map(|i| ((i * 11 + 3) % 256) as u8).collect();
        std::fs::write(&path, &test_data).unwrap();

        let mut reader_disabled = reader_from_path(&path, IocpPolicy::Disabled).unwrap();
        let data_disabled = reader_disabled.read_all().unwrap();

        let mut reader_auto = reader_from_path(&path, IocpPolicy::Auto).unwrap();
        let data_auto = reader_auto.read_all().unwrap();

        assert_eq!(data_disabled, data_auto);
        assert_eq!(data_disabled, test_data);
    }

    #[test]
    fn factory_reader_forced_fallback_produces_std() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("factory_fallback.txt");
        std::fs::write(&path, b"factory test").unwrap();

        let factory = IocpReaderFactory::default().force_fallback(true);
        assert!(!factory.will_use_iocp());
        let reader = factory.open(&path).unwrap();
        assert!(matches!(reader, IocpOrStdReader::Std(_)));
    }

    #[test]
    fn factory_writer_forced_fallback_produces_std() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("factory_fallback_write.txt");

        let factory = IocpWriterFactory::default().force_fallback(true);
        assert!(!factory.will_use_iocp());
        let writer = factory.create(&path).unwrap();
        assert!(matches!(writer, IocpOrStdWriter::Std(_)));
    }

    #[test]
    fn write_then_read_roundtrip_via_policy() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("roundtrip.bin");
        let test_data: Vec<u8> = (0..65536).map(|i| ((i * 17 + 5) % 256) as u8).collect();

        {
            let file = std::fs::File::create(&path).unwrap();
            let mut writer = writer_from_file(file, 16384, IocpPolicy::Disabled).unwrap();
            writer.write_all(&test_data).unwrap();
            writer.flush().unwrap();
        }

        let mut reader = reader_from_path(&path, IocpPolicy::Disabled).unwrap();
        let read_back = reader.read_all().unwrap();
        assert_eq!(read_back, test_data);
    }

    #[test]
    fn empty_file_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty.bin");

        {
            let file = std::fs::File::create(&path).unwrap();
            let mut writer = writer_from_file(file, 8192, IocpPolicy::Disabled).unwrap();
            writer.flush().unwrap();
            assert_eq!(writer.bytes_written(), 0);
        }

        let mut reader = reader_from_path(&path, IocpPolicy::Disabled).unwrap();
        assert_eq!(reader.size(), 0);
        let data = reader.read_all().unwrap();
        assert!(data.is_empty());
    }

    #[test]
    fn policy_default_is_auto() {
        assert_eq!(IocpPolicy::default(), IocpPolicy::Auto);
    }

    #[test]
    fn pump_construction_unsupported_on_stub_platform() {
        let result = CompletionPump::new();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn pump_with_config_unsupported_on_stub_platform() {
        let result = CompletionPump::with_config(IocpPumpConfig::default());
        assert!(result.is_err());
    }

    #[test]
    fn oneshot_handler_returns_no_op_handler() {
        let (handler, rx) = oneshot_handler();
        // The handler is callable; it just discards the result on this
        // platform because no real pump can fire it.
        handler(Ok(0));
        assert!(rx.try_recv().is_err());
    }
}
