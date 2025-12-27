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

    /// Enables or disables preserving symlinks as symlinks.
    #[must_use]
    #[doc(alias = "--links")]
    #[doc(alias = "-l")]
    pub const fn links(mut self, preserve_links: bool) -> Self {
        self.preserve_symlinks = preserve_links;
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

    /// Enables or disables writing file data directly to device files.
    #[must_use]
    #[doc(alias = "--write-devices")]
    pub const fn write_devices(mut self, write_devices: bool) -> Self {
        self.write_devices = write_devices;
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

    /// Enables or disables symlink munging for daemon mode safety.
    ///
    /// When enabled, symlinks are transformed by prefixing a special marker
    /// so they cannot escape the module directory in daemon mode.
    #[must_use]
    #[doc(alias = "--munge-links")]
    pub const fn munge_links(mut self, munge_links: bool) -> Self {
        self.munge_links = munge_links;
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
    fn copy_links_sets_flag() {
        let config = builder().copy_links(true).build();
        assert!(config.copy_links());
    }

    #[test]
    fn copy_links_false_clears_flag() {
        let config = builder().copy_links(true).copy_links(false).build();
        assert!(!config.copy_links());
    }

    #[test]
    fn links_sets_flag() {
        let config = builder().links(true).build();
        assert!(config.links());
    }

    #[test]
    fn links_false_clears_flag() {
        let config = builder().links(true).links(false).build();
        assert!(!config.links());
    }

    #[test]
    fn copy_unsafe_links_sets_flag() {
        let config = builder().copy_unsafe_links(true).build();
        assert!(config.copy_unsafe_links());
    }

    #[test]
    fn copy_unsafe_links_false_clears_flag() {
        let config = builder()
            .copy_unsafe_links(true)
            .copy_unsafe_links(false)
            .build();
        assert!(!config.copy_unsafe_links());
    }

    #[test]
    fn copy_dirlinks_sets_flag() {
        let config = builder().copy_dirlinks(true).build();
        assert!(config.copy_dirlinks());
    }

    #[test]
    fn copy_dirlinks_false_clears_flag() {
        let config = builder().copy_dirlinks(true).copy_dirlinks(false).build();
        assert!(!config.copy_dirlinks());
    }

    #[test]
    fn copy_devices_sets_flag() {
        let config = builder().copy_devices(true).build();
        assert!(config.copy_devices());
    }

    #[test]
    fn copy_devices_false_clears_flag() {
        let config = builder().copy_devices(true).copy_devices(false).build();
        assert!(!config.copy_devices());
    }

    #[test]
    fn write_devices_sets_flag() {
        let config = builder().write_devices(true).build();
        assert!(config.write_devices());
    }

    #[test]
    fn write_devices_false_clears_flag() {
        let config = builder().write_devices(true).write_devices(false).build();
        assert!(!config.write_devices());
    }

    #[test]
    fn keep_dirlinks_sets_flag() {
        let config = builder().keep_dirlinks(true).build();
        assert!(config.keep_dirlinks());
    }

    #[test]
    fn keep_dirlinks_false_clears_flag() {
        let config = builder().keep_dirlinks(true).keep_dirlinks(false).build();
        assert!(!config.keep_dirlinks());
    }

    #[test]
    fn safe_links_sets_flag() {
        let config = builder().safe_links(true).build();
        assert!(config.safe_links());
    }

    #[test]
    fn safe_links_false_clears_flag() {
        let config = builder().safe_links(true).safe_links(false).build();
        assert!(!config.safe_links());
    }

    #[test]
    fn munge_links_sets_flag() {
        let config = builder().munge_links(true).build();
        assert!(config.munge_links());
    }

    #[test]
    fn munge_links_false_clears_flag() {
        let config = builder().munge_links(true).munge_links(false).build();
        assert!(!config.munge_links());
    }

    #[test]
    fn default_copy_links_is_false() {
        let config = builder().build();
        assert!(!config.copy_links());
    }

    #[test]
    fn default_links_is_false() {
        let config = builder().build();
        assert!(!config.links());
    }

    #[test]
    fn default_safe_links_is_false() {
        let config = builder().build();
        assert!(!config.safe_links());
    }

    #[test]
    fn default_munge_links_is_false() {
        let config = builder().build();
        assert!(!config.munge_links());
    }
}
