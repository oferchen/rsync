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

    /// Returns whether the user explicitly specified `--compress-choice`.
    ///
    /// When `true`, the algorithm was set via `--compress-choice=ALGO`,
    /// `--new-compress`, or `--old-compress`. When `false`, the algorithm
    /// is the build-time default (zstd > lz4 > zlib).
    ///
    /// This distinction is required for correct argument forwarding to
    /// the remote peer - upstream `options.c:2800-2805` only sends
    /// `--compress-choice` / `--new-compress` / `--old-compress` when
    /// the user explicitly selected an algorithm.
    #[must_use]
    pub const fn explicit_compress_choice(&self) -> bool {
        self.explicit_compress_choice
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

    /// Returns the fuzzy matching level.
    ///
    /// - 0: disabled (default)
    /// - 1: search destination directory for similar files (`-y`)
    /// - 2: also search reference directories (`-yy`)
    ///
    /// # Upstream Reference
    ///
    /// Mirrors `fuzzy_basis` in upstream `options.c`.
    #[must_use]
    #[doc(alias = "--fuzzy")]
    #[doc(alias = "-y")]
    pub const fn fuzzy_level(&self) -> u8 {
        self.fuzzy_level
    }

    /// Reports whether fuzzy basis file matching is enabled (level >= 1).
    #[must_use]
    pub const fn fuzzy(&self) -> bool {
        self.fuzzy_level > 0
    }

    /// Returns the configured delta-transfer block size override, if any.
    #[doc(alias = "--block-size")]
    pub const fn block_size_override(&self) -> Option<NonZeroU32> {
        self.block_size_override
    }

    /// Returns the maximum memory allocation limit per allocation request.
    ///
    /// When set, this limits how much memory can be allocated in a single
    /// request, providing protection against memory exhaustion attacks.
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

    /// Reports whether oc-rsync advertises the INC_RECURSE (`'i'`)
    /// capability when negotiating with the peer.
    ///
    /// Default `true`, matching upstream's `allow_inc_recurse = 1`
    /// initialization. The capability is included in the `-e.` string sent
    /// in both transfer directions, causing the peer to enable
    /// `compat_flags |= CF_INC_RECURSE` when the negotiated protocol is at
    /// least 30 and `--recursive` (`-r`) is in effect. Pass
    /// `--no-inc-recursive` to clear it.
    ///
    /// # Upstream Reference
    ///
    /// - `compat.c:720 set_allow_inc_recurse()` - capability negotiation
    ///   gate that clears `allow_inc_recurse`.
    /// - `options.c:3003-3050 maybe_add_e_option()` - capability string
    ///   construction.
    #[must_use]
    #[doc(alias = "--inc-recursive")]
    #[doc(alias = "--no-inc-recursive")]
    pub const fn inc_recursive_send(&self) -> bool {
        self.inc_recursive_send
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> ClientConfig {
        ClientConfig::default()
    }

    #[test]
    fn compress_default_is_false() {
        let config = default_config();
        assert!(!config.compress());
    }

    #[test]
    fn compression_level_default_is_none() {
        let config = default_config();
        assert!(config.compression_level().is_none());
    }

    #[test]
    fn compression_algorithm_default_is_valid() {
        let config = default_config();
        let _algo = config.compression_algorithm();
    }

    #[test]
    fn skip_compress_default_exists() {
        let config = default_config();
        let _skip = config.skip_compress();
    }

    #[test]
    fn whole_file_default_is_true() {
        let config = default_config();
        assert!(config.whole_file());
    }

    #[test]
    fn open_noatime_default_is_false() {
        let config = default_config();
        assert!(!config.open_noatime());
    }

    #[test]
    fn sparse_default_is_false() {
        let config = default_config();
        assert!(!config.sparse());
    }

    #[test]
    fn fuzzy_level_default_is_zero() {
        let config = default_config();
        assert_eq!(config.fuzzy_level(), 0);
        assert!(!config.fuzzy());
    }

    #[test]
    fn block_size_override_default_is_none() {
        let config = default_config();
        assert!(config.block_size_override().is_none());
    }

    #[test]
    fn max_alloc_default_is_none() {
        let config = default_config();
        assert!(config.max_alloc().is_none());
    }

    #[test]
    fn qsort_default_is_false() {
        let config = default_config();
        assert!(!config.qsort());
    }

    // upstream: allow_inc_recurse = 1 (default).
    #[test]
    fn inc_recursive_send_default_is_true() {
        let config = default_config();
        assert!(config.inc_recursive_send());
    }
}
