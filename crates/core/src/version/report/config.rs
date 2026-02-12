use crate::builder_setter;
use crate::version::SecludedArgsMode;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::sync::OnceLock;

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
    /// Whether the mimalloc high-performance allocator is active.
    pub supports_mimalloc: bool,
    /// Whether `copy_file_range` zero-copy transfers are available (Linux).
    pub supports_copy_file_range: bool,
    /// Whether `io_uring` async I/O batching is available (Linux 5.6+).
    pub supports_io_uring: bool,
    /// Whether rayon-based parallel processing is enabled.
    pub supports_parallel: bool,
    /// Whether memory-mapped I/O is available.
    pub supports_mmap: bool,
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
            supports_hardlink_specials: cfg!(unix),
            supports_hardlink_symlinks: cfg!(unix),
            supports_ipv6: true,
            supports_atimes: true,
            supports_batchfiles: true,
            supports_inplace: true,
            supports_append: true,
            supports_acls: cfg!(feature = "acl"),
            supports_xattrs: cfg!(feature = "xattr"),
            secluded_args_mode: SecludedArgsMode::Optional,
            supports_iconv: cfg!(feature = "iconv"),
            supports_prealloc: true,
            supports_stop_at: true,
            supports_crtimes: false,
            supports_simd_roll: false,
            supports_asm_roll: false,
            supports_openssl_crypto: false,
            supports_asm_md5: false,
            supports_mimalloc: true,
            supports_copy_file_range: cfg!(target_os = "linux"),
            supports_io_uring: false,
            supports_parallel: true,
            supports_mmap: cfg!(unix),
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
        config.supports_socketpairs = socketpair_available();
        config.supports_simd_roll = checksums::simd_acceleration_available();
        config.supports_openssl_crypto = checksums::openssl_acceleration_available();
        config
    }
}

#[cfg(unix)]
fn socketpair_available() -> bool {
    static SOCKETPAIR_AVAILABLE: OnceLock<bool> = OnceLock::new();

    *SOCKETPAIR_AVAILABLE.get_or_init(|| match UnixStream::pair() {
        Ok((stream_a, stream_b)) => {
            drop(stream_a);
            drop(stream_b);
            true
        }
        Err(_) => false,
    })
}

#[cfg(not(unix))]
fn socketpair_available() -> bool {
    false
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
    supports_socketpairs: bool,
    supports_symlinks: bool,
    supports_symtimes: bool,
    supports_hardlinks: bool,
    supports_hardlink_specials: bool,
    supports_hardlink_symlinks: bool,
    supports_ipv6: bool,
    supports_atimes: bool,
    supports_batchfiles: bool,
    supports_inplace: bool,
    supports_append: bool,
    supports_acls: bool,
    supports_xattrs: bool,
    secluded_args_mode: SecludedArgsMode,
    supports_iconv: bool,
    supports_prealloc: bool,
    supports_stop_at: bool,
    supports_crtimes: bool,
    supports_simd_roll: bool,
    supports_asm_roll: bool,
    supports_openssl_crypto: bool,
    supports_asm_md5: bool,
    supports_mimalloc: bool,
    supports_copy_file_range: bool,
    supports_io_uring: bool,
    supports_parallel: bool,
    supports_mmap: bool,
}

