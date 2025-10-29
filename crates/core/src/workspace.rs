#![deny(unsafe_code)]

//! Workspace metadata exported as compile-time constants.
//!
//! The [`crates/core`](crate) build script reads `[workspace.metadata.oc_rsync]`
//! from the repository `Cargo.toml` and emits the values below as
//! `cargo:rustc-env` pairs. Centralising the brand and packaging details here
//! keeps the binary front-ends and supporting crates free from duplicated string
//! literals. Callers should prefer these helpers instead of hard-coding program
//! names or configuration paths. Consumers that need the entire metadata set can
//! use [`crate::workspace::metadata`] to obtain a snapshot that mirrors the
//! manifest entries.

use std::num::NonZeroU8;
use std::path::Path;

/// Canonical brand identifier configured for this distribution.
#[doc(alias = "oc")]
pub const BRAND: &str = env!("OC_RSYNC_WORKSPACE_BRAND");

/// Returns the canonical brand identifier configured for this distribution.
///
/// This helper avoids exposing the raw environment variable constant to callers
/// while still participating in constant evaluation. Code that only needs the
/// brand string can call [`brand()`] directly instead of materialising the full
/// [`Metadata`] snapshot.
///
/// # Examples
///
/// ```
/// use rsync_core::workspace;
///
/// assert_eq!(workspace::brand(), workspace::metadata().brand());
/// ```
#[must_use]
pub const fn brand() -> &'static str {
    BRAND
}

/// Upstream rsync base version targeted by this build.
#[doc(alias = "3.4.1")]
pub const UPSTREAM_VERSION: &str = env!("OC_RSYNC_WORKSPACE_UPSTREAM_VERSION");

/// Returns the upstream rsync base version targeted by this build.
///
/// The value matches the upstream release string rendered in `--version`
/// output and documentation banners. Callers that only need the version text
/// can rely on this helper instead of reading it through [`Metadata`].
///
/// # Examples
///
/// ```
/// use rsync_core::workspace;
///
/// assert_eq!(workspace::upstream_version(), workspace::metadata().upstream_version());
/// ```
#[must_use]
pub const fn upstream_version() -> &'static str {
    UPSTREAM_VERSION
}

/// Full Rust-branded version string advertised by binaries.
#[doc(alias = "3.4.1-rust")]
pub const RUST_VERSION: &str = env!("OC_RSYNC_WORKSPACE_RUST_VERSION");

/// Returns the Rust-branded version string advertised by binaries.
///
/// The helper is used by banner renderers that need the branded identifier
/// without constructing a [`Metadata`] snapshot.
///
/// # Examples
///
/// ```
/// use rsync_core::workspace;
///
/// assert_eq!(workspace::rust_version(), workspace::metadata().rust_version());
/// ```
#[must_use]
pub const fn rust_version() -> &'static str {
    RUST_VERSION
}

/// Highest protocol version supported by the workspace.
pub const PROTOCOL_VERSION: u32 = parse_u32(env!("OC_RSYNC_WORKSPACE_PROTOCOL"));

/// Canonical client binary name shipped with the distribution.
#[doc(alias = "oc-rsync")]
pub const CLIENT_PROGRAM_NAME: &str = env!("OC_RSYNC_WORKSPACE_CLIENT_BIN");

/// Returns the canonical client binary name shipped with the distribution.
///
/// This helper mirrors [`Metadata::client_program_name`] while remaining
/// `const`, which simplifies usage from static contexts.
///
/// # Examples
///
/// ```
/// use rsync_core::workspace;
///
/// assert_eq!(workspace::client_program_name(), workspace::metadata().client_program_name());
/// ```
#[must_use]
pub const fn client_program_name() -> &'static str {
    CLIENT_PROGRAM_NAME
}

/// Canonical daemon binary name shipped with the distribution.
#[doc(alias = "oc-rsyncd")]
pub const DAEMON_PROGRAM_NAME: &str = env!("OC_RSYNC_WORKSPACE_DAEMON_BIN");

/// Returns the canonical daemon binary name shipped with the distribution.
///
/// # Examples
///
/// ```
/// use rsync_core::workspace;
///
/// assert_eq!(workspace::daemon_program_name(), workspace::metadata().daemon_program_name());
/// ```
#[must_use]
pub const fn daemon_program_name() -> &'static str {
    DAEMON_PROGRAM_NAME
}

/// Upstream-compatible client binary name used for compatibility symlinks.
#[doc(alias = "rsync")]
pub const LEGACY_CLIENT_PROGRAM_NAME: &str = env!("OC_RSYNC_WORKSPACE_LEGACY_CLIENT_BIN");

/// Returns the upstream-compatible client binary name used for compatibility symlinks.
///
/// # Examples
///
/// ```
/// use rsync_core::workspace;
///
/// assert_eq!(workspace::legacy_client_program_name(), workspace::metadata().legacy_client_program_name());
/// ```
#[must_use]
pub const fn legacy_client_program_name() -> &'static str {
    LEGACY_CLIENT_PROGRAM_NAME
}

