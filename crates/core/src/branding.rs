#![deny(unsafe_code)]

//! Branding constants shared across the workspace.
//!
//! The `branding` module centralises the branded program names and filesystem
//! locations that the workspace exposes publicly. Higher-level crates rely on
//! these constants when rendering banners or searching for configuration files
//! so that packaging, documentation, and runtime behaviour remain aligned. By
//! funnelling branding details through this module we keep string literals out
//! of business logic and make it trivial to update paths or names in one place.
//!
//! # Examples
//!
//! Retrieve the canonical daemon configuration and secrets paths that
//! `oc-rsyncd` uses when launched without explicit overrides:
//!
//! ```rust
//! use std::path::Path;
//!
//! let config = rsync_core::branding::oc_daemon_config_path();
//! let secrets = rsync_core::branding::oc_daemon_secrets_path();
//!
//! assert_eq!(config, Path::new("/etc/oc-rsyncd/oc-rsyncd.conf"));
//! assert_eq!(secrets, Path::new("/etc/oc-rsyncd/oc-rsyncd.secrets"));
//! ```

use std::path::Path;

/// Canonical binary name exposed by the client wrapper packaged as `oc-rsync`.
#[doc(alias = "oc-rsync")]
pub const OC_CLIENT_PROGRAM_NAME: &str = "oc-rsync";

/// Canonical binary name exposed by the daemon wrapper packaged as `oc-rsyncd`.
#[doc(alias = "oc-rsyncd")]
pub const OC_DAEMON_PROGRAM_NAME: &str = "oc-rsyncd";

/// Directory that packages install for daemon configuration snippets.
#[doc(alias = "/etc/oc-rsyncd")]
pub const OC_DAEMON_CONFIG_DIR: &str = "/etc/oc-rsyncd";

/// Default configuration file path consumed by the daemon when no override is provided.
#[doc(alias = "/etc/oc-rsyncd/oc-rsyncd.conf")]
pub const OC_DAEMON_CONFIG_PATH: &str = "/etc/oc-rsyncd/oc-rsyncd.conf";

/// Default secrets file path consumed by the daemon when no override is provided.
#[doc(alias = "/etc/oc-rsyncd/oc-rsyncd.secrets")]
pub const OC_DAEMON_SECRETS_PATH: &str = "/etc/oc-rsyncd/oc-rsyncd.secrets";

/// Legacy configuration file path supported for backwards compatibility with upstream deployments.
#[doc(alias = "/etc/rsyncd.conf")]
pub const LEGACY_DAEMON_CONFIG_PATH: &str = "/etc/rsyncd.conf";

/// Legacy secrets file path supported for backwards compatibility with upstream deployments.
#[doc(alias = "/etc/rsyncd.secrets")]
pub const LEGACY_DAEMON_SECRETS_PATH: &str = "/etc/rsyncd.secrets";

/// Returns the canonical configuration path used by `oc-rsyncd`.
#[must_use]
pub fn oc_daemon_config_path() -> &'static Path {
    Path::new(OC_DAEMON_CONFIG_PATH)
}

/// Returns the canonical secrets path used by `oc-rsyncd`.
#[must_use]
pub fn oc_daemon_secrets_path() -> &'static Path {
    Path::new(OC_DAEMON_SECRETS_PATH)
}

/// Returns the legacy configuration path recognised for compatibility with upstream deployments.
#[must_use]
pub fn legacy_daemon_config_path() -> &'static Path {
    Path::new(LEGACY_DAEMON_CONFIG_PATH)
}

/// Returns the legacy secrets path recognised for compatibility with upstream deployments.
#[must_use]
pub fn legacy_daemon_secrets_path() -> &'static Path {
    Path::new(LEGACY_DAEMON_SECRETS_PATH)
}
