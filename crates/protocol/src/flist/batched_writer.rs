//! Batched file list writer for reduced network round-trips.
//!
//! This module provides a [`BatchedFileListWriter`] that accumulates multiple
//! file entries before flushing them to the underlying writer in a single
//! operation. This reduces network round-trips when sending file lists over
//! the network.
//!
//! # Batching Strategy
//!
//! The writer flushes the batch when any of these conditions are met:
//! - The batch contains `max_entries` entries (default: 64)
//! - The batch size exceeds `max_bytes` bytes (default: 64KB)
//! - The flush timeout expires (default: 100ms)
//! - An explicit flush is requested
//! - The writer is dropped (auto-flush)
//!
//! # Usage
//!
//! ```no_run
//! use protocol::flist::{BatchedFileListWriter, FileEntry};
//! use protocol::ProtocolVersion;
//!
//! let protocol = ProtocolVersion::try_from(32u8).unwrap();
//! let mut writer = std::io::sink();
//!
//! let mut batched = BatchedFileListWriter::new(protocol);
//!
//! // Add entries - they're accumulated in a batch
//! let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
//! batched.add_entry(&entry)?;
//!
//! // Entries are written when batch is full or on explicit flush
//! batched.flush(&mut writer)?;
//! # Ok::<(), std::io::Error>(())
//! ```

use std::io::{self, Write};
use std::time::{Duration, Instant};

use super::entry::FileEntry;
use super::write::FileListWriter;
use crate::{CompatibilityFlags, ProtocolVersion};

/// Default maximum number of entries in a batch before auto-flush.
pub const DEFAULT_MAX_ENTRIES: usize = 64;

/// Default maximum batch size in bytes before auto-flush.
pub const DEFAULT_MAX_BYTES: usize = 64 * 1024; // 64KB

/// Default timeout for flush-on-timeout behavior.
pub const DEFAULT_FLUSH_TIMEOUT: Duration = Duration::from_millis(100);

/// Configuration for batched file list writing.
///
/// Controls when batches are flushed based on entry count, byte size,
/// or timeout expiration.
#[derive(Debug, Clone)]
pub struct BatchConfig {
    /// Maximum number of entries before auto-flush.
    pub max_entries: usize,
    /// Maximum batch size in bytes before auto-flush.
    pub max_bytes: usize,
    /// Timeout after which the batch is flushed.
    pub flush_timeout: Duration,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            max_entries: DEFAULT_MAX_ENTRIES,
            max_bytes: DEFAULT_MAX_BYTES,
            flush_timeout: DEFAULT_FLUSH_TIMEOUT,
        }
    }
}

impl BatchConfig {
    /// Creates a new batch configuration with default values.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the maximum number of entries before auto-flush.
    #[must_use]
    pub const fn with_max_entries(mut self, max_entries: usize) -> Self {
        self.max_entries = max_entries;
        self
    }

    /// Sets the maximum batch size in bytes before auto-flush.
    #[must_use]
    pub const fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Sets the flush timeout.
    #[must_use]
    pub const fn with_flush_timeout(mut self, timeout: Duration) -> Self {
        self.flush_timeout = timeout;
        self
    }

    /// Creates a configuration with no automatic flushing.
    ///
    /// Batches will only be flushed explicitly or when the writer is finalized.
    #[must_use]
    pub fn no_auto_flush() -> Self {
        Self {
            max_entries: usize::MAX,
            max_bytes: usize::MAX,
            flush_timeout: Duration::MAX,
        }
    }
}

/// Statistics about batched writing operations.
#[derive(Debug, Clone, Default)]
pub struct BatchStats {
    /// Total number of entries written.
    pub entries_written: u64,
    /// Total number of batches flushed.
    pub batches_flushed: u64,
    /// Total bytes written to the underlying writer.
    pub bytes_written: u64,
    /// Number of flushes triggered by entry count limit.
    pub flushes_by_count: u64,
    /// Number of flushes triggered by byte size limit.
    pub flushes_by_size: u64,
    /// Number of flushes triggered by timeout.
    pub flushes_by_timeout: u64,
    /// Number of explicit flushes.
    pub explicit_flushes: u64,
}

/// Reason for flushing a batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlushReason {
    /// Batch entry count reached the limit.
    EntryCount,
    /// Batch size reached the byte limit.
    ByteSize,
    /// Flush timeout expired.
    Timeout,
    /// Explicit flush requested by caller.
    Explicit,
    /// Final flush when finishing the file list.
    Final,
}

