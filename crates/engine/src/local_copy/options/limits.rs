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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_file_size_sets_limit() {
        let opts = LocalCopyOptions::new().min_file_size(Some(1024));
        assert_eq!(opts.min_file_size_limit(), Some(1024));
    }

    #[test]
    fn min_file_size_none_clears_limit() {
        let opts = LocalCopyOptions::new()
            .min_file_size(Some(1024))
            .min_file_size(None);
        assert!(opts.min_file_size_limit().is_none());
    }

    #[test]
    fn max_file_size_sets_limit() {
        let opts = LocalCopyOptions::new().max_file_size(Some(1_000_000));
        assert_eq!(opts.max_file_size_limit(), Some(1_000_000));
    }

    #[test]
    fn max_file_size_none_clears_limit() {
        let opts = LocalCopyOptions::new()
            .max_file_size(Some(1_000_000))
            .max_file_size(None);
        assert!(opts.max_file_size_limit().is_none());
    }

    #[test]
    fn remove_source_files_enables() {
        let opts = LocalCopyOptions::new().remove_source_files(true);
        assert!(opts.remove_source_files_enabled());
    }

    #[test]
    fn remove_source_files_disables() {
        let opts = LocalCopyOptions::new()
            .remove_source_files(true)
            .remove_source_files(false);
        assert!(!opts.remove_source_files_enabled());
    }

    #[test]
    fn preallocate_enables() {
        let opts = LocalCopyOptions::new().preallocate(true);
        assert!(opts.preallocate_enabled());
    }

    #[test]
    fn preallocate_disables() {
        let opts = LocalCopyOptions::new().preallocate(true).preallocate(false);
        assert!(!opts.preallocate_enabled());
    }

    #[test]
    fn bandwidth_limit_sets_value() {
        let limit = NonZeroU64::new(1_000_000).unwrap();
        let opts = LocalCopyOptions::new().bandwidth_limit(Some(limit));
        assert_eq!(opts.bandwidth_limit_bytes(), Some(limit));
    }

    #[test]
    fn bandwidth_limit_none_clears() {
        let limit = NonZeroU64::new(1_000_000).unwrap();
        let opts = LocalCopyOptions::new()
            .bandwidth_limit(Some(limit))
            .bandwidth_limit(None);
        assert!(opts.bandwidth_limit_bytes().is_none());
    }

    #[test]
    fn bandwidth_burst_sets_value() {
        let burst = NonZeroU64::new(8192).unwrap();
        let opts = LocalCopyOptions::new().bandwidth_burst(Some(burst));
        assert_eq!(opts.bandwidth_burst_bytes(), Some(burst));
    }

    #[test]
    fn bandwidth_burst_none_clears() {
        let burst = NonZeroU64::new(8192).unwrap();
        let opts = LocalCopyOptions::new()
            .bandwidth_burst(Some(burst))
            .bandwidth_burst(None);
        assert!(opts.bandwidth_burst_bytes().is_none());
    }

    #[test]
    fn with_timeout_sets_value() {
        let timeout = Duration::from_secs(60);
        let opts = LocalCopyOptions::new().with_timeout(Some(timeout));
        assert_eq!(opts.timeout(), Some(timeout));
    }

    #[test]
    fn with_timeout_none_clears() {
        let timeout = Duration::from_secs(60);
        let opts = LocalCopyOptions::new()
            .with_timeout(Some(timeout))
            .with_timeout(None);
        assert!(opts.timeout().is_none());
    }

    #[test]
    fn with_stop_at_sets_value() {
        let deadline = SystemTime::now();
        let opts = LocalCopyOptions::new().with_stop_at(Some(deadline));
        assert!(opts.stop_at().is_some());
    }

    #[test]
    fn with_stop_at_none_clears() {
        let deadline = SystemTime::now();
        let opts = LocalCopyOptions::new()
            .with_stop_at(Some(deadline))
            .with_stop_at(None);
        assert!(opts.stop_at().is_none());
    }

    #[test]
    fn defaults_have_no_limits() {
        let opts = LocalCopyOptions::new();
        assert!(opts.min_file_size_limit().is_none());
        assert!(opts.max_file_size_limit().is_none());
        assert!(!opts.remove_source_files_enabled());
        assert!(!opts.preallocate_enabled());
        assert!(opts.bandwidth_limit_bytes().is_none());
        assert!(opts.bandwidth_burst_bytes().is_none());
        assert!(opts.timeout().is_none());
        assert!(opts.stop_at().is_none());
    }
}
