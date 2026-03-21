//! ACL access bit encoding and decoding for wire transmission.
//!
//! Access bits are shifted left by 2, with the lower 2 bits used as flags.
//! This keeps high bits clear for efficient varint encoding.

use super::super::constants::{NAME_IS_USER, XFLAG_NAME_FOLLOWS, XFLAG_NAME_IS_USER};

use crate::acl::constants::ACCESS_SHIFT;

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

/// Decodes access permission bits from wire format.
///
/// Extracts the flags from lower 2 bits and shifts to get permission bits.
///
/// # Returns
///
/// Tuple of (access_with_flags, name_follows) where access_with_flags
/// has `NAME_IS_USER` set if the entry is for a user.
///
/// # Upstream Reference
///
/// Mirrors `recv_acl_access()` in `acls.c` lines 672-695.
pub(crate) fn decode_access(encoded: u32, is_name_entry: bool) -> (u32, bool) {
    if is_name_entry {
        let flags = encoded & 0x03;
        let mut access = encoded >> ACCESS_SHIFT;

        let name_follows = flags & XFLAG_NAME_FOLLOWS != 0;
        if flags & XFLAG_NAME_IS_USER != 0 {
            access |= NAME_IS_USER;
        }

        (access, name_follows)
    } else {
        (encoded, false)
    }
}
