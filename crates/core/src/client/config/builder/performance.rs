use super::*;
use std::num::{NonZeroU8, NonZeroU32, NonZeroUsize};

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

    /// Preserves the raw `--compress-choice` name as typed by the user.
    ///
    /// [`CompressionAlgorithm`] collapses `zlibx` onto `Zlib` (shared deflate
    /// codec), so the enum cannot round-trip the exact name upstream prints in
    /// its `--debug=NSTR` compress summary (`compat.c:206-219`). This retains
    /// the verbatim string so the local-copy summary matches upstream.
    #[must_use]
    #[doc(alias = "--compress-choice")]
    pub fn compress_choice_name(mut self, name: Option<String>) -> Self {
        self.compress_choice_name = name;
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

    /// Records the requested zstd worker thread count.
    ///
    /// `None` (the default) lets the codec pick its own worker count. The
    /// value is currently stored without being forwarded to the encoder; the
    /// zstd `ZSTD_c_nbWorkers` wiring lives in `compress/strategy/zstd.rs`
    /// and is applied in a follow-up change.
    ///
    /// # Upstream Reference
    ///
    /// - `options.c:760-761` - `--compress-threads` / `--zt` long options.
    /// - `token.c:701` - `ZSTD_CCtx_setParameter(.., ZSTD_c_nbWorkers, ..)`.
    #[must_use]
    #[doc(alias = "--compress-threads")]
    #[doc(alias = "--zt")]
    pub const fn compression_threads(mut self, threads: Option<NonZeroU8>) -> Self {
        self.compression_threads = threads;
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

    /// Enables the internal xxh64 file-dedup heuristic on the local receiver.
    ///
    /// When set, the receiver hashes both the source and the existing
    /// destination with xxh64 before computing a delta. Matching digests
    /// bypass delta computation. The flag is internal-only and never alters
    /// the wire protocol forwarded to the peer.
    #[must_use]
    #[doc(alias = "--xxh64-dedup")]
    pub const fn xxh64_dedup(mut self, enabled: bool) -> Self {
        self.xxh64_dedup = enabled;
        self
    }

    builder_setter! {
        /// Applies an explicit delta-transfer block size override.
        #[doc(alias = "--block-size")]
        block_size_override: Option<NonZeroU32>,
    }

    builder_setter! {
        /// Caps the rayon worker pool to a fixed thread count.
        ///
        /// `None` keeps rayon's default of one worker per logical CPU.
        #[doc(alias = "--rayon-threads")]
        rayon_threads: Option<NonZeroUsize>,
    }

    builder_setter! {
        /// Caps the async (tokio) runtime worker count.
        ///
        /// `None` keeps tokio's own defaults. The value is honoured only when
        /// async transports are enabled at build time.
        #[doc(alias = "--tokio-threads")]
        tokio_threads: Option<NonZeroUsize>,
    }

    builder_setter! {
        /// Sets the `--max-alloc` cap in bytes for the global buffer pool.
        ///
        /// When set, the pool tracks outstanding (checked-out) memory and
        /// blocks `acquire` calls that would push the outstanding total past
        /// this limit. Mirrors upstream rsync's `max_alloc` (default 1 GiB;
        /// suffixes K/M/G/T/P/E supported by the CLI parser).
        #[doc(alias = "--max-alloc")]
        max_alloc: Option<u64>,
    }

    builder_setter! {
        /// Enables or disables sparse file handling for the transfer.
        #[doc(alias = "--sparse")]
        #[doc(alias = "-S")]
        sparse: bool,
    }

    builder_setter! {
        /// Selects the strategy used to detect existing holes in source files.
        ///
        /// Mirrors the `--sparse-detect=[auto|seek|map|none]` CLI flag.
        /// `--sparse` controls *whether* sparse semantics apply; this setting
        /// controls *how* the engine probes the source for zero regions.
        #[doc(alias = "--sparse-detect")]
        sparse_detect: engine::SparseDetectStrategy,
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

    /// Sets whether oc-rsync advertises the INC_RECURSE (`'i'`) capability
    /// during connection setup.
    ///
    /// Defaults to `false` while sender-side incremental recursion is
    /// validated against upstream rsync 3.0.9 / 3.1.3 / 3.4.1; pass `true`
    /// (or `--inc-recursive` on the CLI) to opt in. `--no-inc-recursive`
    /// also clears the flag, matching upstream `set_allow_inc_recurse()`
    /// when `--no-inc-recursive` is the only signal.
    ///
    /// # Upstream Reference
    ///
    /// - `compat.c:720 set_allow_inc_recurse()` - capability gate.
    /// - `options.c:3003-3050 maybe_add_e_option()` - capability string.
    #[must_use]
    #[doc(alias = "--inc-recursive")]
    #[doc(alias = "--no-inc-recursive")]
    pub const fn inc_recursive_send(mut self, value: bool) -> Self {
        self.inc_recursive_send = Some(value);
        self
    }
}
