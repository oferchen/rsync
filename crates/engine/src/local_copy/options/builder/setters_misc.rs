//! Setter methods for integrity, timeout, batch, logging, and error handling options.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use fast_io::PlatformCopy;
use metadata::ModifyWindow;

use super::LocalCopyOptionsBuilder;
use crate::batch::BatchWriter;
use crate::local_copy::executor::SparseDetectStrategy;
use crate::signature::SignatureAlgorithm;

impl LocalCopyOptionsBuilder {
    /// Enables sparse file handling.
    #[must_use]
    pub fn sparse(mut self, enabled: bool) -> Self {
        self.sparse = enabled;
        self
    }

    /// Selects the sparse hole-detection strategy used by the read path.
    ///
    /// Independent of [`Self::sparse`]: detection runs whenever the engine
    /// consults [`crate::SparseReader::detect_holes_with`], but the result is
    /// only acted upon for writes when sparse handling is enabled.
    #[must_use]
    pub fn sparse_detect_strategy(mut self, strategy: SparseDetectStrategy) -> Self {
        self.sparse_detect_strategy = strategy;
        self
    }

    /// Enables checksum-based comparison.
    #[must_use]
    pub fn checksum(mut self, enabled: bool) -> Self {
        self.checksum = enabled;
        self
    }

    /// Sets the checksum algorithm.
    #[must_use]
    pub fn checksum_algorithm(mut self, algorithm: SignatureAlgorithm) -> Self {
        self.checksum_algorithm = algorithm;
        self
    }

    /// Sets a fixed checksum seed for reproducible transfers.
    ///
    /// When `None` (the default), the checksum seed is chosen automatically.
    /// Setting a specific value allows reproducible checksums across runs.
    #[must_use]
    #[doc(alias = "--checksum-seed")]
    pub fn with_checksum_seed(mut self, seed: Option<u32>) -> Self {
        self.checksum_seed = seed;
        self
    }

    /// Enables the internal xxh64 file-dedup heuristic.
    ///
    /// When set, the receiver hashes both the source and the existing
    /// destination with xxh64 before computing a rolling+strong delta
    /// signature. Matching digests indicate the files are identical with
    /// very high probability, so the receiver bypasses the delta path
    /// entirely. The heuristic never affects the wire protocol and is
    /// disabled by default.
    #[must_use]
    #[doc(alias = "--xxh64-dedup")]
    pub fn enable_xxh64_dedup(mut self, enabled: bool) -> Self {
        self.enable_xxh64_dedup = enabled;
        self
    }

    /// Sets the maximum file size, in bytes, eligible for the xxh64 dedup
    /// heuristic. Files larger than this are passed straight to the normal
    /// delta path.
    #[must_use]
    pub fn xxh64_dedup_size_limit(mut self, size_limit: u64) -> Self {
        self.xxh64_dedup_size_limit = size_limit;
        self
    }

    /// Enables size-only comparison.
    #[must_use]
    pub fn size_only(mut self, enabled: bool) -> Self {
        self.size_only = enabled;
        self
    }

    /// Enables ignore-times mode.
    #[must_use]
    pub fn ignore_times(mut self, enabled: bool) -> Self {
        self.ignore_times = enabled;
        self
    }

    /// Enables ignore-existing mode.
    #[must_use]
    pub fn ignore_existing(mut self, enabled: bool) -> Self {
        self.ignore_existing = enabled;
        self
    }

    /// Enables existing-only mode.
    #[must_use]
    pub fn existing_only(mut self, enabled: bool) -> Self {
        self.existing_only = enabled;
        self
    }

    /// Enables ignore-missing-args mode.
    #[must_use]
    pub fn ignore_missing_args(mut self, enabled: bool) -> Self {
        self.ignore_missing_args = enabled;
        self
    }

    /// Enables update mode.
    #[must_use]
    pub fn update(mut self, enabled: bool) -> Self {
        self.update = enabled;
        self
    }

    /// Sets the modification time window.
    #[must_use]
    pub fn modify_window(mut self, window: ModifyWindow) -> Self {
        self.modify_window = window;
        self
    }

    /// Sets the timeout.
    #[must_use]
    pub fn timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sets the connection timeout.
    #[must_use]
    pub fn contimeout(mut self, contimeout: Option<Duration>) -> Self {
        self.contimeout = contimeout;
        self
    }

    /// Sets the stop-at deadline.
    #[must_use]
    pub fn stop_at(mut self, deadline: Option<SystemTime>) -> Self {
        self.stop_at = deadline;
        self
    }

    /// Tells `--delete` to proceed even when I/O errors occurred during the transfer.
    #[must_use]
    pub fn ignore_errors(mut self, enabled: bool) -> Self {
        self.ignore_errors = enabled;
        self
    }

    /// Sets the log file path for transfer activity logging.
    #[must_use]
    pub fn log_file<P: Into<PathBuf>>(mut self, path: Option<P>) -> Self {
        self.log_file = path.map(Into::into);
        self
    }

    /// Sets the per-item log format string.
    #[must_use]
    pub fn log_file_format<S: Into<String>>(mut self, format: Option<S>) -> Self {
        self.log_file_format = format.map(Into::into);
        self
    }

    /// Sets the batch writer.
    #[must_use]
    pub fn batch_writer(mut self, writer: Option<Arc<Mutex<BatchWriter>>>) -> Self {
        self.batch_writer = writer;
        self
    }

    /// Replaces the platform copy strategy used by whole-file fast paths.
    ///
    /// Defaults to [`fast_io::DefaultPlatformCopy`]. Tests can substitute a
    /// custom implementation to verify dispatch.
    #[must_use]
    pub fn platform_copy(mut self, platform_copy: Arc<dyn PlatformCopy>) -> Self {
        self.platform_copy = platform_copy;
        self
    }
}
