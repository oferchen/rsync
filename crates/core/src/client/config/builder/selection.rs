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
        /// Sets the filesystem boundary traversal level (0=off, 1=single -x, 2=double -xx).
        one_file_system: u8,
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

    /// Sets the `--files-from` source configuration.
    ///
    /// When active, the file list is read from the specified source rather
    /// than from CLI positional arguments. For remote transfers, this affects
    /// how arguments are forwarded to the server process.
    ///
    /// # Upstream Reference
    ///
    /// - `options.c:2447-2490` — files_from parsing
    /// - `options.c:2944-2956` — server_options() forwarding
    #[must_use]
    pub fn files_from(mut self, source: super::FilesFromSource) -> Self {
        self.files_from = source;
        self
    }

    /// Enables or disables NUL-delimited mode for `--files-from`.
    ///
    /// When true, the file list uses NUL bytes as delimiters instead of
    /// newlines. This corresponds to `--from0` / `-0`.
    #[must_use]
    pub const fn from0(mut self, value: bool) -> Self {
        self.from0 = value;
        self
    }
}
