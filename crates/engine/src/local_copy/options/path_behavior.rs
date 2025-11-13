use super::types::LocalCopyOptions;

impl LocalCopyOptions {
    /// Controls whether whole-file transfers are forced even when delta mode is requested.
    #[must_use]
    #[doc(alias = "--whole-file")]
    #[doc(alias = "--no-whole-file")]
    pub const fn whole_file(mut self, whole: bool) -> Self {
        self.whole_file = whole;
        self
    }

    /// Requests that symlinks be followed and copied as their referents.
    #[must_use]
    #[doc(alias = "--copy-links")]
    #[doc(alias = "-L")]
    pub const fn copy_links(mut self, copy: bool) -> Self {
        self.copy_links = copy;
        self
    }

    /// Preserves symlinks instead of transforming or skipping them.
    #[must_use]
    #[doc(alias = "--links")]
    #[doc(alias = "-l")]
    pub const fn links(mut self, preserve: bool) -> Self {
        self.preserve_symlinks = preserve;
        self
    }

    /// Attempts to open source files without updating their access time.
    #[must_use]
    #[doc(alias = "--open-noatime")]
    #[doc(alias = "--no-open-noatime")]
    pub const fn open_noatime(mut self, enabled: bool) -> Self {
        self.open_noatime = enabled;
        self
    }

    /// Requests that unsafe symlinks be followed and copied as their referents.
    #[must_use]
    #[doc(alias = "--copy-unsafe-links")]
    pub const fn copy_unsafe_links(mut self, copy: bool) -> Self {
        self.copy_unsafe_links = copy;
        self
    }

    /// Skips symlinks whose targets escape the transfer root.
    #[must_use]
    #[doc(alias = "--safe-links")]
    pub const fn safe_links(mut self, enabled: bool) -> Self {
        self.safe_links = enabled;
        self
    }

    /// Treats symlinks to directories as directories when traversing the source tree.
    #[must_use]
    #[doc(alias = "--copy-dirlinks")]
    pub const fn copy_dirlinks(mut self, copy: bool) -> Self {
        self.copy_dirlinks = copy;
        self
    }

    /// Keeps existing destination symlinks that point to directories.
    #[must_use]
    #[doc(alias = "--keep-dirlinks")]
    pub const fn keep_dirlinks(mut self, keep: bool) -> Self {
        self.keep_dirlinks = keep;
        self
    }

    /// Requests that relative source paths be preserved in the destination.
    #[must_use]
    #[doc(alias = "--relative")]
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

    /// Enables or disables copying directory entries when recursion is disabled.
    #[must_use]
    #[doc(alias = "--dirs")]
    #[doc(alias = "-d")]
    pub const fn dirs(mut self, enabled: bool) -> Self {
        self.dirs = enabled;
        self
    }

    /// Controls whether parent directories implied by the source path are created.
    #[must_use]
    #[doc(alias = "--implied-dirs")]
    #[doc(alias = "--no-implied-dirs")]
    pub const fn implied_dirs(mut self, implied: bool) -> Self {
        self.implied_dirs = implied;
        self
    }

    /// Requests creation of missing destination path components prior to copying.
    #[must_use]
    #[doc(alias = "--mkpath")]
    pub const fn mkpath(mut self, mkpath: bool) -> Self {
        self.mkpath = mkpath;
        self
    }

    /// Prunes directories that would otherwise be empty after filtering.
    #[must_use]
    #[doc(alias = "--prune-empty-dirs")]
    #[doc(alias = "-m")]
    pub const fn prune_empty_dirs(mut self, prune: bool) -> Self {
        self.prune_empty_dirs = prune;
        self
    }

    /// Requests that device nodes be copied.
    #[must_use]
    #[doc(alias = "--devices")]
    pub const fn devices(mut self, devices: bool) -> Self {
        self.devices = devices;
        self
    }

    /// Treats device nodes as regular files and copies their contents.
    #[must_use]
    #[doc(alias = "--copy-devices")]
    pub const fn copy_devices_as_files(mut self, copy: bool) -> Self {
        self.copy_devices_as_files = copy;
        self
    }

