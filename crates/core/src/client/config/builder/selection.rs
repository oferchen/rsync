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
    pub fn implied_dirs(mut self, implied: bool) -> Self {
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

    /// Enables or disables pruning of empty directories after filters apply.
    #[must_use]
    #[doc(alias = "--prune-empty-dirs")]
    #[doc(alias = "-m")]
    pub const fn prune_empty_dirs(mut self, prune: bool) -> Self {
        self.prune_empty_dirs = prune;
        self
    }
}
