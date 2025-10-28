#![deny(unsafe_code)]

//! Branding constants and helpers shared across the workspace.
//!
//! The `branding` module centralises the program names and filesystem
//! locations that the workspace exposes publicly. Higher-level crates rely on
//! these constants when rendering banners or searching for configuration files
//! so that packaging, documentation, and runtime behaviour remain aligned. The
//! module records both the upstream-compatible `rsync`/`rsyncd` names (used by
//! symlinks and remote invocations) and the branded `oc-rsync`/`oc-rsyncd`
//! binaries together with convenience accessors that allow the CLI and daemon
//! crates to select the correct identity for a given execution mode. By
//! funnelling branding details through this module we keep string literals out
//! of business logic and make it trivial to update paths or names in one place.

mod brand;
mod constants;
mod detection;
mod override_env;
mod profile;

#[cfg(test)]
mod tests;

pub use brand::{Brand, BrandParseError, default_brand};
pub use constants::{
    BRAND_OVERRIDE_ENV, LEGACY_DAEMON_CONFIG_DIR, LEGACY_DAEMON_CONFIG_PATH,
    LEGACY_DAEMON_SECRETS_PATH, OC_CLIENT_PROGRAM_NAME, OC_DAEMON_CONFIG_DIR,
    OC_DAEMON_CONFIG_PATH, OC_DAEMON_PROGRAM_NAME, OC_DAEMON_SECRETS_PATH,
    UPSTREAM_CLIENT_PROGRAM_NAME, UPSTREAM_DAEMON_PROGRAM_NAME, brand_override_env_var,
};
pub use detection::{brand_for_program_name, detect_brand, resolve_brand_profile};
pub use profile::{
    BrandProfile, client_program_name, client_program_name_os_str, daemon_program_name,
    daemon_program_name_os_str, legacy_daemon_config_dir, legacy_daemon_config_path,
    legacy_daemon_secrets_path, oc_client_program_name, oc_client_program_name_os_str,
    oc_daemon_config_dir, oc_daemon_config_path, oc_daemon_program_name,
    oc_daemon_program_name_os_str, oc_daemon_secrets_path, oc_profile,
    upstream_client_program_name, upstream_daemon_program_name, upstream_profile,
};
