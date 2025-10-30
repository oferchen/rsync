use super::*;

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

    /// Reports whether sparse file handling has been requested.
    #[must_use]
    #[doc(alias = "--sparse")]
    pub const fn sparse(&self) -> bool {
        self.sparse
    }
}
