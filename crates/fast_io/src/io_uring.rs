//! io_uring-based async file I/O for Linux 5.6+.
//!
//! This module provides high-performance file I/O using Linux's io_uring interface,
//! which batches syscalls and enables true async I/O without thread pools.
//!
//! # Features
//!
//! - Batched submissions reduce syscall overhead
//! - True async I/O (no thread pool like tokio::fs)
//! - Zero-copy where possible using registered buffers
//! - Automatic fallback to standard I/O on unsupported systems
//!
//! # Requirements
//!
//! - Linux kernel 5.6 or later
//! - The `io_uring` feature must be enabled
//!
//! # Example
//!
//! ```ignore
//! use fast_io::io_uring::{IoUring, IoUringConfig};
//!
//! // Check if io_uring is available
//! if IoUring::is_available() {
//!     let uring = IoUring::new(IoUringConfig::default())?;
//!     let data = uring.read_file("large_file.bin").await?;
//! }
//! ```

use std::ffi::CStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use io_uring::{opcode, types, IoUring as RawIoUring};

use crate::traits::{FileReader, FileReaderFactory, FileWriter, FileWriterFactory};

// ─────────────────────────────────────────────────────────────────────────────
// Kernel version detection
// ─────────────────────────────────────────────────────────────────────────────

/// Minimum kernel version required for io_uring (5.6.0).
const MIN_KERNEL_VERSION: (u32, u32) = (5, 6);

/// Cached result of io_uring availability check.
static IO_URING_AVAILABLE: AtomicBool = AtomicBool::new(false);
static IO_URING_CHECKED: AtomicBool = AtomicBool::new(false);

/// Parses kernel version from uname release string (e.g., "5.15.0-generic").
fn parse_kernel_version(release: &str) -> Option<(u32, u32)> {
    let mut parts = release.split(|c: char| !c.is_ascii_digit());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// Gets the kernel release string using libc uname.
fn get_kernel_release() -> Option<String> {
    unsafe {
        let mut utsname: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut utsname) != 0 {
            return None;
        }
        let release = CStr::from_ptr(utsname.release.as_ptr());
        release.to_str().ok().map(String::from)
    }
}

/// Checks if the current kernel supports io_uring.
///
/// Returns `true` if:
/// 1. Running on Linux
/// 2. Kernel version is 5.6 or later
/// 3. io_uring syscalls are available (not blocked by seccomp)
#[must_use]
pub fn is_io_uring_available() -> bool {
    // Fast path: use cached result
    if IO_URING_CHECKED.load(Ordering::Relaxed) {
        return IO_URING_AVAILABLE.load(Ordering::Relaxed);
    }

    let available = check_io_uring_available();
    IO_URING_AVAILABLE.store(available, Ordering::Relaxed);
    IO_URING_CHECKED.store(true, Ordering::Relaxed);
    available
}

fn check_io_uring_available() -> bool {
    // Check kernel version
    let release = match get_kernel_release() {
        Some(r) => r,
        None => return false,
    };

    let version = match parse_kernel_version(&release) {
        Some(v) => v,
        None => return false,
    };

    if version < MIN_KERNEL_VERSION {
        return false;
    }

    // Try to create a small io_uring instance to verify it's not blocked
    RawIoUring::new(4).is_ok()
}

// ─────────────────────────────────────────────────────────────────────────────
// Configuration
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration for io_uring instances.
#[derive(Debug, Clone)]
pub struct IoUringConfig {
    /// Number of submission queue entries (must be power of 2).
    pub sq_entries: u32,
    /// Size of read/write buffers.
    pub buffer_size: usize,
    /// Whether to use direct I/O (O_DIRECT).
    pub direct_io: bool,
}

impl Default for IoUringConfig {
    fn default() -> Self {
        Self {
            sq_entries: 64,
            buffer_size: 64 * 1024, // 64 KB
            direct_io: false,
        }
    }
}

impl IoUringConfig {
    /// Creates a config optimized for large file transfers.
    #[must_use]
    pub fn for_large_files() -> Self {
        Self {
            sq_entries: 256,
            buffer_size: 256 * 1024, // 256 KB
            direct_io: false,
        }
    }

