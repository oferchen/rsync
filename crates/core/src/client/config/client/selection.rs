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
            .map(Duration::from_secs)
            .unwrap_or(Duration::ZERO)
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

    /// Returns whether existing destination files should be skipped.
    #[must_use]
    pub const fn ignore_existing(&self) -> bool {
        self.ignore_existing
    }

    /// Returns whether missing source arguments should be ignored.
    #[must_use]
    #[doc(alias = "--ignore-missing-args")]
    pub const fn ignore_missing_args(&self) -> bool {
        self.ignore_missing_args
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

    /// Returns whether empty directories should be pruned after filtering.
    #[must_use]
    #[doc(alias = "--prune-empty-dirs")]
    #[doc(alias = "-m")]
    pub const fn prune_empty_dirs(&self) -> bool {
        self.prune_empty_dirs
    }
}
