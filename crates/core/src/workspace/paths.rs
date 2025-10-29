use std::path::Path;

use super::constants::{
    DAEMON_CONFIG_DIR, DAEMON_CONFIG_PATH, DAEMON_SECRETS_PATH, LEGACY_DAEMON_CONFIG_DIR,
    LEGACY_DAEMON_CONFIG_PATH, LEGACY_DAEMON_SECRETS_PATH,
};

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

/// Returns the upstream-compatible daemon configuration directory as a [`Path`].
///
/// The helper mirrors [`metadata().legacy_daemon_config_dir()`](crate::workspace::Metadata::legacy_daemon_config_dir)
/// so callers that need to reference the historical installation layout (for
/// example, when validating compatibility symlinks or scanning legacy
/// configuration locations) can do so without repeating string literals. The
/// return value is derived from the workspace metadata populated by
/// `build.rs`, ensuring the Rust binaries, packaging assets, and documentation
/// remain aligned.
///
/// # Examples
///
/// ```
/// use rsync_core::workspace;
///
/// assert_eq!(
///     workspace::legacy_daemon_config_dir(),
///     std::path::Path::new(workspace::metadata().legacy_daemon_config_dir())
/// );
/// ```
#[must_use]
pub fn legacy_daemon_config_dir() -> &'static Path {
    Path::new(LEGACY_DAEMON_CONFIG_DIR)
}

/// Returns the upstream-compatible daemon configuration file as a [`Path`].
///
/// The workspace still honours `/etc/rsyncd.conf` for operators that rely on
/// the historical location. Exposing the value through this helper keeps the
/// string centralised so tests, packaging validation, and documentation can
/// reference the path without duplicating literals.
///
/// # Examples
///
/// ```
/// use rsync_core::workspace;
///
/// assert_eq!(
///     workspace::legacy_daemon_config_path(),
///     std::path::Path::new(workspace::metadata().legacy_daemon_config_path())
/// );
/// ```
#[must_use]
pub fn legacy_daemon_config_path() -> &'static Path {
    Path::new(LEGACY_DAEMON_CONFIG_PATH)
}

/// Returns the upstream-compatible daemon secrets file as a [`Path`].
///
/// Packaging still installs a legacy secrets file for deployments that expect
/// the upstream layout. Centralising the path avoids drift between the
/// workspace metadata, runtime lookups, and the packaged configuration
/// templates.
///
/// # Examples
///
/// ```
/// use rsync_core::workspace;
///
/// assert_eq!(
///     workspace::legacy_daemon_secrets_path(),
///     std::path::Path::new(workspace::metadata().legacy_daemon_secrets_path())
/// );
/// ```
#[must_use]
pub fn legacy_daemon_secrets_path() -> &'static Path {
    Path::new(LEGACY_DAEMON_SECRETS_PATH)
}
