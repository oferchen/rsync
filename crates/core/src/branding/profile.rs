//! Branding profiles encapsulating canonical program names and filesystem paths.

use serde::Serialize;
use std::ffi::OsStr;
use std::path::Path;

use super::constants::{
    LEGACY_DAEMON_CONFIG_DIR, LEGACY_DAEMON_CONFIG_PATH, LEGACY_DAEMON_SECRETS_PATH,
    OC_CLIENT_PROGRAM_NAME, OC_DAEMON_CONFIG_DIR, OC_DAEMON_CONFIG_PATH, OC_DAEMON_PROGRAM_NAME,
    OC_DAEMON_SECRETS_PATH, UPSTREAM_CLIENT_PROGRAM_NAME, UPSTREAM_DAEMON_PROGRAM_NAME,
};

/// Describes the public-facing identity used by a binary distribution.
///
/// The structure captures the canonical client and daemon program names
/// together with the configuration directory, configuration file, and secrets
/// file that ship with the distribution. Higher layers select the appropriate
/// [`BrandProfile`] to render banners, locate configuration files, or display
/// diagnostic messages without duplicating string literals across the
/// codebase. The profiles are intentionally lightweight and `Copy` so they can
/// be used in constant contexts such as rustdoc examples and compile-time
/// assertions.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BrandProfile {
    client_program_name: &'static str,
    daemon_program_name: &'static str,
    daemon_config_dir: &'static str,
    daemon_config_path: &'static str,
    daemon_secrets_path: &'static str,
}

impl BrandProfile {
    /// Creates a new [`BrandProfile`] describing a branded distribution.
    #[must_use]
    pub const fn new(
        client_program_name: &'static str,
        daemon_program_name: &'static str,
        daemon_config_dir: &'static str,
        daemon_config_path: &'static str,
        daemon_secrets_path: &'static str,
    ) -> Self {
        Self {
            client_program_name,
            daemon_program_name,
            daemon_config_dir,
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

    /// Returns the daemon configuration directory as a string slice.
    #[must_use]
    pub const fn daemon_config_dir_str(&self) -> &'static str {
        self.daemon_config_dir
    }

    /// Returns the daemon configuration directory as a [`Path`].
    #[must_use]
    pub fn daemon_config_dir(&self) -> &'static Path {
        Path::new(self.daemon_config_dir)
    }

    /// Returns the daemon configuration path as a string slice.
    #[must_use]
    pub const fn daemon_config_path_str(&self) -> &'static str {
        self.daemon_config_path
    }

    /// Returns the daemon configuration path as a [`Path`].
    #[must_use]
    pub fn daemon_config_path(&self) -> &'static Path {
        Path::new(self.daemon_config_path)
    }

    /// Returns the daemon secrets path as a string slice.
    #[must_use]
    pub const fn daemon_secrets_path_str(&self) -> &'static str {
        self.daemon_secrets_path
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
    LEGACY_DAEMON_CONFIG_DIR,
    LEGACY_DAEMON_CONFIG_PATH,
    LEGACY_DAEMON_SECRETS_PATH,
);

const OC_PROFILE: BrandProfile = BrandProfile::new(
    OC_CLIENT_PROGRAM_NAME,
    OC_DAEMON_PROGRAM_NAME,
    OC_DAEMON_CONFIG_DIR,
    OC_DAEMON_CONFIG_PATH,
    OC_DAEMON_SECRETS_PATH,
);

/// Returns the upstream-compatible branding profile used by invocations that
/// employ the legacy program names.
#[must_use]
pub const fn upstream_profile() -> BrandProfile {
    UPSTREAM_PROFILE
}

/// Returns the oc-branded profile used by the canonical binaries.
#[must_use]
pub const fn oc_profile() -> BrandProfile {
    OC_PROFILE
}

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

/// Returns the canonical client program name for upstream-compatible binaries.
#[must_use]
pub const fn client_program_name() -> &'static str {
    upstream_client_program_name()
}

/// Returns the canonical client program name as an [`OsStr`].
#[must_use]
pub fn client_program_name_os_str() -> &'static OsStr {
    OsStr::new(client_program_name())
}

