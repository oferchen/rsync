use std::num::NonZeroU32;
use std::time::Duration;

use crate::signature::SignatureAlgorithm;

use super::types::LocalCopyOptions;

impl LocalCopyOptions {
    /// Enables checksum-based change detection.
    #[must_use]
    #[doc(alias = "--checksum")]
    pub const fn checksum(mut self, checksum: bool) -> Self {
        self.checksum = checksum;
        self
    }

    /// Selects the strong checksum algorithm used when verifying files.
    #[must_use]
    pub const fn with_checksum_algorithm(mut self, algorithm: SignatureAlgorithm) -> Self {
        self.checksum_algorithm = algorithm;
        self
    }

    /// Enables size-only change detection.
    #[must_use]
    #[doc(alias = "--size-only")]
    pub const fn size_only(mut self, size_only: bool) -> Self {
        self.size_only = size_only;
        self
    }

    /// Disables timestamp-based quick checks so files are always rescanned.
    #[must_use]
    #[doc(alias = "--ignore-times")]
    pub const fn ignore_times(mut self, ignore: bool) -> Self {
        self.ignore_times = ignore;
        self
    }

    /// Requests that existing destination files be skipped.
    #[must_use]
    #[doc(alias = "--ignore-existing")]
    pub const fn ignore_existing(mut self, ignore: bool) -> Self {
        self.ignore_existing = ignore;
        self
    }

    /// Requests that new destination entries be skipped when missing.
    #[must_use]
    #[doc(alias = "--existing")]
    pub const fn existing_only(mut self, existing: bool) -> Self {
        self.existing_only = existing;
        self
    }

    /// Requests that missing source arguments be ignored instead of causing an error.
    #[must_use]
    #[doc(alias = "--ignore-missing-args")]
    pub const fn ignore_missing_args(mut self, ignore: bool) -> Self {
        self.ignore_missing_args = ignore;
        self
    }

    /// Requests that destination entries corresponding to missing source arguments be removed.
    #[must_use]
    #[doc(alias = "--delete-missing-args")]
    pub const fn delete_missing_args(mut self, delete: bool) -> Self {
        self.delete_missing_args = delete;
        self
    }

    /// Requests that newer destination files be preserved.
    #[must_use]
    #[doc(alias = "--update")]
    pub const fn update(mut self, update: bool) -> Self {
        self.update = update;
        self
    }

    /// Applies an explicit delta-transfer block size override.
    #[must_use]
    #[doc(alias = "--block-size")]
    pub const fn with_block_size_override(mut self, block_size: Option<NonZeroU32>) -> Self {
        self.block_size_override = block_size;
        self
    }

    /// Applies the modification time tolerance used when comparing files.
    #[must_use]
    #[doc(alias = "--modify-window")]
    pub const fn with_modify_window(mut self, window: Duration) -> Self {
        self.modify_window = window;
        self
    }

    /// Reports whether checksum-based change detection has been requested.
    #[must_use]
    pub const fn checksum_enabled(&self) -> bool {
        self.checksum
    }

    /// Returns the strong checksum algorithm used for comparisons.
    #[must_use]
    pub const fn checksum_algorithm(&self) -> SignatureAlgorithm {
        self.checksum_algorithm
    }

    /// Reports whether size-only change detection has been requested.
    #[must_use]
    pub const fn size_only_enabled(&self) -> bool {
        self.size_only
    }

    /// Reports whether timestamp-based quick checks should be skipped.
    #[must_use]
    pub const fn ignore_times_enabled(&self) -> bool {
        self.ignore_times
    }

    /// Reports whether existing destination files should be skipped.
    #[must_use]
    pub const fn ignore_existing_enabled(&self) -> bool {
        self.ignore_existing
    }

    /// Reports whether missing destination entries should be skipped.
    #[must_use]
    pub const fn existing_only_enabled(&self) -> bool {
        self.existing_only
    }

    /// Reports whether missing source arguments should be ignored.
    #[must_use]
    pub const fn ignore_missing_args_enabled(&self) -> bool {
        self.ignore_missing_args
    }

    /// Reports whether missing source arguments should trigger destination deletions.
    #[must_use]
    pub const fn delete_missing_args_enabled(&self) -> bool {
        self.delete_missing_args
    }

    /// Reports whether newer destination files should be preserved.
    #[must_use]
    pub const fn update_enabled(&self) -> bool {
        self.update
    }

    /// Returns the configured delta-transfer block size override, if any.
    #[must_use]
    pub const fn block_size_override(&self) -> Option<NonZeroU32> {
        self.block_size_override
    }

