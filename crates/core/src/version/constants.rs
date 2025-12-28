use crate::{branding, workspace};
use ::branding::{
    build_revision as branding_build_revision, build_toolchain as branding_build_toolchain,
};
use protocol::ProtocolVersion;

const _: () = {
    let workspace_version = workspace::metadata().protocol_version();
    let crate_version = ProtocolVersion::NEWEST.as_u8() as u32;
    if workspace_version != crate_version {
        panic!("workspace protocol version must match ProtocolVersion::NEWEST");
    }
};

/// Program name rendered by the `oc-rsync` client when displaying version banners.
pub const PROGRAM_NAME: &str = branding::OC_CLIENT_PROGRAM_NAME;

/// Program name rendered by the `oc-rsync --daemon` entry point when displaying version banners.
pub const DAEMON_PROGRAM_NAME: &str = branding::OC_DAEMON_PROGRAM_NAME;

/// Alias of [`PROGRAM_NAME`] retained for backwards compatibility with older code that
/// referenced the branded constant directly.
pub const OC_PROGRAM_NAME: &str = PROGRAM_NAME;

/// Alias of [`DAEMON_PROGRAM_NAME`] retained for backwards compatibility with older code that
/// referenced the branded constant directly.
pub const OC_DAEMON_PROGRAM_NAME: &str = DAEMON_PROGRAM_NAME;

/// Legacy upstream client program name recognised for compatibility shims.
pub const LEGACY_PROGRAM_NAME: &str = branding::UPSTREAM_CLIENT_PROGRAM_NAME;

/// Legacy upstream daemon program name recognised for compatibility shims.
pub const LEGACY_DAEMON_PROGRAM_NAME: &str = branding::UPSTREAM_DAEMON_PROGRAM_NAME;

/// First copyright year advertised by the Rust implementation.
pub const COPYRIGHT_START_YEAR: &str = "2025";

/// Latest copyright year recorded by the Rust implementation.
pub const LATEST_COPYRIGHT_YEAR: &str = "2025";

/// Copyright notice rendered by `rsync`.
pub const COPYRIGHT_NOTICE: &str = "(C) 2025 by Ofer Chen.";

/// Repository URL advertised by version banners and documentation.
pub const SOURCE_URL: &str = workspace::SOURCE_URL;

/// Human-readable toolchain description rendered in `--version` output.
pub const BUILD_TOOLCHAIN: &str = branding_build_toolchain();

/// Subprotocol version appended to the negotiated protocol when non-zero.
pub const SUBPROTOCOL_VERSION: u8 = 0;

/// Upstream base version that the Rust implementation tracks.
#[doc(alias = "3.4.1")]
pub const UPSTREAM_BASE_VERSION: &str = workspace::UPSTREAM_VERSION;

/// Full version string rendered by user-visible banners.
#[doc(alias = "3.4.1-rust")]
pub const RUST_VERSION: &str = workspace::RUST_VERSION;

/// Highest protocol version supported by this build.
pub const HIGHEST_PROTOCOL_VERSION: u8 = workspace::protocol_version_u8();

/// Returns the Git revision baked into the build, if available.
///
/// Whitespace surrounding the revision string is trimmed so the value can be embedded in version
/// banners without introducing stray spaces or newlines. When the environment variable is unset or
/// only contains whitespace the function returns `"unknown"`, mirroring upstream rsync's
/// behaviour when revision metadata is unavailable. Embedded newlines are ignored by taking the
/// first non-empty line, and control characters cause the revision to be reported as
/// `"unknown"` to avoid rendering artifacts in version banners.
#[must_use]
pub const fn build_revision() -> &'static str {
    branding_build_revision()
}

#[cfg(test)]
#[allow(clippy::const_is_empty, clippy::assertions_on_constants)]
mod tests {
    use super::*;

    // Tests for program name constants
    #[test]
    fn program_name_is_not_empty() {
        assert!(!PROGRAM_NAME.is_empty());
    }

