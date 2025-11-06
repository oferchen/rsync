use std::num::NonZeroU64;
use std::time::{Duration, SystemTime};

use super::types::LocalCopyOptions;

impl LocalCopyOptions {
    /// Applies a minimum size filter for regular files.
    #[must_use]
    #[doc(alias = "--min-size")]
    pub const fn min_file_size(mut self, limit: Option<u64>) -> Self {
        self.min_file_size = limit;
        self
    }

    /// Applies a maximum size filter for regular files.
    #[must_use]
    #[doc(alias = "--max-size")]
    pub const fn max_file_size(mut self, limit: Option<u64>) -> Self {
        self.max_file_size = limit;
        self
    }

    /// Requests that source files be removed after successful transfer.
    #[must_use]
    #[doc(alias = "--remove-source-files")]
    #[doc(alias = "--remove-sent-files")]
    pub const fn remove_source_files(mut self, remove: bool) -> Self {
        self.remove_source_files = remove;
        self
    }

    /// Requests that destination files be preallocated before writing begins.
    #[must_use]
    #[doc(alias = "--preallocate")]
    pub const fn preallocate(mut self, preallocate: bool) -> Self {
        self.preallocate = preallocate;
        self
    }

    /// Applies an optional bandwidth limit expressed in bytes per second.
    #[must_use]
    #[doc(alias = "--bwlimit")]
    pub const fn bandwidth_limit(mut self, limit: Option<NonZeroU64>) -> Self {
        self.bandwidth_limit = limit;
        self
    }

    /// Applies an optional burst limit expressed in bytes per read.
    #[must_use]
    #[doc(alias = "--bwlimit")]
    pub const fn bandwidth_burst(mut self, burst: Option<NonZeroU64>) -> Self {
        self.bandwidth_burst = burst;
        self
    }

    /// Configures an optional inactivity timeout.
    #[must_use]
    #[doc(alias = "--timeout")]
    pub fn with_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    /// Configures an absolute stop-at deadline.
    #[must_use]
    #[doc(alias = "--stop-after")]
    #[doc(alias = "--stop-at")]
    pub fn with_stop_at(mut self, deadline: Option<SystemTime>) -> Self {
        self.stop_at = deadline;
        self
    }

    /// Returns the minimum file size filter configured for the run.
    #[must_use]
    pub const fn min_file_size_limit(&self) -> Option<u64> {
        self.min_file_size
    }

    /// Returns the maximum file size filter configured for the run.
    #[must_use]
    pub const fn max_file_size_limit(&self) -> Option<u64> {
        self.max_file_size
    }

    /// Reports whether source files should be removed after transfer.
    #[must_use]
    pub const fn remove_source_files_enabled(&self) -> bool {
        self.remove_source_files
    }

    /// Returns the configured bandwidth limit, if any, in bytes per second.
    #[must_use]
    pub const fn bandwidth_limit_bytes(&self) -> Option<NonZeroU64> {
        self.bandwidth_limit
    }

    /// Returns the configured burst size in bytes, if any.
    #[must_use]
    pub const fn bandwidth_burst_bytes(&self) -> Option<NonZeroU64> {
        self.bandwidth_burst
    }

    /// Returns whether destination files are preallocated before writing.
    #[must_use]
    pub const fn preallocate_enabled(&self) -> bool {
        self.preallocate
    }

    /// Returns the configured inactivity timeout, if any.
    #[must_use]
    pub const fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    /// Returns the configured stop-at deadline, if any.
    #[must_use]
    pub const fn stop_at(&self) -> Option<SystemTime> {
        self.stop_at
    }
}
