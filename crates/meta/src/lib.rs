#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! # Overview
//!
//! `rsync_meta` centralises metadata preservation helpers used by the Rust
//! rsync workspace. The crate focuses on reproducing upstream `rsync`
//! semantics for permission bits and timestamp propagation when copying files,
//! directories, symbolic links, device nodes, and FIFOs on local filesystems.
//! Higher layers wire the helpers into transfer pipelines so metadata handling
//! remains consistent across client and daemon roles.
//!
//! # Design
//!
//! The crate exposes three primary entry points:
//! - [`apply_file_metadata`] sets permissions and timestamps on regular files.
//! - [`apply_directory_metadata`] mirrors metadata for directories.
//! - [`apply_symlink_metadata`] applies timestamp changes to symbolic links
//!   without following the link target.
//! - [`create_fifo`] materialises FIFOs before metadata is applied, allowing
//!   higher layers to reproduce upstream handling of named pipes.
//! - [`create_device_node`] builds character and block device nodes from the
//!   metadata observed on the source filesystem so downstream code can
//!   faithfully mirror special files during local copies.
//!
//! Errors are reported via [`MetadataError`], which stores the failing path and
//! operation context. Callers can integrate the error into user-facing
//! diagnostics while retaining the underlying [`std::io::Error`].
//!
//! # Invariants
//!
//! - All helpers avoid following symbolic links unless explicitly requested.
//! - Permission preservation is best-effort on non-Unix platforms where only
//!   the read-only flag may be applied.
//! - Timestamp propagation always uses nanosecond precision via the
//!   [`filetime`] crate.
//!
//! # Errors
//!
//! Operations surface [`MetadataError`] when the underlying filesystem call
//! fails. The error exposes the context string, path, and original [`std::io::Error`]
//! so higher layers can render diagnostics consistent with upstream `rsync`.
//!
//! # Examples
//!
//! ```
//! use rsync_meta::{apply_file_metadata, MetadataError};
//! use std::fs;
//! use std::path::Path;
//!
//! # fn demo() -> Result<(), MetadataError> {
//! let source = Path::new("source.txt");
//! let dest = Path::new("dest.txt");
//! fs::write(source, b"data").expect("write source");
//! fs::write(dest, b"data").expect("write dest");
//! let metadata = fs::metadata(source).expect("source metadata");
//! apply_file_metadata(dest, &metadata)?;
//! # fs::remove_file(source).expect("remove source");
//! # fs::remove_file(dest).expect("remove dest");
//! Ok(())
//! # }
//! # demo().unwrap();
//! ```
//!
//! # See also
//!
//! - `rsync_core::client` integrates these helpers for local filesystem copies.
//! - [`filetime`] for lower-level timestamp manipulation utilities.

#[cfg(feature = "acl")]
mod acl_support;
mod apply;
mod chmod;
mod error;
#[cfg(unix)]
mod id_lookup;
mod options;
#[cfg(unix)]
mod ownership;
mod special;
#[cfg(feature = "xattr")]
mod xattr;

#[cfg(feature = "acl")]
pub use acl_support::sync_acls;
pub use apply::{
    apply_directory_metadata, apply_directory_metadata_with_options, apply_file_metadata,
    apply_file_metadata_with_options, apply_symlink_metadata, apply_symlink_metadata_with_options,
};
pub use chmod::{ChmodError, ChmodModifiers};
pub use error::MetadataError;
pub use options::MetadataOptions;
pub use special::{create_device_node, create_fifo};
#[cfg(feature = "xattr")]
pub use xattr::sync_xattrs;
