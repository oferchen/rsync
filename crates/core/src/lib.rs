#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc = include_str!("../README.md")]

/// Shared daemon authentication helpers.
pub mod auth;
/// Bandwidth parsing utilities shared by CLI and daemon entry points.
pub mod bandwidth;
/// Branding constants shared across binaries and packaging layers.
pub mod branding;
/// Client orchestration helpers consumed by the CLI binary.
pub mod client;
/// Helpers for interpreting fallback environment overrides shared across crates.
pub mod fallback;
/// Message formatting utilities shared across workspace binaries.
pub mod message;
/// Version constants and capability helpers used by CLI and daemon entry points.
pub mod version;
/// Workspace metadata derived from the repository `Cargo.toml`.
pub mod workspace;
