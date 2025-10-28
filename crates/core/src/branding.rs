#![deny(unsafe_code)]

//! Branding constants shared across the workspace.
//!
//! The `branding` module centralises the program names and filesystem
//! locations that the workspace exposes publicly. Higher-level crates rely on
//! these constants when rendering banners or searching for configuration files
//! so that packaging, documentation, and runtime behaviour remain aligned. The
//! module records both the upstream-compatible `rsync`/`rsyncd` names and the
//! branded `oc-rsync`/`oc-rsyncd` wrappers together with convenience accessors
//! that allow the CLI and daemon crates to select the correct identity for a
//! given execution mode. By funnelling branding details through this module we
//! keep string literals out of business logic and make it trivial to update
//! paths or names in one place.
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

/// Canonical program name used by upstream `rsync` releases.
#[doc(alias = "rsync")]
pub const UPSTREAM_CLIENT_PROGRAM_NAME: &str = "rsync";

/// Canonical program name used by upstream `rsyncd` daemon releases.
#[doc(alias = "rsyncd")]
pub const UPSTREAM_DAEMON_PROGRAM_NAME: &str = "rsyncd";

/// Canonical binary name exposed by the client wrapper packaged as `oc-rsync`.
#[doc(alias = "oc-rsync")]
pub const OC_CLIENT_PROGRAM_NAME: &str = "oc-rsync";

/// Canonical binary name exposed by the branded daemon wrapper packaged as `oc-rsyncd`.
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

/// Returns the canonical upstream client program name (`rsync`).
#[must_use]
pub const fn upstream_client_program_name() -> &'static str {
    UPSTREAM_CLIENT_PROGRAM_NAME
}

/// Returns the canonical upstream daemon program name (`rsyncd`).
#[must_use]
pub const fn upstream_daemon_program_name() -> &'static str {
    UPSTREAM_DAEMON_PROGRAM_NAME
}

/// Returns the branded client program name (`oc-rsync`).
#[must_use]
pub const fn oc_client_program_name() -> &'static str {
    OC_CLIENT_PROGRAM_NAME
}

/// Returns the branded daemon program name (`oc-rsyncd`).
#[must_use]
pub const fn oc_daemon_program_name() -> &'static str {
    OC_DAEMON_PROGRAM_NAME
}

/// Returns the canonical configuration path used by `oc-rsyncd`.
#[must_use]
pub fn oc_daemon_config_path() -> &'static Path {
    Path::new(OC_DAEMON_CONFIG_PATH)
}

/// Returns the canonical client program name for upstream-compatible binaries.
#[must_use]
pub const fn client_program_name() -> &'static str {
    CLIENT_PROGRAM_NAME
}

/// Returns the canonical daemon program name for upstream-compatible binaries.
#[must_use]
pub const fn daemon_program_name() -> &'static str {
    DAEMON_PROGRAM_NAME
}

/// Returns the branded client program name exposed as `oc-rsync`.
#[must_use]
pub const fn oc_client_program_name() -> &'static str {
    OC_CLIENT_PROGRAM_NAME
}

/// Returns the branded daemon program name exposed as `oc-rsyncd`.
#[must_use]
pub const fn oc_daemon_program_name() -> &'static str {
    OC_DAEMON_PROGRAM_NAME
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_names_are_consistent() {
        assert_eq!(client_program_name(), CLIENT_PROGRAM_NAME);
        assert_eq!(daemon_program_name(), DAEMON_PROGRAM_NAME);
        assert_eq!(oc_client_program_name(), OC_CLIENT_PROGRAM_NAME);
        assert_eq!(oc_daemon_program_name(), OC_DAEMON_PROGRAM_NAME);
    }

    #[test]
    fn oc_paths_match_expected_locations() {
        assert_eq!(oc_daemon_config_path(), Path::new(OC_DAEMON_CONFIG_PATH));
        assert_eq!(oc_daemon_secrets_path(), Path::new(OC_DAEMON_SECRETS_PATH));
        assert_eq!(
            legacy_daemon_config_path(),
            Path::new(LEGACY_DAEMON_CONFIG_PATH)
        );
        assert_eq!(
            legacy_daemon_secrets_path(),
            Path::new(LEGACY_DAEMON_SECRETS_PATH)
        );
    }
}
