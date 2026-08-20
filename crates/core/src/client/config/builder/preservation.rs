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

    /// Drops the ownership check on operator-supplied path symlinks.
    ///
    /// Local-only: upstream never forwards `--insecure-links` to a peer
    /// (`options.c:3068`) and a daemon hard-refuses one that arrives anyway
    /// (`options.c:1084`), so a client can never relax a server's confinement.
    #[must_use]
    #[doc(alias = "--insecure-links")]
    pub const fn insecure_links(mut self, insecure_links: bool) -> Self {
        self.insecure_links = insecure_links;
        self
    }

    /// Confines every operator- and peer-supplied path beneath `root`.
    ///
    /// The value is validated absolute at parse time
    /// (`options.c:2386-2389`) and is mutually exclusive with
    /// [`insecure_links`](Self::insecure_links) (`options.c:2391-2396`).
    #[must_use]
    #[doc(alias = "--confine-root")]
    pub fn confine_root(mut self, confine_root: Option<PathBuf>) -> Self {
        self.confine_root = confine_root;
        self
    }

    /// Enables or disables trusting the sender's file list without safety checks.
    ///
    /// When false (default), the receiver rejects file list entries with absolute
    /// paths or `..` components to prevent directory traversal. When true, these
    /// checks are skipped.
    #[must_use]
    #[doc(alias = "--trust-sender")]
    pub const fn trust_sender(mut self, trust_sender: bool) -> Self {
        self.trust_sender = trust_sender;
        self
    }
}