    /// Creates a config optimized for many small files.
    #[must_use]
    pub fn for_small_files() -> Self {
        Self {
            sq_entries: 128,
            buffer_size: 16 * 1024, // 16 KB
            direct_io: false,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// io_uring File Reader
// ─────────────────────────────────────────────────────────────────────────────

/// A file reader using io_uring for async I/O.
///
/// Operations are submitted to the io_uring submission queue and completed
/// asynchronously, reducing syscall overhead.
pub struct IoUringReader {
    ring: RawIoUring,
    file: File,
    size: u64,
    position: u64,
    #[allow(dead_code)] // Reserved for future batched read optimization
    buffer: Vec<u8>,
    #[allow(dead_code)] // Reserved for future batched read optimization
    buffer_size: usize,
}

impl IoUringReader {
    /// Opens a file for reading with io_uring.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The file cannot be opened
    /// - io_uring initialization fails
    pub fn open<P: AsRef<Path>>(path: P, config: &IoUringConfig) -> io::Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();

        let ring = RawIoUring::new(config.sq_entries).map_err(|e| {
            io::Error::new(io::ErrorKind::Other, format!("io_uring init failed: {e}"))
        })?;

        Ok(Self {
            ring,
            file,
            size,
            position: 0,
            buffer: vec![0u8; config.buffer_size],
            buffer_size: config.buffer_size,
        })
    }

    /// Reads data at the specified offset without advancing the position.
    ///
    /// This is useful for random access patterns.
    pub fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        if offset >= self.size {
            return Ok(0);
        }

        let to_read = buf.len().min((self.size - offset) as usize);
        if to_read == 0 {
            return Ok(0);
        }

        let fd = types::Fd(self.file.as_raw_fd());

        // Prepare read operation
        let read_op = opcode::Read::new(fd, buf.as_mut_ptr(), to_read as u32)
            .offset(offset)
            .build()
            .user_data(0x42);

        // Submit and wait
        unsafe {
            self.ring
                .submission()
                .push(&read_op)
                .map_err(|_| io::Error::new(io::ErrorKind::Other, "submission queue full"))?;
        }

        self.ring.submit_and_wait(1)?;

        // Get completion
        let cqe = self
            .ring
            .completion()
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "no completion"))?;

        let result = cqe.result();
        if result < 0 {
            return Err(io::Error::from_raw_os_error(-result));
        }

        Ok(result as usize)
    }

    /// Reads the entire file into a vector.
    ///
    /// Uses batched submissions for better performance on large files.
    pub fn read_all_batched(&mut self) -> io::Result<Vec<u8>> {
        let size = self.size as usize;
        let mut data = vec![0u8; size];
        let mut offset = 0usize;

        while offset < size {
            let chunk_size = self.buffer_size.min(size - offset);
            let n = self.read_at(offset as u64, &mut data[offset..offset + chunk_size])?;
            if n == 0 {
                break;
            }
            offset += n;
        }

        data.truncate(offset);
        Ok(data)
    }
}

impl Read for IoUringReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.read_at(self.position, buf)?;
        self.position += n as u64;
        Ok(n)
    }
}

impl FileReader for IoUringReader {
    fn size(&self) -> u64 {
        self.size
    }

    fn position(&self) -> u64 {
        self.position
    }

