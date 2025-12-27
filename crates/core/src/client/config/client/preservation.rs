use super::*;

impl ClientConfig {
    /// Reports whether symbolic links should be followed when copying files.
    #[must_use]
    #[doc(alias = "--copy-links")]
    #[doc(alias = "-L")]
    pub const fn copy_links(&self) -> bool {
        self.copy_links
    }

    /// Reports whether directory symlinks should be transformed into actual directories.
    #[must_use]
    #[doc(alias = "--copy-dirlinks")]
    pub const fn copy_dirlinks(&self) -> bool {
        self.copy_dirlinks
    }

    /// Reports whether device nodes should be copied as regular files.
    #[must_use]
    #[doc(alias = "--copy-devices")]
    pub const fn copy_devices(&self) -> bool {
        self.copy_devices
    }

    /// Reports whether file data should be written directly to device files.
    #[must_use]
    #[doc(alias = "--write-devices")]
    pub const fn write_devices(&self) -> bool {
        self.write_devices
    }

    /// Reports whether unsafe links should be copied rather than dereferenced.
    #[must_use]
    #[doc(alias = "--copy-unsafe-links")]
    pub const fn copy_unsafe_links(&self) -> bool {
        self.copy_unsafe_links
    }

    /// Returns whether existing destination directory symlinks should be preserved.
    #[must_use]
    #[doc(alias = "--keep-dirlinks")]
    pub const fn keep_dirlinks(&self) -> bool {
        self.keep_dirlinks
    }

    /// Reports whether unsafe symlinks should be ignored (`--safe-links`).
    #[must_use]
    #[doc(alias = "--safe-links")]
    pub const fn safe_links(&self) -> bool {
        self.safe_links
    }

    /// Reports whether symlinks should be munged for daemon mode safety.
    ///
    /// When enabled, symlinks are transformed by prefixing a special marker
    /// so they cannot escape the module directory in daemon mode. This is
    /// a security feature for rsyncd.
    #[must_use]
    #[doc(alias = "--munge-links")]
    pub const fn munge_links(&self) -> bool {
        self.munge_links
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> ClientConfig {
        ClientConfig::default()
    }

    // Tests for copy_links
    #[test]
    fn copy_links_default_is_false() {
        let config = default_config();
        assert!(!config.copy_links());
    }

    // Tests for copy_dirlinks
    #[test]
    fn copy_dirlinks_default_is_false() {
        let config = default_config();
        assert!(!config.copy_dirlinks());
    }

    // Tests for copy_devices
    #[test]
    fn copy_devices_default_is_false() {
        let config = default_config();
        assert!(!config.copy_devices());
    }

    // Tests for write_devices
    #[test]
    fn write_devices_default_is_false() {
        let config = default_config();
        assert!(!config.write_devices());
    }

    // Tests for copy_unsafe_links
    #[test]
    fn copy_unsafe_links_default_is_false() {
        let config = default_config();
        assert!(!config.copy_unsafe_links());
    }

    // Tests for keep_dirlinks
    #[test]
    fn keep_dirlinks_default_is_false() {
        let config = default_config();
        assert!(!config.keep_dirlinks());
    }

    // Tests for safe_links
    #[test]
    fn safe_links_default_is_false() {
        let config = default_config();
        assert!(!config.safe_links());
    }

    // Tests for munge_links
    #[test]
    fn munge_links_default_is_false() {
        let config = default_config();
        assert!(!config.munge_links());
    }
}
