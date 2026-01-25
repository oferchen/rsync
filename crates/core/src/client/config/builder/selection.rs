use super::*;

impl ClientConfigBuilder {
    builder_setter! {
        /// Sets the minimum file size to transfer.
        min_file_size: Option<u64>,
        /// Sets the maximum file size to transfer.
        max_file_size: Option<u64>,
        /// Sets the modification time tolerance used when comparing files.
        modify_window: Option<u64>,
        /// Enables or disables removal of source files after a successful transfer.
        remove_source_files: bool,
        /// Enables or disables size-only change detection.
        size_only: bool,
        /// Enables or disables ignoring file timestamps during quick checks.
        ignore_times: bool,
        /// Enables or disables skipping of existing destination files.
        ignore_existing: bool,
        /// Enables or disables skipping creation of new destination entries.
        existing_only: bool,
        /// Enables or disables ignoring missing source arguments.
        ignore_missing_args: bool,
        /// Enables or disables deletion of destination entries for missing source arguments.
        delete_missing_args: bool,
        /// Enables or disables preservation of newer destination files.
        update: bool,
        /// Enables or disables preservation of source-relative path components.
        relative_paths: bool,
        /// Enables or disables recursive traversal of source directories.
        recursive: bool,
        /// Enables or disables copying of directory entries when recursion is disabled.
        dirs: bool,
        /// Enables or disables traversal across filesystem boundaries.
        one_file_system: bool,
        /// Enables destination path creation prior to copying.
        mkpath: bool,
        /// Enables or disables removal of conflicting destination entries prior to updates.
        force_replacements: bool,
        /// Enables or disables pruning of empty directories after filters apply.
        prune_empty_dirs: bool,
    }

    /// Enables or disables creation of parent directories implied by the source path.
    #[must_use]
    #[doc(alias = "--implied-dirs")]
    #[doc(alias = "--no-implied-dirs")]
    pub const fn implied_dirs(mut self, implied: bool) -> Self {
        self.implied_dirs = Some(implied);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builder() -> ClientConfigBuilder {
        ClientConfigBuilder::default()
    }

    #[test]
    fn min_file_size_sets_limit() {
        let config = builder().min_file_size(Some(1024)).build();
        assert_eq!(config.min_file_size(), Some(1024));
    }

    #[test]
    fn min_file_size_none_clears_limit() {
        let config = builder()
            .min_file_size(Some(1024))
            .min_file_size(None)
            .build();
        assert!(config.min_file_size().is_none());
    }

    #[test]
    fn max_file_size_sets_limit() {
        let config = builder().max_file_size(Some(1048576)).build();
        assert_eq!(config.max_file_size(), Some(1048576));
    }

    #[test]
    fn max_file_size_none_clears_limit() {
        let config = builder()
            .max_file_size(Some(1048576))
            .max_file_size(None)
            .build();
        assert!(config.max_file_size().is_none());
    }

    #[test]
    fn modify_window_sets_value() {
        let config = builder().modify_window(Some(2)).build();
        assert_eq!(config.modify_window(), Some(2));
    }

    #[test]
    fn modify_window_none_clears_value() {
        let config = builder().modify_window(Some(2)).modify_window(None).build();
        assert!(config.modify_window().is_none());
    }

    #[test]
    fn remove_source_files_sets_flag() {
        let config = builder().remove_source_files(true).build();
        assert!(config.remove_source_files());
    }

    #[test]
    fn remove_source_files_false_clears_flag() {
        let config = builder()
            .remove_source_files(true)
            .remove_source_files(false)
            .build();
        assert!(!config.remove_source_files());
    }

    #[test]
    fn size_only_sets_flag() {
        let config = builder().size_only(true).build();
        assert!(config.size_only());
    }

    #[test]
    fn ignore_times_sets_flag() {
        let config = builder().ignore_times(true).build();
        assert!(config.ignore_times());
    }

    #[test]
    fn ignore_existing_sets_flag() {
        let config = builder().ignore_existing(true).build();
        assert!(config.ignore_existing());
    }

    #[test]
    fn existing_only_sets_flag() {
        let config = builder().existing_only(true).build();
        assert!(config.existing_only());
    }

    #[test]
    fn ignore_missing_args_sets_flag() {
        let config = builder().ignore_missing_args(true).build();
        assert!(config.ignore_missing_args());
    }

    #[test]
    fn delete_missing_args_sets_flag() {
        let config = builder().delete_missing_args(true).build();
        assert!(config.delete_missing_args());
    }

    #[test]
    fn update_sets_flag() {
        let config = builder().update(true).build();
        assert!(config.update());
    }

    #[test]
    fn relative_paths_sets_flag() {
        let config = builder().relative_paths(true).build();
        assert!(config.relative_paths());
    }

    #[test]
    fn recursive_sets_flag() {
        let config = builder().recursive(true).build();
        assert!(config.recursive());
    }

    #[test]
    fn dirs_sets_flag() {
        let config = builder().dirs(true).build();
        assert!(config.dirs());
    }

    #[test]
    fn one_file_system_sets_flag() {
        let config = builder().one_file_system(true).build();
        assert!(config.one_file_system());
    }

    #[test]
    fn implied_dirs_sets_true() {
        let config = builder().implied_dirs(true).build();
        assert!(config.implied_dirs());
    }

    #[test]
    fn implied_dirs_sets_false() {
        let config = builder().implied_dirs(false).build();
        assert!(!config.implied_dirs());
    }

    #[test]
    fn mkpath_sets_flag() {
        let config = builder().mkpath(true).build();
        assert!(config.mkpath());
    }

    #[test]
    fn force_replacements_sets_flag() {
        let config = builder().force_replacements(true).build();
        assert!(config.force_replacements());
    }

    #[test]
    fn prune_empty_dirs_sets_flag() {
        let config = builder().prune_empty_dirs(true).build();
        assert!(config.prune_empty_dirs());
    }

    #[test]
    fn default_min_file_size_is_none() {
        let config = builder().build();
        assert!(config.min_file_size().is_none());
    }

    #[test]
    fn default_max_file_size_is_none() {
        let config = builder().build();
        assert!(config.max_file_size().is_none());
    }

    #[test]
    fn default_recursive_is_false() {
        let config = builder().build();
        assert!(!config.recursive());
    }
}
