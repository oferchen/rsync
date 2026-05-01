use super::*;
use std::num::NonZeroU32;

impl ClientConfigBuilder {
    builder_setter! {
        /// Configures the optional bandwidth limit to apply during transfers.
        #[doc(alias = "--bwlimit")]
        bandwidth_limit: Option<BandwidthLimit>,
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
    ///
    /// Calling this method marks the choice as explicit, so the invocation
    /// builder will forward it to the remote peer via `--compress-choice`,
    /// `--new-compress`, or `--old-compress` - matching upstream
    /// `options.c:2800-2805`.
    #[must_use]
    #[doc(alias = "--compress-choice")]
    pub const fn compression_algorithm(mut self, value: CompressionAlgorithm) -> Self {
        self.compression_algorithm = value;
        self.explicit_compress_choice = true;
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

    builder_setter! {
        /// Requests that source files be opened without updating their access times.
        #[doc(alias = "--open-noatime")]
        #[doc(alias = "--no-open-noatime")]
        open_noatime: bool,
    }

    /// Requests that whole-file transfers be used instead of the delta algorithm.
    #[must_use]
    #[doc(alias = "--whole-file")]
    #[doc(alias = "-W")]
    #[doc(alias = "--no-whole-file")]
    pub const fn whole_file(mut self, whole_file: bool) -> Self {
        self.whole_file = Some(whole_file);
        self
    }

    /// Sets the whole-file option as a tri-state value.
    ///
    /// - `Some(true)`: force whole-file mode (`-W`).
    /// - `Some(false)`: force delta mode (`--no-whole-file`).
    /// - `None`: auto-detect (whole-file for local, delta for remote/batch).
    #[must_use]
    pub const fn whole_file_option(mut self, whole_file: Option<bool>) -> Self {
        self.whole_file = whole_file;
        self
    }

    builder_setter! {
        /// Applies an explicit delta-transfer block size override.
        #[doc(alias = "--block-size")]
        block_size_override: Option<NonZeroU32>,
    }

    builder_setter! {
        /// Sets the maximum memory allocation limit per allocation request.
        ///
        /// When set, this limits how much memory can be allocated in a single
        /// request, providing protection against memory exhaustion attacks.
        #[doc(alias = "--max-alloc")]
        max_alloc: Option<u64>,
    }

    builder_setter! {
        /// Enables or disables sparse file handling for the transfer.
        #[doc(alias = "--sparse")]
        #[doc(alias = "-S")]
        sparse: bool,
    }

    /// Sets the fuzzy matching level for delta transfers.
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
    #[doc(alias = "--no-fuzzy")]
    #[doc(alias = "-y")]
    pub const fn fuzzy_level(mut self, level: u8) -> Self {
        self.fuzzy_level = level;
        self
    }

    /// Convenience method: enables fuzzy at level 1 when `true`, disables when `false`.
    #[must_use]
    pub const fn fuzzy(mut self, enabled: bool) -> Self {
        self.fuzzy_level = if enabled { 1 } else { 0 };
        self
    }

    builder_setter! {
        /// Enables qsort instead of merge sort for file list sorting.
        ///
        /// When enabled, uses qsort for file list sorting which may be faster
        /// for certain data patterns but is not a stable sort.
        #[doc(alias = "--qsort")]
        qsort: bool,
    }

    builder_setter! {
        /// Opt-in: advertise the INC_RECURSE (`'i'`) capability when oc-rsync
        /// is acting as the sender.
        ///
        /// Defaults to `false`. Sender-side incremental recursion has not yet
        /// been validated against upstream rsync 3.0.9 / 3.1.3 / 3.4.1, so the
        /// capability is suppressed for push transfers by default. Set this
        /// flag to enable the negotiation for interop testing.
        ///
        /// # Upstream Reference
        ///
        /// - `compat.c:720 set_allow_inc_recurse()` - capability gate.
        /// - `options.c:3003-3050 maybe_add_e_option()` - capability string.
        #[doc(alias = "--inc-recursive-send")]
        inc_recursive_send: bool,
    }
}
