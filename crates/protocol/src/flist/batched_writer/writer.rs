//! Core batched file list writer implementation.

use std::io::{self, Write};
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

use super::config::{BatchConfig, DEFAULT_MAX_BYTES};
use super::stats::{BatchStats, FlushReason};
use crate::flist::entry::FileEntry;
use crate::flist::write::FileListWriter;
use crate::iconv::FilenameConverter;
use crate::{CompatibilityFlags, ProtocolVersion};

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

    /// Sets whether device numbers (block/char) should be written to the wire.
    #[must_use]
    pub fn with_preserve_devices(mut self, preserve: bool) -> Self {
        self.writer = self.writer.with_preserve_devices(preserve);
        self
    }

    /// Sets whether special files (FIFOs/sockets) should be written to the wire.
    #[must_use]
    pub fn with_preserve_specials(mut self, preserve: bool) -> Self {
        self.writer = self.writer.with_preserve_specials(preserve);
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

    /// Sets the checksum seed for xattr abbreviated value digests.
    #[must_use]
    pub fn with_checksum_seed(mut self, seed: i32) -> Self {
        self.writer = self.writer.with_checksum_seed(seed);
        self
    }

    /// Enables checksum mode with the given checksum length.
    #[must_use]
    pub fn with_always_checksum(mut self, csum_len: usize) -> Self {
        self.writer = self.writer.with_always_checksum(csum_len);
        self
    }

    /// Sets the filename encoding converter for `--iconv` support.
    ///
    /// Mirrors [`FileListWriter::with_iconv`] so that the batched sender
    /// path transcodes filenames from local to remote charset before
    /// emission, matching upstream `flist.c send_file_entry()`'s
    /// `iconv_buf(ic_send, ...)` call.
    #[must_use]
    pub fn with_iconv(mut self, converter: FilenameConverter) -> Self {
        self.writer = self.writer.with_iconv(converter);
        self
    }

    /// Returns the batching statistics.
    #[must_use]
    pub const fn stats(&self) -> &BatchStats {
        &self.stats
    }

    /// Returns the file list statistics from the underlying writer.
    #[must_use]
    pub const fn flist_stats(&self) -> &crate::flist::state::FileListStats {
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
    /// Returns `Ok(true)` if the batch was flushed, `Ok(false)` otherwise.
    pub fn add_entry<W: Write>(&mut self, writer: &mut W, entry: &FileEntry) -> io::Result<bool> {
        if self.batch_start.is_none() {
            self.batch_start = Some(Instant::now());
        }

        self.writer.write_entry(&mut self.buffer, entry)?;
        self.entry_count += 1;

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
    /// More efficient than calling [`add_entry`](Self::add_entry) repeatedly
    /// as it minimizes flush checks.
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
        if self.entry_count >= self.config.max_entries {
            return Some(FlushReason::EntryCount);
        }

        if self.buffer.len() >= self.config.max_bytes {
            return Some(FlushReason::ByteSize);
        }

        if let Some(start) = self.batch_start {
            if start.elapsed() >= self.config.flush_timeout {
                return Some(FlushReason::Timeout);
            }
        }

        None
    }

    /// Flushes the current batch to the writer.
    ///
    /// Writes all accumulated entries to the underlying writer and resets the
    /// batch state. No-op if the batch is empty.
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
    fn flush_with_reason<W: Write>(
        &mut self,
        writer: &mut W,
        reason: FlushReason,
    ) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        writer.write_all(&self.buffer)?;

        self.stats.entries_written += self.entry_count as u64;
        self.stats.bytes_written += self.buffer.len() as u64;
        self.stats.batches_flushed += 1;

        match reason {
            FlushReason::EntryCount => self.stats.flushes_by_count += 1,
            FlushReason::ByteSize => self.stats.flushes_by_size += 1,
            FlushReason::Timeout => self.stats.flushes_by_timeout += 1,
            FlushReason::Explicit | FlushReason::Final => self.stats.explicit_flushes += 1,
        }

        self.buffer.clear();
        self.entry_count = 0;
        self.batch_start = None;

        Ok(())
    }

    /// Finishes the file list by flushing remaining entries and writing the end-of-list marker.
    ///
    /// Must be called when all entries have been added to properly terminate the file list.
    pub fn finish<W: Write>(&mut self, writer: &mut W, io_error: Option<i32>) -> io::Result<()> {
        if self.entry_count > 0 {
            self.flush_with_reason(writer, FlushReason::Final)?;
        }

        self.writer.write_end(writer, io_error)
    }

    /// Returns a reference to the underlying `FileListWriter`.
    #[must_use]
    pub const fn inner(&self) -> &FileListWriter {
        &self.writer
    }

    /// Returns a mutable reference to the underlying `FileListWriter`.
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

    /// Backdates the batch start timestamp so that the flush timeout has
    /// already elapsed, making timeout tests fully deterministic.
    #[cfg(test)]
    pub(super) fn expire_batch_timeout(&mut self) {
        if self.batch_start.is_some() {
            self.batch_start =
                Some(Instant::now() - self.config.flush_timeout - Duration::from_millis(1));
        }
    }
}
