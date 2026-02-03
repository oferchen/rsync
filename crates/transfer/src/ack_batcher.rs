//! Batched acknowledgment system for reducing network overhead.
//!
//! This module implements ACK batching to reduce the number of round-trips
//! during rsync file transfers. Instead of sending an acknowledgment for
//! each file individually, we batch multiple ACKs together and send them
//! as a single network message.
//!
//! # Design
//!
//! The `AckBatcher` accumulates file transfer acknowledgments and flushes
//! them when any of the following conditions are met:
//!
//! 1. **Count threshold**: Batch reaches N files (default: 16)
//! 2. **Time threshold**: T milliseconds have elapsed since first ACK (default: 50ms)
//! 3. **Error condition**: An error requires immediate ACK
//! 4. **Explicit flush**: Caller requests immediate flush
//!
//! # Wire Format
//!
//! Batched ACKs are encoded as a sequence of ACK entries in the multiplex stream:
//!
//! ```text
//! ┌─────────────────────────────────────────────────┐
//! │ Batched ACK Message                             │
//! ├─────────────────────────────────────────────────┤
//! │ count: u16 (number of ACKs in batch)            │
//! │ ack[0]: ndx(i32) + status(u8) [+ error_len + error_msg] │
//! │ ack[1]: ndx(i32) + status(u8) [+ error_len + error_msg] │
//! │ ...                                             │
//! │ ack[n-1]: ndx(i32) + status(u8) [+ error_len + error_msg] │
//! └─────────────────────────────────────────────────┘
//! ```
//!
//! # Performance Impact
//!
//! With 92,437 files and 0.5ms network latency:
//! - Individual ACKs: 92,437 * 0.5ms = 46.2s latency overhead
//! - Batched ACKs (batch=16): 92,437 / 16 * 0.5ms = 2.9s latency overhead
//!
//! Combined with request pipelining, this dramatically reduces transfer time.
//!
//! # Protocol Compatibility
//!
//! Batched ACKs are only used when both sides support them (detected via
//! capability negotiation). When communicating with legacy rsync, we fall
//! back to individual ACKs.

use std::io::{self, Write};
use std::time::{Duration, Instant};

/// Default number of ACKs to batch before flushing.
pub const DEFAULT_BATCH_SIZE: usize = 16;

/// Default time threshold before flushing (50ms).
pub const DEFAULT_BATCH_TIMEOUT_MS: u64 = 50;

/// Minimum batch size (must be at least 1).
pub const MIN_BATCH_SIZE: usize = 1;

/// Maximum batch size to prevent memory bloat.
pub const MAX_BATCH_SIZE: usize = 256;

/// Maximum batch timeout (1 second).
pub const MAX_BATCH_TIMEOUT_MS: u64 = 1000;

/// Status codes for individual ACK entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AckStatus {
    /// File transfer completed successfully.
    Success = 0,
    /// File transfer failed with an error.
    Error = 1,
    /// File was skipped (already up to date).
    Skipped = 2,
    /// File checksum verification failed.
    ChecksumError = 3,
    /// File I/O error during transfer.
    IoError = 4,
}

impl AckStatus {
    /// Returns true if this status represents an error condition.
    #[must_use]
    pub const fn is_error(&self) -> bool {
        matches!(
            self,
            AckStatus::Error | AckStatus::ChecksumError | AckStatus::IoError
        )
    }

    /// Converts a u8 to an AckStatus.
    ///
    /// Returns `AckStatus::Error` for unknown values.
    #[must_use]
    pub const fn from_u8(value: u8) -> Self {
        match value {
            0 => AckStatus::Success,
            1 => AckStatus::Error,
            2 => AckStatus::Skipped,
            3 => AckStatus::ChecksumError,
            4 => AckStatus::IoError,
            _ => AckStatus::Error,
        }
    }
}

/// A single acknowledgment entry in a batch.
#[derive(Debug, Clone)]
pub struct AckEntry {
    /// File index (NDX) being acknowledged.
    pub ndx: i32,
    /// Status of the transfer.
    pub status: AckStatus,
    /// Optional error message for error statuses.
    pub error_msg: Option<String>,
}

impl AckEntry {
    /// Creates a new successful ACK entry.
    #[must_use]
    pub const fn success(ndx: i32) -> Self {
        Self {
            ndx,
            status: AckStatus::Success,
            error_msg: None,
        }
    }

    /// Creates a new skipped ACK entry.
    #[must_use]
    pub const fn skipped(ndx: i32) -> Self {
        Self {
            ndx,
            status: AckStatus::Skipped,
            error_msg: None,
        }
    }

    /// Creates a new error ACK entry.
    #[must_use]
    pub fn error(ndx: i32, msg: impl Into<String>) -> Self {
        Self {
            ndx,
            status: AckStatus::Error,
            error_msg: Some(msg.into()),
        }
    }

    /// Creates a new checksum error ACK entry.
    #[must_use]
    pub fn checksum_error(ndx: i32, msg: impl Into<String>) -> Self {
        Self {
            ndx,
            status: AckStatus::ChecksumError,
            error_msg: Some(msg.into()),
        }
    }

    /// Creates a new I/O error ACK entry.
    #[must_use]
    pub fn io_error(ndx: i32, msg: impl Into<String>) -> Self {
        Self {
            ndx,
            status: AckStatus::IoError,
            error_msg: Some(msg.into()),
        }
    }