    #[test]
    fn daemon_program_name_is_not_empty() {
        assert!(!DAEMON_PROGRAM_NAME.is_empty());
    }

    #[test]
    fn oc_program_name_equals_program_name() {
        assert_eq!(OC_PROGRAM_NAME, PROGRAM_NAME);
    }

    #[test]
    fn oc_daemon_program_name_equals_daemon_program_name() {
        assert_eq!(OC_DAEMON_PROGRAM_NAME, DAEMON_PROGRAM_NAME);
    }

    #[test]
    fn legacy_program_name_is_not_empty() {
        assert!(!LEGACY_PROGRAM_NAME.is_empty());
    }

    #[test]
    fn legacy_daemon_program_name_is_not_empty() {
        assert!(!LEGACY_DAEMON_PROGRAM_NAME.is_empty());
    }

    // Tests for copyright constants
    #[test]
    fn copyright_start_year_is_valid() {
        let year: u32 = COPYRIGHT_START_YEAR.parse().expect("valid year");
        assert!((2020..=2100).contains(&year));
    }

    #[test]
    fn latest_copyright_year_is_valid() {
        let year: u32 = LATEST_COPYRIGHT_YEAR.parse().expect("valid year");
        assert!((2020..=2100).contains(&year));
    }

    #[test]
    fn latest_copyright_year_is_not_before_start() {
        let start: u32 = COPYRIGHT_START_YEAR.parse().unwrap();
        let latest: u32 = LATEST_COPYRIGHT_YEAR.parse().unwrap();
        assert!(latest >= start);
    }

    #[test]
    fn copyright_notice_is_not_empty() {
        assert!(!COPYRIGHT_NOTICE.is_empty());
    }

    #[test]
    fn copyright_notice_contains_year() {
        assert!(COPYRIGHT_NOTICE.contains(LATEST_COPYRIGHT_YEAR));
    }

    // Tests for source URL
    #[test]
    fn source_url_is_not_empty() {
        assert!(!SOURCE_URL.is_empty());
    }

    #[test]
    fn source_url_is_valid_url() {
        assert!(SOURCE_URL.starts_with("http://") || SOURCE_URL.starts_with("https://"));
    }

    // Tests for build toolchain
    #[test]
    fn build_toolchain_is_not_empty() {
        assert!(!BUILD_TOOLCHAIN.is_empty());
    }

    // Tests for version constants
    #[test]
    fn upstream_base_version_is_not_empty() {
        assert!(!UPSTREAM_BASE_VERSION.is_empty());
    }

    #[test]
    fn upstream_base_version_contains_dot() {
        assert!(UPSTREAM_BASE_VERSION.contains('.'));
    }

    #[test]
    fn rust_version_is_not_empty() {
        assert!(!RUST_VERSION.is_empty());
    }

    #[test]
    fn rust_version_contains_rust_suffix() {
        assert!(RUST_VERSION.contains("rust"));
    }

    #[test]
    fn rust_version_starts_with_upstream_version() {
        assert!(RUST_VERSION.starts_with(UPSTREAM_BASE_VERSION));
    }

    // Tests for protocol version
    #[test]
    fn highest_protocol_version_in_valid_range() {
        assert!(HIGHEST_PROTOCOL_VERSION >= 28);
        assert!(HIGHEST_PROTOCOL_VERSION <= 40);
    }

    #[test]
    fn highest_protocol_version_matches_newest() {
        assert_eq!(HIGHEST_PROTOCOL_VERSION, ProtocolVersion::NEWEST.as_u8());
    }

    // Tests for build_revision function
    #[test]
    fn build_revision_returns_non_empty_string() {
        let revision = build_revision();
        assert!(!revision.is_empty());
    }

    #[test]
    fn build_revision_has_no_leading_whitespace() {
        let revision = build_revision();
        assert_eq!(revision, revision.trim_start());
    }

    #[test]
    fn build_revision_has_no_trailing_whitespace() {
        let revision = build_revision();
        assert_eq!(revision, revision.trim_end());
    }
}
