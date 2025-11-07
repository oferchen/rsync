use std::path::Path;

use oc_rsync_compress::algorithm::CompressionAlgorithm;
use oc_rsync_compress::zlib::CompressionLevel;

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
