//! Workspace-specific string constants for branded binaries and configuration paths.

use crate::workspace;

/// Environment variable that forces a specific [`Brand`][crate::branding::Brand] at runtime.
#[doc(alias = "OC_RSYNC_BRAND")]
pub const BRAND_OVERRIDE_ENV: &str = "OC_RSYNC_BRAND";

/// Returns the environment variable that forces a specific [`Brand`][crate::branding::Brand] at runtime.
#[must_use]
pub const fn brand_override_env_var() -> &'static str {
    BRAND_OVERRIDE_ENV
}

/// Canonical program name used by upstream `rsync` releases.
#[doc(alias = "rsync")]
pub const UPSTREAM_CLIENT_PROGRAM_NAME: &str = workspace::metadata().legacy_client_program_name();

/// Canonical program name used by upstream `rsync` daemon releases.
#[doc(alias = "rsync")]
pub const UPSTREAM_DAEMON_PROGRAM_NAME: &str = workspace::metadata().legacy_daemon_program_name();

/// Canonical binary name exposed by the client wrapper packaged as `oc-rsync`.
#[doc(alias = "oc-rsync")]
pub const OC_CLIENT_PROGRAM_NAME: &str = workspace::metadata().client_program_name();

/// Canonical binary name exposed by the branded daemon entrypoint (`oc-rsync`).
#[doc(alias = "oc-rsync")]
pub const OC_DAEMON_PROGRAM_NAME: &str = workspace::metadata().daemon_program_name();

/// Directory that packages install for daemon configuration snippets.
#[doc(alias = "/etc/oc-rsyncd")]
pub const OC_DAEMON_CONFIG_DIR: &str = workspace::metadata().daemon_config_dir();

/// Default configuration file path consumed by the daemon when no override is provided.
#[doc(alias = "/etc/oc-rsyncd/oc-rsyncd.conf")]
pub const OC_DAEMON_CONFIG_PATH: &str = workspace::metadata().daemon_config_path();

/// Default secrets file path consumed by the daemon when no override is provided.
#[doc(alias = "/etc/oc-rsyncd/oc-rsyncd.secrets")]
pub const OC_DAEMON_SECRETS_PATH: &str = workspace::metadata().daemon_secrets_path();

/// Legacy configuration file path supported for backwards compatibility with upstream deployments.
#[doc(alias = "/etc/rsyncd.conf")]
pub const LEGACY_DAEMON_CONFIG_PATH: &str = workspace::metadata().legacy_daemon_config_path();

/// Legacy configuration directory that hosts upstream-compatible configuration files.
#[doc(alias = "/etc")]
pub const LEGACY_DAEMON_CONFIG_DIR: &str = workspace::metadata().legacy_daemon_config_dir();

/// Legacy secrets file path supported for backwards compatibility with upstream deployments.
#[doc(alias = "/etc/rsyncd.secrets")]
pub const LEGACY_DAEMON_SECRETS_PATH: &str = workspace::metadata().legacy_daemon_secrets_path();

/// Canonical source repository URL advertised by version banners and documentation.
pub const SOURCE_URL: &str = workspace::metadata().source_url();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brand_override_env_is_non_empty() {
        assert!(!BRAND_OVERRIDE_ENV.is_empty());
    }

    #[test]
    fn brand_override_env_var_matches_const() {
        assert_eq!(brand_override_env_var(), BRAND_OVERRIDE_ENV);
    }

    #[test]
    fn upstream_client_program_name_is_rsync() {
        assert_eq!(UPSTREAM_CLIENT_PROGRAM_NAME, "rsync");
    }

    #[test]
    fn upstream_daemon_program_name_is_rsync() {
        assert_eq!(UPSTREAM_DAEMON_PROGRAM_NAME, "rsync");
    }

    #[test]
    fn oc_client_program_name_is_non_empty() {
        assert!(!OC_CLIENT_PROGRAM_NAME.is_empty());
    }

    #[test]
    fn oc_daemon_program_name_is_non_empty() {
        assert!(!OC_DAEMON_PROGRAM_NAME.is_empty());
    }

    #[test]
    fn oc_daemon_config_dir_is_non_empty() {
        assert!(!OC_DAEMON_CONFIG_DIR.is_empty());
    }

    #[test]
    fn oc_daemon_config_path_is_non_empty() {
        assert!(!OC_DAEMON_CONFIG_PATH.is_empty());
    }

    #[test]
    fn oc_daemon_secrets_path_is_non_empty() {
        assert!(!OC_DAEMON_SECRETS_PATH.is_empty());
    }

    #[test]
    fn legacy_daemon_config_path_is_non_empty() {
        assert!(!LEGACY_DAEMON_CONFIG_PATH.is_empty());
    }

    #[test]
    fn legacy_daemon_config_dir_is_non_empty() {
        assert!(!LEGACY_DAEMON_CONFIG_DIR.is_empty());
    }

    #[test]
    fn legacy_daemon_secrets_path_is_non_empty() {
        assert!(!LEGACY_DAEMON_SECRETS_PATH.is_empty());
    }

    #[test]
    fn source_url_is_non_empty() {
        assert!(!SOURCE_URL.is_empty());
    }

    #[test]
    fn source_url_starts_with_https() {
        assert!(SOURCE_URL.starts_with("https://"));
    }
}
