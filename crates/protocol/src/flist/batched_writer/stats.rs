//! Statistics and flush-reason tracking for batched writes.

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
pub(super) enum FlushReason {
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
