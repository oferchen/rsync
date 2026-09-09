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

use md5::Md5;
use md5::digest::OutputSizeUser;
use md5::digest::typenum::Unsigned;

mod cache;
mod diff;
mod entry;
mod list;
mod prefix;
mod wire;

pub use cache::XattrCache;
pub use diff::{mark_xattr_requests, xattr_diff};
pub use entry::{XattrEntry, XattrState};
pub use list::XattrList;
pub use prefix::{
    is_fake_super_store_attr, is_rsync_internal, local_to_wire, rsync_internal_suffix,
    wire_to_local,
};
pub use wire::{
    RecvXattrResult, XattrDefinition, XattrSet, checksum_matches, compute_xattr_checksum,
    read_xattr_definitions, recv_xattr, recv_xattr_request, recv_xattr_values,
    send_sender_xattr_response, send_xattr, send_xattr_request, send_xattr_values,
};

/// Maximum size for a full xattr value transmission.
///
/// Values larger than this are abbreviated to checksums on the wire.
/// Matches upstream rsync's `MAX_FULL_DATUM`.
pub const MAX_FULL_DATUM: usize = 32;

/// Maximum length of the xattr checksum digest.
///
/// Derived from the output size of the very hasher [`compute_xattr_checksum`]
/// runs, so the buffer and the digest can never disagree.
///
/// upstream: `xattrs.c:48` defines this as `MAX_XATTR_DIGEST_LEN MD5_DIGEST_LEN`
/// rather than as a number, and `lib/md-defines.h:12` sets `MD5_DIGEST_LEN` to
/// 16. Note that upstream pins the xattr digest to MD5's length specifically,
/// *not* to `MAX_DIGEST_LEN` (`lib/md-defines.h:13-21`, up to SHA-512's 64):
/// `compat.c:835-836` fixes `xattr_sum_nni` to the implied MD5 choice, above a
/// comment reserving the right to make the algorithm negotiable later.
pub const MAX_XATTR_DIGEST_LEN: usize = <Md5 as OutputSizeUser>::OutputSize::USIZE;

/// The derivation must stay value-identical to upstream's `MD5_DIGEST_LEN`
/// (`lib/md-defines.h:12`): the abbreviated-xattr digest is a wire field, so a
/// changed length is a protocol break, not an internal detail.
const _: () = assert!(MAX_XATTR_DIGEST_LEN == 16);

/// Hard ceiling on a single peer-supplied xattr value.
///
/// upstream: `xattrs.c:51` `MAX_XATTR_VALUE_BYTES`, enforced in
/// `receive_xattr()` and `recv_xattr_request()`, which abort with
/// `exit_cleanup(RERR_PROTOCOL)` rather than truncating. This is independent
/// of - and stricter than - the `--max-alloc` allocation guard: a peer that
/// raised `--max-alloc` still cannot exceed it.
pub const MAX_XATTR_VALUE_BYTES: usize = 128 * 1024 * 1024;

/// Hard ceiling on the summed name and value bytes of one file's xattr list.
///
/// upstream: `xattrs.c:50` `MAX_XATTR_LIST_BYTES`, accumulated into
/// `total_xattr_bytes` across the entry loop in `receive_xattr()`. Bounds the
/// aggregate a peer can drive with many individually-legal entries, which no
/// per-value cap can constrain on its own.
pub const MAX_XATTR_LIST_BYTES: usize = 512 * 1024 * 1024;

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

#[cfg(test)]
mod digest_len_tests {
    use super::*;
    use md5::Digest;

    const DATUM: &[u8] = b"an xattr value wider than MAX_FULL_DATUM bytes";

    /// The buffer width and the hasher that fills it must be one decision.
    /// Driven from the live hasher output rather than a second literal, so a
    /// digest swap fails here instead of panicking inside `copy_from_slice`
    /// on the first abbreviated xattr.
    #[test]
    fn digest_len_tracks_the_hasher_that_fills_the_buffer() {
        let hasher_output_len = Md5::digest(DATUM).len();
        assert_eq!(MAX_XATTR_DIGEST_LEN, hasher_output_len);
        assert_eq!(compute_xattr_checksum(DATUM, 0).len(), hasher_output_len);
    }

    /// upstream pins the xattr digest to MD5's length (`xattrs.c:48`,
    /// `lib/md-defines.h:12`), never to `MAX_DIGEST_LEN`, which reaches
    /// SHA-512's 64 bytes (`lib/md-defines.h:13-21`). Deriving from the wrong
    /// constant would still compile and still hold a digest, but would put the
    /// wrong number of bytes on the wire.
    #[test]
    fn digest_len_is_upstream_md5_digest_len_not_max_digest_len() {
        assert_eq!(MAX_XATTR_DIGEST_LEN, 16);
    }
}
