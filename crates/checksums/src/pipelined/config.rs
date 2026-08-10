//! Pipeline configuration with builder-pattern construction.

/// Configuration for the double-buffered checksum pipeline.
///
/// Controls buffer sizing, pipelining thresholds, and whether
/// a background I/O thread is spawned at all.
#[derive(Clone, Copy, Debug)]
pub struct PipelineConfig {
    /// Size of each read buffer in bytes.
    ///
    /// Two buffers of this size are used for double-buffering.
    /// Larger values improve throughput but increase memory usage.
    /// Default: 64 KiB.
    pub block_size: usize,

    /// Minimum file size to enable pipelining.
    ///
    /// Files smaller than this threshold use synchronous reading
    /// to avoid thread-spawn overhead for trivial workloads.
    /// Default: 256 KiB.
    pub min_file_size: u64,

    /// Whether to use pipelining.
    ///
    /// When false, reads are always synchronous regardless of file size.
    /// Default: true.
    pub enabled: bool,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            block_size: 64 * 1024,
            min_file_size: 256 * 1024,
            enabled: true,
        }
    }
}

impl PipelineConfig {
    /// Creates a new configuration with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the block size for each buffer.
    #[must_use]
    pub const fn with_block_size(mut self, size: usize) -> Self {
        self.block_size = size;
        self
    }

    /// Sets the minimum file size for enabling pipelining.
    #[must_use]
    pub const fn with_min_file_size(mut self, size: u64) -> Self {
        self.min_file_size = size;
        self
    }

    /// Enables or disables pipelining.
    #[must_use]
    pub const fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}