    /// Writes this ACK entry to the wire.
    ///
    /// Wire format:
    /// - ndx: 4 bytes (i32 LE)
    /// - status: 1 byte
    /// - if error: error_len (u16 LE) + error_msg bytes
    pub fn write<W: Write + ?Sized>(&self, writer: &mut W) -> io::Result<()> {
        // Write NDX
        writer.write_all(&self.ndx.to_le_bytes())?;

        // Write status
        writer.write_all(&[self.status as u8])?;

        // Write error message if present
        if let Some(ref msg) = self.error_msg {
            let msg_bytes = msg.as_bytes();
            let len = msg_bytes.len().min(u16::MAX as usize) as u16;
            writer.write_all(&len.to_le_bytes())?;
            writer.write_all(&msg_bytes[..len as usize])?;
        }

        Ok(())
    }

    /// Reads an ACK entry from the wire.
    pub fn read<R: io::Read + ?Sized>(reader: &mut R) -> io::Result<Self> {
        // Read NDX
        let mut ndx_buf = [0u8; 4];
        reader.read_exact(&mut ndx_buf)?;
        let ndx = i32::from_le_bytes(ndx_buf);

        // Read status
        let mut status_buf = [0u8; 1];
        reader.read_exact(&mut status_buf)?;
        let status = AckStatus::from_u8(status_buf[0]);

        // Read error message if status indicates error
        let error_msg = if status.is_error() {
            let mut len_buf = [0u8; 2];
            reader.read_exact(&mut len_buf)?;
            let len = u16::from_le_bytes(len_buf) as usize;

            if len > 0 {
                let mut msg_buf = vec![0u8; len];
                reader.read_exact(&mut msg_buf)?;
                Some(String::from_utf8_lossy(&msg_buf).into_owned())
            } else {
                None
            }
        } else {
            None
        };

        Ok(Self {
            ndx,
            status,
            error_msg,
        })
    }
}

/// Configuration for the ACK batcher.
#[derive(Debug, Clone)]
pub struct AckBatcherConfig {
    /// Maximum number of ACKs to batch before flushing.
    pub batch_size: usize,
    /// Maximum time to wait before flushing (in milliseconds).
    pub batch_timeout_ms: u64,
    /// Whether batching is enabled (disabled for legacy protocol compatibility).
    pub enabled: bool,
}

impl Default for AckBatcherConfig {
    fn default() -> Self {
        Self {
            batch_size: DEFAULT_BATCH_SIZE,
            batch_timeout_ms: DEFAULT_BATCH_TIMEOUT_MS,
            enabled: true,
        }
    }
}

impl AckBatcherConfig {
    /// Creates a new configuration with the specified batch size.
    #[must_use]
    pub fn with_batch_size(mut self, size: usize) -> Self {
        self.batch_size = size.clamp(MIN_BATCH_SIZE, MAX_BATCH_SIZE);
        self
    }

    /// Creates a new configuration with the specified timeout.
    #[must_use]
    pub fn with_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.batch_timeout_ms = timeout_ms.min(MAX_BATCH_TIMEOUT_MS);
        self
    }

    /// Disables batching (for legacy protocol compatibility).
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            batch_size: 1,
            batch_timeout_ms: 0,
            enabled: false,
        }
    }

    /// Returns true if batching is enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// Batched acknowledgment accumulator.
///
/// Collects ACKs and flushes them when thresholds are reached.
///
/// # Example
///
/// ```ignore
/// use transfer::ack_batcher::{AckBatcher, AckBatcherConfig, AckEntry};
///
/// let mut batcher = AckBatcher::new(AckBatcherConfig::default());
///
/// // Queue successful ACKs
/// batcher.queue(AckEntry::success(0));
/// batcher.queue(AckEntry::success(1));
///
/// // Check if flush is needed
/// if batcher.should_flush() {
///     let batch = batcher.take_batch();
///     send_batch_to_network(batch);
/// }
/// ```
#[derive(Debug)]
pub struct AckBatcher {
    /// Configuration for batching behavior.
    config: AckBatcherConfig,
    /// Pending ACK entries.
    pending: Vec<AckEntry>,
    /// When the first ACK in current batch was queued.
    batch_start: Option<Instant>,
    /// Total ACKs sent (for statistics).
    total_sent: u64,
    /// Total batches sent (for statistics).
    batches_sent: u64,
}

impl AckBatcher {
    /// Creates a new ACK batcher with the given configuration.
    #[must_use]
    pub fn new(config: AckBatcherConfig) -> Self {
        Self {
            pending: Vec::with_capacity(config.batch_size),
            config,
            batch_start: None,
            total_sent: 0,
            batches_sent: 0,
        }
    }

