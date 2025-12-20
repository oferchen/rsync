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
pub use ::branding::branding;
/// Client orchestration helpers consumed by the CLI binary.
pub mod client;
/// Helpers for interpreting fallback environment overrides shared across crates.
pub mod fallback;
/// Message formatting utilities shared across workspace binaries.
pub mod message;
/// Server orchestration helpers consumed by the CLI binary.
/// Server orchestration helpers consumed by CLI and embedding entry points.
pub mod server;
/// Version constants and capability helpers used by CLI and daemon entry points.
pub mod version;
/// Workspace metadata derived from the repository `Cargo.toml`.
pub use ::branding::workspace;

/// Upstream-compatible alias for the `walk` crate.
///
/// This provides a transitional alias matching upstream rsync terminology,
/// where file list operations are grouped under `flist.c`. The alias helps
/// maintainers familiar with the C codebase locate functionality without
/// breaking existing code using the `walk` name.
///
/// # Upstream Reference
///
/// - `flist.c` - Upstream file list building and transmission
///
/// # Examples
///
/// ```ignore
/// // Both names access the same functionality (from within this crate)
/// use crate::walk::WalkBuilder;
/// // use crate::flist::WalkBuilder;  // Equivalent
/// ```
pub use walk as flist;
