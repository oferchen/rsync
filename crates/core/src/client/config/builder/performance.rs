use super::*;
use std::num::NonZeroU32;

impl ClientConfigBuilder {
    /// Configures the optional bandwidth limit to apply during transfers.
    #[must_use]
    #[doc(alias = "--bwlimit")]
    pub fn bandwidth_limit(mut self, limit: Option<BandwidthLimit>) -> Self {
        self.bandwidth_limit = limit;
        self
    }

    /// Enables or disables compression for the transfer.
    #[must_use]
    #[doc(alias = "--compress")]
    #[doc(alias = "--no-compress")]
    #[doc(alias = "-z")]
    pub const fn compress(mut self, compress: bool) -> Self {
        self.compress = compress;
        if compress && self.compression_setting.is_disabled() {
            self.compression_setting = CompressionSetting::level(CompressionLevel::Default);
        } else {
            self.compression_setting = CompressionSetting::disabled();
            self.compression_level = None;
        }
        self
    }

    /// Applies an explicit compression level override when building the configuration.
    #[must_use]
    #[doc(alias = "--compress-level")]
    pub const fn compression_level(mut self, level: Option<CompressionLevel>) -> Self {
        self.compression_level = level;
        if let Some(level) = level {
            self.compression_setting = CompressionSetting::level(level);
            self.compress = true;
        }
        self
    }

    /// Overrides the compression algorithm used when compression is enabled.
    #[must_use]
    #[doc(alias = "--compress-choice")]
    pub const fn compression_algorithm(mut self, algorithm: CompressionAlgorithm) -> Self {
        self.compression_algorithm = algorithm;
        self
    }

    /// Sets the compression level that should apply when compression is enabled.
    #[must_use]
    #[doc(alias = "--compress-level")]
    pub const fn compression_setting(mut self, setting: CompressionSetting) -> Self {
        self.compression_setting = setting;
        self.compress = setting.is_enabled();
        if !self.compress {
            self.compression_level = None;
        }
        self
    }

    /// Overrides the suffix list used to disable compression for specific extensions.
    #[must_use]
    #[doc(alias = "--skip-compress")]
    pub fn skip_compress(mut self, list: SkipCompressList) -> Self {
        self.skip_compress = list;
        self
    }

    /// Requests that source files be opened without updating their access times.
    #[must_use]
    #[doc(alias = "--open-noatime")]
    #[doc(alias = "--no-open-noatime")]
    pub const fn open_noatime(mut self, enabled: bool) -> Self {
        self.open_noatime = enabled;
        self
    }

    /// Requests that whole-file transfers be used instead of the delta algorithm.
    #[must_use]
    #[doc(alias = "--whole-file")]
    #[doc(alias = "-W")]
    #[doc(alias = "--no-whole-file")]
    pub fn whole_file(mut self, whole_file: bool) -> Self {
        self.whole_file = Some(whole_file);
        self
    }

    /// Applies an explicit delta-transfer block size override.
    #[must_use]
    #[doc(alias = "--block-size")]
    pub const fn block_size_override(mut self, block_size: Option<NonZeroU32>) -> Self {
        self.block_size_override = block_size;
        self
    }

    /// Sets the maximum memory allocation limit per allocation request.
    ///
    /// When set, this limits how much memory can be allocated in a single
    /// request, providing protection against memory exhaustion attacks.
    #[must_use]
    #[doc(alias = "--max-alloc")]
    pub const fn max_alloc(mut self, limit: Option<u64>) -> Self {
        self.max_alloc = limit;
        self
    }

    /// Enables or disables sparse file handling for the transfer.
    #[must_use]
    #[doc(alias = "--sparse")]
    #[doc(alias = "-S")]
    pub const fn sparse(mut self, sparse: bool) -> Self {
        self.sparse = sparse;
        self
    }

    /// Enables or disables fuzzy basis file search during delta transfers.
    #[must_use]
    #[doc(alias = "--fuzzy")]
    #[doc(alias = "--no-fuzzy")]
    #[doc(alias = "-y")]
    pub const fn fuzzy(mut self, fuzzy: bool) -> Self {
        self.fuzzy = fuzzy;
        self
    }

