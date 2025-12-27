use super::*;

impl ClientConfig {
    /// Returns whether the run should avoid mutating the destination filesystem.
    #[must_use]
    #[doc(alias = "--dry-run")]
    #[doc(alias = "-n")]
    pub const fn dry_run(&self) -> bool {
        self.dry_run
    }

    /// Returns the configured deletion mode.
    #[must_use]
    pub const fn delete_mode(&self) -> DeleteMode {
        self.delete_mode
    }

    /// Returns whether extraneous destination files should be removed.
    #[must_use]
    #[doc(alias = "--delete")]
    pub const fn delete(&self) -> bool {
        self.delete_mode.is_enabled()
    }

    /// Returns whether extraneous entries should be removed before the transfer begins.
    #[must_use]
    #[doc(alias = "--delete-before")]
    pub const fn delete_before(&self) -> bool {
        matches!(self.delete_mode, DeleteMode::Before)
    }

    /// Returns whether extraneous entries should be removed after the transfer completes.
    #[must_use]
    #[doc(alias = "--delete-after")]
    pub const fn delete_after(&self) -> bool {
        matches!(self.delete_mode, DeleteMode::After)
    }

    /// Returns whether extraneous entries should be removed after transfers using delayed sweeps.
    #[must_use]
    #[doc(alias = "--delete-delay")]
    pub const fn delete_delay(&self) -> bool {
        matches!(self.delete_mode, DeleteMode::Delay)
    }

    /// Returns whether excluded destination entries should also be deleted.
    #[must_use]
    #[doc(alias = "--delete-excluded")]
    pub const fn delete_excluded(&self) -> bool {
        self.delete_excluded
    }

    /// Returns the configured maximum number of deletions, if any.
    #[must_use]
    #[doc(alias = "--max-delete")]
    pub const fn max_delete(&self) -> Option<u64> {
        self.max_delete
    }

    /// Returns whether deletions should proceed even when I/O errors occur.
    ///
    /// When enabled, rsync will continue with the deletion phase even if
    /// there were I/O errors during the transfer. Without this flag,
    /// I/O errors cause the deletion phase to be skipped to prevent
    /// accidental data loss.
    #[must_use]
    #[doc(alias = "--ignore-errors")]
    pub const fn ignore_errors(&self) -> bool {
        self.ignore_errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> ClientConfig {
        ClientConfig::default()
    }

    // Tests for dry_run
    #[test]
    fn dry_run_default_is_false() {
        let config = default_config();
        assert!(!config.dry_run());
    }

    // Tests for delete_mode
    #[test]
    fn delete_mode_default_is_disabled() {
        let config = default_config();
        assert_eq!(config.delete_mode(), DeleteMode::Disabled);
    }

    // Tests for delete
    #[test]
    fn delete_default_is_false() {
        let config = default_config();
        assert!(!config.delete());
    }

    // Tests for delete_before
    #[test]
    fn delete_before_default_is_false() {
        let config = default_config();
        assert!(!config.delete_before());
    }

    // Tests for delete_after
    #[test]
    fn delete_after_default_is_false() {
        let config = default_config();
        assert!(!config.delete_after());
    }

    // Tests for delete_delay
    #[test]
    fn delete_delay_default_is_false() {
        let config = default_config();
        assert!(!config.delete_delay());
    }

    // Tests for delete_excluded
    #[test]
    fn delete_excluded_default_is_false() {
        let config = default_config();
        assert!(!config.delete_excluded());
    }

    // Tests for max_delete
    #[test]
    fn max_delete_default_is_none() {
        let config = default_config();
        assert!(config.max_delete().is_none());
    }

    // Tests for ignore_errors
    #[test]
    fn ignore_errors_default_is_false() {
        let config = default_config();
        assert!(!config.ignore_errors());
    }
}
