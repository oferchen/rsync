//! ACL access bit encoding and decoding for wire transmission.
//!
//! Access bits are shifted left by 2, with the lower 2 bits used as flags.
//! This keeps high bits clear for efficient varint encoding.

use std::io;

use super::super::constants::{NAME_IS_USER, XFLAG_NAME_FOLLOWS, XFLAG_NAME_IS_USER};

use crate::acl::constants::{ACCESS_SHIFT, ACL_VALID_NAME_BITS, ACL_VALID_OBJ_BITS};

/// Encodes access permission bits for wire transmission.
///
/// Shifts access bits left by 2 and sets the lower 2 bits as flags.
/// This encoding keeps high bits clear for efficient varint encoding.
///
/// # Arguments
///
/// * `access` - Permission bits (rwx) with optional `NAME_IS_USER` flag
/// * `include_name` - Whether a name string will follow (sets `XFLAG_NAME_FOLLOWS`)
///
/// # Upstream Reference
///
/// See `acls.c` lines 48-53 for the encoding rationale.
pub(crate) fn encode_access(access: u32, include_name: bool) -> u32 {
    let perms = access & !NAME_IS_USER;
    let mut encoded = perms << ACCESS_SHIFT;

    if include_name {
        encoded |= XFLAG_NAME_FOLLOWS;
    }
    if access & NAME_IS_USER != 0 {
        encoded |= XFLAG_NAME_IS_USER;
    }

    encoded
}

/// Decodes and validates access permission bits from wire format.
///
/// For named entries the flags occupy the lower 2 bits and the permission
/// bits are shifted down; for object entries the value is the raw permission
/// mask. In both cases the permission bits are validated against the valid
/// range before use, rejecting an out-of-range peer value.
///
/// # Returns
///
/// Tuple of (access_with_flags, name_follows) where access_with_flags
/// has `NAME_IS_USER` set if the entry is for a user. `name_follows` is
/// always `false` for object entries.
///
/// # Errors
///
/// Returns an `InvalidData` error - which the core exit-code mapper renders as
/// `RERR_STREAMIO` (exit 12) - when the permission bits fall outside the valid
/// range, mirroring upstream's `exit_cleanup(RERR_STREAMIO)`.
///
/// # Upstream Reference
///
/// Mirrors `recv_acl_access()` in `acls.c` lines 672-695: named entries are
/// checked against `SMB_ACL_VALID_NAME_BITS` and object entries against
/// `SMB_ACL_VALID_OBJ_BITS`, both `(4|2|1)` for POSIX ACLs.
pub(crate) fn decode_access(encoded: u32, is_name_entry: bool) -> io::Result<(u32, bool)> {
    if is_name_entry {
        let flags = encoded & 0x03;
        let mut access = encoded >> ACCESS_SHIFT;

        // upstream: acls.c:679 rejects `access & ~SMB_ACL_VALID_NAME_BITS`
        // after the shift, before folding in NAME_IS_USER.
        if access & !ACL_VALID_NAME_BITS != 0 {
            return Err(access_value_error(access));
        }

        let name_follows = flags & XFLAG_NAME_FOLLOWS != 0;
        if flags & XFLAG_NAME_IS_USER != 0 {
            access |= NAME_IS_USER;
        }

        Ok((access, name_follows))
    } else {
        // upstream: acls.c:687 rejects `access & ~SMB_ACL_VALID_OBJ_BITS`.
        if encoded & !ACL_VALID_OBJ_BITS != 0 {
            return Err(access_value_error(encoded));
        }
        Ok((encoded, false))
    }
}

/// Builds the out-of-range access-bit error.
///
/// upstream: acls.c:688-691 `recv_acl_access()` prints "value out of range"
/// and calls `exit_cleanup(RERR_STREAMIO)`. A bare `InvalidData` io::Error maps
/// to `RERR_STREAMIO` (exit 12) via the core exit-code mapper
/// (`exit_code/codes.rs`), so no dedicated error variant is introduced.
fn access_value_error(access: u32) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("recv_acl_access: value out of range: {access:x}"),
    )
}
