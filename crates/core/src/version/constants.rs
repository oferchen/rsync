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
pub fn build_revision() -> &'static str {
    branding_build_revision()
}
