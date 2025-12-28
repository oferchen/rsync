//! String-based workspace metadata exported as compile-time constants.
//!
//! These values originate from the workspace manifest and are compiled into the
//! crate by the build script. Keeping the literals in a dedicated module lets
//! other helpers share them without re-reading the manifest.

/// Canonical brand identifier configured for this distribution.
#[doc(alias = "oc")]
pub const BRAND: &str = crate::generated::BRAND;

/// Returns the canonical brand identifier configured for this distribution.
///
/// This helper avoids exposing the raw constant directly to callers while still
/// participating in constant evaluation. Code that only needs the brand string
/// can call [`brand()`] instead of materialising the full [`Metadata`](super::Metadata)
/// snapshot.
///
/// # Examples
///
/// ```
/// use branding::workspace;
///
/// assert_eq!(workspace::brand(), workspace::metadata().brand());
/// ```
#[must_use]
pub const fn brand() -> &'static str {
    BRAND
}

/// Upstream rsync base version targeted by this build.
#[doc(alias = "3.4.1")]
pub const UPSTREAM_VERSION: &str = crate::generated::UPSTREAM_VERSION;

/// Returns the upstream rsync base version targeted by this build.
///
/// The value matches the upstream release string rendered in `--version`
/// output and documentation banners. Callers that only need the version text
/// can rely on this helper instead of reading it through [`Metadata`](super::Metadata).
///
/// # Examples
///
/// ```
/// use branding::workspace;
///
/// assert_eq!(workspace::upstream_version(), workspace::metadata().upstream_version());
/// ```
#[must_use]
pub const fn upstream_version() -> &'static str {
    UPSTREAM_VERSION
}

/// Full oc-rsync release version string advertised by binaries.
pub const RUST_VERSION: &str = crate::generated::RUST_VERSION;

/// Returns the oc-rsync release version string advertised by binaries.
///
/// The helper is used by banner renderers that need the branded identifier
/// without constructing a [`Metadata`](super::Metadata) snapshot.
///
/// # Examples
///
/// ```
/// use branding::workspace;
///
/// assert_eq!(workspace::rust_version(), workspace::metadata().rust_version());
/// ```
#[must_use]
pub const fn rust_version() -> &'static str {
    RUST_VERSION
}

/// Canonical client binary name shipped with the distribution.
#[doc(alias = "oc-rsync")]
pub const CLIENT_PROGRAM_NAME: &str = crate::generated::CLIENT_PROGRAM_NAME;

/// Returns the canonical client binary name shipped with the distribution.
///
/// This helper mirrors [`Metadata::client_program_name`](super::Metadata::client_program_name)
/// while remaining `const`, which simplifies usage from static contexts.
///
/// # Examples
///
/// ```
/// use branding::workspace;
///
/// assert_eq!(workspace::client_program_name(), workspace::metadata().client_program_name());
/// ```
#[must_use]
pub const fn client_program_name() -> &'static str {
    CLIENT_PROGRAM_NAME
}

/// Canonical daemon binary name shipped with the distribution.
#[doc(alias = "oc-rsync")]
pub const DAEMON_PROGRAM_NAME: &str = crate::generated::DAEMON_PROGRAM_NAME;

/// Returns the canonical daemon binary name shipped with the distribution.
///
/// # Examples
///
/// ```
/// use branding::workspace;
///
/// assert_eq!(workspace::daemon_program_name(), workspace::metadata().daemon_program_name());
/// ```
#[must_use]
pub const fn daemon_program_name() -> &'static str {
    DAEMON_PROGRAM_NAME
}

/// Upstream-compatible client binary name used for compatibility symlinks.
#[doc(alias = "rsync")]
pub const LEGACY_CLIENT_PROGRAM_NAME: &str = crate::generated::LEGACY_CLIENT_PROGRAM_NAME;

/// Returns the upstream-compatible client binary name used for compatibility symlinks.
///
/// # Examples
///
/// ```
/// use branding::workspace;
///
/// assert_eq!(workspace::legacy_client_program_name(), workspace::metadata().legacy_client_program_name());
/// ```
#[must_use]
pub const fn legacy_client_program_name() -> &'static str {
    LEGACY_CLIENT_PROGRAM_NAME
}

/// Upstream-compatible daemon binary name used for compatibility symlinks.
#[doc(alias = "rsync")]
pub const LEGACY_DAEMON_PROGRAM_NAME: &str = crate::generated::LEGACY_DAEMON_PROGRAM_NAME;

/// Returns the upstream-compatible daemon binary name used for compatibility symlinks.
///
/// # Examples
///
/// ```
/// use branding::workspace;
///
/// assert_eq!(workspace::legacy_daemon_program_name(), workspace::metadata().legacy_daemon_program_name());
/// ```
#[must_use]
pub const fn legacy_daemon_program_name() -> &'static str {
    LEGACY_DAEMON_PROGRAM_NAME
}

/// Configuration directory installed alongside the branded daemon.
#[doc(alias = "/etc/oc-rsync")]
pub const DAEMON_CONFIG_DIR: &str = crate::generated::DAEMON_CONFIG_DIR;

