//! Setter methods for integrity, timeout, batch, logging, and error handling options.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use super::LocalCopyOptionsBuilder;
use crate::batch::BatchWriter;
use crate::signature::SignatureAlgorithm;

impl LocalCopyOptionsBuilder {
    /// Enables sparse file handling.
    #[must_use]
    pub fn sparse(mut self, enabled: bool) -> Self {
        self.sparse = enabled;
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
    pub fn modify_window(mut self, window: Duration) -> Self {
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
}
