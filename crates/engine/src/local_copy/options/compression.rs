use std::path::Path;

use compress::algorithm::CompressionAlgorithm;
use compress::zlib::CompressionLevel;

use super::types::LocalCopyOptions;
use crate::local_copy::skip_compress::SkipCompressList;

impl LocalCopyOptions {
    /// Requests that payload compression be enabled or disabled.
    #[must_use]
    #[doc(alias = "--compress")]
    pub const fn compress(mut self, compress: bool) -> Self {
        self.compress = compress;
        if !compress {
            self.compression_level_override = None;
        }
        self
    }

    /// Applies an explicit compression level override for payload processing.
    #[must_use]
    #[doc(alias = "--compress-level")]
    pub const fn with_compression_level_override(
        mut self,
        level: Option<CompressionLevel>,
    ) -> Self {
        self.compression_level_override = level;
        self
    }

    /// Overrides the compression algorithm to use when compression is enabled.
    #[must_use]
    #[doc(alias = "--compress-choice")]
    pub const fn with_compression_algorithm(mut self, algorithm: CompressionAlgorithm) -> Self {
        self.compression_algorithm = algorithm;
        self
    }

    /// Sets the default compression level used when compression is enabled.
    #[must_use]
    pub const fn with_default_compression_level(mut self, level: CompressionLevel) -> Self {
        self.compression_level = level;
        self
    }

    /// Applies an explicit compression level override supplied by the user.
    #[must_use]
    #[doc(alias = "--compress-level")]
    pub const fn with_compression_level(mut self, level: CompressionLevel) -> Self {
        self.compression_level_override = Some(level);
        self
    }

    /// Overrides the suffix list used to disable compression for specific files.
    #[must_use]
    pub fn with_skip_compress(mut self, list: SkipCompressList) -> Self {
        self.skip_compress = list;
        self
    }

    /// Returns whether compression is enabled for payload handling.
    #[must_use]
    pub const fn compress_enabled(&self) -> bool {
        self.compress
    }

    /// Returns the configured compression level override, if any.
    #[must_use]
    pub const fn compression_level_override(&self) -> Option<CompressionLevel> {
        self.compression_level_override
    }

    /// Returns the compression level that should be used when compression is enabled.
    #[must_use]
    pub const fn compression_level(&self) -> CompressionLevel {
        match self.compression_level_override {
            Some(level) => level,
            None => self.compression_level,
        }
    }

    /// Returns the compression algorithm that should be used when compression is enabled.
    #[must_use]
    pub const fn compression_algorithm(&self) -> CompressionAlgorithm {
        self.compression_algorithm
    }

    /// Returns the effective compression level when compression is enabled.
    #[must_use]
    pub const fn effective_compression_level(&self) -> Option<CompressionLevel> {
        if self.compress {
            Some(self.compression_level())
        } else {
            None
        }
    }

    /// Returns the effective compression algorithm when compression is enabled.
    #[must_use]
    #[doc(alias = "--compress-choice")]
    pub const fn effective_compression_algorithm(&self) -> Option<CompressionAlgorithm> {
        if self.compress {
            Some(self.compression_algorithm)
        } else {
            None
        }
    }

    /// Returns the skip-compress list associated with the options.
    pub fn skip_compress(&self) -> &SkipCompressList {
        &self.skip_compress
    }