/// Upstream-compatible daemon binary name used for compatibility symlinks.
#[doc(alias = "rsyncd")]
pub const LEGACY_DAEMON_PROGRAM_NAME: &str = env!("OC_RSYNC_WORKSPACE_LEGACY_DAEMON_BIN");

/// Returns the upstream-compatible daemon binary name used for compatibility symlinks.
///
/// # Examples
///
/// ```
/// use rsync_core::workspace;
///
/// assert_eq!(workspace::legacy_daemon_program_name(), workspace::metadata().legacy_daemon_program_name());
/// ```
#[must_use]
pub const fn legacy_daemon_program_name() -> &'static str {
    LEGACY_DAEMON_PROGRAM_NAME
}

/// Configuration directory installed alongside the branded daemon.
#[doc(alias = "/etc/oc-rsyncd")]
pub const DAEMON_CONFIG_DIR: &str = env!("OC_RSYNC_WORKSPACE_DAEMON_CONFIG_DIR");

/// Default daemon configuration file path for the branded binary.
#[doc(alias = "/etc/oc-rsyncd/oc-rsyncd.conf")]
pub const DAEMON_CONFIG_PATH: &str = env!("OC_RSYNC_WORKSPACE_DAEMON_CONFIG");

/// Default daemon secrets file path for the branded binary.
#[doc(alias = "/etc/oc-rsyncd/oc-rsyncd.secrets")]
pub const DAEMON_SECRETS_PATH: &str = env!("OC_RSYNC_WORKSPACE_DAEMON_SECRETS");

/// Legacy configuration directory honoured for upstream compatibility.
#[doc(alias = "/etc")]
pub const LEGACY_DAEMON_CONFIG_DIR: &str = env!("OC_RSYNC_WORKSPACE_LEGACY_DAEMON_CONFIG_DIR");

/// Legacy daemon configuration file path honoured for upstream compatibility.
#[doc(alias = "/etc/rsyncd.conf")]
pub const LEGACY_DAEMON_CONFIG_PATH: &str = env!("OC_RSYNC_WORKSPACE_LEGACY_DAEMON_CONFIG");

/// Legacy daemon secrets file path honoured for upstream compatibility.
#[doc(alias = "/etc/rsyncd.secrets")]
pub const LEGACY_DAEMON_SECRETS_PATH: &str = env!("OC_RSYNC_WORKSPACE_LEGACY_DAEMON_SECRETS");

/// Source repository URL advertised by `--version` output.
pub const SOURCE_URL: &str = env!("OC_RSYNC_WORKSPACE_SOURCE");

/// Returns the source repository URL advertised by version banners.
///
/// # Examples
///
/// ```
/// use rsync_core::workspace;
///
/// assert_eq!(workspace::source_url(), workspace::metadata().source_url());
/// ```
#[must_use]
pub const fn source_url() -> &'static str {
    SOURCE_URL
}

/// Immutable snapshot of workspace metadata loaded from `Cargo.toml`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Metadata {
    brand: &'static str,
    upstream_version: &'static str,
    rust_version: &'static str,
    protocol_version: u32,
    client_program_name: &'static str,
    daemon_program_name: &'static str,
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

/// Returns the configured protocol version as an 8-bit integer.
///
/// The workspace manifest records the highest supported protocol as a decimal
/// integer. Upstream rsync encodes negotiated protocol numbers in a single
/// byte, so the manifest value must remain within the `u8` range. The helper
/// performs the bounds check at compile time and therefore causes compilation
/// to fail immediately if the manifest is updated inconsistently. Callers that
/// render diagnostics or capability banners can rely on this accessor without
/// repeating the conversion logic.
///
/// # Examples
///
/// ```
/// use rsync_core::workspace;
///
/// assert_eq!(
///     workspace::protocol_version_u8() as u32,
///     workspace::metadata().protocol_version()
/// );
/// ```
#[must_use]
pub const fn protocol_version_u8() -> u8 {
    let value = metadata().protocol_version();
    if value > u8::MAX as u32 {
        panic!("workspace protocol version must fit within a u8");
    }
    value as u8
}

/// Returns the configured protocol version as a [`NonZeroU8`].
///
/// Upstream rsync has never advertised protocol version `0`. Encoding the value
/// as [`NonZeroU8`] allows call sites to rely on this invariant without
/// repeating ad-hoc checks. The helper reuses [`protocol_version_u8`] to
/// preserve the compile-time bounds validation against the manifest metadata.
///
/// # Examples
///
/// ```
/// use rsync_core::workspace;
///
/// assert_eq!(workspace::protocol_version_nonzero_u8().get(), 32);
/// ```
#[must_use]
pub const fn protocol_version_nonzero_u8() -> NonZeroU8 {
    match NonZeroU8::new(protocol_version_u8()) {
        Some(value) => value,
        None => panic!("workspace protocol version must be non-zero"),
    }
}

