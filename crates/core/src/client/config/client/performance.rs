use super::*;
use std::num::NonZeroU32;

impl ClientConfig {
    /// Reports whether compression was requested for transfers.
    #[must_use]
    #[doc(alias = "--compress")]
    #[doc(alias = "-z")]
    pub const fn compress(&self) -> bool {
        self.compress
    }

    /// Returns the configured compression level override, if any.
    #[must_use]
    #[doc(alias = "--compress-level")]
    pub const fn compression_level(&self) -> Option<CompressionLevel> {
        self.compression_level
    }

    /// Returns the compression algorithm requested by the caller.
    #[must_use]
    #[doc(alias = "--compress-choice")]
    pub const fn compression_algorithm(&self) -> CompressionAlgorithm {
        self.compression_algorithm
    }

    /// Returns the compression setting that should apply when compression is enabled.
    #[must_use]
    #[doc(alias = "--compress-level")]
    pub const fn compression_setting(&self) -> CompressionSetting {
        self.compression_setting
    }

    /// Returns the suffix list that disables compression for matching files.
    pub fn skip_compress(&self) -> &SkipCompressList {
        &self.skip_compress
    }

    /// Reports whether whole-file transfers should be used.
    #[must_use]
    #[doc(alias = "--whole-file")]
    #[doc(alias = "-W")]
    #[doc(alias = "--no-whole-file")]
    pub const fn whole_file(&self) -> bool {
        self.whole_file
    }

    /// Reports whether source files should be opened without updating access times.
    #[must_use]
    #[doc(alias = "--open-noatime")]
    #[doc(alias = "--no-open-noatime")]
    pub const fn open_noatime(&self) -> bool {
        self.open_noatime
    }

    /// Reports whether sparse file handling has been requested.
    #[must_use]
    #[doc(alias = "--sparse")]
    pub const fn sparse(&self) -> bool {
        self.sparse
    }

    /// Reports whether fuzzy basis file matching was requested.
    #[must_use]
    #[doc(alias = "--fuzzy")]
    #[doc(alias = "-y")]
    pub const fn fuzzy(&self) -> bool {
        self.fuzzy
    }

    /// Returns the configured delta-transfer block size override, if any.
    #[must_use]
    #[doc(alias = "--block-size")]
    pub const fn block_size_override(&self) -> Option<NonZeroU32> {
        self.block_size_override
    }

    /// Returns the maximum memory allocation limit per allocation request.
    ///
    /// When set, this limits how much memory can be allocated in a single
    /// request, providing protection against memory exhaustion attacks.
    #[must_use]
    #[doc(alias = "--max-alloc")]
    pub const fn max_alloc(&self) -> Option<u64> {
        self.max_alloc
    }
}
