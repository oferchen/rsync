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
//! For xattr values larger than `MAX_FULL_DATUM` (32 bytes):
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
//! - Upstream rsync 3.4.4 `xattrs.c`

mod cache;
mod diff;
mod entry;
mod list;
mod prefix;
mod wire;

pub use cache::XattrCache;
pub use diff::xattr_diff;
pub use entry::{XattrEntry, XattrState};
pub use list::XattrList;
pub use prefix::{is_rsync_internal, local_to_wire, wire_to_local};
pub use wire::{
    RecvXattrResult, XattrDefinition, XattrSet, checksum_matches, read_xattr_definitions,
    recv_xattr, recv_xattr_request, recv_xattr_values, send_sender_xattr_response, send_xattr,
    send_xattr_request, send_xattr_values,
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

/// Defence-in-depth cap on the number of xattr entries per file from the wire.
///
/// Linux `listxattr(2)` returns at most `XATTR_LIST_MAX` (65536) bytes of
/// name data. With a minimum 2-byte name per entry that is at most ~32K
/// entries. 1024 is well above any real-world usage while preventing a
/// malicious peer from forcing billions of allocations.
///
/// upstream: xattrs.c `receive_xattr()` uses `EXPAND_ITEM_LIST` which
/// reallocs but has no explicit count cap.
pub const MAX_WIRE_XATTR_COUNT: usize = 1024;

/// Defence-in-depth cap on a single xattr name length from the wire.
///
/// Linux `XATTR_NAME_MAX` is 255 bytes. We allow 1024 to accommodate
/// non-Linux platforms with longer names while still bounding allocation.
///
/// upstream: xattrs.c checks `name_len < 1` and NUL terminator but has
/// no upper bound beyond the overflow check against `SIZE_MAX`.
pub const MAX_WIRE_XATTR_NAME_LEN: usize = 1024;

/// Default per-value allocation ceiling before `--max-alloc` is negotiated
/// (1 GiB, equal to [`crate::max_alloc::DEFAULT_MAX_ALLOC`]).
///
/// Linux `XATTR_SIZE_MAX` is 65536 bytes, but some filesystems (XFS, Btrfs)
/// and platforms (macOS resource forks, transferred as `com.apple.ResourceFork`)
/// allow much larger values.
///
/// upstream: xattrs.c:803 reads the datum length via
/// `read_varint_size(f, MAX_WIRE_XATTR_DATALEN, "xattr datum_len")`, where
/// `MAX_WIRE_XATTR_DATALEN` is `0x7fffffff` (~2 GiB, rsync.h:178). Upstream
/// does not bound the datum by that wire ceiling directly; the real
/// allocation guard is `--max-alloc` (default `DEFAULT_MAX_ALLOC` = 1 GiB,
/// options.c:203) enforced inside `new_array()`/`my_alloc()`.
///
/// The decoders no longer compare against this fixed constant. They call
/// [`crate::max_alloc::effective_max_alloc`], which returns this default until
/// `--max-alloc` rewrites the process-global ceiling. A peer that raised
/// `--max-alloc` can then send larger datum values, up to the `0x7fffffff`
/// signed-`int32` field ceiling. This constant remains the documented default
/// (and the value the decoders enforce when `--max-alloc` is unset).
pub const MAX_WIRE_XATTR_VALUE_LEN: usize = crate::max_alloc::DEFAULT_MAX_ALLOC;

/// User namespace prefix for xattrs.
pub const USER_PREFIX: &str = "user.";

/// System namespace prefix for xattrs.
pub const SYSTEM_PREFIX: &str = "system.";
