#![cfg(all(
    feature = "acl",
    any(target_os = "linux", target_os = "macos", target_os = "freebsd")
))]

//! Cross-platform ACL synchronization using the `exacl` crate.
//!
//! This module provides ACL synchronization for Linux (POSIX ACLs), macOS
//! (extended ACLs), and FreeBSD (POSIX and NFSv4 ACLs) using the `exacl`
//! crate for a unified, safe abstraction.
//!
//! # Design
//!
//! The [`sync_acls`] function coordinates the full ACL replication workflow:
//!
//! - Read access and default ACLs from the source without following symbolic
//!   links unless explicitly requested.
//! - Apply the retrieved ACLs to the destination.
//! - When the source has no extended ACL entries, reset the destination to
//!   match its permission bits.
//!
//! # Platform Differences
//!
//! - **Linux**: Uses POSIX ACLs with access and default ACL types.
//! - **macOS**: Uses extended ACLs (NFSv4-style); no default ACLs on directories.
//! - **FreeBSD**: Supports both POSIX and NFSv4 ACLs depending on filesystem.
//!
//! The `exacl` crate handles these differences internally.
//!
//! # Upstream Reference
//!
//! The behavior mirrors upstream rsync's ACL handling in `acls.c` and
//! `lib/sysacls.c`, where:
//! - ACL read/write errors on unsupported filesystems are silently ignored.
//! - Symbolic links do not receive ACL updates (Linux doesn't support link ACLs).
//! - Default ACLs are handled for directories on platforms that support them.
//!
//! # Examples
//!
//! ```rust,ignore
//! use ::metadata::sync_acls;
//! use std::path::Path;
//!
//! # fn demo() -> Result<(), ::metadata::MetadataError> {
//! let source = Path::new("src");
//! let destination = Path::new("dst");
//! sync_acls(source, destination, true)?;
//! # Ok(())
//! # }
//! ```

mod apply;
mod default_perms;
mod error;
mod perms;
mod read;
mod reconstruct;
mod reset;
mod special;
mod sync;

#[cfg(test)]
mod tests;

pub use apply::{apply_acls_from_cache, store_acls_via_fake_super};
pub use default_perms::default_perms_for_dir;
pub use read::get_rsync_acl;
pub use sync::{sync_acls, sync_acls_via_fake_super};
