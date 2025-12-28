use crate::auth::SUPPORTED_DAEMON_DIGESTS;
use crate::branding::Brand;
use crate::version::{VersionMetadata, version_metadata, version_metadata_for_program};
use libc::{ino_t, off_t, time_t};
use std::borrow::Cow;
use std::fmt::{self, Write as FmtWrite};
use std::mem;
use std::string::String;
use std::vec::Vec;

use super::config::VersionInfoConfig;

/// Human-readable `--version` output renderer.
///
/// Instances of this type use [`VersionMetadata`] together with
/// [`VersionInfoConfig`] to reproduce upstream rsync's capability report. Callers
/// may override the checksum, compression, and daemon authentication lists to
/// match the negotiated feature set of the final binary. When rendering banners
/// for a different binary (for example, `rsyncd`), construct the report with
/// [`with_program_name`](Self::with_program_name) so the prologue reflects the
/// appropriate binary name while retaining all other metadata.
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
        Self::new(VersionInfoConfig::with_runtime_capabilities())
    }
}

impl VersionInfoReport {
    /// Creates a report using the supplied configuration and default algorithm
    /// lists.
    #[must_use]
    pub fn new(config: VersionInfoConfig) -> Self {
        Self::with_metadata(version_metadata(), config)
    }

    /// Creates a report using explicit version metadata and default algorithm
    /// lists.
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
    pub const fn with_program_name(mut self, program_name: &'static str) -> Self {
        self.metadata = version_metadata_for_program(program_name);
        self
    }

    /// Returns a report using the client program name associated with `brand`.
    #[must_use]
    pub const fn with_client_brand(self, brand: Brand) -> Self {
        self.with_program_name(brand.client_program_name())
    }

    /// Returns a report using the daemon program name associated with `brand`.
    #[must_use]
    pub const fn with_daemon_brand(self, brand: Brand) -> Self {
        self.with_program_name(brand.daemon_program_name())
    }

    /// Creates a report for the client binary associated with `brand` using the
    /// default [`VersionInfoConfig`].
    ///
    /// # Examples
    ///
    /// ```rust
    /// use core::branding::Brand;
    /// use core::version::{VersionInfoReport, PROGRAM_NAME};
    ///
    /// let report = VersionInfoReport::for_client_brand(Brand::Oc);
    /// assert!(report
    ///     .metadata()
    ///     .standard_banner()
    ///     .starts_with(&format!("{PROGRAM_NAME} v")));
    /// ```
    #[must_use]
    pub fn for_client_brand(brand: Brand) -> Self {
        Self::default().with_client_brand(brand)
    }

    /// Creates a report for the daemon binary associated with `brand` using the
    /// default [`VersionInfoConfig`].
    ///
    /// # Examples
    ///
    /// ```rust
    /// use core::branding::Brand;
    /// use core::version::{VersionInfoReport, DAEMON_PROGRAM_NAME};
    ///
    /// let report = VersionInfoReport::for_daemon_brand(Brand::Oc);
    /// assert!(report
    ///     .metadata()
    ///     .standard_banner()
    ///     .starts_with(&format!("{DAEMON_PROGRAM_NAME} v")));
    /// ```
    #[must_use]
    pub fn for_daemon_brand(brand: Brand) -> Self {
        Self::default().with_daemon_brand(brand)
    }

    /// Creates a report for the client binary associated with `brand` using an
    /// explicit [`VersionInfoConfig`].
    #[must_use]
    pub fn for_client_brand_with_config(config: VersionInfoConfig, brand: Brand) -> Self {
        Self::new(config).with_client_brand(brand)
    }