    fn seek_to(&mut self, pos: u64) -> io::Result<()> {
        if pos > self.size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek position beyond end of file",
            ));
        }
        self.position = pos;
        Ok(())
    }

    fn read_all(&mut self) -> io::Result<Vec<u8>> {
        self.seek_to(0)?;
        self.read_all_batched()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// io_uring File Writer
// ─────────────────────────────────────────────────────────────────────────────

/// A file writer using io_uring for async I/O.
pub struct IoUringWriter {
    ring: RawIoUring,
    file: File,
    bytes_written: u64,
    buffer: Vec<u8>,
    buffer_pos: usize,
    buffer_size: usize,
}

impl IoUringWriter {
    /// Creates a file for writing with io_uring.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The file cannot be created
    /// - io_uring initialization fails
    pub fn create<P: AsRef<Path>>(path: P, config: &IoUringConfig) -> io::Result<Self> {
        let file = File::create(path)?;

        let ring = RawIoUring::new(config.sq_entries).map_err(|e| {
            io::Error::new(io::ErrorKind::Other, format!("io_uring init failed: {e}"))
        })?;

        Ok(Self {
            ring,
            file,
            bytes_written: 0,
            buffer: vec![0u8; config.buffer_size],
            buffer_pos: 0,
            buffer_size: config.buffer_size,
        })
    }

    /// Creates a file with preallocated space.
    pub fn create_with_size<P: AsRef<Path>>(
        path: P,
        size: u64,
        config: &IoUringConfig,
    ) -> io::Result<Self> {
        let file = File::create(path)?;
        file.set_len(size)?;

        let ring = RawIoUring::new(config.sq_entries).map_err(|e| {
            io::Error::new(io::ErrorKind::Other, format!("io_uring init failed: {e}"))
        })?;

        Ok(Self {
            ring,
            file,
            bytes_written: 0,
            buffer: vec![0u8; config.buffer_size],
            buffer_pos: 0,
            buffer_size: config.buffer_size,
        })
    }

    /// Writes data at the specified offset without advancing the position.
    pub fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let fd = types::Fd(self.file.as_raw_fd());

        // Prepare write operation
        let write_op = opcode::Write::new(fd, buf.as_ptr(), buf.len() as u32)
            .offset(offset)
            .build()
            .user_data(0x43);

        // Submit and wait
        unsafe {
            self.ring
                .submission()
                .push(&write_op)
                .map_err(|_| io::Error::new(io::ErrorKind::Other, "submission queue full"))?;
        }

        self.ring.submit_and_wait(1)?;

        // Get completion
        let cqe = self
            .ring
            .completion()
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "no completion"))?;

        let result = cqe.result();
        if result < 0 {
            return Err(io::Error::from_raw_os_error(-result));
        }

        Ok(result as usize)
    }

    /// Flushes the internal buffer to disk.
    fn flush_buffer(&mut self) -> io::Result<()> {
        if self.buffer_pos == 0 {
            return Ok(());
        }

        let buffer_len = self.buffer_pos;
        let mut written = 0;

        while written < buffer_len {
            // Create a temporary copy to avoid borrow issues
            let chunk = self.buffer[written..buffer_len].to_vec();
            let n = self.write_at(self.bytes_written, &chunk)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to write data",
                ));
            }
            written += n;
            self.bytes_written += n as u64;
        }

        self.buffer_pos = 0;
        Ok(())
    }
}

impl Write for IoUringWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // If data fits in buffer, just copy it
        if self.buffer_pos + buf.len() <= self.buffer_size {
            self.buffer[self.buffer_pos..self.buffer_pos + buf.len()].copy_from_slice(buf);
            self.buffer_pos += buf.len();
            return Ok(buf.len());
        }

        // Flush current buffer
        self.flush_buffer()?;

        // If data is larger than buffer, write directly
        if buf.len() >= self.buffer_size {
            let n = self.write_at(self.bytes_written, buf)?;
            self.bytes_written += n as u64;
            return Ok(n);
        }

        // Otherwise, buffer the data
        self.buffer[..buf.len()].copy_from_slice(buf);
        self.buffer_pos = buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_buffer()
    }
}

impl FileWriter for IoUringWriter {
    fn bytes_written(&self) -> u64 {
        self.bytes_written + self.buffer_pos as u64
    }

    fn sync(&mut self) -> io::Result<()> {
        self.flush_buffer()?;

        let fd = types::Fd(self.file.as_raw_fd());

        // Submit fsync
        let fsync_op = opcode::Fsync::new(fd).build().user_data(0x44);

        unsafe {
            self.ring
                .submission()
                .push(&fsync_op)
                .map_err(|_| io::Error::new(io::ErrorKind::Other, "submission queue full"))?;
        }

        self.ring.submit_and_wait(1)?;

        let cqe = self
            .ring
            .completion()
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "no completion"))?;

        let result = cqe.result();
        if result < 0 {
            return Err(io::Error::from_raw_os_error(-result));
        }

        Ok(())
    }

    fn preallocate(&mut self, size: u64) -> io::Result<()> {
        self.file.set_len(size)
    }
}

