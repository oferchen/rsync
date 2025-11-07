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
}
