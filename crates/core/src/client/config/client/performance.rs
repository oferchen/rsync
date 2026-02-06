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
    pub const fn skip_compress(&self) -> &SkipCompressList {
        &self.skip_compress
    }

    /// Reports whether whole-file transfers should be used.
    ///
    /// Returns `true` when explicitly forced or when auto-detecting for local
    /// copies. Returns `false` when explicitly forced to delta mode.
    #[must_use]
    #[doc(alias = "--whole-file")]
    #[doc(alias = "-W")]
    #[doc(alias = "--no-whole-file")]
    pub const fn whole_file(&self) -> bool {
        match self.whole_file {
            Some(v) => v,
            None => true,
        }
    }

    /// Returns the raw tri-state whole-file setting.
    #[must_use]
    pub const fn whole_file_raw(&self) -> Option<bool> {
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

    /// Reports whether qsort should be used instead of merge sort for file lists.
    ///
    /// When enabled, uses qsort for file list sorting which may be faster
    /// for certain data patterns but is not a stable sort.
    #[must_use]
    #[doc(alias = "--qsort")]
    pub const fn qsort(&self) -> bool {
        self.qsort
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> ClientConfig {
        ClientConfig::default()
    }

    // Tests for compress
    #[test]
    fn compress_default_is_false() {
        let config = default_config();
        assert!(!config.compress());
    }

    // Tests for compression_level
    #[test]
    fn compression_level_default_is_none() {
        let config = default_config();
        assert!(config.compression_level().is_none());
    }

    // Tests for compression_algorithm
    #[test]
    fn compression_algorithm_default_is_valid() {
        let config = default_config();
        let _algo = config.compression_algorithm();
        // Just verify it returns successfully
    }

    // Tests for skip_compress
    #[test]
    fn skip_compress_default_exists() {
        let config = default_config();
        let _skip = config.skip_compress();
        // Just verify it returns successfully
    }

    // Tests for whole_file
    #[test]
    fn whole_file_default_is_true() {
        let config = default_config();
        assert!(config.whole_file());
    }

    // Tests for open_noatime
    #[test]
    fn open_noatime_default_is_false() {
        let config = default_config();
        assert!(!config.open_noatime());
    }

    // Tests for sparse
    #[test]
    fn sparse_default_is_false() {
        let config = default_config();
        assert!(!config.sparse());
    }

    // Tests for fuzzy
    #[test]
    fn fuzzy_default_is_false() {
        let config = default_config();
        assert!(!config.fuzzy());
    }

    // Tests for block_size_override
    #[test]
    fn block_size_override_default_is_none() {
        let config = default_config();
        assert!(config.block_size_override().is_none());
    }

    // Tests for max_alloc
    #[test]
    fn max_alloc_default_is_none() {
        let config = default_config();
        assert!(config.max_alloc().is_none());
    }

    // Tests for qsort
    #[test]
    fn qsort_default_is_false() {
        let config = default_config();
        assert!(!config.qsort());
    }
}
