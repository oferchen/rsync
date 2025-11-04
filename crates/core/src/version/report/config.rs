use crate::version::SecludedArgsMode;

/// Configuration describing which capabilities the current build exposes.
///
/// The structure mirrors the feature toggles used by upstream
/// `print_rsync_version()` when it prints the capabilities and optimisation
/// sections. Higher layers populate the fields based on actual runtime support
/// so [`VersionInfoReport`](super::VersionInfoReport) can render an accurate
/// report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VersionInfoConfig {
    /// Whether socketpair-based transports are available.
    pub supports_socketpairs: bool,
    /// Whether symbolic links are preserved.
    pub supports_symlinks: bool,
    /// Whether symbolic link timestamps are propagated.
    pub supports_symtimes: bool,
    /// Whether hard links are preserved.
    pub supports_hardlinks: bool,
    /// Whether hard links to special files are preserved.
    pub supports_hardlink_specials: bool,
    /// Whether hard links to symbolic links are preserved.
    pub supports_hardlink_symlinks: bool,
    /// Whether IPv6 transports are supported.
    pub supports_ipv6: bool,
    /// Whether access times are preserved.
    pub supports_atimes: bool,
    /// Whether batch file generation and replay are implemented.
    pub supports_batchfiles: bool,
    /// Whether in-place updates are supported.
    pub supports_inplace: bool,
    /// Whether append mode is supported.
    pub supports_append: bool,
    /// Whether POSIX ACL propagation is implemented.
    pub supports_acls: bool,
    /// Whether extended attribute propagation is implemented.
    pub supports_xattrs: bool,
    /// How secluded-argument support is advertised.
    pub secluded_args_mode: SecludedArgsMode,
    /// Whether iconv-based charset conversion is implemented.
    pub supports_iconv: bool,
    /// Whether preallocation is implemented.
    pub supports_prealloc: bool,
    /// Whether `--stop-at` style cut-offs are supported.
    pub supports_stop_at: bool,
    /// Whether change-time preservation is implemented.
    pub supports_crtimes: bool,
    /// Whether SIMD acceleration is used for the rolling checksum.
    pub supports_simd_roll: bool,
    /// Whether assembly acceleration is used for the rolling checksum.
    pub supports_asm_roll: bool,
    /// Whether OpenSSL-backed cryptography is available.
    pub supports_openssl_crypto: bool,
    /// Whether assembly acceleration is used for MD5.
    pub supports_asm_md5: bool,
}

impl VersionInfoConfig {
    /// Creates a configuration reflecting the currently implemented workspace
    /// capabilities.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            supports_socketpairs: false,
            supports_symlinks: cfg!(unix),
            supports_symtimes: cfg!(unix),
            supports_hardlinks: cfg!(unix),
            supports_hardlink_specials: false,
            supports_hardlink_symlinks: false,
            supports_ipv6: true,
            supports_atimes: true,
            supports_batchfiles: false,
            supports_inplace: true,
            supports_append: false,
            supports_acls: cfg!(feature = "acl"),
            supports_xattrs: cfg!(feature = "xattr"),
            secluded_args_mode: SecludedArgsMode::Optional,
            supports_iconv: cfg!(feature = "iconv"),
            supports_prealloc: true,
            supports_stop_at: false,
            supports_crtimes: false,
            supports_simd_roll: false,
            supports_asm_roll: false,
            supports_openssl_crypto: false,
            supports_asm_md5: false,
        }
    }

    /// Returns a builder for constructing customised capability configurations.
    ///
    /// The builder follows the fluent style used across the workspace, making it
    /// straightforward to toggle capabilities while reusing the compile-time
    /// defaults produced by [`VersionInfoConfig::new`]. Feature-gated entries
    /// (ACLs, xattrs, and iconv) are automatically clamped so callers cannot
    /// advertise support for capabilities that were not compiled in.
    #[must_use]
    pub const fn builder() -> VersionInfoConfigBuilder {
        VersionInfoConfigBuilder::new()
    }

    /// Converts the configuration into a builder so individual fields can be
    /// tweaked fluently.
    #[must_use]
    pub const fn to_builder(self) -> VersionInfoConfigBuilder {
        VersionInfoConfigBuilder::from_config(self)
    }

    /// Returns a configuration that reflects runtime-detected capabilities.
    ///
    /// The helper starts from [`VersionInfoConfig::new`] and toggles fields that
    /// depend on CPU feature detection (currently SIMD rolling checksums) so the
    /// rendered version report advertises the same optimisations that the
    /// transfer engine selects.
    #[must_use]
    pub fn with_runtime_capabilities() -> Self {
        let mut config = Self::new();
        config.supports_simd_roll = rsync_checksums::simd_acceleration_available();
        config
    }
}

impl Default for VersionInfoConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Fluent builder for [`VersionInfoConfig`].
///
/// The builder starts from the compile-time defaults exposed by
/// [`VersionInfoConfig::new`] and provides chainable setters for each
/// capability flag. It clamps ACL, xattr, and iconv support to the compiled
/// feature set so higher layers cannot misreport unavailable functionality.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VersionInfoConfigBuilder {
    config: VersionInfoConfig,
}