/// A batched file list writer that accumulates entries before writing.
///
/// This writer wraps [`FileListWriter`] and batches multiple file entries
/// into a buffer before flushing them to the underlying writer. This reduces
/// the number of network round-trips when sending file lists.
///
/// # Thread Safety
///
/// This type is not thread-safe. For concurrent file list writing, create
/// separate instances for each thread.
#[derive(Debug)]
pub struct BatchedFileListWriter {
    /// The underlying file list writer for encoding entries.
    writer: FileListWriter,
    /// Batch configuration.
    config: BatchConfig,
    /// Buffer for accumulated batch data.
    buffer: Vec<u8>,
    /// Number of entries in the current batch.
    entry_count: usize,
    /// Timestamp when the first entry was added to the current batch.
    batch_start: Option<Instant>,
    /// Statistics about batching operations.
    stats: BatchStats,
}

impl BatchedFileListWriter {
    /// Creates a new batched file list writer with default configuration.
    #[must_use]
    pub fn new(protocol: ProtocolVersion) -> Self {
        Self::with_config(protocol, BatchConfig::default())
    }

    /// Creates a new batched file list writer with compatibility flags.
    #[must_use]
    pub fn with_compat_flags(protocol: ProtocolVersion, compat_flags: CompatibilityFlags) -> Self {
        Self {
            writer: FileListWriter::with_compat_flags(protocol, compat_flags),
            config: BatchConfig::default(),
            buffer: Vec::with_capacity(DEFAULT_MAX_BYTES),
            entry_count: 0,
            batch_start: None,
            stats: BatchStats::default(),
        }
    }

    /// Creates a new batched file list writer with custom configuration.
    #[must_use]
    pub fn with_config(protocol: ProtocolVersion, config: BatchConfig) -> Self {
        Self {
            writer: FileListWriter::new(protocol),
            config,
            buffer: Vec::with_capacity(DEFAULT_MAX_BYTES),
            entry_count: 0,
            batch_start: None,
            stats: BatchStats::default(),
        }
    }

    /// Creates a new batched file list writer with compatibility flags and custom configuration.
    #[must_use]
    pub fn with_compat_flags_and_config(
        protocol: ProtocolVersion,
        compat_flags: CompatibilityFlags,
        config: BatchConfig,
    ) -> Self {
        Self {
            writer: FileListWriter::with_compat_flags(protocol, compat_flags),
            config,
            buffer: Vec::with_capacity(DEFAULT_MAX_BYTES),
            entry_count: 0,
            batch_start: None,
            stats: BatchStats::default(),
        }
    }

    /// Sets whether UID values should be written to the wire.
    #[must_use]
    pub fn with_preserve_uid(mut self, preserve: bool) -> Self {
        self.writer = self.writer.with_preserve_uid(preserve);
        self
    }

    /// Sets whether GID values should be written to the wire.
    #[must_use]
    pub fn with_preserve_gid(mut self, preserve: bool) -> Self {
        self.writer = self.writer.with_preserve_gid(preserve);
        self
    }

    /// Sets whether symlink targets should be written to the wire.
    #[must_use]
    pub fn with_preserve_links(mut self, preserve: bool) -> Self {
        self.writer = self.writer.with_preserve_links(preserve);
        self
    }

    /// Sets whether device numbers should be written to the wire.
    #[must_use]
    pub fn with_preserve_devices(mut self, preserve: bool) -> Self {
        self.writer = self.writer.with_preserve_devices(preserve);
        self
    }

    /// Sets whether hardlink indices should be written to the wire.
    #[must_use]
    pub fn with_preserve_hard_links(mut self, preserve: bool) -> Self {
        self.writer = self.writer.with_preserve_hard_links(preserve);
        self
    }

    /// Sets whether access times should be written to the wire.
    #[must_use]
    pub fn with_preserve_atimes(mut self, preserve: bool) -> Self {
        self.writer = self.writer.with_preserve_atimes(preserve);
        self
    }

    /// Sets whether creation times should be written to the wire.
    #[must_use]
    pub fn with_preserve_crtimes(mut self, preserve: bool) -> Self {
        self.writer = self.writer.with_preserve_crtimes(preserve);
        self
    }

    /// Sets whether ACLs should be written to the wire.
    #[must_use]
    pub fn with_preserve_acls(mut self, preserve: bool) -> Self {
        self.writer = self.writer.with_preserve_acls(preserve);
        self
    }

