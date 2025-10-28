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

/// Describes the public-facing identity used by a binary distribution.
///
/// The structure captures the canonical client and daemon program names
/// together with the configuration and secrets paths that ship with the
/// distribution. Higher layers select the appropriate [`BrandProfile`] to
/// render banners, locate configuration files, or display diagnostic
/// messages without duplicating string literals across the codebase. The
/// profiles are intentionally lightweight and `Copy` so they can be used in
/// constant contexts such as rustdoc examples and compile-time assertions.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BrandProfile {
    client_program_name: &'static str,
    daemon_program_name: &'static str,
    daemon_config_path: &'static str,
    daemon_secrets_path: &'static str,
}

impl BrandProfile {
    /// Creates a new [`BrandProfile`] describing a branded distribution.
    #[must_use]
    pub const fn new(
        client_program_name: &'static str,
        daemon_program_name: &'static str,
        daemon_config_path: &'static str,
        daemon_secrets_path: &'static str,
    ) -> Self {
        Self {
            client_program_name,
            daemon_program_name,
            daemon_config_path,
            daemon_secrets_path,
        }
    }

    /// Returns the client program name associated with the profile.
    #[must_use]
    pub const fn client_program_name(&self) -> &'static str {
        self.client_program_name
    }

    /// Returns the daemon program name associated with the profile.
    #[must_use]
    pub const fn daemon_program_name(&self) -> &'static str {
        self.daemon_program_name
    }

    /// Returns the daemon configuration path as a string slice.
    #[must_use]
    pub const fn daemon_config_path_str(&self) -> &'static str {
        self.daemon_config_path
    }

    /// Returns the daemon secrets path as a string slice.
    #[must_use]
    pub const fn daemon_secrets_path_str(&self) -> &'static str {
        self.daemon_secrets_path
    }

    /// Returns the daemon configuration path as a [`Path`].
    #[must_use]
    pub fn daemon_config_path(&self) -> &'static Path {
        Path::new(self.daemon_config_path)
    }

    /// Returns the daemon secrets path as a [`Path`].
    #[must_use]
    pub fn daemon_secrets_path(&self) -> &'static Path {
        Path::new(self.daemon_secrets_path)
    }
}

const UPSTREAM_PROFILE: BrandProfile = BrandProfile::new(
    UPSTREAM_CLIENT_PROGRAM_NAME,
    UPSTREAM_DAEMON_PROGRAM_NAME,
    LEGACY_DAEMON_CONFIG_PATH,
    LEGACY_DAEMON_SECRETS_PATH,
);

const OC_PROFILE: BrandProfile = BrandProfile::new(
    OC_CLIENT_PROGRAM_NAME,
    OC_DAEMON_PROGRAM_NAME,
    OC_DAEMON_CONFIG_PATH,
    OC_DAEMON_SECRETS_PATH,
);

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
    UPSTREAM_PROFILE.client_program_name()
}

/// Returns the canonical upstream daemon program name (`rsyncd`).
#[must_use]
pub const fn upstream_daemon_program_name() -> &'static str {
    UPSTREAM_PROFILE.daemon_program_name()
}

/// Returns the branded client program name (`oc-rsync`).
#[must_use]
pub const fn oc_client_program_name() -> &'static str {
    OC_PROFILE.client_program_name()
}

/// Returns the branded daemon program name (`oc-rsyncd`).
#[must_use]
pub const fn oc_daemon_program_name() -> &'static str {
    OC_PROFILE.daemon_program_name()
}

/// Returns the canonical configuration path used by `oc-rsyncd`.
#[must_use]
pub fn oc_daemon_config_path() -> &'static Path {
    OC_PROFILE.daemon_config_path()
}

/// Returns the canonical client program name for upstream-compatible binaries.
#[must_use]
pub const fn client_program_name() -> &'static str {
    upstream_client_program_name()
}

/// Returns the canonical daemon program name for upstream-compatible binaries.
#[must_use]
pub const fn daemon_program_name() -> &'static str {
    upstream_daemon_program_name()
}

/// Returns the canonical secrets path used by `oc-rsyncd`.
#[must_use]
pub fn oc_daemon_secrets_path() -> &'static Path {
    OC_PROFILE.daemon_secrets_path()
}

/// Returns the legacy configuration path recognised for compatibility with upstream deployments.
#[must_use]
pub fn legacy_daemon_config_path() -> &'static Path {
    UPSTREAM_PROFILE.daemon_config_path()
}

/// Returns the legacy secrets path recognised for compatibility with upstream deployments.
#[must_use]
pub fn legacy_daemon_secrets_path() -> &'static Path {
    UPSTREAM_PROFILE.daemon_secrets_path()
}

/// Returns the upstream-compatible branding profile used by compatibility wrappers.
#[must_use]
pub const fn upstream_profile() -> BrandProfile {
    UPSTREAM_PROFILE
}

/// Returns the oc-branded profile used by the canonical binaries.
///
/// # Examples
///
/// ```
/// use rsync_core::branding;
///
/// let profile = branding::oc_profile();
/// assert_eq!(profile.client_program_name(), "oc-rsync");
/// assert_eq!(profile.daemon_program_name(), "oc-rsyncd");
/// assert_eq!(profile.daemon_config_path(), branding::oc_daemon_config_path());
/// assert_eq!(profile.daemon_secrets_path(), branding::oc_daemon_secrets_path());
/// ```
#[must_use]
pub const fn oc_profile() -> BrandProfile {
    OC_PROFILE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_names_are_consistent() {
        let upstream = upstream_profile();
        assert_eq!(upstream.client_program_name(), UPSTREAM_CLIENT_PROGRAM_NAME);
        assert_eq!(upstream.daemon_program_name(), UPSTREAM_DAEMON_PROGRAM_NAME);

        let oc = oc_profile();
        assert_eq!(oc.client_program_name(), OC_CLIENT_PROGRAM_NAME);
        assert_eq!(oc.daemon_program_name(), OC_DAEMON_PROGRAM_NAME);
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

    #[test]
    fn profile_helpers_align_with_functions() {
        assert_eq!(
            upstream_profile().client_program_name(),
            upstream_client_program_name()
        );
        assert_eq!(
            upstream_profile().daemon_program_name(),
            upstream_daemon_program_name()
        );
        assert_eq!(oc_profile().client_program_name(), oc_client_program_name());
        assert_eq!(oc_profile().daemon_program_name(), oc_daemon_program_name());
        assert_eq!(oc_profile().daemon_config_path(), oc_daemon_config_path());
        assert_eq!(oc_profile().daemon_secrets_path(), oc_daemon_secrets_path());
        assert_eq!(
            upstream_profile().daemon_config_path(),
            legacy_daemon_config_path()
        );
        assert_eq!(
            upstream_profile().daemon_secrets_path(),
            legacy_daemon_secrets_path()
        );
    }
}