impl VersionInfoConfigBuilder {
    /// Creates a builder initialised with [`VersionInfoConfig::new`].
    #[must_use]
    pub const fn new() -> Self {
        let config = VersionInfoConfig::new();
        Self {
            supports_socketpairs: config.supports_socketpairs,
            supports_symlinks: config.supports_symlinks,
            supports_symtimes: config.supports_symtimes,
            supports_hardlinks: config.supports_hardlinks,
            supports_hardlink_specials: config.supports_hardlink_specials,
            supports_hardlink_symlinks: config.supports_hardlink_symlinks,
            supports_ipv6: config.supports_ipv6,
            supports_atimes: config.supports_atimes,
            supports_batchfiles: config.supports_batchfiles,
            supports_inplace: config.supports_inplace,
            supports_append: config.supports_append,
            supports_acls: config.supports_acls,
            supports_xattrs: config.supports_xattrs,
            secluded_args_mode: config.secluded_args_mode,
            supports_iconv: config.supports_iconv,
            supports_prealloc: config.supports_prealloc,
            supports_stop_at: config.supports_stop_at,
            supports_crtimes: config.supports_crtimes,
            supports_simd_roll: config.supports_simd_roll,
            supports_asm_roll: config.supports_asm_roll,
            supports_openssl_crypto: config.supports_openssl_crypto,
            supports_asm_md5: config.supports_asm_md5,
            supports_mimalloc: config.supports_mimalloc,
            supports_copy_file_range: config.supports_copy_file_range,
            supports_io_uring: config.supports_io_uring,
            supports_parallel: config.supports_parallel,
            supports_mmap: config.supports_mmap,
        }
    }

    /// Creates a builder seeded with an existing configuration.
    #[must_use]
    pub const fn from_config(config: VersionInfoConfig) -> Self {
        Self {
            supports_socketpairs: config.supports_socketpairs,
            supports_symlinks: config.supports_symlinks,
            supports_symtimes: config.supports_symtimes,
            supports_hardlinks: config.supports_hardlinks,
            supports_hardlink_specials: config.supports_hardlink_specials,
            supports_hardlink_symlinks: config.supports_hardlink_symlinks,
            supports_ipv6: config.supports_ipv6,
            supports_atimes: config.supports_atimes,
            supports_batchfiles: config.supports_batchfiles,
            supports_inplace: config.supports_inplace,
            supports_append: config.supports_append,
            supports_acls: config.supports_acls,
            supports_xattrs: config.supports_xattrs,
            secluded_args_mode: config.secluded_args_mode,
            supports_iconv: config.supports_iconv,
            supports_prealloc: config.supports_prealloc,
            supports_stop_at: config.supports_stop_at,
            supports_crtimes: config.supports_crtimes,
            supports_simd_roll: config.supports_simd_roll,
            supports_asm_roll: config.supports_asm_roll,
            supports_openssl_crypto: config.supports_openssl_crypto,
            supports_asm_md5: config.supports_asm_md5,
            supports_mimalloc: config.supports_mimalloc,
            supports_copy_file_range: config.supports_copy_file_range,
            supports_io_uring: config.supports_io_uring,
            supports_parallel: config.supports_parallel,
            supports_mmap: config.supports_mmap,
        }
    }

    builder_setter! {
        /// Enables or disables socketpair support.
        supports_socketpairs: bool,
        /// Enables or disables symbolic link preservation.
        supports_symlinks: bool,
        /// Enables or disables symbolic link timestamp preservation.
        supports_symtimes: bool,
        /// Enables or disables hard link preservation.
        supports_hardlinks: bool,
        /// Enables or disables hard link support for special files.
        supports_hardlink_specials: bool,
        /// Enables or disables hard link support for symbolic links.
        supports_hardlink_symlinks: bool,
        /// Enables or disables IPv6 transport support.
        supports_ipv6: bool,
        /// Enables or disables access-time preservation.
        supports_atimes: bool,
        /// Enables or disables batch file support.
        supports_batchfiles: bool,
        /// Enables or disables in-place update support.
        supports_inplace: bool,
        /// Enables or disables append mode support.
        supports_append: bool,
    }

    /// Enables or disables ACL propagation, clamped to the compiled feature set.
    #[must_use]
    pub const fn supports_acls(mut self, enabled: bool) -> Self {
        self.supports_acls = enabled && cfg!(feature = "acl");
        self
    }

    /// Enables or disables extended attribute propagation, clamped to the
    /// compiled feature set.
    #[must_use]
    pub const fn supports_xattrs(mut self, enabled: bool) -> Self {
        self.supports_xattrs = enabled && cfg!(feature = "xattr");
        self
    }

    builder_setter! {
        /// Sets the advertised secluded-argument mode.
        secluded_args_mode: SecludedArgsMode,
    }

