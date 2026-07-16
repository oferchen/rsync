//! ACL wire protocol constants.
//!
//! These constants mirror upstream rsync's `acls.c` definitions for
//! encoding ACL data on the wire.

/// Flag indicating user object entry is present.
///
/// Upstream: `XMIT_USER_OBJ (1<<0)` in `acls.c` line 38.
pub const XMIT_USER_OBJ: u8 = 1 << 0;

/// Flag indicating group object entry is present.
///
/// Upstream: `XMIT_GROUP_OBJ (1<<1)` in `acls.c` line 39.
pub const XMIT_GROUP_OBJ: u8 = 1 << 1;

/// Flag indicating mask object entry is present.
///
/// Upstream: `XMIT_MASK_OBJ (1<<2)` in `acls.c` line 40.
pub const XMIT_MASK_OBJ: u8 = 1 << 2;

/// Flag indicating other object entry is present.
///
/// Upstream: `XMIT_OTHER_OBJ (1<<3)` in `acls.c` line 41.
pub const XMIT_OTHER_OBJ: u8 = 1 << 3;

/// Flag indicating named user/group entries follow.
///
/// Upstream: `XMIT_NAME_LIST (1<<4)` in `acls.c` line 42.
pub const XMIT_NAME_LIST: u8 = 1 << 4;

/// Sentinel value for absent ACL entries.
///
/// Standard ACL entries (user_obj, group_obj, mask_obj, other_obj) use
/// this value to indicate the entry is not present in the ACL.
///
/// Upstream: `NO_ENTRY ((uchar)0x80)` in `acls.c` line 44.
pub const NO_ENTRY: u8 = 0x80;

/// Flag in access bits indicating a name string follows the ID.
///
/// When set in the lower 2 bits of encoded access, the receiver should
/// read a length-prefixed name string after the ID varint.
///
/// Upstream: `XFLAG_NAME_FOLLOWS 0x0001u` in `acls.c` line 52.
pub const XFLAG_NAME_FOLLOWS: u32 = 0x0001;

/// Flag in access bits indicating this is a user entry (vs group).
///
/// Upstream: `XFLAG_NAME_IS_USER 0x0002u` in `acls.c` line 53.
pub const XFLAG_NAME_IS_USER: u32 = 0x0002;

/// Marker bit for user entries in the id_access `access` field.
///
/// This bit is set in memory only (not on wire) to distinguish user
/// entries from group entries in the ida_entries list.
///
/// Upstream: `NAME_IS_USER (1u<<31)` in `acls.c` line 46.
pub const NAME_IS_USER: u32 = 1 << 31;

/// Valid permission bits for named entries.
///
/// Named user/group ACL entries can have read (4), write (2), execute (1).
pub const ACL_VALID_NAME_BITS: u32 = 0x07;

/// Valid permission bits for object entries.
///
/// Standard object entries (user_obj, etc.) use the same permission bits.
pub const ACL_VALID_OBJ_BITS: u32 = 0x07;

/// Number of bits to shift access when encoding on wire.
///
/// Access bits are shifted left by 2 to make room for XFLAG bits.
pub const ACCESS_SHIFT: u32 = 2;

/// Defence-in-depth cap on the number of ACL named entries from the wire.
///
/// POSIX ACLs typically have fewer than a dozen named entries. 65536 is
/// well above any real-world usage while preventing a malicious peer from
/// forcing unbounded allocations via a crafted count varint.
///
/// upstream: acls.c:700 `recv_ida_entries()` reads the count via
/// `read_varint_bounded(f, 0, MAX_WIRE_ACL_COUNT, "ACL count")`, and
/// `MAX_WIRE_ACL_COUNT` is 65536 (rsync.h:179). We match that ceiling
/// exactly so a transfer upstream 3.4.4 accepts is not rejected here.
pub const MAX_WIRE_ACL_ENTRIES: usize = 65536;