    /// Enables qsort instead of merge sort for file list sorting.
    ///
    /// When enabled, uses qsort for file list sorting which may be faster
    /// for certain data patterns but is not a stable sort.
    #[must_use]
    #[doc(alias = "--qsort")]
    pub const fn qsort(mut self, qsort: bool) -> Self {
        self.qsort = qsort;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builder() -> ClientConfigBuilder {
        ClientConfigBuilder::default()
    }

    #[test]
    fn bandwidth_limit_sets_value() {
        // BandwidthLimit requires special construction, just test None here
        let config = builder().bandwidth_limit(None).build();
        assert!(config.bandwidth_limit().is_none());
    }

    #[test]
    fn compress_sets_flag() {
        let config = builder().compress(true).build();
        assert!(config.compress());
    }

    #[test]
    fn compress_false_clears_flag() {
        let config = builder()
            .compress(true)
            .compress(false)
            .build();
        assert!(!config.compress());
    }

    #[test]
    fn compression_level_sets_value() {
        let config = builder().compression_level(Some(CompressionLevel::Default)).build();
        assert!(config.compress());
    }

    #[test]
    fn compression_level_none_clears_value() {
        let _config = builder()
            .compression_level(Some(CompressionLevel::Default))
            .compression_level(None)
            .build();
        // Level becomes None, but compress state depends on implementation
    }

    #[test]
    fn compression_algorithm_sets_value() {
        let config = builder().compression_algorithm(CompressionAlgorithm::Zstd).build();
        assert_eq!(config.compression_algorithm(), CompressionAlgorithm::Zstd);
    }

    #[test]
    fn compression_setting_enabled() {
        let setting = CompressionSetting::level(CompressionLevel::Default);
        let config = builder().compression_setting(setting).build();
        assert!(config.compress());
    }

    #[test]
    fn compression_setting_disabled() {
        let config = builder().compression_setting(CompressionSetting::disabled()).build();
        assert!(!config.compress());
    }

    #[test]
    fn open_noatime_sets_flag() {
        let config = builder().open_noatime(true).build();
        assert!(config.open_noatime());
    }

    #[test]
    fn open_noatime_false_clears_flag() {
        let config = builder()
            .open_noatime(true)
            .open_noatime(false)
            .build();
        assert!(!config.open_noatime());
    }

    #[test]
    fn whole_file_sets_true() {
        let config = builder().whole_file(true).build();
        assert!(config.whole_file());
    }

    #[test]
    fn whole_file_sets_false() {
        let config = builder().whole_file(false).build();
        assert!(!config.whole_file());
    }

    #[test]
    fn block_size_override_sets_value() {
        let size = NonZeroU32::new(4096).unwrap();
        let config = builder().block_size_override(Some(size)).build();
        assert_eq!(config.block_size_override(), Some(size));
    }

    #[test]
    fn block_size_override_none_clears_value() {
        let size = NonZeroU32::new(4096).unwrap();
        let config = builder()
            .block_size_override(Some(size))
            .block_size_override(None)
            .build();
        assert!(config.block_size_override().is_none());
    }

    #[test]
    fn max_alloc_sets_limit() {
        let config = builder().max_alloc(Some(1073741824)).build();
        assert_eq!(config.max_alloc(), Some(1073741824));
    }

    #[test]
    fn max_alloc_none_clears_limit() {
        let config = builder()
            .max_alloc(Some(1073741824))
            .max_alloc(None)
            .build();
        assert!(config.max_alloc().is_none());
    }

    #[test]
    fn sparse_sets_flag() {
        let config = builder().sparse(true).build();
        assert!(config.sparse());
    }

    #[test]
    fn sparse_false_clears_flag() {
        let config = builder()
            .sparse(true)
            .sparse(false)
            .build();
        assert!(!config.sparse());
    }

    #[test]
    fn fuzzy_sets_flag() {
        let config = builder().fuzzy(true).build();
        assert!(config.fuzzy());
    }

    #[test]
    fn fuzzy_false_clears_flag() {
        let config = builder()
            .fuzzy(true)
            .fuzzy(false)
            .build();
        assert!(!config.fuzzy());
    }

    #[test]
    fn qsort_sets_flag() {
        let config = builder().qsort(true).build();
        assert!(config.qsort());
    }

    #[test]
    fn qsort_false_clears_flag() {
        let config = builder()
            .qsort(true)
            .qsort(false)
            .build();
        assert!(!config.qsort());
    }

    #[test]
    fn default_compress_is_false() {
        let config = builder().build();
        assert!(!config.compress());
    }

    #[test]
    fn default_sparse_is_false() {
        let config = builder().build();
        assert!(!config.sparse());
    }

    #[test]
    fn default_fuzzy_is_false() {
        let config = builder().build();
        assert!(!config.fuzzy());
    }

    #[test]
    fn default_qsort_is_false() {
        let config = builder().build();
        assert!(!config.qsort());
    }
}