/// Returns the configured daemon configuration directory as a [`Path`].
#[must_use]
pub fn daemon_config_dir() -> &'static Path {
    Path::new(DAEMON_CONFIG_DIR)
}

/// Returns the configured daemon configuration file as a [`Path`].
#[must_use]
pub fn daemon_config_path() -> &'static Path {
    Path::new(DAEMON_CONFIG_PATH)
}

/// Returns the configured daemon secrets file as a [`Path`].
#[must_use]
pub fn daemon_secrets_path() -> &'static Path {
    Path::new(DAEMON_SECRETS_PATH)
}

/// Parses an ASCII decimal integer at compile time.
const fn parse_u32(input: &str) -> u32 {
    let bytes = input.as_bytes();
    let mut value = 0u32;
    let mut index = 0;
    if bytes.is_empty() {
        panic!("protocol must not be empty");
    }
    while index < bytes.len() {
        let digit = bytes[index];
        if !digit.is_ascii_digit() {
            panic!("protocol must be an ASCII integer");
        }
        value = value * 10 + (digit - b'0') as u32;
        index += 1;
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_protocol_matches_env() {
        assert_eq!(metadata().protocol_version(), 32);
        assert_eq!(protocol_version_u8(), 32);
        assert_eq!(protocol_version_nonzero_u8().get(), 32);
        assert_eq!(daemon_config_dir(), Path::new(DAEMON_CONFIG_DIR));
        assert_eq!(daemon_config_path(), Path::new(DAEMON_CONFIG_PATH));
        assert_eq!(daemon_secrets_path(), Path::new(DAEMON_SECRETS_PATH));
    }

    #[test]
    fn const_accessors_match_metadata() {
        let snapshot = metadata();

        assert_eq!(brand(), snapshot.brand());
        assert_eq!(upstream_version(), snapshot.upstream_version());
        assert_eq!(rust_version(), snapshot.rust_version());
        assert_eq!(client_program_name(), snapshot.client_program_name());
        assert_eq!(daemon_program_name(), snapshot.daemon_program_name());
        assert_eq!(
            legacy_client_program_name(),
            snapshot.legacy_client_program_name()
        );
        assert_eq!(
            legacy_daemon_program_name(),
            snapshot.legacy_daemon_program_name()
        );
        assert_eq!(source_url(), snapshot.source_url());
    }

    #[test]
    fn metadata_matches_manifest() {
        let manifest = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml"));
        let value: toml::Table = manifest.parse().expect("parse manifest");
        let workspace = value
            .get("workspace")
            .and_then(toml::Value::as_table)
            .expect("workspace table");
        let metadata_table = workspace
            .get("metadata")
            .and_then(toml::Value::as_table)
            .expect("metadata table");
        let oc = metadata_table
            .get("oc_rsync")
            .and_then(toml::Value::as_table)
            .expect("oc_rsync table");

        let snapshot = metadata();

        assert_eq!(snapshot.brand(), oc["brand"].as_str().expect("brand"));
        assert_eq!(
            snapshot.upstream_version(),
            oc["upstream_version"].as_str().expect("upstream_version")
        );
        assert_eq!(
            snapshot.rust_version(),
            oc["rust_version"].as_str().expect("rust_version")
        );
        assert_eq!(
            snapshot.protocol_version(),
            oc["protocol"].as_integer().expect("protocol") as u32
        );
        assert_eq!(
            snapshot.client_program_name(),
            oc["client_bin"].as_str().expect("client_bin")
        );
        assert_eq!(
            snapshot.daemon_program_name(),
            oc["daemon_bin"].as_str().expect("daemon_bin")
        );
        assert_eq!(
            snapshot.legacy_client_program_name(),
            oc["legacy_client_bin"].as_str().expect("legacy_client_bin")
        );
        assert_eq!(
            snapshot.legacy_daemon_program_name(),
            oc["legacy_daemon_bin"].as_str().expect("legacy_daemon_bin")
        );
        assert_eq!(
            snapshot.daemon_config_dir(),
            oc["daemon_config_dir"].as_str().expect("daemon_config_dir")
        );
        assert_eq!(
            snapshot.daemon_config_path(),
            oc["daemon_config"].as_str().expect("daemon_config")
        );
        assert_eq!(
            snapshot.daemon_secrets_path(),
            oc["daemon_secrets"].as_str().expect("daemon_secrets")
        );
        assert_eq!(
            snapshot.legacy_daemon_config_dir(),
            oc["legacy_daemon_config_dir"]
                .as_str()
                .expect("legacy_daemon_config_dir")
        );
        assert_eq!(
            snapshot.legacy_daemon_config_path(),
            oc["legacy_daemon_config"]
                .as_str()
                .expect("legacy_daemon_config")
        );
        assert_eq!(
            snapshot.legacy_daemon_secrets_path(),
            oc["legacy_daemon_secrets"]
                .as_str()
                .expect("legacy_daemon_secrets")
        );
        assert_eq!(
            snapshot.source_url(),
            oc["source"].as_str().expect("source")
        );
    }
}
