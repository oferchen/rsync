use super::*;

impl ClientConfigBuilder {
    /// Sets the minimum file size to transfer.
    #[must_use]
    #[doc(alias = "--min-size")]
    pub const fn min_file_size(mut self, limit: Option<u64>) -> Self {
        self.min_file_size = limit;
        self
    }

    /// Sets the maximum file size to transfer.
    #[must_use]
    #[doc(alias = "--max-size")]
    pub const fn max_file_size(mut self, limit: Option<u64>) -> Self {
        self.max_file_size = limit;
        self
    }

    /// Sets the modification time tolerance used when comparing files.
    #[must_use]
    #[doc(alias = "--modify-window")]
    pub const fn modify_window(mut self, window: Option<u64>) -> Self {
        self.modify_window = window;
        self
    }

    /// Enables or disables removal of source files after a successful transfer.
    #[must_use]
    #[doc(alias = "--remove-source-files")]
    #[doc(alias = "--remove-sent-files")]
    pub const fn remove_source_files(mut self, remove: bool) -> Self {
        self.remove_source_files = remove;
        self
    }

    /// Enables or disables size-only change detection.
    #[must_use]
    #[doc(alias = "--size-only")]
    pub const fn size_only(mut self, size_only: bool) -> Self {
        self.size_only = size_only;
        self
    }

    /// Enables or disables ignoring file timestamps during quick checks.
    #[must_use]
    #[doc(alias = "--ignore-times")]
    pub const fn ignore_times(mut self, ignore_times: bool) -> Self {
        self.ignore_times = ignore_times;
        self
    }

    /// Enables or disables skipping of existing destination files.
    #[must_use]
    #[doc(alias = "--ignore-existing")]
    pub const fn ignore_existing(mut self, ignore_existing: bool) -> Self {
        self.ignore_existing = ignore_existing;
        self
    }

    /// Enables or disables skipping creation of new destination entries.
    #[must_use]
    #[doc(alias = "--existing")]
    pub const fn existing_only(mut self, existing_only: bool) -> Self {
        self.existing_only = existing_only;
        self
    }

    /// Enables or disables ignoring missing source arguments.
    #[must_use]
    #[doc(alias = "--ignore-missing-args")]
    pub const fn ignore_missing_args(mut self, ignore: bool) -> Self {
        self.ignore_missing_args = ignore;
        self
    }

    /// Enables or disables deletion of destination entries for missing source arguments.
    #[must_use]
    #[doc(alias = "--delete-missing-args")]
    pub const fn delete_missing_args(mut self, delete: bool) -> Self {
        self.delete_missing_args = delete;
        self
    }

    /// Enables or disables preservation of newer destination files.
    #[must_use]
    #[doc(alias = "--update")]
    #[doc(alias = "-u")]
    pub const fn update(mut self, update: bool) -> Self {
        self.update = update;
        self
    }

    /// Enables or disables preservation of source-relative path components.
    #[must_use]
    #[doc(alias = "--relative")]
    #[doc(alias = "-R")]
    pub const fn relative_paths(mut self, relative: bool) -> Self {
        self.relative_paths = relative;
        self
    }

    /// Enables or disables recursive traversal of source directories.
    #[must_use]
    #[doc(alias = "--recursive")]
    #[doc(alias = "-r")]
    pub const fn recursive(mut self, recursive: bool) -> Self {
        self.recursive = recursive;
        self
    }

    /// Enables or disables copying of directory entries when recursion is disabled.
    #[must_use]
    #[doc(alias = "--dirs")]
    #[doc(alias = "-d")]
    pub const fn dirs(mut self, enabled: bool) -> Self {
        self.dirs = enabled;
        self
    }

    /// Enables or disables traversal across filesystem boundaries.
    #[must_use]
    #[doc(alias = "--one-file-system")]
    #[doc(alias = "-x")]
    pub const fn one_file_system(mut self, enabled: bool) -> Self {
        self.one_file_system = enabled;
        self
    }

    /// Enables or disables creation of parent directories implied by the source path.
    #[must_use]
    #[doc(alias = "--implied-dirs")]
    #[doc(alias = "--no-implied-dirs")]
    pub const fn implied_dirs(mut self, implied: bool) -> Self {
        self.implied_dirs = Some(implied);
        self
    }

    /// Enables destination path creation prior to copying.
    #[must_use]
    #[doc(alias = "--mkpath")]
    pub const fn mkpath(mut self, mkpath: bool) -> Self {
        self.mkpath = mkpath;
        self
    }

    /// Enables or disables removal of conflicting destination entries prior to updates.
    #[must_use]
    #[doc(alias = "--force")]
    #[doc(alias = "--no-force")]
    pub const fn force_replacements(mut self, force: bool) -> Self {
        self.force_replacements = force;
        self
    }

    /// Enables or disables pruning of empty directories after filters apply.
    #[must_use]
    #[doc(alias = "--prune-empty-dirs")]
    #[doc(alias = "-m")]
    pub const fn prune_empty_dirs(mut self, prune: bool) -> Self {
        self.prune_empty_dirs = prune;
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