    /// Returns the modification time tolerance applied during comparisons.
    #[must_use]
    pub const fn modify_window(&self) -> Duration {
        self.modify_window
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_enables() {
        let opts = LocalCopyOptions::new().checksum(true);
        assert!(opts.checksum_enabled());
    }

    #[test]
    fn checksum_disables() {
        let opts = LocalCopyOptions::new().checksum(true).checksum(false);
        assert!(!opts.checksum_enabled());
    }

    #[test]
    fn with_checksum_algorithm_sets_value() {
        let opts =
            LocalCopyOptions::new().with_checksum_algorithm(SignatureAlgorithm::Xxh64 { seed: 42 });
        assert!(matches!(
            opts.checksum_algorithm(),
            SignatureAlgorithm::Xxh64 { seed: 42 }
        ));
    }

    #[test]
    fn size_only_enables() {
        let opts = LocalCopyOptions::new().size_only(true);
        assert!(opts.size_only_enabled());
    }

    #[test]
    fn size_only_disables() {
        let opts = LocalCopyOptions::new().size_only(true).size_only(false);
        assert!(!opts.size_only_enabled());
    }

    #[test]
    fn ignore_times_enables() {
        let opts = LocalCopyOptions::new().ignore_times(true);
        assert!(opts.ignore_times_enabled());
    }

    #[test]
    fn ignore_times_disables() {
        let opts = LocalCopyOptions::new()
            .ignore_times(true)
            .ignore_times(false);
        assert!(!opts.ignore_times_enabled());
    }

    #[test]
    fn ignore_existing_enables() {
        let opts = LocalCopyOptions::new().ignore_existing(true);
        assert!(opts.ignore_existing_enabled());
    }

    #[test]
    fn ignore_existing_disables() {
        let opts = LocalCopyOptions::new()
            .ignore_existing(true)
            .ignore_existing(false);
        assert!(!opts.ignore_existing_enabled());
    }

    #[test]
    fn existing_only_enables() {
        let opts = LocalCopyOptions::new().existing_only(true);
        assert!(opts.existing_only_enabled());
    }

    #[test]
    fn existing_only_disables() {
        let opts = LocalCopyOptions::new()
            .existing_only(true)
            .existing_only(false);
        assert!(!opts.existing_only_enabled());
    }

    #[test]
    fn ignore_missing_args_enables() {
        let opts = LocalCopyOptions::new().ignore_missing_args(true);
        assert!(opts.ignore_missing_args_enabled());
    }

    #[test]
    fn ignore_missing_args_disables() {
        let opts = LocalCopyOptions::new()
            .ignore_missing_args(true)
            .ignore_missing_args(false);
        assert!(!opts.ignore_missing_args_enabled());
    }

    #[test]
    fn delete_missing_args_enables() {
        let opts = LocalCopyOptions::new().delete_missing_args(true);
        assert!(opts.delete_missing_args_enabled());
    }

    #[test]
    fn delete_missing_args_disables() {
        let opts = LocalCopyOptions::new()
            .delete_missing_args(true)
            .delete_missing_args(false);
        assert!(!opts.delete_missing_args_enabled());
    }

    #[test]
    fn update_enables() {
        let opts = LocalCopyOptions::new().update(true);
        assert!(opts.update_enabled());
    }

    #[test]
    fn update_disables() {
        let opts = LocalCopyOptions::new().update(true).update(false);
        assert!(!opts.update_enabled());
    }

    #[test]
    fn with_block_size_override_sets_value() {
        let block_size = NonZeroU32::new(4096).unwrap();
        let opts = LocalCopyOptions::new().with_block_size_override(Some(block_size));
        assert_eq!(opts.block_size_override(), Some(block_size));
    }

    #[test]
    fn with_block_size_override_none_clears() {
        let block_size = NonZeroU32::new(4096).unwrap();
        let opts = LocalCopyOptions::new()
            .with_block_size_override(Some(block_size))
            .with_block_size_override(None);
        assert!(opts.block_size_override().is_none());
    }

    #[test]
    fn with_modify_window_sets_value() {
        let window = Duration::from_secs(2);
        let opts = LocalCopyOptions::new().with_modify_window(window);
        assert_eq!(opts.modify_window(), window);
    }

    #[test]
    fn defaults_have_no_integrity_overrides() {
        let opts = LocalCopyOptions::new();
        assert!(!opts.checksum_enabled());
        assert!(!opts.size_only_enabled());
        assert!(!opts.ignore_times_enabled());
        assert!(!opts.ignore_existing_enabled());
        assert!(!opts.existing_only_enabled());
        assert!(!opts.ignore_missing_args_enabled());
        assert!(!opts.delete_missing_args_enabled());
        assert!(!opts.update_enabled());
        assert!(opts.block_size_override().is_none());
        assert_eq!(opts.modify_window(), Duration::ZERO);
    }
}