    /// Reports whether compression should be bypassed for `path`.
    pub fn should_skip_compress(&self, path: &Path) -> bool {
        self.skip_compress.matches_path(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compress_enables_compression() {
        let opts = LocalCopyOptions::new().compress(true);
        assert!(opts.compress_enabled());
    }

    #[test]
    fn compress_false_disables_compression() {
        let opts = LocalCopyOptions::new().compress(true).compress(false);
        assert!(!opts.compress_enabled());
    }

    #[test]
    fn compress_false_clears_level_override() {
        let opts = LocalCopyOptions::new()
            .compress(true)
            .with_compression_level(CompressionLevel::Best)
            .compress(false);
        assert!(opts.compression_level_override().is_none());
    }

    #[test]
    fn with_compression_level_sets_override() {
        let opts = LocalCopyOptions::new().with_compression_level(CompressionLevel::Fast);
        assert_eq!(
            opts.compression_level_override(),
            Some(CompressionLevel::Fast)
        );
    }

    #[test]
    fn with_compression_level_override_sets_level() {
        let opts =
            LocalCopyOptions::new().with_compression_level_override(Some(CompressionLevel::Best));
        assert_eq!(
            opts.compression_level_override(),
            Some(CompressionLevel::Best)
        );
    }

    #[test]
    fn with_compression_level_override_none_clears_level() {
        let opts = LocalCopyOptions::new()
            .with_compression_level(CompressionLevel::Fast)
            .with_compression_level_override(None);
        assert!(opts.compression_level_override().is_none());
    }

    #[test]
    fn with_compression_algorithm_sets_algorithm() {
        let opts = LocalCopyOptions::new().with_compression_algorithm(CompressionAlgorithm::Zstd);
        assert_eq!(opts.compression_algorithm(), CompressionAlgorithm::Zstd);
    }

    #[test]
    fn with_default_compression_level_sets_level() {
        let opts = LocalCopyOptions::new().with_default_compression_level(CompressionLevel::Best);
        assert_eq!(opts.compression_level(), CompressionLevel::Best);
    }

    #[test]
    fn compression_level_returns_override_when_set() {
        let opts = LocalCopyOptions::new()
            .with_default_compression_level(CompressionLevel::Default)
            .with_compression_level(CompressionLevel::Best);
        assert_eq!(opts.compression_level(), CompressionLevel::Best);
    }

    #[test]
    fn compression_level_returns_default_when_no_override() {
        let opts = LocalCopyOptions::new().with_default_compression_level(CompressionLevel::Fast);
        assert_eq!(opts.compression_level(), CompressionLevel::Fast);
    }

    #[test]
    fn effective_compression_level_returns_some_when_enabled() {
        let opts = LocalCopyOptions::new()
            .compress(true)
            .with_compression_level(CompressionLevel::Best);
        assert_eq!(
            opts.effective_compression_level(),
            Some(CompressionLevel::Best)
        );
    }

    #[test]
    fn effective_compression_level_returns_none_when_disabled() {
        let opts = LocalCopyOptions::new()
            .compress(false)
            .with_compression_level(CompressionLevel::Best);
        assert!(opts.effective_compression_level().is_none());
    }

    #[test]
    fn effective_compression_algorithm_returns_some_when_enabled() {
        let opts = LocalCopyOptions::new()
            .compress(true)
            .with_compression_algorithm(CompressionAlgorithm::Zstd);
        assert_eq!(
            opts.effective_compression_algorithm(),
            Some(CompressionAlgorithm::Zstd)
        );
    }

    #[test]
    fn effective_compression_algorithm_returns_none_when_disabled() {
        let opts = LocalCopyOptions::new()
            .compress(false)
            .with_compression_algorithm(CompressionAlgorithm::Zstd);
        assert!(opts.effective_compression_algorithm().is_none());
    }

    #[test]
    fn with_skip_compress_sets_list() {
        let list = SkipCompressList::parse("gz/zip").unwrap();
        let opts = LocalCopyOptions::new().with_skip_compress(list);
        assert!(opts.should_skip_compress(Path::new("file.gz")));
    }

    #[test]
    fn should_skip_compress_returns_false_for_non_matching() {
        let list = SkipCompressList::parse("gz").unwrap();
        let opts = LocalCopyOptions::new().with_skip_compress(list);
        assert!(!opts.should_skip_compress(Path::new("file.txt")));
    }

    #[test]
    fn compress_default_is_disabled() {
        let opts = LocalCopyOptions::new();
        assert!(!opts.compress_enabled());
    }

    #[test]
    fn compression_algorithm_default() {
        let opts = LocalCopyOptions::new();
        assert_eq!(
            opts.compression_algorithm(),
            CompressionAlgorithm::default_algorithm()
        );
    }
}
