use super::*;

impl ClientConfigBuilder {
    /// Enables or disables copying symlink referents.
    #[must_use]
    #[doc(alias = "--copy-links")]
    #[doc(alias = "-L")]
    pub const fn copy_links(mut self, copy_links: bool) -> Self {
        self.copy_links = copy_links;
        self
    }

    /// Enables or disables copying unsafe symlink referents.
    #[must_use]
    #[doc(alias = "--copy-unsafe-links")]
    pub const fn copy_unsafe_links(mut self, copy_unsafe_links: bool) -> Self {
        self.copy_unsafe_links = copy_unsafe_links;
        self
    }

    /// Enables treating symlinks that target directories as directories during traversal.
    #[must_use]
    #[doc(alias = "--copy-dirlinks")]
    #[doc(alias = "-k")]
    pub const fn copy_dirlinks(mut self, copy_dirlinks: bool) -> Self {
        self.copy_dirlinks = copy_dirlinks;
        self
    }

    /// Enables copying device contents as regular files.
    #[must_use]
    #[doc(alias = "--copy-devices")]
    pub const fn copy_devices(mut self, copy_devices: bool) -> Self {
        self.copy_devices = copy_devices;
        self
    }

    /// Preserves existing destination symlinks that refer to directories.
    #[must_use]
    #[doc(alias = "--keep-dirlinks")]
    pub const fn keep_dirlinks(mut self, keep_dirlinks: bool) -> Self {
        self.keep_dirlinks = keep_dirlinks;
        self
    }

    /// Enables or disables skipping unsafe symlinks.
    #[must_use]
    #[doc(alias = "--safe-links")]
    pub const fn safe_links(mut self, safe_links: bool) -> Self {
        self.safe_links = safe_links;
        self
    }
}