    /// Creates a new ACK batcher with default configuration.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(AckBatcherConfig::default())
    }

    /// Creates a disabled ACK batcher (immediate flush for each ACK).
    #[must_use]
    pub fn disabled() -> Self {
        Self::new(AckBatcherConfig::disabled())
    }

    /// Returns true if batching is enabled.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.config.is_enabled()
    }

    /// Returns the number of pending ACKs.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Returns true if there are no pending ACKs.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Returns the configured batch size.
    #[must_use]
    pub fn batch_size(&self) -> usize {
        self.config.batch_size
    }

    /// Queues an ACK entry for batching.
    ///
    /// If the entry has an error status, this will trigger an immediate flush
    /// on the next `should_flush()` check.
    pub fn queue(&mut self, entry: AckEntry) {
        if self.batch_start.is_none() {
            self.batch_start = Some(Instant::now());
        }
        self.pending.push(entry);
    }

    /// Queues a successful ACK for the given NDX.
    pub fn queue_success(&mut self, ndx: i32) {
        self.queue(AckEntry::success(ndx));
    }

    /// Queues a skipped ACK for the given NDX.
    pub fn queue_skipped(&mut self, ndx: i32) {
        self.queue(AckEntry::skipped(ndx));
    }

    /// Queues an error ACK for the given NDX.
    pub fn queue_error(&mut self, ndx: i32, msg: impl Into<String>) {
        self.queue(AckEntry::error(ndx, msg));
    }

    /// Returns true if the batch should be flushed now.
    ///
    /// A batch should be flushed if any of the following conditions are met:
    /// 1. Batch has reached the size threshold
    /// 2. Batch has exceeded the time threshold
    /// 3. Batch contains an error entry (errors should be reported immediately)
    /// 4. Batching is disabled (every ACK is immediately flushable)
    #[must_use]
    pub fn should_flush(&self) -> bool {
        if self.pending.is_empty() {
            return false;
        }

        // Check if batching is disabled
        if !self.config.enabled {
            return true;
        }

        // Check batch size threshold
        if self.pending.len() >= self.config.batch_size {
            return true;
        }

        // Check for error entries (errors should be reported immediately)
        if self.pending.iter().any(|e| e.status.is_error()) {
            return true;
        }

        // Check time threshold
        if let Some(start) = self.batch_start {
            let elapsed = start.elapsed();
            if elapsed >= Duration::from_millis(self.config.batch_timeout_ms) {
                return true;
            }
        }

        false
    }

    /// Takes the current batch, resetting the batcher state.
    ///
    /// Returns the pending ACK entries and resets the batch timer.
    pub fn take_batch(&mut self) -> Vec<AckEntry> {
        let batch = std::mem::take(&mut self.pending);
        self.batch_start = None;
        self.pending.reserve(self.config.batch_size);

        // Update statistics
        if !batch.is_empty() {
            self.total_sent += batch.len() as u64;
            self.batches_sent += 1;
        }

        batch
    }

    /// Writes a batch of ACKs to the wire.
    ///
    /// Wire format:
    /// - count: u16 LE (number of ACKs)
    /// - entries: sequence of AckEntry
    pub fn write_batch<W: Write + ?Sized>(batch: &[AckEntry], writer: &mut W) -> io::Result<()> {
        if batch.is_empty() {
            return Ok(());
        }

        // Write count
        let count = batch.len().min(u16::MAX as usize) as u16;
        writer.write_all(&count.to_le_bytes())?;

        // Write entries
        for entry in batch.iter().take(count as usize) {
            entry.write(writer)?;
        }

        Ok(())
    }

    /// Reads a batch of ACKs from the wire.
    pub fn read_batch<R: io::Read + ?Sized>(reader: &mut R) -> io::Result<Vec<AckEntry>> {
        // Read count
        let mut count_buf = [0u8; 2];
        reader.read_exact(&mut count_buf)?;
        let count = u16::from_le_bytes(count_buf) as usize;

        // Read entries
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            entries.push(AckEntry::read(reader)?);
        }

        Ok(entries)
    }

    /// Flushes the batch to the given writer if flush conditions are met.
    ///
    /// Returns the number of ACKs flushed, or 0 if no flush was needed.
    pub fn flush_if_needed<W: Write + ?Sized>(&mut self, writer: &mut W) -> io::Result<usize> {
        if !self.should_flush() {
            return Ok(0);
        }

        let batch = self.take_batch();
        let count = batch.len();

        Self::write_batch(&batch, writer)?;
        writer.flush()?;

        Ok(count)
    }

    /// Forces a flush of all pending ACKs.
    ///
    /// Returns the number of ACKs flushed.
    pub fn force_flush<W: Write + ?Sized>(&mut self, writer: &mut W) -> io::Result<usize> {
        if self.pending.is_empty() {
            return Ok(0);
        }

        let batch = self.take_batch();
        let count = batch.len();

        Self::write_batch(&batch, writer)?;
        writer.flush()?;

        Ok(count)
    }

    /// Returns statistics about the batcher.
    #[must_use]
    pub fn stats(&self) -> AckBatcherStats {
        AckBatcherStats {
            total_sent: self.total_sent,
            batches_sent: self.batches_sent,
            currently_pending: self.pending.len(),
            average_batch_size: if self.batches_sent > 0 {
                self.total_sent as f64 / self.batches_sent as f64
            } else {
                0.0
            },
        }
    }

    /// Calculates the time remaining until the timeout flush.
    ///
    /// Returns `None` if no batch is pending or timeout is disabled.
    #[must_use]
    pub fn time_until_timeout(&self) -> Option<Duration> {
        let start = self.batch_start?;
        let timeout = Duration::from_millis(self.config.batch_timeout_ms);
        let elapsed = start.elapsed();

        if elapsed >= timeout {
            Some(Duration::ZERO)
        } else {
            Some(timeout - elapsed)
        }
    }
}

/// Statistics about ACK batching performance.
#[derive(Debug, Clone, Default)]
pub struct AckBatcherStats {
    /// Total number of individual ACKs sent.
    pub total_sent: u64,
    /// Total number of batches sent.
    pub batches_sent: u64,
    /// Number of ACKs currently pending.
    pub currently_pending: usize,
    /// Average number of ACKs per batch.
    pub average_batch_size: f64,
}

