#![deny(unsafe_code)]

//! Rendering utilities for human-readable `--version` output.
//!
//! This module splits the configuration structures that describe compiled
//! capabilities from the renderer that formats user-visible banners. The
//! resulting pieces mirror upstream `print_rsync_version()` while keeping
//! individual files concise for easier review and maintenance.

mod config;
mod renderer;

pub use config::{VersionInfoConfig, VersionInfoConfigBuilder};
pub use renderer::VersionInfoReport;
#[cfg(test)]
pub(crate) use renderer::{
    default_checksum_algorithms, default_compress_algorithms, default_daemon_auth_algorithms,
};