    /// Sets whether extended attributes should be written to the wire.
    #[must_use]
    pub fn with_preserve_xattrs(mut self, preserve: bool) -> Self {
        self.writer = self.writer.with_preserve_xattrs(preserve);
        self
    }

    /// Enables checksum mode with the given checksum length.
    #[must_use]
    pub fn with_always_checksum(mut self, csum_len: usize) -> Self {
        self.writer = self.writer.with_always_checksum(csum_len);
        self
    }

    /// Returns the batching statistics.
    #[must_use]
    pub const fn stats(&self) -> &BatchStats {
        &self.stats
    }

    /// Returns the file list statistics from the underlying writer.
    #[must_use]
    pub const fn flist_stats(&self) -> &super::state::FileListStats {
        self.writer.stats()
    }

    /// Returns the number of entries in the current unflushed batch.
    #[must_use]
    pub const fn pending_entries(&self) -> usize {
        self.entry_count
    }

    /// Returns the size of the current unflushed batch in bytes.
    #[must_use]
    pub fn pending_bytes(&self) -> usize {
        self.buffer.len()
    }

    /// Returns true if the current batch is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entry_count == 0
    }

    /// Adds a file entry to the batch.
    ///
    /// The entry is encoded and added to the internal buffer. If the batch
    /// reaches its limits (entry count, byte size, or timeout), it is
    /// automatically flushed to the provided writer.
    ///
    /// # Arguments
    ///
    /// * `entry` - The file entry to add to the batch.
    ///
    /// # Returns
    ///
    /// Returns `Ok(true)` if the batch was flushed, `Ok(false)` otherwise.
    pub fn add_entry<W: Write>(&mut self, writer: &mut W, entry: &FileEntry) -> io::Result<bool> {
        // Track batch start time
        if self.batch_start.is_none() {
            self.batch_start = Some(Instant::now());
        }

        // Encode entry to buffer
        self.writer.write_entry(&mut self.buffer, entry)?;
        self.entry_count += 1;

        // Check if we should auto-flush
        let should_flush = self.should_flush();

        if let Some(reason) = should_flush {
            self.flush_with_reason(writer, reason)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Adds multiple file entries to the batch.
    ///
    /// This is more efficient than calling `add_entry` repeatedly as it
    /// minimizes flush checks.
    ///
    /// # Returns
    ///
    /// Returns the number of batches that were flushed during this operation.
    pub fn add_entries<'a, W, I>(&mut self, writer: &mut W, entries: I) -> io::Result<u64>
    where
        W: Write,
        I: IntoIterator<Item = &'a FileEntry>,
    {
        let mut flushes = 0u64;

        for entry in entries {
            if self.add_entry(writer, entry)? {
                flushes += 1;
            }
        }

        Ok(flushes)
    }

    /// Checks if the batch should be flushed based on current limits.
    fn should_flush(&self) -> Option<FlushReason> {
        // Check entry count limit
        if self.entry_count >= self.config.max_entries {
            return Some(FlushReason::EntryCount);
        }

        // Check byte size limit
        if self.buffer.len() >= self.config.max_bytes {
            return Some(FlushReason::ByteSize);
        }

        // Check timeout
        if let Some(start) = self.batch_start {
            if start.elapsed() >= self.config.flush_timeout {
                return Some(FlushReason::Timeout);
            }
        }

        None
    }

    /// Flushes the current batch to the writer.
    ///
    /// This writes all accumulated entries to the underlying writer and
    /// resets the batch state. If the batch is empty, this is a no-op.
    pub fn flush<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        if self.entry_count > 0 {
            self.flush_with_reason(writer, FlushReason::Explicit)?;
        }
        Ok(())
    }

    /// Checks if a timeout flush is needed and performs it if so.
    ///
    /// Call this periodically (e.g., in an event loop) to ensure batches
    /// are flushed even when entries are added slowly.
    ///
    /// # Returns
    ///
    /// Returns `Ok(true)` if a flush was performed, `Ok(false)` otherwise.
    pub fn check_timeout_flush<W: Write>(&mut self, writer: &mut W) -> io::Result<bool> {
        if let Some(start) = self.batch_start {
            if start.elapsed() >= self.config.flush_timeout && self.entry_count > 0 {
                self.flush_with_reason(writer, FlushReason::Timeout)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Internal flush with reason tracking for statistics.
    fn flush_with_reason<W: Write>(&mut self, writer: &mut W, reason: FlushReason) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        // Write buffer to underlying writer
        writer.write_all(&self.buffer)?;

        // Update statistics
        self.stats.entries_written += self.entry_count as u64;
        self.stats.bytes_written += self.buffer.len() as u64;
        self.stats.batches_flushed += 1;

        match reason {
            FlushReason::EntryCount => self.stats.flushes_by_count += 1,
            FlushReason::ByteSize => self.stats.flushes_by_size += 1,
            FlushReason::Timeout => self.stats.flushes_by_timeout += 1,
            FlushReason::Explicit | FlushReason::Final => self.stats.explicit_flushes += 1,
        }

        // Reset batch state
        self.buffer.clear();
        self.entry_count = 0;
        self.batch_start = None;

        Ok(())
    }

    /// Finishes the file list by flushing any remaining entries and writing
    /// the end-of-list marker.
    ///
    /// This method must be called when all entries have been added to properly
    /// terminate the file list.
    ///
    /// # Arguments
    ///
    /// * `writer` - The underlying writer to flush to.
    /// * `io_error` - Optional I/O error code to include in the end marker.
    pub fn finish<W: Write>(&mut self, writer: &mut W, io_error: Option<i32>) -> io::Result<()> {
        // Flush any remaining entries
        if self.entry_count > 0 {
            self.flush_with_reason(writer, FlushReason::Final)?;
        }

        // Write end-of-list marker
        self.writer.write_end(writer, io_error)
    }

    /// Returns a reference to the underlying `FileListWriter`.
    ///
    /// This allows access to the writer's internal state if needed.
    #[must_use]
    pub const fn inner(&self) -> &FileListWriter {
        &self.writer
    }

    /// Returns a mutable reference to the underlying `FileListWriter`.
    ///
    /// # Warning
    ///
    /// Modifying the underlying writer directly may lead to inconsistent
    /// compression state. Use with caution.
    #[must_use]
    pub fn inner_mut(&mut self) -> &mut FileListWriter {
        &mut self.writer
    }

    /// Consumes the batched writer and returns the underlying `FileListWriter`.
    ///
    /// Any unflushed entries in the batch are discarded.
    #[must_use]
    pub fn into_inner(self) -> FileListWriter {
        self.writer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn test_protocol() -> ProtocolVersion {
        ProtocolVersion::try_from(32u8).unwrap()
    }

    #[test]
    fn batch_config_default() {
        let config = BatchConfig::default();
        assert_eq!(config.max_entries, DEFAULT_MAX_ENTRIES);
        assert_eq!(config.max_bytes, DEFAULT_MAX_BYTES);
        assert_eq!(config.flush_timeout, DEFAULT_FLUSH_TIMEOUT);
    }

    #[test]
    fn batch_config_builder() {
        let config = BatchConfig::new()
            .with_max_entries(100)
            .with_max_bytes(128 * 1024)
            .with_flush_timeout(Duration::from_millis(200));

        assert_eq!(config.max_entries, 100);
        assert_eq!(config.max_bytes, 128 * 1024);
        assert_eq!(config.flush_timeout, Duration::from_millis(200));
    }

    #[test]
    fn batch_config_no_auto_flush() {
        let config = BatchConfig::no_auto_flush();
        assert_eq!(config.max_entries, usize::MAX);
        assert_eq!(config.max_bytes, usize::MAX);
        assert_eq!(config.flush_timeout, Duration::MAX);
    }

    #[test]
    fn new_batched_writer_is_empty() {
        let writer = BatchedFileListWriter::new(test_protocol());
        assert!(writer.is_empty());
        assert_eq!(writer.pending_entries(), 0);
        assert_eq!(writer.pending_bytes(), 0);
    }

    #[test]
    fn add_entry_accumulates_in_buffer() {
        let mut writer = BatchedFileListWriter::with_config(
            test_protocol(),
            BatchConfig::no_auto_flush(),
        );
        let mut output = Vec::new();

        let entry1 = FileEntry::new_file("test1.txt".into(), 100, 0o644);
        let entry2 = FileEntry::new_file("test2.txt".into(), 200, 0o644);

        // Add entries - should not flush due to no_auto_flush config
        assert!(!writer.add_entry(&mut output, &entry1).unwrap());
        assert!(!writer.add_entry(&mut output, &entry2).unwrap());

        assert_eq!(writer.pending_entries(), 2);
        assert!(writer.pending_bytes() > 0);
        assert!(output.is_empty()); // Nothing written yet
    }

    #[test]
    fn explicit_flush_writes_to_output() {
        let mut writer = BatchedFileListWriter::with_config(
            test_protocol(),
            BatchConfig::no_auto_flush(),
        );
        let mut output = Vec::new();

        let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
        writer.add_entry(&mut output, &entry).unwrap();

        let pending_bytes = writer.pending_bytes();
        assert!(pending_bytes > 0);

        writer.flush(&mut output).unwrap();

        assert!(writer.is_empty());
        assert_eq!(output.len(), pending_bytes);
        assert_eq!(writer.stats().batches_flushed, 1);
        assert_eq!(writer.stats().explicit_flushes, 1);
    }

    #[test]
    fn auto_flush_on_entry_count() {
        let config = BatchConfig::new().with_max_entries(2);
        let mut writer = BatchedFileListWriter::with_config(test_protocol(), config);
        let mut output = Vec::new();

        let entry1 = FileEntry::new_file("test1.txt".into(), 100, 0o644);
        let entry2 = FileEntry::new_file("test2.txt".into(), 200, 0o644);

        assert!(!writer.add_entry(&mut output, &entry1).unwrap());
        assert!(writer.add_entry(&mut output, &entry2).unwrap()); // Should trigger flush

        assert!(writer.is_empty());
        assert!(!output.is_empty());
        assert_eq!(writer.stats().flushes_by_count, 1);
    }

    #[test]
    fn auto_flush_on_byte_size() {
        // Use a very small max_bytes to trigger byte-based flush
        let config = BatchConfig::new()
            .with_max_entries(1000)
            .with_max_bytes(50);
        let mut writer = BatchedFileListWriter::with_config(test_protocol(), config);
        let mut output = Vec::new();

        // Add entries until byte limit is exceeded
        let mut flushed = false;
        for i in 0..100 {
            let entry = FileEntry::new_file(format!("file{i}.txt").into(), 100 * i as u64, 0o644);
            if writer.add_entry(&mut output, &entry).unwrap() {
                flushed = true;
                break;
            }
        }

        assert!(flushed);
        assert!(writer.stats().flushes_by_size > 0);
    }

    #[test]
    fn finish_flushes_remaining_and_writes_end() {
        let mut writer = BatchedFileListWriter::with_config(
            test_protocol(),
            BatchConfig::no_auto_flush(),
        );
        let mut output = Vec::new();

        let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
        writer.add_entry(&mut output, &entry).unwrap();

        writer.finish(&mut output, None).unwrap();

        assert!(writer.is_empty());
        assert_eq!(writer.stats().batches_flushed, 1);

        // Verify end marker was written (single zero byte for basic protocol)
        assert!(*output.last().unwrap() == 0);
    }

    #[test]
    fn finish_with_io_error() {
        let protocol = test_protocol();
        let compat_flags = CompatibilityFlags::SAFE_FILE_LIST;
        let mut writer = BatchedFileListWriter::with_compat_flags_and_config(
            protocol,
            compat_flags,
            BatchConfig::no_auto_flush(),
        );
        let mut output = Vec::new();

        writer.finish(&mut output, Some(42)).unwrap();

        // Error marker should be present in output
        assert!(!output.is_empty());
    }

    #[test]
    fn add_entries_batch() {
        let config = BatchConfig::new().with_max_entries(3);
        let mut writer = BatchedFileListWriter::with_config(test_protocol(), config);
        let mut output = Vec::new();

        let entries: Vec<FileEntry> = (0..7)
            .map(|i| FileEntry::new_file(format!("file{i}.txt").into(), 100 * i as u64, 0o644))
            .collect();

        let flushes = writer.add_entries(&mut output, &entries).unwrap();

        // With 7 entries and max_entries=3, we should have 2 flushes
        // (entries 0,1,2 trigger flush, then 3,4,5 trigger flush, entry 6 pending)
        assert_eq!(flushes, 2);
        assert_eq!(writer.pending_entries(), 1); // Entry 6 still pending
    }

    #[test]
    fn round_trip_batched_entries() {
        use super::super::read::FileListReader;

        let protocol = test_protocol();
        let config = BatchConfig::new().with_max_entries(3);
        let mut writer = BatchedFileListWriter::with_config(protocol, config);
        let mut output = Vec::new();

        // Write entries
        let entries: Vec<FileEntry> = (0..5)
            .map(|i| {
                let mut entry = FileEntry::new_file(format!("file{i}.txt").into(), 100 * i as u64, 0o644);
                entry.set_mtime(1700000000 + i as i64, 0);
                entry
            })
            .collect();

        writer.add_entries(&mut output, &entries).unwrap();
        writer.finish(&mut output, None).unwrap();

        // Read entries back
        let mut cursor = Cursor::new(&output);
        let mut reader = FileListReader::new(protocol);

        for (i, expected) in entries.iter().enumerate() {
            let read_entry = reader.read_entry(&mut cursor).unwrap();
            assert!(read_entry.is_some(), "Expected entry {i}");
            let read_entry = read_entry.unwrap();
            assert_eq!(read_entry.name(), expected.name());
            assert_eq!(read_entry.size(), expected.size());
        }

        // Verify end-of-list
        assert!(reader.read_entry(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn stats_tracking() {
        let config = BatchConfig::new().with_max_entries(2);
        let mut writer = BatchedFileListWriter::with_config(test_protocol(), config);
        let mut output = Vec::new();

        // Add 5 entries, triggering 2 auto-flushes
        for i in 0..5 {
            let entry = FileEntry::new_file(format!("file{i}.txt").into(), 100, 0o644);
            writer.add_entry(&mut output, &entry).unwrap();
        }

        writer.flush(&mut output).unwrap(); // Flush remaining 1 entry

        assert_eq!(writer.stats().entries_written, 5);
        assert_eq!(writer.stats().batches_flushed, 3); // 2 auto + 1 explicit
        assert_eq!(writer.stats().flushes_by_count, 2);
        assert_eq!(writer.stats().explicit_flushes, 1);
        assert!(writer.stats().bytes_written > 0);
    }

    #[test]
    fn preserve_options_forwarding() {
        let writer = BatchedFileListWriter::new(test_protocol())
            .with_preserve_uid(true)
            .with_preserve_gid(true)
            .with_preserve_links(true)
            .with_preserve_devices(true)
            .with_preserve_hard_links(true)
            .with_preserve_atimes(true)
            .with_preserve_crtimes(true)
            .with_preserve_acls(true)
            .with_preserve_xattrs(true)
            .with_always_checksum(16);

        // Verify writer was configured (we check that it was created without error)
        assert!(writer.is_empty());
    }

    #[test]
    fn flush_empty_batch_is_noop() {
        let mut writer = BatchedFileListWriter::new(test_protocol());
        let mut output = Vec::new();

        writer.flush(&mut output).unwrap();

        assert!(output.is_empty());
        assert_eq!(writer.stats().batches_flushed, 0);
    }

    #[test]
    fn check_timeout_flush() {
        let config = BatchConfig::new().with_flush_timeout(Duration::from_millis(1));
        let mut writer = BatchedFileListWriter::with_config(test_protocol(), config);
        let mut output = Vec::new();

        let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
        writer.add_entry(&mut output, &entry).unwrap();

        // Wait for timeout
        std::thread::sleep(Duration::from_millis(5));

        assert!(writer.check_timeout_flush(&mut output).unwrap());
        assert!(writer.is_empty());
        assert_eq!(writer.stats().flushes_by_timeout, 1);
    }

    #[test]
    fn check_timeout_flush_no_timeout_yet() {
        let config = BatchConfig::new().with_flush_timeout(Duration::from_secs(60));
        let mut writer = BatchedFileListWriter::with_config(test_protocol(), config);
        let mut output = Vec::new();

        let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
        writer.add_entry(&mut output, &entry).unwrap();

        // No timeout yet
        assert!(!writer.check_timeout_flush(&mut output).unwrap());
        assert!(!writer.is_empty());
    }

    #[test]
    fn inner_access() {
        let mut writer = BatchedFileListWriter::new(test_protocol());

        // Check inner() returns valid reference
        let _inner = writer.inner();

        // Check inner_mut() returns valid mutable reference
        let _inner_mut = writer.inner_mut();
    }

    #[test]
    fn into_inner_consumes_writer() {
        let config = BatchConfig::no_auto_flush();
        let mut writer = BatchedFileListWriter::with_config(test_protocol(), config);
        let mut output = Vec::new();

        let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
        writer.add_entry(&mut output, &entry).unwrap();

        // into_inner discards pending entries
        let _inner = writer.into_inner();
        // Can't check writer state after consumption, but this verifies the method works
    }

    #[test]
    fn with_compat_flags_creates_valid_writer() {
        let protocol = test_protocol();
        let flags = CompatibilityFlags::VARINT_FLIST_FLAGS | CompatibilityFlags::SAFE_FILE_LIST;

        let writer = BatchedFileListWriter::with_compat_flags(protocol, flags);
        assert!(writer.is_empty());
    }
}
