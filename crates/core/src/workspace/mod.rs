#![deny(unsafe_code)]

//! Workspace metadata exported as compile-time constants.
//!
//! The [`crates/core`](crate) build script reads `[workspace.metadata.oc_rsync]`
//! from the repository `Cargo.toml` and emits the values below as
//! `cargo:rustc-env` pairs. Centralising the brand and packaging details here
//! keeps the binary front-ends and supporting crates free from duplicated string
//! literals. Callers should prefer these helpers instead of hard-coding program
//! names or configuration paths. Consumers that need the entire metadata set can
//! use [`metadata()`](crate::workspace::metadata) to obtain a snapshot that mirrors
//! the manifest entries.

mod constants;
mod json;
mod metadata;
mod paths;
mod protocol;

pub use constants::{
    BRAND, CLIENT_PROGRAM_NAME, DAEMON_CONFIG_DIR, DAEMON_CONFIG_PATH, DAEMON_PROGRAM_NAME,
    DAEMON_SECRETS_PATH, DAEMON_WRAPPER_PROGRAM_NAME, LEGACY_CLIENT_PROGRAM_NAME,
    LEGACY_DAEMON_CONFIG_DIR, LEGACY_DAEMON_CONFIG_PATH, LEGACY_DAEMON_PROGRAM_NAME,
    LEGACY_DAEMON_SECRETS_PATH, RUST_VERSION, SOURCE_URL, UPSTREAM_VERSION, brand,
    client_program_name, daemon_program_name, daemon_wrapper_program_name,
    legacy_client_program_name, legacy_daemon_program_name, rust_version, source_url,
    upstream_version,
};
pub use json::{metadata_json, metadata_json_pretty};
pub use metadata::{Metadata, metadata};
pub use paths::{
    daemon_config_dir, daemon_config_path, daemon_secrets_path, legacy_daemon_config_dir,
    legacy_daemon_config_path, legacy_daemon_secrets_path,
};
pub use protocol::{PROTOCOL_VERSION, protocol_version_nonzero_u8, protocol_version_u8};

#[cfg(test)]
mod tests;