/// Default daemon configuration file path for the branded binary.
#[doc(alias = "/etc/oc-rsyncd/oc-rsyncd.conf")]
pub const DAEMON_CONFIG_PATH: &str = crate::generated::DAEMON_CONFIG_PATH;

/// Default daemon secrets file path for the branded binary.
#[doc(alias = "/etc/oc-rsyncd/oc-rsyncd.secrets")]
pub const DAEMON_SECRETS_PATH: &str = crate::generated::DAEMON_SECRETS_PATH;

/// Legacy configuration directory honoured for upstream compatibility.
#[doc(alias = "/etc")]
pub const LEGACY_DAEMON_CONFIG_DIR: &str = crate::generated::LEGACY_DAEMON_CONFIG_DIR;

/// Legacy daemon configuration file path honoured for upstream compatibility.
#[doc(alias = "/etc/rsyncd.conf")]
pub const LEGACY_DAEMON_CONFIG_PATH: &str = crate::generated::LEGACY_DAEMON_CONFIG_PATH;

/// Legacy daemon secrets file path honoured for upstream compatibility.
#[doc(alias = "/etc/rsyncd.secrets")]
pub const LEGACY_DAEMON_SECRETS_PATH: &str = crate::generated::LEGACY_DAEMON_SECRETS_PATH;

/// Source repository URL advertised by `--version` output.
pub const SOURCE_URL: &str = crate::generated::SOURCE_URL;

/// Returns the source repository URL advertised by version banners.
///
/// # Examples
///
/// ```
/// use branding::workspace;
///
/// assert_eq!(workspace::source_url(), workspace::metadata().source_url());
/// ```
#[must_use]
pub const fn source_url() -> &'static str {
    SOURCE_URL
}

#[cfg(test)]
#[allow(clippy::const_is_empty)]
mod tests {
    use super::*;

    #[test]
    fn brand_constant_is_not_empty() {
        assert!(!BRAND.is_empty());
    }

    #[test]
    fn brand_function_returns_same_as_constant() {
        assert_eq!(brand(), BRAND);
    }

    #[test]
    fn upstream_version_constant_is_not_empty() {
        assert!(!UPSTREAM_VERSION.is_empty());
    }

    #[test]
    fn upstream_version_function_returns_same_as_constant() {
        assert_eq!(upstream_version(), UPSTREAM_VERSION);
    }

    #[test]
    fn rust_version_constant_is_not_empty() {
        assert!(!RUST_VERSION.is_empty());
    }

    #[test]
    fn rust_version_function_returns_same_as_constant() {
        assert_eq!(rust_version(), RUST_VERSION);
    }

    #[test]
    fn client_program_name_constant_is_not_empty() {
        assert!(!CLIENT_PROGRAM_NAME.is_empty());
    }

    #[test]
    fn client_program_name_function_returns_same_as_constant() {
        assert_eq!(client_program_name(), CLIENT_PROGRAM_NAME);
    }

    #[test]
    fn daemon_program_name_constant_is_not_empty() {
        assert!(!DAEMON_PROGRAM_NAME.is_empty());
    }

    #[test]
    fn daemon_program_name_function_returns_same_as_constant() {
        assert_eq!(daemon_program_name(), DAEMON_PROGRAM_NAME);
    }

    #[test]
    fn legacy_client_program_name_constant_is_not_empty() {
        assert!(!LEGACY_CLIENT_PROGRAM_NAME.is_empty());
    }

    #[test]
    fn legacy_client_program_name_function_returns_same_as_constant() {
        assert_eq!(legacy_client_program_name(), LEGACY_CLIENT_PROGRAM_NAME);
    }

    #[test]
    fn legacy_daemon_program_name_constant_is_not_empty() {
        assert!(!LEGACY_DAEMON_PROGRAM_NAME.is_empty());
    }

    #[test]
    fn legacy_daemon_program_name_function_returns_same_as_constant() {
        assert_eq!(legacy_daemon_program_name(), LEGACY_DAEMON_PROGRAM_NAME);
    }

    #[test]
    fn daemon_config_dir_constant_is_not_empty() {
        assert!(!DAEMON_CONFIG_DIR.is_empty());
    }

    #[test]
    fn daemon_config_path_constant_is_not_empty() {
        assert!(!DAEMON_CONFIG_PATH.is_empty());
    }

    #[test]
    fn daemon_secrets_path_constant_is_not_empty() {
        assert!(!DAEMON_SECRETS_PATH.is_empty());
    }

    #[test]
    fn legacy_daemon_config_dir_constant_is_not_empty() {
        assert!(!LEGACY_DAEMON_CONFIG_DIR.is_empty());
    }

    #[test]
    fn legacy_daemon_config_path_constant_is_not_empty() {
        assert!(!LEGACY_DAEMON_CONFIG_PATH.is_empty());
    }

    #[test]
    fn legacy_daemon_secrets_path_constant_is_not_empty() {
        assert!(!LEGACY_DAEMON_SECRETS_PATH.is_empty());
    }

    #[test]
    fn source_url_constant_is_not_empty() {
        assert!(!SOURCE_URL.is_empty());
    }

    #[test]
    fn source_url_function_returns_same_as_constant() {
        assert_eq!(source_url(), SOURCE_URL);
    }
}
