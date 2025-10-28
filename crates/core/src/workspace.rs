#![deny(unsafe_code)]

//! Workspace metadata exported as compile-time constants.
//!
//! The [`crates/core`](crate) build script reads `[workspace.metadata.oc_rsync]`
//! from the repository `Cargo.toml` and emits the values below as
//! `cargo:rustc-env` pairs. Centralising the brand and packaging details here
//! keeps the binary front-ends and supporting crates free from duplicated string
//! literals. Callers should prefer these helpers instead of hard-coding program
//! names or configuration paths.

use std::path::Path;

/// Canonical brand identifier configured for this distribution.
#[doc(alias = "oc")]
pub const BRAND: &str = env!("OC_RSYNC_WORKSPACE_BRAND");

/// Upstream rsync base version targeted by this build.
#[doc(alias = "3.4.1")]
pub const UPSTREAM_VERSION: &str = env!("OC_RSYNC_WORKSPACE_UPSTREAM_VERSION");

/// Full Rust-branded version string advertised by binaries.
#[doc(alias = "3.4.1-rust")]
pub const RUST_VERSION: &str = env!("OC_RSYNC_WORKSPACE_RUST_VERSION");

/// Highest protocol version supported by the workspace.
pub const PROTOCOL_VERSION: u32 = parse_u32(env!("OC_RSYNC_WORKSPACE_PROTOCOL"));

/// Canonical client binary name shipped with the distribution.
#[doc(alias = "oc-rsync")]
pub const CLIENT_PROGRAM_NAME: &str = env!("OC_RSYNC_WORKSPACE_CLIENT_BIN");

/// Canonical daemon binary name shipped with the distribution.
#[doc(alias = "oc-rsyncd")]
pub const DAEMON_PROGRAM_NAME: &str = env!("OC_RSYNC_WORKSPACE_DAEMON_BIN");

/// Upstream-compatible client binary name used for compatibility symlinks.
#[doc(alias = "rsync")]
pub const LEGACY_CLIENT_PROGRAM_NAME: &str = env!("OC_RSYNC_WORKSPACE_LEGACY_CLIENT_BIN");

/// Upstream-compatible daemon binary name used for compatibility symlinks.
#[doc(alias = "rsyncd")]
pub const LEGACY_DAEMON_PROGRAM_NAME: &str = env!("OC_RSYNC_WORKSPACE_LEGACY_DAEMON_BIN");

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
        assert_eq!(PROTOCOL_VERSION, 32);
        assert_eq!(daemon_config_dir(), Path::new(DAEMON_CONFIG_DIR));
        assert_eq!(daemon_config_path(), Path::new(DAEMON_CONFIG_PATH));
        assert_eq!(daemon_secrets_path(), Path::new(DAEMON_SECRETS_PATH));
    }
}
