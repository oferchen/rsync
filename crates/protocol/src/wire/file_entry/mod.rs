#![deny(unsafe_code)]
//! File entry wire format encoding for the rsync protocol.
//!
//! This module provides low-level wire format encoding functions for file list entries,
//! matching upstream rsync's `flist.c:send_file_entry()` behavior. These functions
//! are building blocks for the higher-level [`FileListWriter`](crate::flist::FileListWriter).
//!
//! # Wire Format Overview
//!
//! Each file entry is encoded as:
//! 1. **Flags** - XMIT flags indicating which fields follow and compression state
//! 2. **Name** - Path with prefix compression (reuses prefix from previous entry)
//! 3. **Size** - File size (varlong30 or longint, protocol-dependent)
//! 4. **Mtime** - Modification time (varlong or fixed i32, conditional)
//! 5. **Mode** - Unix mode bits (conditional, when different from previous)
//! 6. **UID/GID** - User/group IDs with optional names (conditional)
//! 7. **Rdev** - Device numbers for block/char devices (conditional)
//! 8. **Symlink target** - For symbolic links (conditional)
//! 9. **Hardlink info** - Index or dev/ino pair (conditional, protocol-dependent)
//! 10. **Checksum** - File checksum in --checksum mode (conditional)
//!
//! # Submodules
//!
//! - `constants` - XMIT flag constants for wire format encoding
//! - `encode` - Wire format encoding functions for each entry field
//! - `flags` - Flag calculation helpers comparing entries
//!
//! # Upstream Reference
//!
//! See `flist.c:send_file_entry()` lines 470-750 for the canonical wire encoding.

mod constants;
mod encode;
mod flags;

#[cfg(test)]
mod tests;

// Re-export all constants
pub use self::constants::{
    XMIT_CRTIME_EQ_MTIME, XMIT_EXTENDED_FLAGS, XMIT_GROUP_NAME_FOLLOWS, XMIT_HLINK_FIRST,
    XMIT_HLINKED, XMIT_IO_ERROR_ENDLIST, XMIT_LONG_NAME, XMIT_MOD_NSEC, XMIT_NO_CONTENT_DIR,
    XMIT_RDEV_MINOR_8_PRE30, XMIT_SAME_ATIME, XMIT_SAME_DEV_PRE30, XMIT_SAME_GID, XMIT_SAME_MODE,
    XMIT_SAME_NAME, XMIT_SAME_RDEV_MAJOR, XMIT_SAME_RDEV_PRE28, XMIT_SAME_TIME, XMIT_SAME_UID,
    XMIT_TOP_DIR, XMIT_USER_NAME_FOLLOWS,
};

// Re-export encoding functions
pub use self::encode::{
    encode_atime, encode_checksum, encode_crtime, encode_end_marker, encode_flags, encode_gid,
    encode_hardlink_dev_ino, encode_hardlink_idx, encode_mode, encode_mtime, encode_mtime_nsec,
    encode_name, encode_owner_name, encode_rdev, encode_size, encode_symlink_target, encode_uid,
};

// Re-export flag calculation helpers
pub use self::flags::{
    calculate_basic_flags, calculate_device_flags, calculate_hardlink_flags,
    calculate_name_prefix_len, calculate_time_flags,
};