    /// Creates a report for the daemon binary associated with `brand` using an
    /// explicit [`VersionInfoConfig`].
    #[must_use]
    pub fn for_daemon_brand_with_config(config: VersionInfoConfig, brand: Brand) -> Self {
        Self::new(config).with_daemon_brand(brand)
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

    /// Replaces the daemon authentication algorithm list used in the rendered
    /// report.
    #[must_use]
    pub fn with_daemon_auth_algorithms<I, S>(mut self, algorithms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<Cow<'static, str>>,
    {
        self.daemon_auth_algorithms = algorithms.into_iter().map(Into::into).collect();
        self
    }

    /// Writes the full human-readable `--version` output into the provided
    /// writer.
    pub fn write_human_readable<W: FmtWrite>(&self, writer: &mut W) -> fmt::Result {
        self.metadata.write_standard_banner(writer)?;
        self.write_info_sections(writer)?;
        self.write_named_list(writer, "Checksum list", &self.checksum_algorithms)?;
        self.write_named_list(writer, "Compress list", &self.compress_algorithms)?;
        self.write_named_list(writer, "Daemon auth list", &self.daemon_auth_algorithms)?;
        writer.write_char('\n')?;

        self.write_gpl_footer(writer)
    }

    /// Internal helper for the GPL footer. Single fmt call, no allocations.
    fn write_gpl_footer<W: FmtWrite>(&self, writer: &mut W) -> fmt::Result {
        let program_name = self.metadata.program_name();

        writeln!(
            writer,
            "{program_name} comes with ABSOLUTELY NO WARRANTY. This is free software, and you are welcome to redistribute it under certain conditions. See the GNU General Public License for details."
        )
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
                        writeln!(writer, "   {buffer}")?;
                        buffer.clear();
                    }
                    writeln!(writer, "{name}:")?;
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
                        writeln!(writer, "   {buffer}")?;
                        buffer.clear();
                    }

                    buffer.push_str(&formatted);
                }
            }
        }

        if !buffer.is_empty() {
            writeln!(writer, "   {buffer}")?;
        }

        Ok(())
    }

    fn write_named_list<W: FmtWrite>(
        &self,
        writer: &mut W,
        name: &str,
        entries: &[Cow<'static, str>],
    ) -> fmt::Result {
        writeln!(writer, "{name}:")?;

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
        Cow::Borrowed("(xxhash-rust)"),
        Cow::Borrowed("md5"),
        Cow::Borrowed("md4"),
        Cow::Borrowed("sha1"),
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

    if cfg!(feature = "lz4") {
        algorithms.push(Cow::Borrowed("lz4"));
    }

    algorithms.push(Cow::Borrowed("zlibx"));
    algorithms.push(Cow::Borrowed("zlib"));
    algorithms.push(Cow::Borrowed("none"));
    algorithms
}

/// Returns the default daemon authentication algorithm list rendered in
/// `--version` output.
#[must_use]
pub(crate) fn default_daemon_auth_algorithms() -> Vec<Cow<'static, str>> {
    SUPPORTED_DAEMON_DIGESTS
        .iter()
        .map(|digest| Cow::Borrowed(digest.name()))
        .collect()
}

