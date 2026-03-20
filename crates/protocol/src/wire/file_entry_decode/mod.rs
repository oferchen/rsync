#![deny(unsafe_code)]
//! File entry wire format decoding for the rsync protocol.
//!
//! Low-level wire format decoding functions for file list entries,
//! matching upstream rsync's `flist.c:recv_file_entry()` behavior. These functions
//! are building blocks for higher-level file list reading.
//!
//! # Wire Format Overview
//!
//! Each file entry is decoded as:
//! 1. **Flags** - XMIT flags indicating which fields follow and compression state
//! 2. **Name** - Path with prefix decompression (reuses prefix from previous entry)
//! 3. **Size** - File size (varlong30 or longint, protocol-dependent)
//! 4. **Mtime** - Modification time (varlong or fixed i32, conditional)
//! 5. **Mode** - Unix mode bits (conditional, when different from previous)
//! 6. **UID/GID** - User/group IDs with optional names (conditional)
//! 7. **Rdev** - Device numbers for block/char devices (conditional)
//! 8. **Symlink target** - For symbolic links (conditional)
//! 9. **Hardlink info** - Index or dev/ino pair (conditional, protocol-dependent)
//! 10. **Checksum** - File checksum in --checksum mode (conditional)
//!
//! # Upstream Reference
//!
//! See `flist.c:recv_file_entry()` lines 750-1050 for the canonical wire decoding.

mod checksum;
mod device;
mod flags;
mod hardlink;
mod mode;
mod name;
mod ownership;
mod size;
mod symlink;
mod timestamps;

#[cfg(test)]
mod tests;

pub use self::checksum::decode_checksum;
pub use self::device::decode_rdev;
pub use self::flags::{decode_end_marker, decode_flags, is_io_error_end_marker};
pub use self::hardlink::{decode_hardlink_dev_ino, decode_hardlink_idx};
pub use self::mode::decode_mode;
pub use self::name::decode_name;
pub use self::ownership::{decode_gid, decode_uid};
pub use self::size::decode_size;
pub use self::symlink::{MAX_SYMLINK_TARGET_LEN, decode_symlink_target};
pub use self::timestamps::{decode_atime, decode_crtime, decode_mtime, decode_mtime_nsec};
