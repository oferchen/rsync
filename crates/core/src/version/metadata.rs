use crate::branding::Brand;
use protocol::ProtocolVersion;
use std::fmt::{self, Write as FmtWrite};
use std::string::String;

use super::constants::build_revision;
use super::constants::{
    BUILD_TOOLCHAIN, COPYRIGHT_NOTICE, RUST_VERSION, SOURCE_URL, SUBPROTOCOL_VERSION,
    UPSTREAM_BASE_VERSION,
};
use super::constants::{
    DAEMON_PROGRAM_NAME, OC_DAEMON_PROGRAM_NAME, OC_PROGRAM_NAME, PROGRAM_NAME,
};

/// Returns a description of the build target platform.
#[must_use]
fn target_description() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

/// Static metadata describing the standard version banner rendered by `oc-rsync`.
///
/// The structure mirrors upstream `print_rsync_version()` so higher layers can
/// render byte-identical banners without hard-coding strings at the call site
/// while honouring the Rust-specific branding.
/// It captures the program name, version identifiers, protocol numbers, and the
/// canonical copyright notice.
///
/// # Examples
///
/// ```
/// use core::version::{version_metadata, PROGRAM_NAME, RUST_VERSION, SOURCE_URL};
///
/// let metadata = version_metadata();
/// let banner = metadata.standard_banner();
///
/// assert!(banner.starts_with(&format!(
///     "{PROGRAM_NAME} v{} (revision #",
///     RUST_VERSION
/// )));
/// assert!(banner.contains("protocol version 32"));
/// assert!(banner.contains("revision #"));
/// assert!(banner.contains(&format!("Source: {}", SOURCE_URL)));
/// ```
#[doc(alias = "--version")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VersionMetadata {
    program_name: &'static str,
    upstream_version: &'static str,
    rust_version: &'static str,
    protocol_version: ProtocolVersion,
    subprotocol_version: u8,
    copyright_notice: &'static str,
    source_url: &'static str,
    build_toolchain: &'static str,
}

impl VersionMetadata {
    /// Returns the program name rendered at the start of the banner.
    #[must_use]
    pub const fn program_name(&self) -> &'static str {
        self.program_name
    }

    /// Returns the upstream version string without the Rust suffix.
    #[must_use]
    pub const fn upstream_version(&self) -> &'static str {
        self.upstream_version
    }

    /// Returns the oc-rsync release version string (e.g., `0.5.0`).
    #[must_use]
    pub const fn rust_version(&self) -> &'static str {
        self.rust_version
    }

    /// Returns the negotiated protocol version advertised by the banner.
    #[must_use]
    pub const fn protocol_version(&self) -> ProtocolVersion {
        self.protocol_version
    }

    /// Returns the optional subprotocol used for pre-release builds.
    #[must_use]
    pub const fn subprotocol_version(&self) -> u8 {
        self.subprotocol_version
    }

    /// Returns the canonical copyright notice rendered by upstream rsync.
    #[must_use]
    pub const fn copyright_notice(&self) -> &'static str {
        self.copyright_notice
    }

    /// Returns the source URL advertised by the banner.
    #[must_use]
    pub const fn source_url(&self) -> &'static str {
        self.source_url
    }

    /// Returns the build toolchain description (e.g., "Built in Rust 2024").
    #[must_use]
    pub const fn build_toolchain(&self) -> &'static str {
        self.build_toolchain
    }

    /// Writes the standard textual banner into the provided [`fmt::Write`] sink.
    pub fn write_standard_banner<W: FmtWrite>(&self, writer: &mut W) -> fmt::Result {
        write!(
            writer,
            "{} v{} (revision #{}) protocol version {}",
            self.program_name(),
            self.rust_version(),
            build_revision(),
            self.protocol_version().as_u8()
        )?;

        if self.subprotocol_version() != 0 {
            write!(writer, ".PR{}", self.subprotocol_version())?;
        }

        writer.write_char('\n')?;

        // Upstream compatibility line
        writeln!(
            writer,
            "Compatible with rsync {} wire protocol",
            self.upstream_version()
        )?;

        // Build information
        writeln!(
            writer,
            "{} for {}",
            self.build_toolchain(),
            target_description()
        )?;

        writer.write_str("Copyright ")?;
        writer.write_str(self.copyright_notice())?;
        writer.write_char('\n')?;
        writer.write_str("Source: ")?;
        writer.write_str(self.source_url())?;
        writer.write_char('\n')
    }

    /// Returns the standard banner rendered into an owned [`String`].
    #[must_use]
    pub fn standard_banner(&self) -> String {
        let mut banner = String::new();
        self.write_standard_banner(&mut banner)
            .expect("writing to String cannot fail");
        banner
    }
}

impl Default for VersionMetadata {
    fn default() -> Self {
        version_metadata()
    }
}

/// Returns the canonical metadata used to render `--version` output.
#[doc(alias = "--version")]
#[must_use]
pub const fn version_metadata() -> VersionMetadata {
    version_metadata_for_program(PROGRAM_NAME)
}