    /// Enables or disables iconv charset conversion, clamped to the compiled
    /// feature set.
    #[must_use]
    pub const fn supports_iconv(mut self, enabled: bool) -> Self {
        self.supports_iconv = enabled && cfg!(feature = "iconv");
        self
    }

    builder_setter! {
        /// Enables or disables preallocation support.
        supports_prealloc: bool,
        /// Enables or disables `--stop-at` style cut-off support.
        supports_stop_at: bool,
        /// Enables or disables change-time preservation.
        supports_crtimes: bool,
        /// Enables or disables SIMD-accelerated rolling checksums.
        supports_simd_roll: bool,
        /// Enables or disables assembly-accelerated rolling checksums.
        supports_asm_roll: bool,
        /// Enables or disables OpenSSL-backed cryptography support.
        supports_openssl_crypto: bool,
        /// Enables or disables assembly-accelerated MD5.
        supports_asm_md5: bool,
        /// Enables or disables the mimalloc allocator.
        supports_mimalloc: bool,
        /// Enables or disables copy_file_range zero-copy transfers.
        supports_copy_file_range: bool,
        /// Enables or disables io_uring async I/O batching.
        supports_io_uring: bool,
        /// Enables or disables rayon-based parallel processing.
        supports_parallel: bool,
        /// Enables or disables memory-mapped I/O.
        supports_mmap: bool,
    }

    /// Finalises the builder and returns the constructed configuration.
    #[must_use]
    pub const fn build(self) -> VersionInfoConfig {
        VersionInfoConfig {
            supports_socketpairs: self.supports_socketpairs,
            supports_symlinks: self.supports_symlinks,
            supports_symtimes: self.supports_symtimes,
            supports_hardlinks: self.supports_hardlinks,
            supports_hardlink_specials: self.supports_hardlink_specials,
            supports_hardlink_symlinks: self.supports_hardlink_symlinks,
            supports_ipv6: self.supports_ipv6,
            supports_atimes: self.supports_atimes,
            supports_batchfiles: self.supports_batchfiles,
            supports_inplace: self.supports_inplace,
            supports_append: self.supports_append,
            supports_acls: self.supports_acls,
            supports_xattrs: self.supports_xattrs,
            secluded_args_mode: self.secluded_args_mode,
            supports_iconv: self.supports_iconv,
            supports_prealloc: self.supports_prealloc,
            supports_stop_at: self.supports_stop_at,
            supports_crtimes: self.supports_crtimes,
            supports_simd_roll: self.supports_simd_roll,
            supports_asm_roll: self.supports_asm_roll,
            supports_openssl_crypto: self.supports_openssl_crypto,
            supports_asm_md5: self.supports_asm_md5,
            supports_mimalloc: self.supports_mimalloc,
            supports_copy_file_range: self.supports_copy_file_range,
            supports_io_uring: self.supports_io_uring,
            supports_parallel: self.supports_parallel,
            supports_mmap: self.supports_mmap,
        }
    }
}

