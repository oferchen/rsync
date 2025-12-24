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

/// File list generation and traversal (mirrors upstream `flist.c`).
///
/// This crate handles file list building and transmission, matching the
/// functionality in upstream rsync's `flist.c`. The name `flist` aligns
/// with upstream terminology for easier cross-referencing.
///
/// # Upstream Reference
///
/// - `flist.c` - File list building and transmission
pub use ::flist;

/// Socket and pipe I/O utilities (mirrors upstream `io.c`).
///
/// This crate provides multiplexed I/O, negotiation streams, and transport
/// helpers matching upstream rsync's `io.c`. The module is imported as
/// `rsync_io` to avoid conflicts with `std::io`.
///
/// # Upstream Reference
///
/// - `io.c` - Socket and pipe I/O utilities
pub use rsync_io as io;