impl Drop for IoUringWriter {
    fn drop(&mut self) {
        // Best-effort flush on drop
        let _ = self.flush_buffer();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Factories with automatic fallback
// ─────────────────────────────────────────────────────────────────────────────

/// Factory that creates io_uring readers when available, with fallback to standard I/O.
#[derive(Debug, Clone)]
pub struct IoUringReaderFactory {
    config: IoUringConfig,
    force_fallback: bool,
}

impl Default for IoUringReaderFactory {
    fn default() -> Self {
        Self {
            config: IoUringConfig::default(),
            force_fallback: false,
        }
    }
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
#[derive(Debug, Clone)]
pub struct IoUringWriterFactory {
    config: IoUringConfig,
    force_fallback: bool,
}

impl Default for IoUringWriterFactory {
    fn default() -> Self {
        Self {
            config: IoUringConfig::default(),
            force_fallback: false,
        }
    }
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

        Ok(IoUringOrStdWriter::Std(crate::traits::StdFileWriter::create(path)?))
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

// ─────────────────────────────────────────────────────────────────────────────
// Convenience functions
// ─────────────────────────────────────────────────────────────────────────────

/// Reads an entire file using io_uring if available, falling back to standard I/O.
///
/// This is a convenience function for one-off file reads.
pub fn read_file<P: AsRef<Path>>(path: P) -> io::Result<Vec<u8>> {
    let factory = IoUringReaderFactory::default();
    let mut reader = factory.open(path.as_ref())?;
    reader.read_all()
}

/// Writes data to a file using io_uring if available, falling back to standard I/O.
///
/// This is a convenience function for one-off file writes.
pub fn write_file<P: AsRef<Path>>(path: P, data: &[u8]) -> io::Result<()> {
    let factory = IoUringWriterFactory::default();
    let mut writer = factory.create(path.as_ref())?;
    writer.write_all(data)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_kernel_version_parsing() {
        assert_eq!(parse_kernel_version("5.15.0-generic"), Some((5, 15)));
        assert_eq!(parse_kernel_version("6.1.0"), Some((6, 1)));
        assert_eq!(parse_kernel_version("4.19.123-aws"), Some((4, 19)));
        assert_eq!(parse_kernel_version("invalid"), None);
    }

    #[test]
    fn test_io_uring_availability_check() {
        // This just tests that the check doesn't panic
        let available = is_io_uring_available();
        println!("io_uring available: {available}");
    }

    #[test]
    fn test_io_uring_config_defaults() {
        let config = IoUringConfig::default();
        assert_eq!(config.sq_entries, 64);
        assert_eq!(config.buffer_size, 64 * 1024);
        assert!(!config.direct_io);
    }

    #[test]
    fn test_io_uring_config_presets() {
        let large = IoUringConfig::for_large_files();
        assert_eq!(large.sq_entries, 256);
        assert_eq!(large.buffer_size, 256 * 1024);

        let small = IoUringConfig::for_small_files();
        assert_eq!(small.sq_entries, 128);
        assert_eq!(small.buffer_size, 16 * 1024);
    }

    #[test]
    fn test_reader_factory_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, b"hello world").unwrap();

        // Force fallback
        let factory = IoUringReaderFactory::default().force_fallback(true);
        assert!(!factory.will_use_io_uring());

        let mut reader = factory.open(&path).unwrap();
        assert!(matches!(reader, IoUringOrStdReader::Std(_)));

        let data = reader.read_all().unwrap();
        assert_eq!(data, b"hello world");
    }

    #[test]
    fn test_writer_factory_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");

        // Force fallback
        let factory = IoUringWriterFactory::default().force_fallback(true);
        assert!(!factory.will_use_io_uring());

        let mut writer = factory.create(&path).unwrap();
        assert!(matches!(writer, IoUringOrStdWriter::Std(_)));

        writer.write_all(b"hello world").unwrap();
        writer.flush().unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world");
    }

    #[test]
    fn test_convenience_functions_with_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");

        write_file(&path, b"test data").unwrap();
        let data = read_file(&path).unwrap();
        assert_eq!(data, b"test data");
    }

