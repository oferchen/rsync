use super::constants::{
    BRAND, CLIENT_PROGRAM_NAME, DAEMON_CONFIG_DIR, DAEMON_CONFIG_PATH, DAEMON_PROGRAM_NAME,
    DAEMON_SECRETS_PATH, DAEMON_WRAPPER_PROGRAM_NAME, LEGACY_CLIENT_PROGRAM_NAME,
    LEGACY_DAEMON_CONFIG_DIR, LEGACY_DAEMON_CONFIG_PATH, LEGACY_DAEMON_PROGRAM_NAME,
    LEGACY_DAEMON_SECRETS_PATH, RUST_VERSION, SOURCE_URL, UPSTREAM_VERSION,
};
use super::protocol::PROTOCOL_VERSION;
use serde::Serialize;

/// Immutable snapshot of workspace metadata loaded from `Cargo.toml`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct Metadata {
    brand: &'static str,
    upstream_version: &'static str,
    rust_version: &'static str,
    protocol_version: u32,
    client_program_name: &'static str,
    daemon_program_name: &'static str,
    daemon_wrapper_program_name: &'static str,
    legacy_client_program_name: &'static str,
    legacy_daemon_program_name: &'static str,
    daemon_config_dir: &'static str,
    daemon_config_path: &'static str,
    daemon_secrets_path: &'static str,
    legacy_daemon_config_dir: &'static str,
    legacy_daemon_config_path: &'static str,
    legacy_daemon_secrets_path: &'static str,
    source_url: &'static str,
}

impl Metadata {
    /// Returns the canonical brand identifier configured for the workspace.
    #[must_use]
    pub const fn brand(self) -> &'static str {
        self.brand
    }

    /// Returns the upstream base version the implementation targets.
    #[must_use]
    pub const fn upstream_version(self) -> &'static str {
        self.upstream_version
    }

    /// Returns the Rust-branded version string advertised by binaries.
    #[must_use]
    pub const fn rust_version(self) -> &'static str {
        self.rust_version
    }

    /// Returns the highest rsync protocol version supported by this build.
    #[must_use]
    pub const fn protocol_version(self) -> u32 {
        self.protocol_version
    }

    /// Returns the canonical client program name shipped with the distribution.
    #[must_use]
    pub const fn client_program_name(self) -> &'static str {
        self.client_program_name
    }

    /// Returns the canonical daemon program name shipped with the distribution.
    #[must_use]
    pub const fn daemon_program_name(self) -> &'static str {
        self.daemon_program_name
    }

    /// Returns the compatibility wrapper name installed alongside the daemon.
    #[must_use]
    pub const fn daemon_wrapper_program_name(self) -> &'static str {
        self.daemon_wrapper_program_name
    }

    /// Returns the upstream-compatible client program name used for symlinks.
    #[must_use]
    pub const fn legacy_client_program_name(self) -> &'static str {
        self.legacy_client_program_name
    }

    /// Returns the upstream-compatible daemon program name used for symlinks.
    #[must_use]
    pub const fn legacy_daemon_program_name(self) -> &'static str {
        self.legacy_daemon_program_name
    }

    /// Returns the configuration directory installed alongside the daemon.
    #[must_use]
    pub const fn daemon_config_dir(self) -> &'static str {
        self.daemon_config_dir
    }

    /// Returns the daemon configuration file path.
    #[must_use]
    pub const fn daemon_config_path(self) -> &'static str {
        self.daemon_config_path
    }

    /// Returns the daemon secrets file path.
    #[must_use]
    pub const fn daemon_secrets_path(self) -> &'static str {
        self.daemon_secrets_path
    }

    /// Returns the legacy configuration directory supported for compatibility.
    #[must_use]
    pub const fn legacy_daemon_config_dir(self) -> &'static str {
        self.legacy_daemon_config_dir
    }

    /// Returns the legacy daemon configuration path supported for compatibility.
    #[must_use]
    pub const fn legacy_daemon_config_path(self) -> &'static str {
        self.legacy_daemon_config_path
    }

    /// Returns the legacy daemon secrets path supported for compatibility.
    #[must_use]
    pub const fn legacy_daemon_secrets_path(self) -> &'static str {
        self.legacy_daemon_secrets_path
    }

    /// Returns the source repository URL used by version banners.
    #[must_use]
    pub const fn source_url(self) -> &'static str {
        self.source_url
    }
}

const WORKSPACE_METADATA: Metadata = Metadata {
    brand: BRAND,
    upstream_version: UPSTREAM_VERSION,
    rust_version: RUST_VERSION,
    protocol_version: PROTOCOL_VERSION,
    client_program_name: CLIENT_PROGRAM_NAME,
    daemon_program_name: DAEMON_PROGRAM_NAME,
    daemon_wrapper_program_name: DAEMON_WRAPPER_PROGRAM_NAME,
    legacy_client_program_name: LEGACY_CLIENT_PROGRAM_NAME,
    legacy_daemon_program_name: LEGACY_DAEMON_PROGRAM_NAME,
    daemon_config_dir: DAEMON_CONFIG_DIR,
    daemon_config_path: DAEMON_CONFIG_PATH,
    daemon_secrets_path: DAEMON_SECRETS_PATH,
    legacy_daemon_config_dir: LEGACY_DAEMON_CONFIG_DIR,
    legacy_daemon_config_path: LEGACY_DAEMON_CONFIG_PATH,
    legacy_daemon_secrets_path: LEGACY_DAEMON_SECRETS_PATH,
    source_url: SOURCE_URL,
};

/// Returns an immutable snapshot of the workspace branding and packaging metadata.
#[must_use]
pub const fn metadata() -> Metadata {
    WORKSPACE_METADATA
}
