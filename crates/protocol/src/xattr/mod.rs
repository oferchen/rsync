//! Extended attribute wire protocol support.
//!
//! This module implements rsync's xattr (extended attributes) wire protocol
//! for the `--xattrs` (`-X`) option. Xattrs are synchronized using an
//! index-based system with optional abbreviation for large values.
//!
//! # Wire Protocol Overview
//!
//! Xattrs are transmitted in two phases:
//!
//! 1. **File list phase**: Each file entry includes an `xattr_ndx` field
//!    that references a cached xattr list or signals new data follows.
//!
//! 2. **Data exchange phase**: When `xattr_ndx == 0`, literal xattr data
//!    follows. Large xattr values (>32 bytes) are abbreviated to checksums
//!    and requested on-demand.
//!
//! # Abbreviation Protocol
//!
//! For xattr values larger than [`MAX_FULL_DATUM`] (32 bytes):
//!
//! - Sender transmits only the MD5 checksum (16 bytes) instead of full value
//! - Receiver marks these as `XSTATE_ABBREV` (needs data)
//! - After comparing with local xattrs, receiver requests missing values
//! - Sender responds with full data for requested items
//!
//! This optimization significantly reduces bandwidth for files with large
//! xattr values (e.g., security labels, capabilities, selinux contexts).
//!
//! # Reference
//!
//! - Upstream rsync 3.4.1 `xattrs.c`

mod entry;
mod list;
mod wire;

pub use entry::{XattrEntry, XattrState};
pub use list::XattrList;
pub use wire::{
    RecvXattrResult, checksum_matches, recv_xattr, recv_xattr_request, recv_xattr_values,
    send_xattr, send_xattr_request, send_xattr_values,
};

/// Maximum size for a full xattr value transmission.
///
/// Values larger than this are abbreviated to checksums on the wire.
/// Matches upstream rsync's `MAX_FULL_DATUM`.
pub const MAX_FULL_DATUM: usize = 32;

/// Maximum length of the xattr checksum digest.
///
/// Uses MD5 (16 bytes) for compatibility with upstream rsync.
pub const MAX_XATTR_DIGEST_LEN: usize = 16;

/// Rsync xattr namespace prefix for special attributes.
#[cfg(target_os = "linux")]
pub const RSYNC_PREFIX: &str = "user.rsync.";

/// Rsync xattr namespace prefix for special attributes (non-Linux).
#[cfg(not(target_os = "linux"))]
pub const RSYNC_PREFIX: &str = "rsync.";

/// User namespace prefix for xattrs.
pub const USER_PREFIX: &str = "user.";

/// System namespace prefix for xattrs.
pub const SYSTEM_PREFIX: &str = "system.";