/// Returns metadata configured for the upstream-compatible `rsync` daemon banner.
#[must_use]
pub const fn daemon_version_metadata() -> VersionMetadata {
    version_metadata_for_program(DAEMON_PROGRAM_NAME)
}

/// Returns metadata configured for the branded `oc-rsync` client banner.
#[must_use]
pub const fn oc_version_metadata() -> VersionMetadata {
    version_metadata_for_program(OC_PROGRAM_NAME)
}

/// Returns metadata configured for the branded `oc-rsync` daemon banner.
#[must_use]
pub const fn oc_daemon_version_metadata() -> VersionMetadata {
    version_metadata_for_program(OC_DAEMON_PROGRAM_NAME)
}

/// Returns version metadata tailored to the client program associated with `brand`.
#[must_use]
pub const fn version_metadata_for_client_brand(brand: Brand) -> VersionMetadata {
    version_metadata_for_program(brand.client_program_name())
}

/// Returns version metadata tailored to the daemon program associated with `brand`.
#[must_use]
pub const fn version_metadata_for_daemon_brand(brand: Brand) -> VersionMetadata {
    version_metadata_for_program(brand.daemon_program_name())
}

/// Returns version metadata that renders a banner for the supplied program name.
#[must_use]
pub const fn version_metadata_for_program(program_name: &'static str) -> VersionMetadata {
    VersionMetadata {
        program_name,
        upstream_version: UPSTREAM_BASE_VERSION,
        rust_version: RUST_VERSION,
        protocol_version: ProtocolVersion::NEWEST,
        subprotocol_version: SUBPROTOCOL_VERSION,
        copyright_notice: COPYRIGHT_NOTICE,
        source_url: SOURCE_URL,
        build_toolchain: BUILD_TOOLCHAIN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests for version_metadata factory function
    #[test]
    fn version_metadata_returns_valid_metadata() {
        let meta = version_metadata();
        assert_eq!(meta.program_name(), PROGRAM_NAME);
        assert_eq!(meta.upstream_version(), UPSTREAM_BASE_VERSION);
        assert_eq!(meta.rust_version(), RUST_VERSION);
    }

    #[test]
    fn version_metadata_protocol_version_is_newest() {
        let meta = version_metadata();
        assert_eq!(meta.protocol_version(), ProtocolVersion::NEWEST);
    }

    #[test]
    fn version_metadata_has_copyright_notice() {
        let meta = version_metadata();
        assert_eq!(meta.copyright_notice(), COPYRIGHT_NOTICE);
        assert!(!meta.copyright_notice().is_empty());
    }

    #[test]
    fn version_metadata_has_source_url() {
        let meta = version_metadata();
        assert_eq!(meta.source_url(), SOURCE_URL);
        assert!(!meta.source_url().is_empty());
    }

    // Tests for daemon_version_metadata
    #[test]
    fn daemon_version_metadata_uses_daemon_program_name() {
        let meta = daemon_version_metadata();
        assert_eq!(meta.program_name(), DAEMON_PROGRAM_NAME);
    }

    #[test]
    fn daemon_version_metadata_has_same_version_as_client() {
        let client = version_metadata();
        let daemon = daemon_version_metadata();
        assert_eq!(client.rust_version(), daemon.rust_version());
        assert_eq!(client.protocol_version(), daemon.protocol_version());
    }

    // Tests for oc_version_metadata
    #[test]
    fn oc_version_metadata_uses_oc_program_name() {
        let meta = oc_version_metadata();
        assert_eq!(meta.program_name(), OC_PROGRAM_NAME);
    }

    // Tests for oc_daemon_version_metadata
    #[test]
    fn oc_daemon_version_metadata_uses_oc_daemon_program_name() {
        let meta = oc_daemon_version_metadata();
        assert_eq!(meta.program_name(), OC_DAEMON_PROGRAM_NAME);
    }

    // Tests for version_metadata_for_program
    #[test]
    fn version_metadata_for_program_uses_custom_name() {
        let meta = version_metadata_for_program("custom-rsync");
        assert_eq!(meta.program_name(), "custom-rsync");
    }

    #[test]
    fn version_metadata_for_program_keeps_other_fields() {
        let meta = version_metadata_for_program("custom-rsync");
        assert_eq!(meta.rust_version(), RUST_VERSION);
        assert_eq!(meta.protocol_version(), ProtocolVersion::NEWEST);
    }

    // Tests for version_metadata_for_client_brand
    #[test]
    fn version_metadata_for_client_brand_uses_brand_name() {
        let meta = version_metadata_for_client_brand(Brand::Upstream);
        assert_eq!(meta.program_name(), Brand::Upstream.client_program_name());
    }

    // Tests for version_metadata_for_daemon_brand
    #[test]
    fn version_metadata_for_daemon_brand_uses_brand_name() {
        let meta = version_metadata_for_daemon_brand(Brand::Upstream);
        assert_eq!(meta.program_name(), Brand::Upstream.daemon_program_name());
    }

    // Tests for accessor methods
    #[test]
    fn program_name_accessor_returns_correct_value() {
        let meta = version_metadata();
        assert!(!meta.program_name().is_empty());
    }

    #[test]
    fn upstream_version_accessor_returns_correct_value() {
        let meta = version_metadata();
        assert!(!meta.upstream_version().is_empty());
        assert!(meta.upstream_version().contains('.'));
    }

    #[test]
    fn rust_version_accessor_is_valid_semver() {
        let meta = version_metadata();
        let version = meta.rust_version();
        let parts: Vec<&str> = version.split('.').collect();
        assert_eq!(parts.len(), 3, "rust_version should have three components");
        for part in parts {
            assert!(
                part.parse::<u32>().is_ok(),
                "each component should be numeric"
            );
        }
    }

    #[test]
    fn subprotocol_version_is_accessible() {
        let meta = version_metadata();
        let _ = meta.subprotocol_version(); // Just verify it's accessible
    }

    #[test]
    fn build_toolchain_accessor_returns_correct_value() {
        let meta = version_metadata();
        assert_eq!(meta.build_toolchain(), BUILD_TOOLCHAIN);
        assert!(!meta.build_toolchain().is_empty());
    }

    // Tests for write_standard_banner
    #[test]
    fn write_standard_banner_includes_program_name() {
        let meta = version_metadata();
        let mut output = String::new();
        meta.write_standard_banner(&mut output).unwrap();
        assert!(output.contains(meta.program_name()));
    }

    #[test]
    fn write_standard_banner_includes_version() {
        let meta = version_metadata();
        let mut output = String::new();
        meta.write_standard_banner(&mut output).unwrap();
        assert!(output.contains(meta.rust_version()));
    }

    #[test]
    fn write_standard_banner_includes_protocol_version() {
        let meta = version_metadata();
        let mut output = String::new();
        meta.write_standard_banner(&mut output).unwrap();
        assert!(output.contains(&format!(
            "protocol version {}",
            meta.protocol_version().as_u8()
        )));
    }

    #[test]
    fn write_standard_banner_includes_copyright() {
        let meta = version_metadata();
        let mut output = String::new();
        meta.write_standard_banner(&mut output).unwrap();
        assert!(output.contains("Copyright"));
    }

    #[test]
    fn write_standard_banner_includes_source_url() {
        let meta = version_metadata();
        let mut output = String::new();
        meta.write_standard_banner(&mut output).unwrap();
        assert!(output.contains("Source:"));
        assert!(output.contains(meta.source_url()));
    }

    #[test]
    fn write_standard_banner_includes_upstream_compatibility() {
        let meta = version_metadata();
        let mut output = String::new();
        meta.write_standard_banner(&mut output).unwrap();
        assert!(output.contains("Compatible with rsync"));
        assert!(output.contains(meta.upstream_version()));
        assert!(output.contains("wire protocol"));
    }

    #[test]
    fn write_standard_banner_includes_build_toolchain() {
        let meta = version_metadata();
        let mut output = String::new();
        meta.write_standard_banner(&mut output).unwrap();
        assert!(output.contains(meta.build_toolchain()));
    }

    #[test]
    fn write_standard_banner_includes_target_platform() {
        let meta = version_metadata();
        let mut output = String::new();
        meta.write_standard_banner(&mut output).unwrap();
        // Should contain the arch and OS
        assert!(output.contains(std::env::consts::ARCH));
        assert!(output.contains(std::env::consts::OS));
    }

    #[test]
    fn write_standard_banner_ends_with_newline() {
        let meta = version_metadata();
        let mut output = String::new();
        meta.write_standard_banner(&mut output).unwrap();
        assert!(output.ends_with('\n'));
    }

    // Tests for standard_banner
    #[test]
    fn standard_banner_returns_non_empty_string() {
        let meta = version_metadata();
        let banner = meta.standard_banner();
        assert!(!banner.is_empty());
    }

    #[test]
    fn standard_banner_matches_write_standard_banner() {
        let meta = version_metadata();
        let mut expected = String::new();
        meta.write_standard_banner(&mut expected).unwrap();
        let actual = meta.standard_banner();
        assert_eq!(expected, actual);
    }

    // Tests for Default implementation
    #[test]
    fn default_returns_same_as_version_metadata() {
        let default = VersionMetadata::default();
        let explicit = version_metadata();
        assert_eq!(default, explicit);
    }

    // Tests for trait implementations
    #[test]
    fn version_metadata_is_clone() {
        let meta = version_metadata();
        let cloned = meta;
        assert_eq!(meta, cloned);
    }

    #[test]
    fn version_metadata_is_copy() {
        let meta = version_metadata();
        let copied = meta;
        assert_eq!(meta, copied);
    }

    #[test]
    fn version_metadata_debug_includes_struct_name() {
        let meta = version_metadata();
        let debug = format!("{meta:?}");
        assert!(debug.contains("VersionMetadata"));
    }

    #[test]
    fn version_metadata_equality() {
        let meta1 = version_metadata();
        let meta2 = version_metadata();
        assert_eq!(meta1, meta2);
    }

    #[test]
    fn version_metadata_inequality_on_different_program_names() {
        let meta1 = version_metadata_for_program("program-a");
        let meta2 = version_metadata_for_program("program-b");
        assert_ne!(meta1, meta2);
    }
}