/// Returns the canonical daemon program name for upstream-compatible binaries.
#[must_use]
pub const fn daemon_program_name() -> &'static str {
    upstream_daemon_program_name()
}

/// Returns the canonical daemon program name as an [`OsStr`].
#[must_use]
pub fn daemon_program_name_os_str() -> &'static OsStr {
    OsStr::new(daemon_program_name())
}

/// Returns the branded client program name exposed as `oc-rsync`.
#[must_use]
pub const fn oc_client_program_name() -> &'static str {
    OC_CLIENT_PROGRAM_NAME
}

/// Returns the branded client program name as an [`OsStr`].
#[must_use]
pub fn oc_client_program_name_os_str() -> &'static OsStr {
    OsStr::new(oc_client_program_name())
}

/// Returns the branded daemon program name exposed as `oc-rsyncd`.
#[must_use]
pub const fn oc_daemon_program_name() -> &'static str {
    OC_DAEMON_PROGRAM_NAME
}

/// Returns the branded daemon program name as an [`OsStr`].
#[must_use]
pub fn oc_daemon_program_name_os_str() -> &'static OsStr {
    OsStr::new(oc_daemon_program_name())
}

/// Returns the canonical configuration directory used by `oc-rsyncd`.
#[must_use]
pub fn oc_daemon_config_dir() -> &'static Path {
    oc_profile().daemon_config_dir()
}

/// Returns the canonical configuration path used by `oc-rsyncd`.
#[must_use]
pub fn oc_daemon_config_path() -> &'static Path {
    oc_profile().daemon_config_path()
}

/// Returns the canonical secrets path used by `oc-rsyncd`.
#[must_use]
pub fn oc_daemon_secrets_path() -> &'static Path {
    oc_profile().daemon_secrets_path()
}

/// Returns the legacy configuration path recognised for compatibility with upstream deployments.
#[must_use]
pub fn legacy_daemon_config_path() -> &'static Path {
    upstream_profile().daemon_config_path()
}

/// Returns the legacy configuration directory recognised for upstream deployments.
#[must_use]
pub fn legacy_daemon_config_dir() -> &'static Path {
    upstream_profile().daemon_config_dir()
}

/// Returns the legacy secrets path recognised for compatibility with upstream deployments.
#[must_use]
pub fn legacy_daemon_secrets_path() -> &'static Path {
    upstream_profile().daemon_secrets_path()
}

/// Returns the preferred daemon configuration search order for this brand.
#[must_use]
pub(super) const fn config_path_candidate_strs(brand: super::Brand) -> [&'static str; 2] {
    match brand {
        super::Brand::Oc => [OC_DAEMON_CONFIG_PATH, LEGACY_DAEMON_CONFIG_PATH],
        super::Brand::Upstream => [LEGACY_DAEMON_CONFIG_PATH, OC_DAEMON_CONFIG_PATH],
    }
}

/// Returns the preferred daemon configuration search order as [`Path`]s.
#[must_use]
pub(super) fn config_path_candidates(brand: super::Brand) -> [&'static Path; 2] {
    let [primary, secondary] = config_path_candidate_strs(brand);
    [Path::new(primary), Path::new(secondary)]
}

/// Returns the preferred secrets-file search order for this brand.
#[must_use]
pub(super) const fn secrets_path_candidate_strs(brand: super::Brand) -> [&'static str; 2] {
    match brand {
        super::Brand::Oc => [OC_DAEMON_SECRETS_PATH, LEGACY_DAEMON_SECRETS_PATH],
        super::Brand::Upstream => [LEGACY_DAEMON_SECRETS_PATH, OC_DAEMON_SECRETS_PATH],
    }
}

/// Returns the preferred secrets-file search order as [`Path`]s.
#[must_use]
pub(super) fn secrets_path_candidates(brand: super::Brand) -> [&'static Path; 2] {
    let [primary, secondary] = secrets_path_candidate_strs(brand);
    [Path::new(primary), Path::new(secondary)]
}
