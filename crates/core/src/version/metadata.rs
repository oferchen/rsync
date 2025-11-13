use crate::branding::Brand;
use protocol::ProtocolVersion;
use std::fmt::{self, Write as FmtWrite};
use std::string::String;

use super::constants::build_revision;
use super::constants::{
    COPYRIGHT_NOTICE, RUST_VERSION, SOURCE_URL, SUBPROTOCOL_VERSION, UPSTREAM_BASE_VERSION,
};
use super::constants::{
    DAEMON_PROGRAM_NAME, OC_DAEMON_PROGRAM_NAME, OC_PROGRAM_NAME, PROGRAM_NAME,
};

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
///     "{PROGRAM_NAME}  version {} ",
///     RUST_VERSION
/// )));
/// assert!(banner.contains("protocol version 32"));
/// assert!(banner.contains("revision/build #"));
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

    /// Returns the Rust-flavoured version string (`3.4.1-rust`).
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
    }
}
