//! Batched acknowledgment accumulator and flush logic.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use super::types::{AckBatcherConfig, AckBatcherStats, AckEntry};

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
/// batcher.queue(AckEntry::success(0));
/// batcher.queue(AckEntry::success(1));
///
/// if batcher.should_flush() {
///     let batch = batcher.take_batch();
///     send_batch_to_network(batch);
/// }
/// ```
#[derive(Debug)]
pub struct AckBatcher {
    config: AckBatcherConfig,
    pending: Vec<AckEntry>,
    batch_start: Option<Instant>,
    total_sent: u64,
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

        if !self.config.enabled {
            return true;
        }

        if self.pending.len() >= self.config.batch_size {
            return true;
        }

        if self.pending.iter().any(|e| e.status.is_error()) {
            return true;
        }

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
    /// - entries: sequence of `AckEntry`
    pub fn write_batch<W: Write + ?Sized>(batch: &[AckEntry], writer: &mut W) -> io::Result<()> {
        if batch.is_empty() {
            return Ok(());
        }

        let count = batch.len().min(u16::MAX as usize) as u16;
        writer.write_all(&count.to_le_bytes())?;

        for entry in batch.iter().take(count as usize) {
            entry.write(writer)?;
        }

        Ok(())
    }

    /// Reads a batch of ACKs from the wire.
    pub fn read_batch<R: io::Read + ?Sized>(reader: &mut R) -> io::Result<Vec<AckEntry>> {
        let mut count_buf = [0u8; 2];
        reader.read_exact(&mut count_buf)?;
        let count = u16::from_le_bytes(count_buf) as usize;

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