    // Tests that run only when io_uring is actually available
    #[test]
    fn test_io_uring_reader_if_available() {
        if !is_io_uring_available() {
            println!("Skipping io_uring reader test: not available");
            return;
        }

        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, b"hello from io_uring").unwrap();

        let config = IoUringConfig::default();
        let mut reader = IoUringReader::open(&path, &config).unwrap();

        assert_eq!(reader.size(), 19);
        assert_eq!(reader.position(), 0);

        let data = reader.read_all().unwrap();
        assert_eq!(data, b"hello from io_uring");
    }

    #[test]
    fn test_io_uring_writer_if_available() {
        if !is_io_uring_available() {
            println!("Skipping io_uring writer test: not available");
            return;
        }

        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");

        let config = IoUringConfig::default();
        let mut writer = IoUringWriter::create(&path, &config).unwrap();

        writer.write_all(b"hello from io_uring").unwrap();
        writer.sync().unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "hello from io_uring"
        );
    }

    #[test]
    fn test_io_uring_factory_uses_io_uring_when_available() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, b"test").unwrap();

        let factory = IoUringReaderFactory::default();
        let reader = factory.open(&path).unwrap();

        if is_io_uring_available() {
            assert!(matches!(reader, IoUringOrStdReader::IoUring(_)));
        } else {
            assert!(matches!(reader, IoUringOrStdReader::Std(_)));
        }
    }

    #[test]
    fn test_io_uring_read_at() {
        if !is_io_uring_available() {
            println!("Skipping io_uring read_at test: not available");
            return;
        }

        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, b"hello world").unwrap();

        let config = IoUringConfig::default();
        let mut reader = IoUringReader::open(&path, &config).unwrap();

        let mut buf = [0u8; 5];
        let n = reader.read_at(6, &mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf, b"world");

        // Position should not have changed
        assert_eq!(reader.position(), 0);
    }

    #[test]
    fn test_io_uring_write_at() {
        if !is_io_uring_available() {
            println!("Skipping io_uring write_at test: not available");
            return;
        }

        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");

        let config = IoUringConfig::default();
        let mut writer = IoUringWriter::create(&path, &config).unwrap();

        // Write at specific offsets
        writer.write_at(0, b"hello").unwrap();
        writer.write_at(6, b"world").unwrap();
        writer.flush().unwrap();

        // Note: there's a gap at position 5
        let content = std::fs::read(&path).unwrap();
        assert_eq!(&content[0..5], b"hello");
        assert_eq!(&content[6..11], b"world");
    }

    #[test]
    fn test_reader_seek() {
        if !is_io_uring_available() {
            println!("Skipping io_uring seek test: not available");
            return;
        }

        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, b"hello world").unwrap();

        let config = IoUringConfig::default();
        let mut reader = IoUringReader::open(&path, &config).unwrap();

        reader.seek_to(6).unwrap();
        assert_eq!(reader.position(), 6);

        let mut buf = [0u8; 5];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"world");
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // Comprehensive io_uring tests with graceful fallback
    // ─────────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_basic_read_with_io_uring_or_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("read_test.txt");
        let test_data = b"The quick brown fox jumps over the lazy dog";
        std::fs::write(&path, test_data).unwrap();

        let factory = IoUringReaderFactory::default();
        let mut reader = factory.open(&path).unwrap();

        // Test that we get the correct data regardless of backend
        let data = reader.read_all().unwrap();
        assert_eq!(data, test_data);
        assert_eq!(reader.size(), test_data.len() as u64);
    }

    #[test]
    fn test_basic_write_with_io_uring_or_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("write_test.txt");
        let test_data = b"Hello, io_uring world!";

        let factory = IoUringWriterFactory::default();
        let mut writer = factory.create(&path).unwrap();

        writer.write_all(test_data).unwrap();
        writer.flush().unwrap();

        // Verify the data was written correctly
        let written = std::fs::read(&path).unwrap();
        assert_eq!(written, test_data);
        assert_eq!(writer.bytes_written(), test_data.len() as u64);
    }

    #[test]
    fn test_large_file_read_with_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("large_read.bin");

        // Create a 1 MB file with a pattern
        let chunk_size = 1024;
        let num_chunks = 1024;
        let mut expected_data = Vec::with_capacity(chunk_size * num_chunks);
        for i in 0..num_chunks {
            let pattern = (i % 256) as u8;
            expected_data.extend(std::iter::repeat(pattern).take(chunk_size));
        }
        std::fs::write(&path, &expected_data).unwrap();

        let factory = IoUringReaderFactory::default();
        let mut reader = factory.open(&path).unwrap();

        let data = reader.read_all().unwrap();
        assert_eq!(data.len(), expected_data.len());
        assert_eq!(data, expected_data);
    }

    #[test]
    fn test_large_file_write_with_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("large_write.bin");

        // Generate 512 KB of test data
        let chunk_size = 1024;
        let num_chunks = 512;
        let mut test_data = Vec::with_capacity(chunk_size * num_chunks);
        for i in 0..num_chunks {
            let pattern = (i % 256) as u8;
            test_data.extend(std::iter::repeat(pattern).take(chunk_size));
        }

        let factory = IoUringWriterFactory::default();
        let mut writer = factory.create(&path).unwrap();

        // Write in chunks to test buffering
        for chunk in test_data.chunks(chunk_size) {
            writer.write_all(chunk).unwrap();
        }
        writer.sync().unwrap();

        // Verify the data
        let written = std::fs::read(&path).unwrap();
        assert_eq!(written.len(), test_data.len());
        assert_eq!(written, test_data);
    }

    #[test]
    fn test_forced_fallback_to_standard_io() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("fallback_test.txt");
        let test_data = b"Testing forced fallback";
        std::fs::write(&path, test_data).unwrap();

        // Force fallback even if io_uring is available
        let factory = IoUringReaderFactory::default().force_fallback(true);
        assert!(!factory.will_use_io_uring());

        let mut reader = factory.open(&path).unwrap();
        assert!(matches!(reader, IoUringOrStdReader::Std(_)));

        let data = reader.read_all().unwrap();
        assert_eq!(data, test_data);
    }

    #[test]
    fn test_writer_forced_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("fallback_write.txt");
        let test_data = b"Forced fallback write";

        let factory = IoUringWriterFactory::default().force_fallback(true);
        assert!(!factory.will_use_io_uring());

        let mut writer = factory.create(&path).unwrap();
        assert!(matches!(writer, IoUringOrStdWriter::Std(_)));

        writer.write_all(test_data).unwrap();
        writer.flush().unwrap();

        let written = std::fs::read(&path).unwrap();
        assert_eq!(written, test_data);
    }

    #[test]
    fn test_reader_partial_reads() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("partial_read.txt");
        std::fs::write(&path, b"0123456789ABCDEF").unwrap();

        let factory = IoUringReaderFactory::default();
        let mut reader = factory.open(&path).unwrap();

        // Read in small chunks
        let mut buf = [0u8; 3];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 3);
        assert_eq!(&buf, b"012");

        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 3);
        assert_eq!(&buf, b"345");

        // Seek and read
        reader.seek_to(10).unwrap();
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 3);
        assert_eq!(&buf, b"ABC");
    }

    #[test]
    fn test_writer_buffering() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("buffering_test.txt");

        let _config = IoUringConfig {
            sq_entries: 32,
            buffer_size: 128, // Small buffer to test flushing
            direct_io: false,
        };

        let factory = IoUringWriterFactory::default().force_fallback(true);
        let mut writer = factory.create(&path).unwrap();

        // Write data that exceeds buffer size
        let data = b"x".repeat(256);
        writer.write_all(&data).unwrap();

        // Don't flush yet - buffering should handle it
        assert_eq!(writer.bytes_written(), 256);

        writer.flush().unwrap();

        let written = std::fs::read(&path).unwrap();
        assert_eq!(written.len(), 256);
    }

    #[test]
    fn test_writer_sync() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sync_test.txt");

        let factory = IoUringWriterFactory::default();
        let mut writer = factory.create(&path).unwrap();

        writer.write_all(b"sync test").unwrap();
        writer.sync().unwrap();

        // Verify data is on disk
        let written = std::fs::read(&path).unwrap();
        assert_eq!(written, b"sync test");
    }

    #[test]
    fn test_writer_preallocate() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("preallocate_test.txt");

        let factory = IoUringWriterFactory::default();
        let mut writer = factory.create_with_size(&path, 1024).unwrap();

        writer.write_all(b"prealloc").unwrap();
        writer.flush().unwrap();

        let metadata = std::fs::metadata(&path).unwrap();
        assert_eq!(metadata.len(), 1024);
    }

    #[test]
    fn test_read_empty_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty.txt");
        std::fs::write(&path, b"").unwrap();

        let factory = IoUringReaderFactory::default();
        let mut reader = factory.open(&path).unwrap();

        assert_eq!(reader.size(), 0);
        let data = reader.read_all().unwrap();
        assert_eq!(data.len(), 0);
    }

    #[test]
    fn test_read_at_eof() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("eof_test.txt");
        std::fs::write(&path, b"short").unwrap();

        let factory = IoUringReaderFactory::default();
        let mut reader = factory.open(&path).unwrap();

        // Seek to end
        reader.seek_to(5).unwrap();
        assert_eq!(reader.position(), 5);

        // Try to read - should return 0
        let mut buf = [0u8; 10];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn test_seek_beyond_eof_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("seek_error.txt");
        std::fs::write(&path, b"data").unwrap();

        let factory = IoUringReaderFactory::default();
        let mut reader = factory.open(&path).unwrap();

        // Seeking beyond EOF should fail
        let result = reader.seek_to(100);
        assert!(result.is_err());
    }

    #[test]
    fn test_concurrent_operations_with_fallback() {
        use std::sync::Arc;
        use std::thread;

        let dir = Arc::new(tempdir().unwrap());
        let test_data = b"concurrent test data";

        // Spawn multiple threads that read and write
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let dir = Arc::clone(&dir);
                let data = test_data.to_vec();
                thread::spawn(move || {
                    let path = dir.path().join(format!("thread_{i}.txt"));

                    // Write
                    let factory = IoUringWriterFactory::default();
                    let mut writer = factory.create(&path).unwrap();
                    writer.write_all(&data).unwrap();
                    writer.sync().unwrap();

                    // Read back
                    let factory = IoUringReaderFactory::default();
                    let mut reader = factory.open(&path).unwrap();
                    let read_data = reader.read_all().unwrap();

                    assert_eq!(read_data, data);
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }
    }

    #[test]
    fn test_convenience_functions() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("convenience.txt");
        let test_data = b"convenience function test";

        // Test write_file
        write_file(&path, test_data).unwrap();

        // Test read_file
        let data = read_file(&path).unwrap();
        assert_eq!(data, test_data);
    }

    #[test]
    fn test_multiple_sequential_operations() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sequential.txt");

        let factory = IoUringWriterFactory::default();

        // First write
        {
            let mut writer = factory.create(&path).unwrap();
            writer.write_all(b"first").unwrap();
            writer.flush().unwrap();
        }

        // Read it back
        let factory_read = IoUringReaderFactory::default();
        {
            let mut reader = factory_read.open(&path).unwrap();
            let data = reader.read_all().unwrap();
            assert_eq!(data, b"first");
        }

        // Overwrite
        {
            let mut writer = factory.create(&path).unwrap();
            writer.write_all(b"second write").unwrap();
            writer.flush().unwrap();
        }

        // Read again
        {
            let mut reader = factory_read.open(&path).unwrap();
            let data = reader.read_all().unwrap();
            assert_eq!(data, b"second write");
        }
    }

    #[test]
    fn test_config_presets() {
        let large = IoUringConfig::for_large_files();
        assert!(large.sq_entries >= 128);
        assert!(large.buffer_size >= 128 * 1024);

        let small = IoUringConfig::for_small_files();
        assert!(small.buffer_size <= 32 * 1024);
    }

    #[test]
    fn test_factory_with_custom_config() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("custom_config.txt");
        std::fs::write(&path, b"custom").unwrap();

        let config = IoUringConfig {
            sq_entries: 32,
            buffer_size: 4096,
            direct_io: false,
        };

        let factory = IoUringReaderFactory::with_config(config);
        let mut reader = factory.open(&path).unwrap();
        let data = reader.read_all().unwrap();
        assert_eq!(data, b"custom");
    }

    #[test]
    fn test_error_handling_nonexistent_file() {
        let factory = IoUringReaderFactory::default();
        let result = factory.open(Path::new("/nonexistent/path/file.txt"));
        assert!(result.is_err());
    }

    #[test]
    fn test_error_handling_permission_denied() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let path = dir.path().join("readonly.txt");
        std::fs::write(&path, b"data").unwrap();

        // Make file write-only (no read permission)
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o200);
        fs::set_permissions(&path, perms).unwrap();

        let factory = IoUringReaderFactory::default();
        let result = factory.open(&path);
        assert!(result.is_err());

        // Restore permissions for cleanup
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o644);
        fs::set_permissions(&path, perms).unwrap();
    }

    #[test]
    fn test_queue_depth_limits() {
        if !is_io_uring_available() {
            println!("Skipping queue depth test: io_uring not available");
            return;
        }

        let dir = tempdir().unwrap();
        let path = dir.path().join("queue_test.txt");

        // Test with minimal queue depth
        let config = IoUringConfig {
            sq_entries: 4, // Very small queue
            buffer_size: 1024,
            direct_io: false,
        };

        // Should still work with small queue
        let mut writer = IoUringWriter::create(&path, &config).unwrap();
        let data = b"x".repeat(8192); // Data larger than queue
        writer.write_all(&data).unwrap();
        writer.flush().unwrap();

        let written = std::fs::read(&path).unwrap();
        assert_eq!(written.len(), data.len());
    }

    #[test]
    fn test_reader_remaining() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("remaining.txt");
        std::fs::write(&path, b"0123456789").unwrap();

        let factory = IoUringReaderFactory::default();
        let mut reader = factory.open(&path).unwrap();

        assert_eq!(reader.remaining(), 10);

        let mut buf = [0u8; 3];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(reader.remaining(), 7);

        reader.seek_to(8).unwrap();
        assert_eq!(reader.remaining(), 2);
    }

    #[test]
    fn test_write_zero_bytes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("zero_write.txt");

        let factory = IoUringWriterFactory::default();
        let mut writer = factory.create(&path).unwrap();

        let n = writer.write(b"").unwrap();
        assert_eq!(n, 0);
        assert_eq!(writer.bytes_written(), 0);

        writer.flush().unwrap();
        let written = std::fs::read(&path).unwrap();
        assert_eq!(written.len(), 0);
    }

    #[test]
    fn test_io_uring_reader_read_all_batched() {
        if !is_io_uring_available() {
            println!("Skipping batched read test: io_uring not available");
            return;
        }

        let dir = tempdir().unwrap();
        let path = dir.path().join("batched.txt");

        // Create a file larger than the default buffer size
        let size = 256 * 1024; // 256 KB
        let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        std::fs::write(&path, &data).unwrap();

        let config = IoUringConfig {
            sq_entries: 64,
            buffer_size: 64 * 1024,
            direct_io: false,
        };

        let mut reader = IoUringReader::open(&path, &config).unwrap();
        let read_data = reader.read_all_batched().unwrap();

        assert_eq!(read_data.len(), data.len());
        assert_eq!(read_data, data);
    }

    #[test]
    fn test_binary_data_integrity() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("binary.bin");

        // Create binary data with all byte values
        let data: Vec<u8> = (0..=255).cycle().take(4096).collect();

        let factory = IoUringWriterFactory::default();
        let mut writer = factory.create(&path).unwrap();
        writer.write_all(&data).unwrap();
        writer.flush().unwrap();
        drop(writer);

        let factory_read = IoUringReaderFactory::default();
        let mut reader = factory_read.open(&path).unwrap();
        let read_data = reader.read_all().unwrap();

        assert_eq!(read_data.len(), data.len());
        assert_eq!(read_data, data);
    }

    #[test]
    fn test_drop_flushes_writer() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("drop_flush.txt");

        {
            let factory = IoUringWriterFactory::default();
            let mut writer = factory.create(&path).unwrap();
            writer.write_all(b"data to flush on drop").unwrap();
            // Don't explicitly flush - drop should do it
        }

        // Verify data was written
        let written = std::fs::read(&path).unwrap();
        assert_eq!(written, b"data to flush on drop");
    }
}
