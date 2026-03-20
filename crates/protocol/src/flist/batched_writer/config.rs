//! Batch configuration for controlling flush behavior.

use std::time::Duration;

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
