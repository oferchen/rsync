//! Setter methods for path behavior, symlink handling, and traversal options.

use super::LocalCopyOptionsBuilder;

impl LocalCopyOptionsBuilder {
    /// Enables opening files without updating access time.
    #[must_use]
    pub fn open_noatime(mut self, enabled: bool) -> Self {
        self.open_noatime = enabled;
        self
    }

    /// Controls whole-file transfer mode.
    ///
    /// - `true` forces whole-file transfers (`--whole-file` / `-W`).
    /// - `false` forces delta-transfer mode (`--no-whole-file`).
    #[must_use]
    pub fn whole_file(mut self, enabled: bool) -> Self {
        self.whole_file = Some(enabled);
        self
    }

    /// Sets the whole-file transfer mode as a tri-state option.
    ///
    /// - `Some(true)` forces whole-file transfers (`--whole-file` / `-W`).
    /// - `Some(false)` forces delta-transfer mode (`--no-whole-file`).
    /// - `None` uses automatic detection based on transfer context.
    #[must_use]
    pub fn whole_file_option(mut self, option: Option<bool>) -> Self {
        self.whole_file = option;
        self
    }

    /// Enables copying symlinks as their targets.
    #[must_use]
    pub fn copy_links(mut self, enabled: bool) -> Self {
        self.copy_links = enabled;
        self
    }

    /// Enables preserving symlinks as symlinks.
    #[must_use]
    pub fn preserve_symlinks(mut self, enabled: bool) -> Self {
        self.preserve_symlinks = enabled;
        self
    }

    /// Alias for `preserve_symlinks` for rsync compatibility.
    #[must_use]
    pub fn links(mut self, enabled: bool) -> Self {
        self.preserve_symlinks = enabled;
        self
    }

    /// Enables copying directory symlinks as directories.
    #[must_use]
    pub fn copy_dirlinks(mut self, enabled: bool) -> Self {
        self.copy_dirlinks = enabled;
        self
    }

    /// Enables copying unsafe symlinks.
    #[must_use]
    pub fn copy_unsafe_links(mut self, enabled: bool) -> Self {
        self.copy_unsafe_links = enabled;
        self
    }

    /// Enables keeping existing directory symlinks.
    #[must_use]
    pub fn keep_dirlinks(mut self, enabled: bool) -> Self {
        self.keep_dirlinks = enabled;
        self
    }

    /// Enables safe link mode.
    #[must_use]
    pub fn safe_links(mut self, enabled: bool) -> Self {
        self.safe_links = enabled;
        self
    }

    /// Enables or disables symlink munging for daemon-mode security.
    #[must_use]
    pub fn munge_links(mut self, enabled: bool) -> Self {
        self.munge_links = enabled;
        self
    }

    /// Enables relative path handling.
    #[must_use]
    pub fn relative_paths(mut self, enabled: bool) -> Self {
        self.relative_paths = enabled;
        self
    }

    /// Enables one-file-system mode.
    ///
    /// Passing `true` sets the level to 1 (single `-x`).
    /// Passing `false` disables it (level 0).
    /// Use [`one_file_system_level`](Self::one_file_system_level) for finer control.
    #[must_use]
    pub fn one_file_system(mut self, enabled: bool) -> Self {
        self.one_file_system = if enabled { 1 } else { 0 };
        self
    }

    /// Sets the one-file-system level directly.
    ///
    /// - `0`: disabled (default).
    /// - `1`: single `-x` - skip directories on different filesystems during recursion.
    /// - `2`: double `-xx` - also skip mount-point directories at the transfer root level.
    #[must_use]
    pub fn one_file_system_level(mut self, level: u8) -> Self {
        self.one_file_system = level;
        self
    }

    /// Enables recursive mode.
    #[must_use]
    pub fn recursive(mut self, enabled: bool) -> Self {
        self.recursive = enabled;
        self
    }

    /// Enables dirs mode.
    #[must_use]
    pub fn dirs(mut self, enabled: bool) -> Self {
        self.dirs = enabled;
        self
    }

    /// Enables device handling.
    #[must_use]
    pub fn devices(mut self, enabled: bool) -> Self {
        self.devices = enabled;
        self
    }

    /// Enables copying devices as files.
    #[must_use]
    pub fn copy_devices_as_files(mut self, enabled: bool) -> Self {
        self.copy_devices_as_files = enabled;
        self
    }

    /// Enables special file handling.
    #[must_use]
    pub fn specials(mut self, enabled: bool) -> Self {
        self.specials = enabled;
        self
    }

    /// Enables force replacements.
    #[must_use]
    pub fn force_replacements(mut self, enabled: bool) -> Self {
        self.force_replacements = enabled;
        self
    }

    /// Enables implied directories.
    #[must_use]
    pub fn implied_dirs(mut self, enabled: bool) -> Self {
        self.implied_dirs = enabled;
        self
    }

    /// Enables mkpath mode.
    #[must_use]
    pub fn mkpath(mut self, enabled: bool) -> Self {
        self.mkpath = enabled;
        self
    }

    /// Enables prune-empty-dirs mode.
    #[must_use]
    pub fn prune_empty_dirs(mut self, enabled: bool) -> Self {
        self.prune_empty_dirs = enabled;
        self
    }
}