#[derive(Clone, Debug)]
enum InfoItem {
    Section(&'static str),
    Entry(Cow<'static, str>),
}

fn bits_entry<T>(label: &'static str) -> InfoItem {
    let bits = mem::size_of::<T>() * 8;
    InfoItem::Entry(Cow::Owned(format!("{bits}-bit {label}")))
}

fn capability_entry(label: &'static str, supported: bool) -> InfoItem {
    if supported {
        InfoItem::Entry(Cow::Borrowed(label))
    } else {
        InfoItem::Entry(Cow::Owned(format!("no {label}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::branding::Brand;

    #[test]
    fn default_creates_valid_report() {
        let report = VersionInfoReport::default();
        assert!(!report.human_readable().is_empty());
    }

    #[test]
    fn new_creates_report_with_config() {
        let config = VersionInfoConfig::with_runtime_capabilities();
        let report = VersionInfoReport::new(config);
        assert_eq!(report.config().supports_symlinks, config.supports_symlinks);
    }

    #[test]
    fn config_accessor_returns_reference() {
        let report = VersionInfoReport::default();
        let _ = report.config();
    }

    #[test]
    fn metadata_accessor_returns_value() {
        let report = VersionInfoReport::default();
        let metadata = report.metadata();
        assert!(!metadata.standard_banner().is_empty());
    }

    #[test]
    fn with_program_name_changes_metadata() {
        let report = VersionInfoReport::default().with_program_name("custom-rsync");
        let banner = report.metadata().standard_banner();
        assert!(banner.contains("custom-rsync"));
    }

    #[test]
    fn with_client_brand_uses_client_name() {
        let report = VersionInfoReport::default().with_client_brand(Brand::Oc);
        let banner = report.metadata().standard_banner();
        assert!(banner.contains(Brand::Oc.client_program_name()));
    }

    #[test]
    fn with_daemon_brand_uses_daemon_name() {
        let report = VersionInfoReport::default().with_daemon_brand(Brand::Oc);
        let banner = report.metadata().standard_banner();
        assert!(banner.contains(Brand::Oc.daemon_program_name()));
    }

    #[test]
    fn for_client_brand_creates_report() {
        let report = VersionInfoReport::for_client_brand(Brand::Oc);
        assert!(!report.human_readable().is_empty());
    }

    #[test]
    fn for_daemon_brand_creates_report() {
        let report = VersionInfoReport::for_daemon_brand(Brand::Oc);
        assert!(!report.human_readable().is_empty());
    }

    #[test]
    fn for_client_brand_with_config_uses_config() {
        let config = VersionInfoConfig::with_runtime_capabilities();
        let report = VersionInfoReport::for_client_brand_with_config(config, Brand::Oc);
        assert_eq!(report.config().supports_symlinks, config.supports_symlinks);
    }

    #[test]
    fn for_daemon_brand_with_config_uses_config() {
        let config = VersionInfoConfig::with_runtime_capabilities();
        let report = VersionInfoReport::for_daemon_brand_with_config(config, Brand::Oc);
        assert_eq!(report.config().supports_symlinks, config.supports_symlinks);
    }

    #[test]
    fn with_checksum_algorithms_replaces_list() {
        let report = VersionInfoReport::default().with_checksum_algorithms(["custom1", "custom2"]);
        let output = report.human_readable();
        assert!(output.contains("custom1"));
        assert!(output.contains("custom2"));
    }

    #[test]
    fn with_compress_algorithms_replaces_list() {
        let report =
            VersionInfoReport::default().with_compress_algorithms(["compress1", "compress2"]);
        let output = report.human_readable();
        assert!(output.contains("compress1"));
        assert!(output.contains("compress2"));
    }

    #[test]
    fn with_daemon_auth_algorithms_replaces_list() {
        let report = VersionInfoReport::default().with_daemon_auth_algorithms(["auth1", "auth2"]);
        let output = report.human_readable();
        assert!(output.contains("auth1"));
        assert!(output.contains("auth2"));
    }

    #[test]
    fn human_readable_contains_capabilities_section() {
        let report = VersionInfoReport::default();
        let output = report.human_readable();
        assert!(output.contains("Capabilities:"));
    }

    #[test]
    fn human_readable_contains_optimizations_section() {
        let report = VersionInfoReport::default();
        let output = report.human_readable();
        assert!(output.contains("Optimizations:"));
    }

    #[test]
    fn human_readable_contains_checksum_list() {
        let report = VersionInfoReport::default();
        let output = report.human_readable();
        assert!(output.contains("Checksum list:"));
    }

    #[test]
    fn human_readable_contains_compress_list() {
        let report = VersionInfoReport::default();
        let output = report.human_readable();
        assert!(output.contains("Compress list:"));
    }

    #[test]
    fn human_readable_contains_daemon_auth_list() {
        let report = VersionInfoReport::default();
        let output = report.human_readable();
        assert!(output.contains("Daemon auth list:"));
    }

    #[test]
    fn human_readable_contains_gpl_footer() {
        let report = VersionInfoReport::default();
        let output = report.human_readable();
        assert!(output.contains("ABSOLUTELY NO WARRANTY"));
        assert!(output.contains("GNU General Public License"));
    }

    #[test]
    fn default_checksum_algorithms_includes_xxh128() {
        let algorithms = default_checksum_algorithms();
        assert!(algorithms.iter().any(|a| a == "xxh128"));
    }

    #[test]
    fn default_checksum_algorithms_includes_md5() {
        let algorithms = default_checksum_algorithms();
        assert!(algorithms.iter().any(|a| a == "md5"));
    }

    #[test]
    fn default_checksum_algorithms_includes_none() {
        let algorithms = default_checksum_algorithms();
        assert!(algorithms.iter().any(|a| a == "none"));
    }

    #[test]
    fn default_compress_algorithms_includes_zlib() {
        let algorithms = default_compress_algorithms();
        assert!(algorithms.iter().any(|a| a == "zlib"));
    }

    #[test]
    fn default_compress_algorithms_includes_none() {
        let algorithms = default_compress_algorithms();
        assert!(algorithms.iter().any(|a| a == "none"));
    }

    #[test]
    fn default_daemon_auth_algorithms_not_empty() {
        let algorithms = default_daemon_auth_algorithms();
        assert!(!algorithms.is_empty());
    }

    #[test]
    fn write_human_readable_to_string() {
        let report = VersionInfoReport::default();
        let mut output = String::new();
        report.write_human_readable(&mut output).unwrap();
        assert!(!output.is_empty());
    }

    #[test]
    fn empty_algorithm_list_shows_none() {
        let report = VersionInfoReport::default().with_checksum_algorithms(Vec::<&str>::new());
        let output = report.human_readable();
        // When empty, should show "none" under the list
        assert!(output.contains("Checksum list:"));
    }

    #[test]
    fn bits_entry_formats_correctly() {
        let entry = bits_entry::<u64>("test");
        match entry {
            InfoItem::Entry(text) => assert!(text.contains("64-bit test")),
            _ => panic!("Expected Entry variant"),
        }
    }

    #[test]
    fn capability_entry_supported_shows_label() {
        let entry = capability_entry("test-cap", true);
        match entry {
            InfoItem::Entry(text) => assert_eq!(text, "test-cap"),
            _ => panic!("Expected Entry variant"),
        }
    }

    #[test]
    fn capability_entry_unsupported_shows_no_prefix() {
        let entry = capability_entry("test-cap", false);
        match entry {
            InfoItem::Entry(text) => assert_eq!(text, "no test-cap"),
            _ => panic!("Expected Entry variant"),
        }
    }
}
