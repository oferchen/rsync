use crate::{branding, workspace};
use oc_rsync_protocol::ProtocolVersion;
use std::string::String;

const _: () = {
    let workspace_version = workspace::metadata().protocol_version();
    let crate_version = ProtocolVersion::NEWEST.as_u8() as u32;
    if workspace_version != crate_version {
        panic!("workspace protocol version must match ProtocolVersion::NEWEST");
    }
};

/// Program name rendered by the `rsync` client when displaying version banners.
pub const PROGRAM_NAME: &str = branding::UPSTREAM_CLIENT_PROGRAM_NAME;

/// Program name rendered by the `rsyncd` daemon when displaying version banners.
pub const DAEMON_PROGRAM_NAME: &str = branding::UPSTREAM_DAEMON_PROGRAM_NAME;

/// Program name used by the standalone `oc-rsync` client wrapper.
pub const OC_PROGRAM_NAME: &str = branding::OC_CLIENT_PROGRAM_NAME;

/// Program name used by the standalone `oc-rsyncd` daemon wrapper.
pub const OC_DAEMON_PROGRAM_NAME: &str = branding::OC_DAEMON_PROGRAM_NAME;

/// First copyright year advertised by the Rust implementation.
pub const COPYRIGHT_START_YEAR: &str = "2025";

/// Latest copyright year recorded by the Rust implementation.
pub const LATEST_COPYRIGHT_YEAR: &str = "2025";

/// Copyright notice rendered by `rsync`.
pub const COPYRIGHT_NOTICE: &str = "(C) 2025 by Ofer Chen.";

/// Web site advertised by `rsync` in `--version` output.
pub const WEB_SITE: &str = workspace::WEB_SITE;

/// Repository URL advertised by version banners and documentation.
pub const SOURCE_URL: &str = workspace::SOURCE_URL;

/// Human-readable toolchain description rendered in `--version` output.
pub const BUILD_TOOLCHAIN: &str = "Built in Rust 2024";

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

pub(crate) fn sanitize_build_revision(raw: Option<&'static str>) -> &'static str {
    let Some(value) = raw else {
        return "unknown";
    };

    let trimmed = value.trim();
    if trimmed.is_empty() {
        return "unknown";
    }

    let head = trimmed.split(['\r', '\n']).next().unwrap_or("");
    let cleaned = head.trim();

    if cleaned.is_empty() || cleaned.chars().any(char::is_control) {
        "unknown"
    } else {
        cleaned
    }
}

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
    sanitize_build_revision(option_env!("OC_RSYNC_BUILD_REV"))
}

/// Returns the build information line rendered in the capability section.
#[must_use]
pub fn build_info_line() -> String {
    format!(
        "Rust rsync implementation supporting protocol version {};\n    {};\n    source: {};\n    revision/build: #{}",
        HIGHEST_PROTOCOL_VERSION,
        BUILD_TOOLCHAIN,
        SOURCE_URL,
        build_revision()
    )
}
