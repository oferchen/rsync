use crate::branding::Brand;
use libc::{ino_t, off_t, time_t};
use std::borrow::Cow;
use std::fmt::{self, Write as FmtWrite};
use std::mem;
use std::string::String;
use std::vec::Vec;

use super::constants::build_info_line;
use super::features::compiled_features_display;
use super::metadata::{VersionMetadata, version_metadata, version_metadata_for_program};
use super::secluded_args::SecludedArgsMode;

/// Configuration describing which capabilities the current build exposes.
///
/// The structure mirrors the feature toggles used by upstream `print_rsync_version()` when it
/// prints the capabilities and optimisation sections. Higher layers populate the fields based on
/// actual runtime support so `VersionInfoReport` can render an accurate report.
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
    /// Creates a configuration reflecting the currently implemented workspace capabilities.
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

    /// Converts the configuration into a builder so individual fields can be tweaked fluently.
    #[must_use]
    pub const fn to_builder(self) -> VersionInfoConfigBuilder {
        VersionInfoConfigBuilder::from_config(self)
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
/// [`VersionInfoConfig::new`] and provides chainable setters for each capability flag. It clamps
/// ACL, xattr, and iconv support to the compiled feature set so higher layers cannot misreport
/// unavailable functionality.
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

    /// Enables or disables extended attribute propagation, clamped to the compiled feature set.
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

    /// Enables or disables iconv charset conversion, clamped to the compiled feature set.
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

/// Human-readable `--version` output renderer.
///
/// Instances of this type use [`VersionMetadata`] together with [`VersionInfoConfig`] to reproduce
/// upstream rsync's capability report. Callers may override the checksum, compression, and daemon
/// authentication lists to match the negotiated feature set of the final binary. When rendering
/// banners for a different binary (for example, `rsyncd`), construct the report with
/// [`with_program_name`](Self::with_program_name) so the prologue reflects the appropriate binary
/// name while retaining all other metadata.
#[derive(Clone, Debug)]
pub struct VersionInfoReport {
    metadata: VersionMetadata,
    config: VersionInfoConfig,
    checksum_algorithms: Vec<Cow<'static, str>>,
    compress_algorithms: Vec<Cow<'static, str>>,
    daemon_auth_algorithms: Vec<Cow<'static, str>>,
}

impl Default for VersionInfoReport {
    fn default() -> Self {
        Self::new(VersionInfoConfig::default())
    }
}

impl VersionInfoReport {
    /// Creates a report using the supplied configuration and default algorithm lists.
    #[must_use]
    pub fn new(config: VersionInfoConfig) -> Self {
        Self::with_metadata(version_metadata(), config)
    }

    /// Creates a report using explicit version metadata and default algorithm lists.
    #[must_use]
    pub fn with_metadata(metadata: VersionMetadata, config: VersionInfoConfig) -> Self {
        Self {
            metadata,
            config,
            checksum_algorithms: default_checksum_algorithms(),
            compress_algorithms: default_compress_algorithms(),
            daemon_auth_algorithms: default_daemon_auth_algorithms(),
        }
    }

    /// Returns the configuration associated with the report.
    #[must_use]
    pub const fn config(&self) -> &VersionInfoConfig {
        &self.config
    }

    /// Returns the metadata associated with the report.
    #[must_use]
    pub const fn metadata(&self) -> VersionMetadata {
        self.metadata
    }

    /// Returns a report with the supplied program name.
    #[must_use]
    pub fn with_program_name(mut self, program_name: &'static str) -> Self {
        self.metadata = version_metadata_for_program(program_name);
        self
    }

    /// Returns a report using the client program name associated with `brand`.
    #[must_use]
    pub fn with_client_brand(self, brand: Brand) -> Self {
        self.with_program_name(brand.client_program_name())
    }

    /// Returns a report using the daemon program name associated with `brand`.
    #[must_use]
    pub fn with_daemon_brand(self, brand: Brand) -> Self {
        self.with_program_name(brand.daemon_program_name())
    }

    /// Replaces the checksum algorithm list used in the rendered report.
    #[must_use]
    pub fn with_checksum_algorithms<I, S>(mut self, algorithms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<Cow<'static, str>>,
    {
        self.checksum_algorithms = algorithms.into_iter().map(Into::into).collect();
        self
    }

    /// Replaces the compression algorithm list used in the rendered report.
    #[must_use]
    pub fn with_compress_algorithms<I, S>(mut self, algorithms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<Cow<'static, str>>,
    {
        self.compress_algorithms = algorithms.into_iter().map(Into::into).collect();
        self
    }

    /// Replaces the daemon authentication algorithm list used in the rendered report.
    #[must_use]
    pub fn with_daemon_auth_algorithms<I, S>(mut self, algorithms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<Cow<'static, str>>,
    {
        self.daemon_auth_algorithms = algorithms.into_iter().map(Into::into).collect();
        self
    }

    /// Writes the full human-readable `--version` output into the provided writer.
    pub fn write_human_readable<W: FmtWrite>(&self, writer: &mut W) -> fmt::Result {
        self.metadata.write_standard_banner(writer)?;
        self.write_info_sections(writer)?;
        self.write_named_list(writer, "Checksum list", &self.checksum_algorithms)?;
        self.write_named_list(writer, "Compress list", &self.compress_algorithms)?;
        self.write_named_list(writer, "Daemon auth list", &self.daemon_auth_algorithms)?;
        writer.write_char('\n')?;
        writer.write_str(
            "rsync comes with ABSOLUTELY NO WARRANTY.  This is free software, and you\n",
        )?;
        writer
            .write_str("are welcome to redistribute it under certain conditions.  See the GNU\n")?;
        writer.write_str("General Public Licence for details.\n")
    }

    /// Returns the rendered report as an owned string.
    #[must_use]
    pub fn human_readable(&self) -> String {
        let mut rendered = String::new();
        self.write_human_readable(&mut rendered)
            .expect("writing to String cannot fail");
        rendered
    }

    fn write_info_sections<W: FmtWrite>(&self, writer: &mut W) -> fmt::Result {
        let mut buffer = String::new();
        let mut items = self.info_items().into_iter().peekable();

        while let Some(item) = items.next() {
            match item {
                InfoItem::Section(name) => {
                    if !buffer.is_empty() {
                        writeln!(writer, "   {}", buffer)?;
                        buffer.clear();
                    }
                    writeln!(writer, "{}:", name)?;
                }
                InfoItem::Entry(text) => {
                    let needs_comma = matches!(items.peek(), Some(InfoItem::Entry(_)));
                    let mut formatted = String::with_capacity(text.len() + 3);
                    formatted.push(' ');
                    formatted.push_str(text.as_ref());
                    if needs_comma {
                        formatted.push(',');
                    }

                    if !buffer.is_empty() && buffer.len() + formatted.len() >= 75 {
                        writeln!(writer, "   {}", buffer)?;
                        buffer.clear();
                    }

                    buffer.push_str(&formatted);
                }
            }
        }

        if !buffer.is_empty() {
            writeln!(writer, "   {}", buffer)?;
        }

        Ok(())
    }

    fn write_named_list<W: FmtWrite>(
        &self,
        writer: &mut W,
        name: &str,
        entries: &[Cow<'static, str>],
    ) -> fmt::Result {
        writeln!(writer, "{}:", name)?;

        if entries.is_empty() {
            writeln!(writer, "    none")
        } else {
            writer.write_str("    ")?;
            for (index, entry) in entries.iter().enumerate() {
                if index > 0 {
                    writer.write_char(' ')?;
                }
                writer.write_str(entry.as_ref())?;
            }
            writer.write_char('\n')
        }
    }

    fn info_items(&self) -> Vec<InfoItem> {
        const BASE_CAPACITY: usize = 32;

        let config = self.config;
        let mut items = Vec::with_capacity(BASE_CAPACITY);

        items.push(InfoItem::Section("Capabilities"));
        items.push(bits_entry::<off_t>("files"));
        items.push(bits_entry::<ino_t>("inums"));
        items.push(bits_entry::<time_t>("timestamps"));
        items.push(bits_entry::<i64>("long ints"));
        items.push(capability_entry("socketpairs", config.supports_socketpairs));
        items.push(capability_entry("symlinks", config.supports_symlinks));
        items.push(capability_entry("symtimes", config.supports_symtimes));
        items.push(capability_entry("hardlinks", config.supports_hardlinks));
        items.push(capability_entry(
            "hardlink-specials",
            config.supports_hardlink_specials,
        ));
        items.push(capability_entry(
            "hardlink-symlinks",
            config.supports_hardlink_symlinks,
        ));
        items.push(capability_entry("IPv6", config.supports_ipv6));
        items.push(capability_entry("atimes", config.supports_atimes));
        items.push(capability_entry("batchfiles", config.supports_batchfiles));
        items.push(capability_entry("inplace", config.supports_inplace));
        items.push(capability_entry("append", config.supports_append));
        items.push(capability_entry("ACLs", config.supports_acls));
        items.push(capability_entry("xattrs", config.supports_xattrs));
        items.push(InfoItem::Entry(Cow::Borrowed(
            config.secluded_args_mode.label(),
        )));
        items.push(capability_entry("iconv", config.supports_iconv));
        items.push(capability_entry("prealloc", config.supports_prealloc));
        items.push(capability_entry("stop-at", config.supports_stop_at));
        items.push(capability_entry("crtimes", config.supports_crtimes));
        items.push(InfoItem::Section("Optimizations"));
        items.push(capability_entry("SIMD-roll", config.supports_simd_roll));
        items.push(capability_entry("asm-roll", config.supports_asm_roll));
        items.push(capability_entry(
            "openssl-crypto",
            config.supports_openssl_crypto,
        ));
        items.push(capability_entry("asm-MD5", config.supports_asm_md5));

        items.push(InfoItem::Section("Compiled features"));
        let compiled_features = compiled_features_display();
        if compiled_features.is_empty() {
            items.push(InfoItem::Entry(Cow::Borrowed("none")));
        } else {
            items.push(InfoItem::Entry(Cow::Owned(compiled_features.to_string())));
        }

        items.push(InfoItem::Section("Build info"));
        items.push(InfoItem::Entry(Cow::Owned(build_info_line())));

        debug_assert!(items.capacity() >= BASE_CAPACITY);
        items
    }
}

/// Returns the default checksum algorithm list rendered in `--version` output.
#[must_use]
pub(crate) fn default_checksum_algorithms() -> Vec<Cow<'static, str>> {
    vec![
        Cow::Borrowed("xxh128"),
        Cow::Borrowed("xxh3"),
        Cow::Borrowed("xxh64"),
        Cow::Borrowed("md5"),
        Cow::Borrowed("md4"),
        Cow::Borrowed("none"),
    ]
}

/// Returns the default compression algorithm list rendered in `--version` output.
#[must_use]
pub(crate) fn default_compress_algorithms() -> Vec<Cow<'static, str>> {
    let mut algorithms = Vec::new();

    if cfg!(feature = "zstd") {
        algorithms.push(Cow::Borrowed("zstd"));
    }

    algorithms.push(Cow::Borrowed("none"));
    algorithms
}

/// Returns the default daemon authentication algorithm list rendered in `--version` output.
#[must_use]
pub(crate) fn default_daemon_auth_algorithms() -> Vec<Cow<'static, str>> {
    vec![Cow::Borrowed("md5"), Cow::Borrowed("md4")]
}

#[derive(Clone, Debug)]
enum InfoItem {
    Section(&'static str),
    Entry(Cow<'static, str>),
}

fn bits_entry<T>(label: &'static str) -> InfoItem {
    let bits = mem::size_of::<T>() * 8;
    InfoItem::Entry(Cow::Owned(format!("{}-bit {}", bits, label)))
}

fn capability_entry(label: &'static str, supported: bool) -> InfoItem {
    if supported {
        InfoItem::Entry(Cow::Borrowed(label))
    } else {
        InfoItem::Entry(Cow::Owned(format!("no {}", label)))
    }
}
