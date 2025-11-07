//! String-based workspace metadata exported as compile-time constants.
//!
//! These values originate from the workspace manifest and are surfaced via
//! `cargo:rustc-env` assignments in the build script. Keeping the literals in a
//! dedicated module lets other helpers share them without re-reading the
//! manifest.

/// Canonical brand identifier configured for this distribution.
#[doc(alias = "oc")]
pub const BRAND: &str = env!("OC_RSYNC_WORKSPACE_BRAND");

/// Returns the canonical brand identifier configured for this distribution.
///
/// This helper avoids exposing the raw environment variable constant to callers
/// while still participating in constant evaluation. Code that only needs the
/// brand string can call [`brand()`] directly instead of materialising the full
/// [`Metadata`](super::Metadata) snapshot.
///
/// # Examples
///
/// ```
/// use oc_rsync_core::workspace;
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
/// can rely on this helper instead of reading it through [`Metadata`](super::Metadata).
///
/// # Examples
///
/// ```
/// use oc_rsync_core::workspace;
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
/// without constructing a [`Metadata`](super::Metadata) snapshot.
///
/// # Examples
///
/// ```
/// use oc_rsync_core::workspace;
///
/// assert_eq!(workspace::rust_version(), workspace::metadata().rust_version());
/// ```
#[must_use]
pub const fn rust_version() -> &'static str {
    RUST_VERSION
}

/// Canonical client binary name shipped with the distribution.
#[doc(alias = "oc-rsync")]
pub const CLIENT_PROGRAM_NAME: &str = env!("OC_RSYNC_WORKSPACE_CLIENT_BIN");

/// Returns the canonical client binary name shipped with the distribution.
///
/// This helper mirrors [`Metadata::client_program_name`](super::Metadata::client_program_name)
/// while remaining `const`, which simplifies usage from static contexts.
///
/// # Examples
///
/// ```
/// use oc_rsync_core::workspace;
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
/// use oc_rsync_core::workspace;
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
/// use oc_rsync_core::workspace;
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
/// use oc_rsync_core::workspace;
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
/// use oc_rsync_core::workspace;
///
/// assert_eq!(workspace::source_url(), workspace::metadata().source_url());
/// ```
#[must_use]
pub const fn source_url() -> &'static str {
    SOURCE_URL
}