    /// Requests that special files such as FIFOs be copied.
    #[must_use]
    #[doc(alias = "--specials")]
    pub const fn specials(mut self, specials: bool) -> Self {
        self.specials = specials;
        self
    }

    /// Restricts traversal to a single filesystem when enabled.
    #[must_use]
    #[doc(alias = "--one-file-system")]
    #[doc(alias = "-x")]
    pub const fn one_file_system(mut self, enabled: bool) -> Self {
        self.one_file_system = enabled;
        self
    }

    /// Returns `true` when the copy should remain on the source filesystem.
    #[must_use]
    pub const fn one_file_system_enabled(&self) -> bool {
        self.one_file_system
    }

    /// Reports whether whole-file transfers are requested.
    #[must_use]
    pub const fn whole_file_enabled(&self) -> bool {
        self.whole_file
    }

    /// Reports whether source files should be opened without updating access times.
    #[must_use]
    pub const fn open_noatime_enabled(&self) -> bool {
        self.open_noatime
    }

    /// Returns whether symlinks should be materialised as their referents.
    #[must_use]
    pub const fn copy_links_enabled(&self) -> bool {
        self.copy_links
    }

    /// Returns whether symlinks should be preserved as symlinks.
    #[must_use]
    pub const fn links_enabled(&self) -> bool {
        self.preserve_symlinks
    }

    /// Returns whether unsafe symlinks should be materialised as their referents.
    #[must_use]
    pub const fn copy_unsafe_links_enabled(&self) -> bool {
        self.copy_unsafe_links
    }

    /// Reports whether unsafe symlinks should be ignored.
    #[must_use]
    pub const fn safe_links_enabled(&self) -> bool {
        self.safe_links
    }

    /// Reports whether symlinks to directories should be followed as directories.
    #[must_use]
    pub const fn copy_dirlinks_enabled(&self) -> bool {
        self.copy_dirlinks
    }

    /// Reports whether existing destination directory symlinks should be preserved.
    #[must_use]
    pub const fn keep_dirlinks_enabled(&self) -> bool {
        self.keep_dirlinks
    }

    /// Reports whether relative path preservation has been requested.
    #[must_use]
    pub const fn relative_paths_enabled(&self) -> bool {
        self.relative_paths
    }

    /// Reports whether recursive traversal is enabled.
    #[must_use]
    #[doc(alias = "--recursive")]
    #[doc(alias = "-r")]
    pub const fn recursive_enabled(&self) -> bool {
        self.recursive
    }

    /// Reports whether directory entries should be copied when recursion is disabled.
    #[must_use]
    #[doc(alias = "--dirs")]
    #[doc(alias = "-d")]
    pub const fn dirs_enabled(&self) -> bool {
        self.dirs
    }

    /// Reports whether implied parent directories should be created automatically.
    #[must_use]
    pub const fn implied_dirs_enabled(&self) -> bool {
        self.implied_dirs
    }

    /// Reports whether `--mkpath` style directory creation is enabled.
    #[must_use]
    #[doc(alias = "--mkpath")]
    pub const fn mkpath_enabled(&self) -> bool {
        self.mkpath
    }

    /// Returns whether empty directories should be pruned after filtering.
    #[must_use]
    pub const fn prune_empty_dirs_enabled(&self) -> bool {
        self.prune_empty_dirs
    }

    /// Reports whether copying of device nodes has been requested.
    #[must_use]
    pub const fn devices_enabled(&self) -> bool {
        self.devices
    }

    /// Returns whether device nodes should be copied as regular files.
    #[must_use]
    #[doc(alias = "--copy-devices")]
    pub const fn copy_devices_as_files_enabled(&self) -> bool {
        self.copy_devices_as_files
    }

    /// Reports whether copying of special files has been requested.
    #[must_use]
    pub const fn specials_enabled(&self) -> bool {
        self.specials
    }
}