impl AckBatcherStats {
    /// Returns the efficiency gain from batching.
    ///
    /// This is the ratio of individual ACKs that would have been sent
    /// to the number of batches actually sent. A value of 8.0 means
    /// batching reduced network messages by 8x.
    #[must_use]
    pub fn efficiency_ratio(&self) -> f64 {
        if self.batches_sent == 0 {
            return 1.0;
        }
        self.total_sent as f64 / self.batches_sent as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_ack_entry_success() {
        let entry = AckEntry::success(42);
        assert_eq!(entry.ndx, 42);
        assert_eq!(entry.status, AckStatus::Success);
        assert!(entry.error_msg.is_none());
    }

    #[test]
    fn test_ack_entry_error() {
        let entry = AckEntry::error(10, "test error");
        assert_eq!(entry.ndx, 10);
        assert_eq!(entry.status, AckStatus::Error);
        assert_eq!(entry.error_msg.as_deref(), Some("test error"));
    }

    #[test]
    fn test_ack_entry_roundtrip_success() {
        let entry = AckEntry::success(100);
        let mut buf = Vec::new();
        entry.write(&mut buf).unwrap();

        let mut cursor = Cursor::new(&buf);
        let read_entry = AckEntry::read(&mut cursor).unwrap();

        assert_eq!(read_entry.ndx, 100);
        assert_eq!(read_entry.status, AckStatus::Success);
        assert!(read_entry.error_msg.is_none());
    }

    #[test]
    fn test_ack_entry_roundtrip_error() {
        let entry = AckEntry::error(50, "file not found");
        let mut buf = Vec::new();
        entry.write(&mut buf).unwrap();

        let mut cursor = Cursor::new(&buf);
        let read_entry = AckEntry::read(&mut cursor).unwrap();

        assert_eq!(read_entry.ndx, 50);
        assert_eq!(read_entry.status, AckStatus::Error);
        assert_eq!(read_entry.error_msg.as_deref(), Some("file not found"));
    }

    #[test]
    fn test_ack_batcher_queue_and_take() {
        let mut batcher = AckBatcher::new(AckBatcherConfig::default().with_batch_size(4));

        batcher.queue_success(0);
        batcher.queue_success(1);
        batcher.queue_skipped(2);

        assert_eq!(batcher.pending_count(), 3);
        assert!(!batcher.should_flush()); // Not at threshold

        batcher.queue_success(3);
        assert!(batcher.should_flush()); // At threshold

        let batch = batcher.take_batch();
        assert_eq!(batch.len(), 4);
        assert!(batcher.is_empty());
    }

    #[test]
    fn test_ack_batcher_error_triggers_flush() {
        let mut batcher = AckBatcher::new(AckBatcherConfig::default().with_batch_size(16));

        batcher.queue_success(0);
        assert!(!batcher.should_flush());

        batcher.queue_error(1, "test error");
        assert!(batcher.should_flush()); // Error triggers immediate flush
    }

    #[test]
    fn test_ack_batcher_disabled() {
        let mut batcher = AckBatcher::disabled();

        batcher.queue_success(0);
        assert!(batcher.should_flush()); // Every ACK triggers flush when disabled
    }

    #[test]
    fn test_batch_write_and_read() {
        let batch = vec![
            AckEntry::success(0),
            AckEntry::success(1),
            AckEntry::skipped(2),
            AckEntry::error(3, "error msg"),
        ];

        let mut buf = Vec::new();
        AckBatcher::write_batch(&batch, &mut buf).unwrap();

        let mut cursor = Cursor::new(&buf);
        let read_batch = AckBatcher::read_batch(&mut cursor).unwrap();

        assert_eq!(read_batch.len(), 4);
        assert_eq!(read_batch[0].ndx, 0);
        assert_eq!(read_batch[0].status, AckStatus::Success);
        assert_eq!(read_batch[2].status, AckStatus::Skipped);
        assert_eq!(read_batch[3].status, AckStatus::Error);
        assert_eq!(read_batch[3].error_msg.as_deref(), Some("error msg"));
    }

    #[test]
    fn test_batcher_config_clamps_values() {
        let config = AckBatcherConfig::default()
            .with_batch_size(0)
            .with_timeout_ms(10000);

        assert_eq!(config.batch_size, MIN_BATCH_SIZE);
        assert_eq!(config.batch_timeout_ms, MAX_BATCH_TIMEOUT_MS);

        let config2 = AckBatcherConfig::default().with_batch_size(1000);
        assert_eq!(config2.batch_size, MAX_BATCH_SIZE);
    }

    #[test]
    fn test_batcher_stats() {
        let mut batcher = AckBatcher::new(AckBatcherConfig::default().with_batch_size(2));

        batcher.queue_success(0);
        batcher.queue_success(1);
        let _ = batcher.take_batch();

        batcher.queue_success(2);
        batcher.queue_success(3);
        let _ = batcher.take_batch();

        let stats = batcher.stats();
        assert_eq!(stats.total_sent, 4);
        assert_eq!(stats.batches_sent, 2);
        assert!((stats.average_batch_size - 2.0).abs() < f64::EPSILON);
        assert!((stats.efficiency_ratio() - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_empty_batch_write() {
        let batch: Vec<AckEntry> = Vec::new();
        let mut buf = Vec::new();
        AckBatcher::write_batch(&batch, &mut buf).unwrap();
        assert!(buf.is_empty());
    }

    #[test]
    fn test_ack_status_from_u8() {
        assert_eq!(AckStatus::from_u8(0), AckStatus::Success);
        assert_eq!(AckStatus::from_u8(1), AckStatus::Error);
        assert_eq!(AckStatus::from_u8(2), AckStatus::Skipped);
        assert_eq!(AckStatus::from_u8(3), AckStatus::ChecksumError);
        assert_eq!(AckStatus::from_u8(4), AckStatus::IoError);
        assert_eq!(AckStatus::from_u8(255), AckStatus::Error); // Unknown maps to Error
    }

    #[test]
    fn test_ack_status_is_error() {
        assert!(!AckStatus::Success.is_error());
        assert!(!AckStatus::Skipped.is_error());
        assert!(AckStatus::Error.is_error());
        assert!(AckStatus::ChecksumError.is_error());
        assert!(AckStatus::IoError.is_error());
    }

    #[test]
    fn test_flush_if_needed_no_pending() {
        let mut batcher = AckBatcher::with_defaults();
        let mut buf = Vec::new();
        let count = batcher.flush_if_needed(&mut buf).unwrap();
        assert_eq!(count, 0);
        assert!(buf.is_empty());
    }

    #[test]
    fn test_force_flush() {
        let mut batcher = AckBatcher::new(AckBatcherConfig::default().with_batch_size(100));
        batcher.queue_success(0);
        batcher.queue_success(1);

        assert!(!batcher.should_flush()); // Below threshold

        let mut buf = Vec::new();
        let count = batcher.force_flush(&mut buf).unwrap();

        assert_eq!(count, 2);
        assert!(batcher.is_empty());
        assert!(!buf.is_empty());
    }

    #[test]
    fn test_time_until_timeout_no_pending() {
        let batcher = AckBatcher::with_defaults();
        assert!(batcher.time_until_timeout().is_none());
    }

    #[test]
    fn test_time_until_timeout_with_pending() {
        let mut batcher =
            AckBatcher::new(AckBatcherConfig::default().with_timeout_ms(1000));
        batcher.queue_success(0);

        let timeout = batcher.time_until_timeout();
        assert!(timeout.is_some());
        // Should be close to 1000ms (minus tiny elapsed time)
        assert!(timeout.unwrap() > Duration::from_millis(900));
    }

    #[test]
    fn test_ack_entry_checksum_error() {
        let entry = AckEntry::checksum_error(5, "mismatch");
        assert_eq!(entry.status, AckStatus::ChecksumError);
        assert!(entry.status.is_error());
    }

    #[test]
    fn test_ack_entry_io_error() {
        let entry = AckEntry::io_error(7, "disk full");
        assert_eq!(entry.status, AckStatus::IoError);
        assert!(entry.status.is_error());
    }

    #[test]
    fn test_batch_roundtrip_various_statuses() {
        let batch = vec![
            AckEntry::success(0),
            AckEntry::skipped(1),
            AckEntry::error(2, "generic error"),
            AckEntry::checksum_error(3, "checksum mismatch"),
            AckEntry::io_error(4, "permission denied"),
        ];

        let mut buf = Vec::new();
        AckBatcher::write_batch(&batch, &mut buf).unwrap();

        let mut cursor = Cursor::new(&buf);
        let read_batch = AckBatcher::read_batch(&mut cursor).unwrap();

        assert_eq!(read_batch.len(), 5);
        assert_eq!(read_batch[0].status, AckStatus::Success);
        assert_eq!(read_batch[1].status, AckStatus::Skipped);
        assert_eq!(read_batch[2].status, AckStatus::Error);
        assert_eq!(read_batch[3].status, AckStatus::ChecksumError);
        assert_eq!(read_batch[4].status, AckStatus::IoError);

        // Verify error messages preserved
        assert_eq!(read_batch[2].error_msg.as_deref(), Some("generic error"));
        assert_eq!(
            read_batch[3].error_msg.as_deref(),
            Some("checksum mismatch")
        );
        assert_eq!(
            read_batch[4].error_msg.as_deref(),
            Some("permission denied")
        );
    }

    // ========================================================================
    // Integration Tests
    // ========================================================================

    /// Simulates a transfer scenario where multiple files are processed
    /// and ACKs are batched before being sent.
    #[test]
    fn test_transfer_scenario_batching() {
        let config = AckBatcherConfig::default().with_batch_size(4);
        let mut batcher = AckBatcher::new(config);
        let mut network_output = Vec::new();
        let mut batches_sent = 0;

        // Simulate processing 10 files
        for ndx in 0..10i32 {
            // Process file (simulated)
            let result = if ndx == 5 {
                // File 5 has an error
                AckEntry::io_error(ndx, "write failed")
            } else if ndx == 7 {
                // File 7 was skipped
                AckEntry::skipped(ndx)
            } else {
                AckEntry::success(ndx)
            };

            batcher.queue(result);

            // Flush if needed
            if batcher.should_flush() {
                let count = batcher.force_flush(&mut network_output).unwrap();
                if count > 0 {
                    batches_sent += 1;
                }
            }
        }

        // Final flush for remaining ACKs
        let count = batcher.force_flush(&mut network_output).unwrap();
        if count > 0 {
            batches_sent += 1;
        }

        // With batch_size=4 and 10 files:
        // - Batch 1: files 0-3 (triggered by count)
        // - Batch 2: files 4-5 (triggered by error at file 5)
        // - Batch 3: files 6-9 (final flush)
        // Total: 3 batches (fewer than 10 individual messages!)
        assert!(batches_sent >= 1); // At least one batch should be sent
        assert!(batches_sent <= 5); // Much fewer than 10 individual ACKs

        // Verify we can read back the batches
        let mut cursor = Cursor::new(&network_output);
        let mut all_entries = Vec::new();

        // Read all batches
        while cursor.position() < network_output.len() as u64 {
            let batch = AckBatcher::read_batch(&mut cursor).unwrap();
            all_entries.extend(batch);
        }

        // All 10 entries should be recoverable
        assert_eq!(all_entries.len(), 10);

        // Verify the entries are correct
        assert_eq!(all_entries[5].status, AckStatus::IoError);
        assert_eq!(all_entries[7].status, AckStatus::Skipped);
        for i in [0, 1, 2, 3, 4, 6, 8, 9] {
            assert_eq!(all_entries[i].status, AckStatus::Success);
        }
    }

    /// Tests that the batcher efficiently handles large transfers.
    #[test]
    fn test_large_transfer_efficiency() {
        let file_count = 1000;
        let batch_size = 16;

        let config = AckBatcherConfig::default().with_batch_size(batch_size);
        let mut batcher = AckBatcher::new(config);
        let mut network_output = Vec::new();

        // Simulate processing many files
        for ndx in 0..file_count {
            batcher.queue_success(ndx);

            if batcher.should_flush() {
                batcher.force_flush(&mut network_output).unwrap();
            }
        }
        batcher.force_flush(&mut network_output).unwrap();

        let stats = batcher.stats();

        // Verify efficiency
        assert_eq!(stats.total_sent, file_count as u64);
        // With batch_size=16 and 1000 files, we expect ~63 batches (1000/16 = 62.5)
        let expected_batches = (file_count as f64 / batch_size as f64).ceil() as u64;
        assert!(stats.batches_sent <= expected_batches);

        // Efficiency ratio should be close to batch_size
        let efficiency = stats.efficiency_ratio();
        assert!(
            efficiency >= (batch_size - 1) as f64,
            "efficiency {efficiency} should be >= {}",
            batch_size - 1
        );
    }

    /// Tests that errors trigger immediate flush.
    #[test]
    fn test_error_immediate_flush() {
        let config = AckBatcherConfig::default()
            .with_batch_size(100) // Very high threshold
            .with_timeout_ms(10000); // Very long timeout

        let mut batcher = AckBatcher::new(config);

        // Queue some successes
        batcher.queue_success(0);
        batcher.queue_success(1);
        assert!(!batcher.should_flush()); // Not yet

        // Queue an error - should trigger immediate flush
        batcher.queue_error(2, "test error");
        assert!(batcher.should_flush()); // Error triggers flush
    }

    /// Tests round-trip of large batches with various statuses.
    #[test]
    fn test_large_batch_roundtrip() {
        let mut batch = Vec::with_capacity(256);

        // Create a batch with various statuses
        for i in 0..256i32 {
            let entry = match i % 5 {
                0 => AckEntry::success(i),
                1 => AckEntry::skipped(i),
                2 => AckEntry::error(i, format!("error for file {i}")),
                3 => AckEntry::checksum_error(i, format!("checksum failed for {i}")),
                _ => AckEntry::io_error(i, format!("io error at {i}")),
            };
            batch.push(entry);
        }

        // Write the batch
        let mut buf = Vec::new();
        AckBatcher::write_batch(&batch, &mut buf).unwrap();

        // Read it back
        let mut cursor = Cursor::new(&buf);
        let read_batch = AckBatcher::read_batch(&mut cursor).unwrap();

        // Verify all entries match
        assert_eq!(read_batch.len(), 256);
        for (i, entry) in read_batch.iter().enumerate() {
            assert_eq!(entry.ndx, i as i32);
            match i % 5 {
                0 => assert_eq!(entry.status, AckStatus::Success),
                1 => assert_eq!(entry.status, AckStatus::Skipped),
                2 => {
                    assert_eq!(entry.status, AckStatus::Error);
                    assert_eq!(
                        entry.error_msg.as_deref(),
                        Some(&*format!("error for file {i}"))
                    );
                }
                3 => {
                    assert_eq!(entry.status, AckStatus::ChecksumError);
                    assert_eq!(
                        entry.error_msg.as_deref(),
                        Some(&*format!("checksum failed for {i}"))
                    );
                }
                _ => {
                    assert_eq!(entry.status, AckStatus::IoError);
                    assert_eq!(
                        entry.error_msg.as_deref(),
                        Some(&*format!("io error at {i}"))
                    );
                }
            }
        }
    }

    /// Tests disabled batching mode.
    #[test]
    fn test_disabled_batching() {
        let mut batcher = AckBatcher::disabled();
        let mut output = Vec::new();

        // Each ACK should trigger immediate flush
        batcher.queue_success(0);
        assert!(batcher.should_flush());
        let count = batcher.force_flush(&mut output).unwrap();
        assert_eq!(count, 1);

        batcher.queue_success(1);
        assert!(batcher.should_flush());
        let count = batcher.force_flush(&mut output).unwrap();
        assert_eq!(count, 1);

        // Stats should show individual sends
        let stats = batcher.stats();
        assert_eq!(stats.total_sent, 2);
        assert_eq!(stats.batches_sent, 2);
        assert!((stats.efficiency_ratio() - 1.0).abs() < f64::EPSILON);
    }

    /// Tests pipeline config integration.
    #[test]
    fn test_pipeline_config_integration() {
        use crate::pipeline::PipelineConfig;

        // Create pipeline config with specific ACK settings
        let pipeline_config = PipelineConfig::default()
            .with_ack_batch_size(32)
            .with_ack_batch_timeout_ms(100)
            .with_ack_batching(true);

        // Get the ACK batcher config
        let ack_config = pipeline_config.ack_batcher_config();
        assert!(ack_config.is_enabled());
        assert_eq!(ack_config.batch_size, 32);
        assert_eq!(ack_config.batch_timeout_ms, 100);

        // Create batcher with derived config
        let batcher = AckBatcher::new(ack_config);
        assert_eq!(batcher.batch_size(), 32);
        assert!(batcher.is_enabled());
    }

    // ========================================================================
    // Wire Format Tests (Task #67)
    // ========================================================================

    /// Verifies the exact wire format byte layout for AckEntry::write().
    /// Wire format:
    /// - ndx: 4 bytes (i32 LE)
    /// - status: 1 byte
    /// - if error: error_len (u16 LE) + error_msg bytes
    #[test]
    fn test_ack_entry_wire_format_success() {
        let entry = AckEntry::success(0x12345678);
        let mut buf = Vec::new();
        entry.write(&mut buf).unwrap();

        // Success: 4 bytes ndx + 1 byte status = 5 bytes total
        assert_eq!(buf.len(), 5);
        // NDX in little-endian
        assert_eq!(&buf[0..4], &[0x78, 0x56, 0x34, 0x12]);
        // Status byte (Success = 0)
        assert_eq!(buf[4], 0);
    }

    #[test]
    fn test_ack_entry_wire_format_skipped() {
        let entry = AckEntry::skipped(-1); // Negative NDX
        let mut buf = Vec::new();
        entry.write(&mut buf).unwrap();

        // Skipped: 4 bytes ndx + 1 byte status = 5 bytes total
        assert_eq!(buf.len(), 5);
        // NDX -1 in little-endian (0xFFFFFFFF)
        assert_eq!(&buf[0..4], &[0xFF, 0xFF, 0xFF, 0xFF]);
        // Status byte (Skipped = 2)
        assert_eq!(buf[4], 2);
    }

    #[test]
    fn test_ack_entry_wire_format_error_with_message() {
        let entry = AckEntry::error(42, "test");
        let mut buf = Vec::new();
        entry.write(&mut buf).unwrap();

        // Error with "test": 4 bytes ndx + 1 byte status + 2 bytes len + 4 bytes msg = 11 bytes
        assert_eq!(buf.len(), 11);
        // NDX in little-endian
        assert_eq!(&buf[0..4], &[42, 0, 0, 0]);
        // Status byte (Error = 1)
        assert_eq!(buf[4], 1);
        // Message length in little-endian (4)
        assert_eq!(&buf[5..7], &[4, 0]);
        // Message bytes
        assert_eq!(&buf[7..11], b"test");
    }

    #[test]
    fn test_ack_entry_wire_format_checksum_error() {
        let entry = AckEntry::checksum_error(100, "bad");
        let mut buf = Vec::new();
        entry.write(&mut buf).unwrap();

        // 4 bytes ndx + 1 byte status + 2 bytes len + 3 bytes msg = 10 bytes
        assert_eq!(buf.len(), 10);
        assert_eq!(&buf[0..4], &[100, 0, 0, 0]);
        // Status byte (ChecksumError = 3)
        assert_eq!(buf[4], 3);
        // Message length (3)
        assert_eq!(&buf[5..7], &[3, 0]);
        assert_eq!(&buf[7..10], b"bad");
    }

    #[test]
    fn test_ack_entry_wire_format_io_error() {
        let entry = AckEntry::io_error(255, "IO");
        let mut buf = Vec::new();
        entry.write(&mut buf).unwrap();

        // 4 bytes ndx + 1 byte status + 2 bytes len + 2 bytes msg = 9 bytes
        assert_eq!(buf.len(), 9);
        assert_eq!(&buf[0..4], &[255, 0, 0, 0]);
        // Status byte (IoError = 4)
        assert_eq!(buf[4], 4);
        assert_eq!(&buf[5..7], &[2, 0]);
        assert_eq!(&buf[7..9], b"IO");
    }

    /// Tests that error messages longer than 64KB are truncated.
    #[test]
    fn test_ack_entry_message_truncation_at_64kb() {
        // Create a message larger than u16::MAX (65535)
        let long_msg = "x".repeat(70000);
        let entry = AckEntry::error(1, long_msg.clone());

        let mut buf = Vec::new();
        entry.write(&mut buf).unwrap();

        // Read it back
        let mut cursor = Cursor::new(&buf);
        let read_entry = AckEntry::read(&mut cursor).unwrap();

        // Message should be truncated to 65535 bytes
        let read_msg = read_entry.error_msg.unwrap();
        assert_eq!(read_msg.len(), u16::MAX as usize);
        assert!(read_msg.chars().all(|c| c == 'x'));
    }

    /// Tests reading from truncated/incomplete data fails gracefully.
    #[test]
    fn test_ack_entry_read_truncated_ndx() {
        // Only 2 bytes instead of 4 for NDX
        let buf = [0x01, 0x02];
        let mut cursor = Cursor::new(&buf);
        let result = AckEntry::read(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn test_ack_entry_read_truncated_status() {
        // 4 bytes NDX but no status byte
        let buf = [0x01, 0x00, 0x00, 0x00];
        let mut cursor = Cursor::new(&buf);
        let result = AckEntry::read(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn test_ack_entry_read_truncated_error_len() {
        // NDX + error status but no length bytes
        let buf = [0x01, 0x00, 0x00, 0x00, 0x01]; // status=1 (Error)
        let mut cursor = Cursor::new(&buf);
        let result = AckEntry::read(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn test_ack_entry_read_truncated_error_msg() {
        // NDX + error status + length says 10 bytes but only 2 present
        let buf = [
            0x01, 0x00, 0x00, 0x00, // ndx = 1
            0x01, // status = Error
            0x0A, 0x00, // len = 10
            b'a', b'b', // only 2 bytes of message
        ];
        let mut cursor = Cursor::new(&buf);
        let result = AckEntry::read(&mut cursor);
        assert!(result.is_err());
    }

    /// Tests reading with zero-length error message.
    #[test]
    fn test_ack_entry_read_zero_length_error_msg() {
        // NDX + error status + length=0
        let buf = [
            0x2A, 0x00, 0x00, 0x00, // ndx = 42
            0x01, // status = Error
            0x00, 0x00, // len = 0
        ];
        let mut cursor = Cursor::new(&buf);
        let result = AckEntry::read(&mut cursor).unwrap();

        assert_eq!(result.ndx, 42);
        assert_eq!(result.status, AckStatus::Error);
        // Zero-length message results in None
        assert!(result.error_msg.is_none());
    }

    /// Tests batch wire format: count prefix + entries.
    #[test]
    fn test_batch_wire_format_count_prefix() {
        let batch = vec![
            AckEntry::success(1),
            AckEntry::success(2),
            AckEntry::success(3),
        ];

        let mut buf = Vec::new();
        AckBatcher::write_batch(&batch, &mut buf).unwrap();

        // First 2 bytes should be count in little-endian
        assert_eq!(&buf[0..2], &[3, 0]); // count = 3

        // Each success entry is 5 bytes, so total = 2 + 3*5 = 17 bytes
        assert_eq!(buf.len(), 17);
    }

    /// Tests reading batch with truncated count.
    #[test]
    fn test_batch_read_truncated_count() {
        // Only 1 byte instead of 2 for count
        let buf = [0x05];
        let mut cursor = Cursor::new(&buf);
        let result = AckBatcher::read_batch(&mut cursor);
        assert!(result.is_err());
    }

    /// Tests reading batch with count but truncated entries.
    #[test]
    fn test_batch_read_truncated_entries() {
        // Count says 5 entries but only partial data for 1
        let buf = [
            0x05, 0x00, // count = 5
            0x01, 0x00, 0x00, 0x00, 0x00, // entry 0 (success)
            0x02, 0x00, // partial entry 1 (only 2 bytes of NDX)
        ];
        let mut cursor = Cursor::new(&buf);
        let result = AckBatcher::read_batch(&mut cursor);
        assert!(result.is_err());
    }

    /// Tests that non-UTF8 bytes in error messages are handled gracefully.
    #[test]
    fn test_ack_entry_non_utf8_error_message() {
        // Manually construct wire data with invalid UTF-8
        let buf = [
            0x01, 0x00, 0x00, 0x00, // ndx = 1
            0x01, // status = Error
            0x04, 0x00, // len = 4
            0x80, 0x81, 0x82, 0x83, // invalid UTF-8 bytes
        ];
        let mut cursor = Cursor::new(&buf);
        let result = AckEntry::read(&mut cursor).unwrap();

        // Should use lossy conversion (replacement characters)
        assert_eq!(result.ndx, 1);
        assert_eq!(result.status, AckStatus::Error);
        let msg = result.error_msg.unwrap();
        // Invalid bytes become replacement characters
        assert!(msg.contains('\u{FFFD}'));
    }

    /// Round-trip test for all status types verifying exact byte layout.
    #[test]
    fn test_ack_entry_roundtrip_all_statuses_wire_verified() {
        let test_cases = vec![
            (AckEntry::success(0), 0u8, None::<&str>),
            (AckEntry::skipped(1), 2u8, None),
            (AckEntry::error(2, "err"), 1u8, Some("err")),
            (AckEntry::checksum_error(3, "chk"), 3u8, Some("chk")),
            (AckEntry::io_error(4, "io"), 4u8, Some("io")),
        ];

        for (entry, expected_status, expected_msg) in test_cases {
            let mut buf = Vec::new();
            entry.write(&mut buf).unwrap();

            // Verify status byte position (byte 4)
            assert_eq!(buf[4], expected_status);

            // Round-trip
            let mut cursor = Cursor::new(&buf);
            let read_entry = AckEntry::read(&mut cursor).unwrap();

            assert_eq!(read_entry.ndx, entry.ndx);
            assert_eq!(read_entry.status, entry.status);
            assert_eq!(read_entry.error_msg.as_deref(), expected_msg);
        }
    }
}