impl VersionInfoConfigBuilder {
    /// Creates a builder initialised with [`VersionInfoConfig::new`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            config: VersionInfoConfig::new(),
        }
    }

    /// Creates a builder seeded with an existing configuration.
    #[must_use]
    pub const fn from_config(config: VersionInfoConfig) -> Self {
        Self { config }
    }

    /// Enables or disables socketpair support.
    #[must_use]
    pub fn supports_socketpairs(mut self, enabled: bool) -> Self {
        self.config.supports_socketpairs = enabled;
        self
    }

    /// Enables or disables symbolic link preservation.
    #[must_use]
    pub fn supports_symlinks(mut self, enabled: bool) -> Self {
        self.config.supports_symlinks = enabled;
        self
    }

    /// Enables or disables symbolic link timestamp preservation.
    #[must_use]
    pub fn supports_symtimes(mut self, enabled: bool) -> Self {
        self.config.supports_symtimes = enabled;
        self
    }

    /// Enables or disables hard link preservation.
    #[must_use]
    pub fn supports_hardlinks(mut self, enabled: bool) -> Self {
        self.config.supports_hardlinks = enabled;
        self
    }

    /// Enables or disables hard link support for special files.
    #[must_use]
    pub fn supports_hardlink_specials(mut self, enabled: bool) -> Self {
        self.config.supports_hardlink_specials = enabled;
        self
    }

    /// Enables or disables hard link support for symbolic links.
    #[must_use]
    pub fn supports_hardlink_symlinks(mut self, enabled: bool) -> Self {
        self.config.supports_hardlink_symlinks = enabled;
        self
    }

    /// Enables or disables IPv6 transport support.
    #[must_use]
    pub fn supports_ipv6(mut self, enabled: bool) -> Self {
        self.config.supports_ipv6 = enabled;
        self
    }

    /// Enables or disables access-time preservation.
    #[must_use]
    pub fn supports_atimes(mut self, enabled: bool) -> Self {
        self.config.supports_atimes = enabled;
        self
    }

    /// Enables or disables batch file support.
    #[must_use]
    pub fn supports_batchfiles(mut self, enabled: bool) -> Self {
        self.config.supports_batchfiles = enabled;
        self
    }

    /// Enables or disables in-place update support.
    #[must_use]
    pub fn supports_inplace(mut self, enabled: bool) -> Self {
        self.config.supports_inplace = enabled;
        self
    }

    /// Enables or disables append mode support.
    #[must_use]
    pub fn supports_append(mut self, enabled: bool) -> Self {
        self.config.supports_append = enabled;
        self
    }

    /// Enables or disables ACL propagation, clamped to the compiled feature set.
    #[must_use]
    pub fn supports_acls(mut self, enabled: bool) -> Self {
        self.config.supports_acls = enabled && cfg!(feature = "acl");
        self
    }

    /// Enables or disables extended attribute propagation, clamped to the
    /// compiled feature set.
    #[must_use]
    pub fn supports_xattrs(mut self, enabled: bool) -> Self {
        self.config.supports_xattrs = enabled && cfg!(feature = "xattr");
        self
    }

    /// Sets the advertised secluded-argument mode.
    #[must_use]
    pub fn secluded_args_mode(mut self, mode: SecludedArgsMode) -> Self {
        self.config.secluded_args_mode = mode;
        self
    }

    /// Enables or disables iconv charset conversion, clamped to the compiled
    /// feature set.
    #[must_use]
    pub fn supports_iconv(mut self, enabled: bool) -> Self {
        self.config.supports_iconv = enabled && cfg!(feature = "iconv");
        self
    }

    /// Enables or disables preallocation support.
    #[must_use]
    pub fn supports_prealloc(mut self, enabled: bool) -> Self {
        self.config.supports_prealloc = enabled;
        self
    }

    /// Enables or disables `--stop-at` style cut-off support.
    #[must_use]
    pub fn supports_stop_at(mut self, enabled: bool) -> Self {
        self.config.supports_stop_at = enabled;
        self
    }

    /// Enables or disables change-time preservation.
    #[must_use]
    pub fn supports_crtimes(mut self, enabled: bool) -> Self {
        self.config.supports_crtimes = enabled;
        self
    }

    /// Enables or disables SIMD-accelerated rolling checksums.
    #[must_use]
    pub fn supports_simd_roll(mut self, enabled: bool) -> Self {
        self.config.supports_simd_roll = enabled;
        self
    }

    /// Enables or disables assembly-accelerated rolling checksums.
    #[must_use]
    pub fn supports_asm_roll(mut self, enabled: bool) -> Self {
        self.config.supports_asm_roll = enabled;
        self
    }

    /// Enables or disables OpenSSL-backed cryptography support.
    #[must_use]
    pub fn supports_openssl_crypto(mut self, enabled: bool) -> Self {
        self.config.supports_openssl_crypto = enabled;
        self
    }

    /// Enables or disables assembly-accelerated MD5.
    #[must_use]
    pub fn supports_asm_md5(mut self, enabled: bool) -> Self {
        self.config.supports_asm_md5 = enabled;
        self
    }

    /// Finalises the builder and returns the constructed configuration.
    #[must_use]
    pub const fn build(self) -> VersionInfoConfig {
        self.config
    }
}

impl Default for VersionInfoConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}
