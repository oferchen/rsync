use crate::auth::SUPPORTED_DAEMON_DIGESTS;
use crate::branding::Brand;
use crate::version::{VersionMetadata, version_metadata, version_metadata_for_program};
use libc::{ino_t, off_t};
use std::borrow::Cow;

// libc::time_t is deprecated on musl targets (musl 1.2.0+ uses 64-bit time_t).
// Provide a platform-safe alias: i64 on musl, libc::time_t elsewhere.
#[cfg(target_env = "musl")]
pub(crate) type TimeT = i64;
#[cfg(not(target_env = "musl"))]
pub(crate) type TimeT = libc::time_t;
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
        self.write_platform_io(writer)?;
        self.write_io_uring_detail(writer)?;
        self.write_named_list(writer, "Checksum list", &self.checksum_algorithms)?;
        self.write_named_list(writer, "Compress list", &self.compress_algorithms)?;
        self.write_named_list(writer, "Daemon auth list", &self.daemon_auth_algorithms)?;
        self.write_named_list(writer, "Build features", &compiled_build_features())?;
        writer.write_char('\n')?;

        self.write_gpl_footer(writer)
    }

    /// Writes the "Platform I/O" section listing runtime-detected fast I/O paths.
    fn write_platform_io<W: FmtWrite>(&self, writer: &mut W) -> fmt::Result {
        let caps = fast_io::platform_io_capabilities();
        if caps.is_empty() {
            return Ok(());
        }
        write!(writer, "Platform I/O:")?;
        for (i, cap) in caps.iter().enumerate() {
            if i > 0 {
                writer.write_char(',')?;
            }
            writer.write_char(' ')?;
            writer.write_str(cap)?;
        }
        writer.write_char('\n')
    }

    /// Writes the io_uring availability detail line.
    fn write_io_uring_detail<W: FmtWrite>(&self, writer: &mut W) -> fmt::Result {
        let detail = fast_io::io_uring_status_detail();
        writeln!(writer, "io_uring: {detail}")
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

    /// Writes the JSON capability report into the provided writer.
    ///
    /// This mirrors upstream rsync's `-VV` output (`print_rsync_version(FNONE)`):
    /// a JSON object with program metadata, nested `capabilities` and
    /// `optimizations` objects containing boolean/numeric/string values,
    /// algorithm lists as arrays, and closing `license`/`caveat` strings.
    pub fn write_json<W: FmtWrite>(&self, writer: &mut W) -> fmt::Result {
        // upstream: usage.c - json_line() macro opens with '{' on first line
        let meta = &self.metadata;
        let protocol = format!(
            "{}.{}",
            meta.protocol_version().as_u8(),
            meta.subprotocol_version()
        );

        write!(writer, "{{\n")?;
        write!(writer, "  \"program\": \"{}\",\n", meta.program_name())?;
        write!(writer, "  \"version\": \"{}\",\n", meta.rust_version())?;
        write!(writer, "  \"protocol\": \"{protocol}\",\n")?;
        // upstream: json_line("copyright", copyright) where copyright is just
        // "(C) YYYY-YYYY by ..." without a "Copyright" prefix
        write!(
            writer,
            "  \"copyright\": \"{}\",\n",
            meta.copyright_notice()
        )?;
        write!(writer, "  \"url\": \"{}\"", meta.source_url())?;

        // Capabilities section
        // upstream: print_info_flags(FNONE) outputs nested JSON objects
        self.write_json_info_flags(writer)?;

        // Algorithm lists
        // upstream: output_nno_list(FNONE, ...) outputs JSON arrays
        self.write_json_algorithm_list(writer, "checksum_list", &self.checksum_algorithms)?;
        self.write_json_algorithm_list(writer, "compress_list", &self.compress_algorithms)?;
        self.write_json_algorithm_list(writer, "daemon_auth_list", &self.daemon_auth_algorithms)?;

        // Closing metadata
        // upstream: usage.c - json_line("license", "GPLv3")
        write!(writer, ",\n  \"license\": \"GPLv3\"")?;
        write!(
            writer,
            ",\n  \"caveat\": \"rsync comes with ABSOLUTELY NO WARRANTY\""
        )?;
        write!(writer, "\n}}\n")?;
        Ok(())
    }

    /// Writes the capabilities and optimizations sections as nested JSON objects.
    ///
    /// Mirrors upstream `print_info_flags(FNONE)` which converts each info flag
    /// into a JSON key-value pair with underscores replacing spaces/hyphens and
    /// lowercase keys.
    fn write_json_info_flags<W: FmtWrite>(&self, writer: &mut W) -> fmt::Result {
        let config = self.config;

        // upstream: "*Capabilities" becomes "capabilities": { ... }
        write!(writer, ",\n  \"capabilities\": {{\n")?;

        let file_bits = mem::size_of::<off_t>() * 8;
        let inum_bits = mem::size_of::<ino_t>() * 8;
        let timestamp_bits = mem::size_of::<TimeT>() * 8;
        let long_int_bits = mem::size_of::<i64>() * 8;

        // upstream: "64-bit files" -> "file_bits": 64
        // The upstream code converts "N-bit label" to "label_bits": N
        write!(writer, "    \"file_bits\": {file_bits},\n")?;
        write!(writer, "    \"inum_bits\": {inum_bits},\n")?;
        write!(writer, "    \"timestamp_bits\": {timestamp_bits},\n")?;
        write!(writer, "    \"long_int_bits\": {long_int_bits},\n")?;

        // Boolean capabilities
        // upstream: "socketpairs" -> true, "no socketpairs" -> false
        write!(
            writer,
            "    \"socketpairs\": {},\n",
            config.supports_socketpairs
        )?;
        write!(writer, "    \"symlinks\": {},\n", config.supports_symlinks)?;
        write!(writer, "    \"symtimes\": {},\n", config.supports_symtimes)?;
        write!(
            writer,
            "    \"hardlinks\": {},\n",
            config.supports_hardlinks
        )?;
        // upstream: "hardlink-specials" -> "hardlink_specials"
        write!(
            writer,
            "    \"hardlink_specials\": {},\n",
            config.supports_hardlink_specials
        )?;
        write!(
            writer,
            "    \"hardlink_symlinks\": {},\n",
            config.supports_hardlink_symlinks
        )?;
        // upstream: "IPv6" -> "IPv6" (uppercase preserved, hyphens -> underscores)
        write!(writer, "    \"IPv6\": {},\n", config.supports_ipv6)?;
        write!(writer, "    \"atimes\": {},\n", config.supports_atimes)?;
        write!(
            writer,
            "    \"batchfiles\": {},\n",
            config.supports_batchfiles
        )?;
        write!(writer, "    \"inplace\": {},\n", config.supports_inplace)?;
        write!(writer, "    \"append\": {},\n", config.supports_append)?;
        write!(writer, "    \"ACLs\": {},\n", config.supports_acls)?;
        write!(writer, "    \"xattrs\": {},\n", config.supports_xattrs)?;
        // upstream: "optional secluded-args" -> "secluded_args": "optional"
        let secluded_value = match config.secluded_args_mode {
            super::super::SecludedArgsMode::Optional => "optional",
            super::super::SecludedArgsMode::Default => "default",
        };
        write!(writer, "    \"secluded_args\": \"{secluded_value}\",\n")?;
        write!(writer, "    \"iconv\": {},\n", config.supports_iconv)?;
        write!(writer, "    \"prealloc\": {},\n", config.supports_prealloc)?;
        write!(writer, "    \"stop_at\": {},\n", config.supports_stop_at)?;
        write!(writer, "    \"crtimes\": {}\n", config.supports_crtimes)?;
        write!(writer, "  }}")?;

        // upstream: "*Optimizations" becomes "optimizations": { ... }
        write!(writer, ",\n  \"optimizations\": {{\n")?;
        write!(
            writer,
            "    \"SIMD_roll\": {},\n",
            config.supports_simd_roll
        )?;
        write!(writer, "    \"asm_roll\": {},\n", config.supports_asm_roll)?;
        write!(
            writer,
            "    \"openssl_crypto\": {},\n",
            config.supports_openssl_crypto
        )?;
        write!(writer, "    \"asm_MD5\": {},\n", config.supports_asm_md5)?;
        // oc-rsync-specific optimizations (not in upstream, but present in config)
        write!(writer, "    \"mimalloc\": {},\n", config.supports_mimalloc)?;
        write!(
            writer,
            "    \"copy_file_range\": {},\n",
            config.supports_copy_file_range
        )?;
        write!(writer, "    \"io_uring\": {},\n", config.supports_io_uring)?;
        write!(writer, "    \"parallel\": {},\n", config.supports_parallel)?;
        write!(writer, "    \"mmap\": {}\n", config.supports_mmap)?;
        write!(writer, "  }}")?;

        Ok(())
    }

    /// Writes a named algorithm list as a JSON array.
    ///
    /// Mirrors upstream `output_nno_list(FNONE, ...)` which outputs:
    /// `"checksum_list": [ "xxh128", "md5", ... ]`
    fn write_json_algorithm_list<W: FmtWrite>(
        &self,
        writer: &mut W,
        name: &str,
        entries: &[Cow<'static, str>],
    ) -> fmt::Result {
        write!(writer, ",\n  \"{name}\": [\n   ")?;
        // upstream: skip entries starting with '(' (aliases)
        let filtered: Vec<&str> = entries
            .iter()
            .map(|e| e.as_ref())
            .filter(|e| !e.starts_with('('))
            .collect();
        for (i, entry) in filtered.iter().enumerate() {
            if i > 0 {
                write!(writer, ",")?;
            }
            write!(writer, " \"{entry}\"")?;
        }
        write!(writer, "\n  ]")?;
        Ok(())
    }

    /// Returns the JSON capability report as an owned string.
    ///
    /// This is the `-VV` equivalent of [`human_readable`](Self::human_readable).
    #[must_use]
    pub fn json(&self) -> String {
        let mut rendered = String::new();
        self.write_json(&mut rendered)
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
        const BASE_CAPACITY: usize = 40;

        let config = self.config;
        let mut items = Vec::with_capacity(BASE_CAPACITY);

        items.push(InfoItem::Section("Capabilities"));
        items.push(bits_entry::<off_t>("files"));
        items.push(bits_entry::<ino_t>("inums"));
        items.push(bits_entry::<TimeT>("timestamps"));
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
        items.push(capability_entry("mimalloc", config.supports_mimalloc));
        items.push(capability_entry(
            "copy-file-range",
            config.supports_copy_file_range,
        ));
        items.push(capability_entry("io-uring", config.supports_io_uring));
        items.push(capability_entry("parallel", config.supports_parallel));
        items.push(capability_entry("mmap", config.supports_mmap));

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

/// Returns the list of Cargo features the binary was compiled with.
///
/// Each entry is the canonical Cargo feature name as declared in the
/// workspace `Cargo.toml`. Only features visible to the `core` crate
/// (compression, ACL, xattr, iconv, async, embedded SSH, zlib-ng) are
/// detectable here; bin-only features that do not propagate into `core`
/// (`parallel`, `io_uring`, `iocp`, `copy_file_range`, `openssl`,
/// `openssl-vendored`, `mmap-free-basis`, `sd-notify`, `mimalloc`) are
/// surfaced via the existing `Capabilities` / `Optimizations` sections
/// and the `Platform I/O` line above this one.
#[must_use]
pub(crate) fn compiled_build_features() -> Vec<Cow<'static, str>> {
    let mut features: Vec<Cow<'static, str>> = Vec::new();

    if cfg!(feature = "zstd") {
        features.push(Cow::Borrowed("zstd"));
    }
    if cfg!(feature = "lz4") {
        features.push(Cow::Borrowed("lz4"));
    }
    if cfg!(feature = "zlib-ng") {
        features.push(Cow::Borrowed("zlib-ng"));
    }
    if cfg!(feature = "acl") {
        features.push(Cow::Borrowed("acl"));
    }
    if cfg!(feature = "xattr") {
        features.push(Cow::Borrowed("xattr"));
    }
    if cfg!(feature = "iconv") {
        features.push(Cow::Borrowed("iconv"));
    }
    if cfg!(feature = "async") {
        features.push(Cow::Borrowed("async"));
    }
    if cfg!(feature = "embedded-ssh") {
        features.push(Cow::Borrowed("embedded-ssh"));
    }

    features
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
    fn human_readable_contains_platform_io_on_supported_platforms() {
        let report = VersionInfoReport::default();
        let output = report.human_readable();
        let caps = fast_io::platform_io_capabilities();
        if caps.is_empty() {
            assert!(!output.contains("Platform I/O:"));
        } else {
            assert!(output.contains("Platform I/O:"));
        }
    }

    #[test]
    fn human_readable_contains_io_uring_detail() {
        let report = VersionInfoReport::default();
        let output = report.human_readable();
        assert!(output.contains("io_uring:"));
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
    fn default_checksum_algorithms_excludes_none() {
        let algorithms = default_checksum_algorithms();
        assert!(!algorithms.iter().any(|a| a == "none"));
    }

    #[test]
    fn default_compress_algorithms_includes_zlib() {
        let algorithms = default_compress_algorithms();
        assert!(algorithms.iter().any(|a| a == "zlib"));
    }

    #[test]
    fn default_compress_algorithms_excludes_none() {
        let algorithms = default_compress_algorithms();
        assert!(!algorithms.iter().any(|a| a == "none"));
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

    #[test]
    fn json_returns_non_empty_string() {
        let report = VersionInfoReport::default();
        let output = report.json();
        assert!(!output.is_empty());
    }

    #[test]
    fn json_starts_with_open_brace() {
        let report = VersionInfoReport::default();
        let output = report.json();
        assert!(output.starts_with('{'));
    }

    #[test]
    fn json_ends_with_close_brace_newline() {
        let report = VersionInfoReport::default();
        let output = report.json();
        assert!(output.ends_with("}\n"));
    }

    #[test]
    fn json_contains_program_key() {
        let report = VersionInfoReport::for_client_brand(Brand::Upstream);
        let output = report.json();
        assert!(output.contains("\"program\""));
        assert!(output.contains(Brand::Upstream.client_program_name()));
    }

    #[test]
    fn json_contains_version_key() {
        let report = VersionInfoReport::default();
        let output = report.json();
        assert!(output.contains("\"version\""));
    }

    #[test]
    fn json_contains_protocol_key() {
        let report = VersionInfoReport::default();
        let output = report.json();
        assert!(output.contains("\"protocol\""));
    }

    #[test]
    fn json_contains_capabilities_section() {
        let report = VersionInfoReport::default();
        let output = report.json();
        assert!(output.contains("\"capabilities\""));
    }

    #[test]
    fn json_contains_optimizations_section() {
        let report = VersionInfoReport::default();
        let output = report.json();
        assert!(output.contains("\"optimizations\""));
    }

    #[test]
    fn json_contains_atimes_key() {
        let report = VersionInfoReport::default();
        let output = report.json();
        assert!(output.contains("\"atimes\""));
    }

    #[test]
    fn json_contains_crtimes_key() {
        let report = VersionInfoReport::default();
        let output = report.json();
        assert!(output.contains("\"crtimes\""));
    }

    #[test]
    fn json_contains_acls_key() {
        let report = VersionInfoReport::default();
        let output = report.json();
        assert!(output.contains("\"ACLs\""));
    }

    #[test]
    fn json_contains_xattrs_key() {
        let report = VersionInfoReport::default();
        let output = report.json();
        assert!(output.contains("\"xattrs\""));
    }

    #[test]
    fn json_contains_license() {
        let report = VersionInfoReport::default();
        let output = report.json();
        assert!(output.contains("\"license\": \"GPLv3\""));
    }

    #[test]
    fn json_contains_checksum_list() {
        let report = VersionInfoReport::default();
        let output = report.json();
        assert!(output.contains("\"checksum_list\""));
    }

    #[test]
    fn json_contains_compress_list() {
        let report = VersionInfoReport::default();
        let output = report.json();
        assert!(output.contains("\"compress_list\""));
    }

    #[test]
    fn json_contains_daemon_auth_list() {
        let report = VersionInfoReport::default();
        let output = report.json();
        assert!(output.contains("\"daemon_auth_list\""));
    }

    #[test]
    fn json_atimes_reflects_config() {
        let config = VersionInfoConfig::builder().supports_atimes(true).build();
        let report = VersionInfoReport::new(config);
        let output = report.json();
        assert!(output.contains("\"atimes\": true"));
    }

    #[test]
    fn json_atimes_false_reflects_config() {
        let config = VersionInfoConfig::builder().supports_atimes(false).build();
        let report = VersionInfoReport::new(config);
        let output = report.json();
        assert!(output.contains("\"atimes\": false"));
    }

    #[test]
    fn json_skips_parenthesized_checksum_aliases() {
        let report = VersionInfoReport::default();
        let output = report.json();
        // The default checksum list includes "(xxhash-rust)" which should be skipped
        assert!(!output.contains("(xxhash-rust)"));
    }

    #[test]
    fn write_json_to_string() {
        let report = VersionInfoReport::default();
        let mut output = String::new();
        report.write_json(&mut output).unwrap();
        assert!(!output.is_empty());
    }
}
