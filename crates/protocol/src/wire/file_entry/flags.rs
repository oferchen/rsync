//! Flag calculation helpers for file entry wire format encoding.
//!
//! These functions compute XMIT flag values by comparing the current entry
//! against the previous entry's fields, following upstream rsync's
//! `flist.c:send_file_entry()` flag calculation logic.

use super::constants::{
    XMIT_CRTIME_EQ_MTIME, XMIT_HLINK_FIRST, XMIT_HLINKED, XMIT_LONG_NAME, XMIT_MOD_NSEC,
    XMIT_RDEV_MINOR_8_PRE30, XMIT_SAME_ATIME, XMIT_SAME_DEV_PRE30, XMIT_SAME_GID, XMIT_SAME_MODE,
    XMIT_SAME_NAME, XMIT_SAME_RDEV_MAJOR, XMIT_SAME_TIME, XMIT_SAME_UID, XMIT_TOP_DIR,
};

/// Calculates the common prefix length between two byte slices.
///
/// Returns the number of bytes that match, capped at 255 (max for single byte encoding).
///
/// # Examples
///
/// ```
/// use protocol::wire::file_entry::calculate_name_prefix_len;
///
/// let prefix_len = calculate_name_prefix_len(b"dir/file1.txt", b"dir/file2.txt");
/// assert_eq!(prefix_len, 8); // "dir/file" is common
/// ```
#[must_use]
pub fn calculate_name_prefix_len(prev_name: &[u8], name: &[u8]) -> usize {
    prev_name
        .iter()
        .zip(name.iter())
        .take_while(|(a, b)| a == b)
        .count()
        .min(255)
}

/// Calculates basic transmission flags for an entry.
///
/// This computes the primary flag byte (bits 0-7) based on comparison
/// with the previous entry's values.
///
/// # Returns
///
/// Primary flags (bits 0-7) as u8
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn calculate_basic_flags(
    mode: u32,
    prev_mode: u32,
    mtime: i64,
    prev_mtime: i64,
    uid: u32,
    prev_uid: u32,
    gid: u32,
    prev_gid: u32,
    same_len: usize,
    suffix_len: usize,
    preserve_uid: bool,
    preserve_gid: bool,
    is_top_dir: bool,
) -> u8 {
    let mut flags: u8 = 0;

    if is_top_dir {
        flags |= XMIT_TOP_DIR;
    }

    if mode == prev_mode {
        flags |= XMIT_SAME_MODE;
    }

    if mtime == prev_mtime {
        flags |= XMIT_SAME_TIME;
    }

    if preserve_uid && uid == prev_uid {
        flags |= XMIT_SAME_UID;
    }

    if preserve_gid && gid == prev_gid {
        flags |= XMIT_SAME_GID;
    }

    if same_len > 0 {
        flags |= XMIT_SAME_NAME;
    }

    if suffix_len > 255 {
        flags |= XMIT_LONG_NAME;
    }

    flags
}

/// Calculates device-related extended flags.
///
/// # Returns
///
/// Extended flags (bits 8-15) as u8
#[must_use]
pub fn calculate_device_flags(
    rdev_major: u32,
    prev_rdev_major: u32,
    rdev_minor: u32,
    protocol_version: u8,
) -> u8 {
    let mut flags: u8 = 0;

    if rdev_major == prev_rdev_major {
        flags |= XMIT_SAME_RDEV_MAJOR;
    }

    // Protocol 28-29: XMIT_RDEV_MINOR_8_PRE30 if minor fits in byte
    if (28..30).contains(&protocol_version) && rdev_minor <= 0xFF {
        flags |= XMIT_RDEV_MINOR_8_PRE30;
    }

    flags
}

/// Calculates hardlink-related extended flags.
///
/// # Returns
///
/// Extended flags (bits 8-15) as u8
#[must_use]
pub fn calculate_hardlink_flags(
    hardlink_idx: Option<u32>,
    hardlink_dev: Option<i64>,
    prev_hardlink_dev: i64,
    protocol_version: u8,
    is_dir: bool,
) -> u8 {
    let mut flags: u8 = 0;

    if is_dir {
        return flags;
    }

    if protocol_version >= 30 {
        if let Some(idx) = hardlink_idx {
            flags |= XMIT_HLINKED;
            if idx == u32::MAX {
                flags |= XMIT_HLINK_FIRST;
            }
        }
    } else if protocol_version >= 28 {
        if let Some(dev) = hardlink_dev {
            if dev == prev_hardlink_dev {
                flags |= XMIT_SAME_DEV_PRE30;
            }
        }
    }

    flags
}

/// Calculates time-related extended flags.
///
/// # Returns
///
/// Extended flags (bits 8-15 and 16-23 packed) as u16
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn calculate_time_flags(
    atime: i64,
    prev_atime: i64,
    crtime: i64,
    mtime: i64,
    mtime_nsec: u32,
    protocol_version: u8,
    preserve_atimes: bool,
    preserve_crtimes: bool,
    is_dir: bool,
) -> u16 {
    let mut flags: u16 = 0;

    // Same atime (non-directories only)
    if preserve_atimes && !is_dir && atime == prev_atime {
        flags |= XMIT_SAME_ATIME as u16; // bit 6 of extended byte
    }

    // Crtime equals mtime (bits 16+, varint mode)
    if preserve_crtimes && crtime == mtime {
        flags |= (XMIT_CRTIME_EQ_MTIME as u16) << 8; // bit 1 of extended16 byte
    }

    // Mtime nanoseconds (protocol 31+)
    if protocol_version >= 31 && mtime_nsec != 0 {
        flags |= XMIT_MOD_NSEC as u16; // bit 5 of extended byte
    }

    flags
}
