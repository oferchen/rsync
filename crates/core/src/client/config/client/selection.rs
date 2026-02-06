use super::*;

impl ClientConfig {
    /// Returns the minimum file size filter, if configured.
    #[must_use]
    #[doc(alias = "--min-size")]
    pub const fn min_file_size(&self) -> Option<u64> {
        self.min_file_size
    }

    /// Returns the maximum file size filter, if configured.
    #[must_use]
    #[doc(alias = "--max-size")]
    pub const fn max_file_size(&self) -> Option<u64> {
        self.max_file_size
    }

    /// Returns the modification time tolerance, if configured.
    #[must_use]
    #[doc(alias = "--modify-window")]
    pub const fn modify_window(&self) -> Option<u64> {
        self.modify_window
    }

    /// Returns the modification time tolerance as a [`Duration`].
    #[must_use]
    pub fn modify_window_duration(&self) -> Duration {
        self.modify_window
            .map_or(Duration::ZERO, Duration::from_secs)
    }

    /// Returns whether the sender should remove source files after transfer.
    #[must_use]
    #[doc(alias = "--remove-source-files")]
    #[doc(alias = "--remove-sent-files")]
    pub const fn remove_source_files(&self) -> bool {
        self.remove_source_files
    }

    /// Reports whether size-only change detection should be used when evaluating updates.
    #[must_use]
    #[doc(alias = "--size-only")]
    pub const fn size_only(&self) -> bool {
        self.size_only
    }

    /// Reports whether timestamp-based quick checks should be skipped.
    #[must_use]
    #[doc(alias = "--ignore-times")]
    pub const fn ignore_times(&self) -> bool {
        self.ignore_times
    }

    /// Returns whether existing destination files should be skipped.
    #[must_use]
    pub const fn ignore_existing(&self) -> bool {
        self.ignore_existing
    }

    /// Returns whether new destination entries should be skipped.
    #[must_use]
    #[doc(alias = "--existing")]
    pub const fn existing_only(&self) -> bool {
        self.existing_only
    }

    /// Returns whether missing source arguments should be ignored.
    #[must_use]
    #[doc(alias = "--ignore-missing-args")]
    pub const fn ignore_missing_args(&self) -> bool {
        self.ignore_missing_args
    }

    /// Returns whether destination entries should be removed for missing source arguments.
    #[must_use]
    #[doc(alias = "--delete-missing-args")]
    pub const fn delete_missing_args(&self) -> bool {
        self.delete_missing_args
    }

    /// Reports whether files newer on the destination should be preserved.
    #[must_use]
    #[doc(alias = "--update")]
    #[doc(alias = "-u")]
    pub const fn update(&self) -> bool {
        self.update
    }

    /// Reports whether relative path preservation was requested.
    #[must_use]
    #[doc(alias = "--relative")]
    #[doc(alias = "-R")]
    pub const fn relative_paths(&self) -> bool {
        self.relative_paths
    }

    /// Reports whether traversal should remain on a single filesystem.
    #[must_use]
    #[doc(alias = "--one-file-system")]
    #[doc(alias = "-x")]
    pub const fn one_file_system(&self) -> bool {
        self.one_file_system >= 1
    }

    /// Returns the filesystem boundary traversal level.
    ///
    /// - `0` means disabled
    /// - `1` means single `-x` (skip different filesystems during recursion)
    /// - `2` means double `-xx` (also skip root-level mount points)
    #[must_use]
    pub const fn one_file_system_level(&self) -> u8 {
        self.one_file_system
    }

    /// Returns whether parent directories implied by the source path should be created.
    #[must_use]
    #[doc(alias = "--implied-dirs")]
    #[doc(alias = "--no-implied-dirs")]
    pub const fn implied_dirs(&self) -> bool {
        self.implied_dirs
    }

    /// Returns whether destination path components should be created when missing.
    #[must_use]
    #[doc(alias = "--mkpath")]
    pub const fn mkpath(&self) -> bool {
        self.mkpath
    }

    /// Returns whether conflicting destination entries should be removed prior to updates.
    #[must_use]
    #[doc(alias = "--force")]
    #[doc(alias = "--no-force")]
    pub const fn force_replacements(&self) -> bool {
        self.force_replacements
    }

    /// Returns whether empty directories should be pruned after filtering.
    #[must_use]
    #[doc(alias = "--prune-empty-dirs")]
    #[doc(alias = "-m")]
    pub const fn prune_empty_dirs(&self) -> bool {
        self.prune_empty_dirs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> ClientConfig {
        ClientConfig::default()
    }

    // Tests for file size filters
    #[test]
    fn min_file_size_default_is_none() {
        let config = default_config();
        assert!(config.min_file_size().is_none());
    }

    #[test]
    fn max_file_size_default_is_none() {
        let config = default_config();
        assert!(config.max_file_size().is_none());
    }

    // Tests for modify window
    #[test]
    fn modify_window_default_is_none() {
        let config = default_config();
        assert!(config.modify_window().is_none());
    }

    #[test]
    fn modify_window_duration_default_is_zero() {
        let config = default_config();
        assert_eq!(config.modify_window_duration(), Duration::ZERO);
    }

    // Tests for remove source files
    #[test]
    fn remove_source_files_default_is_false() {
        let config = default_config();
        assert!(!config.remove_source_files());
    }

    // Tests for size-only mode
    #[test]
    fn size_only_default_is_false() {
        let config = default_config();
        assert!(!config.size_only());
    }

    // Tests for ignore times
    #[test]
    fn ignore_times_default_is_false() {
        let config = default_config();
        assert!(!config.ignore_times());
    }

    // Tests for ignore existing
    #[test]
    fn ignore_existing_default_is_false() {
        let config = default_config();
        assert!(!config.ignore_existing());
    }

    // Tests for existing only
    #[test]
    fn existing_only_default_is_false() {
        let config = default_config();
        assert!(!config.existing_only());
    }

    // Tests for ignore missing args
    #[test]
    fn ignore_missing_args_default_is_false() {
        let config = default_config();
        assert!(!config.ignore_missing_args());
    }

    // Tests for delete missing args
    #[test]
    fn delete_missing_args_default_is_false() {
        let config = default_config();
        assert!(!config.delete_missing_args());
    }

    // Tests for update mode
    #[test]
    fn update_default_is_false() {
        let config = default_config();
        assert!(!config.update());
    }

    // Tests for relative paths
    #[test]
    fn relative_paths_default_is_false() {
        let config = default_config();
        assert!(!config.relative_paths());
    }

    // Tests for one file system
    #[test]
    fn one_file_system_default_is_false() {
        let config = default_config();
        assert!(!config.one_file_system());
        assert_eq!(config.one_file_system_level(), 0);
    }

    // Tests for implied dirs
    #[test]
    fn implied_dirs_default_is_true() {
        let config = default_config();
        assert!(config.implied_dirs());
    }

    // Tests for mkpath
    #[test]
    fn mkpath_default_is_false() {
        let config = default_config();
        assert!(!config.mkpath());
    }

    // Tests for force replacements
    #[test]
    fn force_replacements_default_is_false() {
        let config = default_config();
        assert!(!config.force_replacements());
    }

    // Tests for prune empty dirs
    #[test]
    fn prune_empty_dirs_default_is_false() {
        let config = default_config();
        assert!(!config.prune_empty_dirs());
    }
}
