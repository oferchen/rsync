//! Setter methods for transfer behavior, compression, and size limit options.

use std::num::{NonZeroU32, NonZeroU64};

use compress::algorithm::CompressionAlgorithm;
use compress::zlib::CompressionLevel;

use super::LocalCopyOptionsBuilder;
use crate::local_copy::skip_compress::SkipCompressList;

impl LocalCopyOptionsBuilder {
    /// Sets the minimum file size for transfers.
    #[must_use]
    pub fn min_file_size(mut self, size: Option<u64>) -> Self {
        self.min_file_size = size;
        self
    }

    /// Sets the maximum file size for transfers.
    #[must_use]
    pub fn max_file_size(mut self, size: Option<u64>) -> Self {
        self.max_file_size = size;
        self
    }

    /// Sets the block size override for delta transfers.
    #[must_use]
    pub fn block_size(mut self, size: Option<NonZeroU32>) -> Self {
        self.block_size_override = size;
        self
    }

    /// Enables removal of source files after successful transfer.
    #[must_use]
    pub fn remove_source_files(mut self, enabled: bool) -> Self {
        self.remove_source_files = enabled;
        self
    }

    /// Enables preallocation of destination files.
    #[must_use]
    pub fn preallocate(mut self, enabled: bool) -> Self {
        self.preallocate = enabled;
        self
    }

    /// Enables fsync after file writes.
    #[must_use]
    pub fn fsync(mut self, enabled: bool) -> Self {
        self.fsync = enabled;
        self
    }

    /// Sets the bandwidth limit in bytes per second.
    #[must_use]
    pub fn bandwidth_limit(mut self, limit: Option<NonZeroU64>) -> Self {
        self.bandwidth_limit = limit;
        self
    }

    /// Sets the bandwidth burst limit in bytes.
    #[must_use]
    pub fn bandwidth_burst(mut self, burst: Option<NonZeroU64>) -> Self {
        self.bandwidth_burst = burst;
        self
    }

    /// Enables or disables compression.
    #[must_use]
    pub fn compress(mut self, enabled: bool) -> Self {
        self.compress = enabled;
        if !enabled {
            self.compression_level_override = None;
        }
        self
    }

    /// Sets the compression algorithm.
    #[must_use]
    pub fn compression_algorithm(mut self, algorithm: CompressionAlgorithm) -> Self {
        self.compression_algorithm = algorithm;
        self
    }

    /// Sets the compression level.
    #[must_use]
    pub fn compression_level(mut self, level: CompressionLevel) -> Self {
        self.compression_level = level;
        self
    }

    /// Sets the compression level override.
    #[must_use]
    pub fn compression_level_override(mut self, level: Option<CompressionLevel>) -> Self {
        self.compression_level_override = level;
        self
    }

    /// Sets the skip-compress list for file suffixes.
    #[must_use]
    pub fn skip_compress(mut self, list: SkipCompressList) -> Self {
        self.skip_compress = list;
        self
    }
}