impl Default for VersionInfoConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests for VersionInfoConfig::new
    #[test]
    fn new_creates_default_config() {
        let config = VersionInfoConfig::new();
        assert!(!config.supports_socketpairs);
        assert!(config.supports_ipv6);
        assert!(config.supports_atimes);
        assert!(config.supports_batchfiles);
        assert!(config.supports_inplace);
        assert!(config.supports_append);
        assert!(config.supports_prealloc);
        assert!(config.supports_stop_at);
        assert!(!config.supports_crtimes);
        assert!(!config.supports_simd_roll);
        assert!(!config.supports_asm_roll);
        assert!(!config.supports_openssl_crypto);
        assert!(!config.supports_asm_md5);
        assert!(config.supports_mimalloc);
        assert_eq!(config.supports_copy_file_range, cfg!(target_os = "linux"));
        assert!(!config.supports_io_uring);
        assert!(config.supports_parallel);
        assert_eq!(config.supports_mmap, cfg!(unix));
    }

    #[test]
    fn new_secluded_args_mode_is_optional() {
        let config = VersionInfoConfig::new();
        assert_eq!(config.secluded_args_mode, SecludedArgsMode::Optional);
    }

    // Tests for Default trait
    #[test]
    fn default_equals_new() {
        let default_config = VersionInfoConfig::default();
        let new_config = VersionInfoConfig::new();
        assert_eq!(default_config, new_config);
    }

    // Tests for builder
    #[test]
    fn builder_creates_same_as_new() {
        let builder_config = VersionInfoConfig::builder().build();
        let new_config = VersionInfoConfig::new();
        assert_eq!(builder_config, new_config);
    }

    #[test]
    fn builder_default_equals_new() {
        let default_builder = VersionInfoConfigBuilder::default();
        let new_builder = VersionInfoConfigBuilder::new();
        assert_eq!(default_builder, new_builder);
    }

    // Tests for builder setters
    #[test]
    fn builder_supports_socketpairs() {
        let config = VersionInfoConfig::builder()
            .supports_socketpairs(true)
            .build();
        assert!(config.supports_socketpairs);
    }

    #[test]
    fn builder_supports_symlinks() {
        let config = VersionInfoConfig::builder().supports_symlinks(true).build();
        assert!(config.supports_symlinks);
    }

    #[test]
    fn builder_supports_symtimes() {
        let config = VersionInfoConfig::builder().supports_symtimes(true).build();
        assert!(config.supports_symtimes);
    }

    #[test]
    fn builder_supports_hardlinks() {
        let config = VersionInfoConfig::builder()
            .supports_hardlinks(true)
            .build();
        assert!(config.supports_hardlinks);
    }

    #[test]
    fn builder_supports_hardlink_specials() {
        let config = VersionInfoConfig::builder()
            .supports_hardlink_specials(true)
            .build();
        assert!(config.supports_hardlink_specials);
    }

    #[test]
    fn builder_supports_hardlink_symlinks() {
        let config = VersionInfoConfig::builder()
            .supports_hardlink_symlinks(true)
            .build();
        assert!(config.supports_hardlink_symlinks);
    }

    #[test]
    fn builder_supports_ipv6_false() {
        let config = VersionInfoConfig::builder().supports_ipv6(false).build();
        assert!(!config.supports_ipv6);
    }

    #[test]
    fn builder_supports_atimes_false() {
        let config = VersionInfoConfig::builder().supports_atimes(false).build();
        assert!(!config.supports_atimes);
    }

    #[test]
    fn builder_supports_batchfiles_false() {
        let config = VersionInfoConfig::builder()
            .supports_batchfiles(false)
            .build();
        assert!(!config.supports_batchfiles);
    }

    #[test]
    fn builder_supports_inplace_false() {
        let config = VersionInfoConfig::builder().supports_inplace(false).build();
        assert!(!config.supports_inplace);
    }

    #[test]
    fn builder_supports_append_false() {
        let config = VersionInfoConfig::builder().supports_append(false).build();
        assert!(!config.supports_append);
    }

    #[test]
    fn builder_supports_prealloc_false() {
        let config = VersionInfoConfig::builder()
            .supports_prealloc(false)
            .build();
        assert!(!config.supports_prealloc);
    }

    #[test]
    fn builder_supports_stop_at_false() {
        let config = VersionInfoConfig::builder().supports_stop_at(false).build();
        assert!(!config.supports_stop_at);
    }

    #[test]
    fn builder_supports_crtimes() {
        let config = VersionInfoConfig::builder().supports_crtimes(true).build();
        assert!(config.supports_crtimes);
    }

    #[test]
    fn builder_supports_simd_roll() {
        let config = VersionInfoConfig::builder()
            .supports_simd_roll(true)
            .build();
        assert!(config.supports_simd_roll);
    }

    #[test]
    fn builder_supports_asm_roll() {
        let config = VersionInfoConfig::builder().supports_asm_roll(true).build();
        assert!(config.supports_asm_roll);
    }

    #[test]
    fn builder_supports_openssl_crypto() {
        let config = VersionInfoConfig::builder()
            .supports_openssl_crypto(true)
            .build();
        assert!(config.supports_openssl_crypto);
    }

    #[test]
    fn builder_supports_asm_md5() {
        let config = VersionInfoConfig::builder().supports_asm_md5(true).build();
        assert!(config.supports_asm_md5);
    }

    #[test]
    fn builder_supports_mimalloc_false() {
        let config = VersionInfoConfig::builder()
            .supports_mimalloc(false)
            .build();
        assert!(!config.supports_mimalloc);
    }

    #[test]
    fn builder_supports_copy_file_range() {
        let config = VersionInfoConfig::builder()
            .supports_copy_file_range(true)
            .build();
        assert!(config.supports_copy_file_range);
    }

    #[test]
    fn builder_supports_io_uring() {
        let config = VersionInfoConfig::builder().supports_io_uring(true).build();
        assert!(config.supports_io_uring);
    }

    #[test]
    fn builder_supports_parallel() {
        let config = VersionInfoConfig::builder().supports_parallel(true).build();
        assert!(config.supports_parallel);
    }

    #[test]
    fn builder_supports_mmap() {
        let config = VersionInfoConfig::builder().supports_mmap(true).build();
        assert!(config.supports_mmap);
    }

    #[test]
    fn builder_secluded_args_mode() {
        let config = VersionInfoConfig::builder()
            .secluded_args_mode(SecludedArgsMode::Default)
            .build();
        assert_eq!(config.secluded_args_mode, SecludedArgsMode::Default);
    }

    // Tests for to_builder
    #[test]
    fn to_builder_preserves_values() {
        let original = VersionInfoConfig::builder()
            .supports_socketpairs(true)
            .supports_simd_roll(true)
            .build();
        let rebuilt = original.to_builder().build();
        assert_eq!(original, rebuilt);
    }

    #[test]
    fn to_builder_allows_modification() {
        let original = VersionInfoConfig::new();
        let modified = original.to_builder().supports_crtimes(true).build();
        assert!(!original.supports_crtimes);
        assert!(modified.supports_crtimes);
    }

    // Tests for from_config
    #[test]
    fn from_config_preserves_values() {
        let config = VersionInfoConfig::builder()
            .supports_openssl_crypto(true)
            .build();
        let builder = VersionInfoConfigBuilder::from_config(config);
        assert_eq!(builder.build(), config);
    }

    // Tests for builder chaining
    #[test]
    fn builder_chaining_works() {
        let config = VersionInfoConfig::builder()
            .supports_socketpairs(true)
            .supports_ipv6(false)
            .supports_simd_roll(true)
            .supports_asm_md5(true)
            .build();
        assert!(config.supports_socketpairs);
        assert!(!config.supports_ipv6);
        assert!(config.supports_simd_roll);
        assert!(config.supports_asm_md5);
    }

    // Tests for trait implementations
    #[test]
    fn config_is_clone() {
        let config = VersionInfoConfig::new();
        let cloned = config;
        assert_eq!(config, cloned);
    }

    #[test]
    fn config_is_copy() {
        let config = VersionInfoConfig::new();
        let copied = config;
        assert_eq!(config, copied);
    }

    #[test]
    fn config_debug_is_not_empty() {
        let config = VersionInfoConfig::new();
        let debug = format!("{config:?}");
        assert!(!debug.is_empty());
        assert!(debug.contains("VersionInfoConfig"));
    }

    #[test]
    fn builder_is_clone() {
        let builder = VersionInfoConfigBuilder::new();
        let cloned = builder;
        assert_eq!(builder, cloned);
    }

    #[test]
    fn builder_is_copy() {
        let builder = VersionInfoConfigBuilder::new();
        let copied = builder;
        assert_eq!(builder, copied);
    }

    // Tests for with_runtime_capabilities
    #[test]
    fn with_runtime_capabilities_returns_config() {
        let config = VersionInfoConfig::with_runtime_capabilities();
        // Basic sanity checks - runtime values vary
        assert!(config.supports_ipv6);
        assert!(config.supports_atimes);
    }
}
