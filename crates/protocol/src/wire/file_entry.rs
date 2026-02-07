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
//! # Upstream Reference
//!
//! See `flist.c:send_file_entry()` lines 470-750 for the canonical wire encoding.

use std::io::{self, Write};

use crate::varint::{write_varint, write_varint30_int, write_varlong};

// ============================================================================
// XMIT Flag Constants - Match upstream rsync.h
// ============================================================================
//
// These constants define the wire format flags for file entry encoding.
// They are defined here for use in wire format encoding and are re-exported
// for convenience.

// Primary flags (bits 0-7)

/// Flag indicating this is the top-level directory in the transfer.
pub const XMIT_TOP_DIR: u8 = 1 << 0;

/// Flag indicating the entry has the same mode as the previous entry.
pub const XMIT_SAME_MODE: u8 = 1 << 1;

/// Flag indicating that extended flags follow the first byte (protocol 28+).
pub const XMIT_EXTENDED_FLAGS: u8 = 1 << 2;

/// Flag indicating same rdev as previous entry (protocols 20-27).
/// Shares bit position with XMIT_EXTENDED_FLAGS.
pub const XMIT_SAME_RDEV_PRE28: u8 = 1 << 2;

/// Flag indicating the entry has the same UID as the previous entry.
pub const XMIT_SAME_UID: u8 = 1 << 3;

/// Flag indicating the entry has the same GID as the previous entry.
pub const XMIT_SAME_GID: u8 = 1 << 4;

/// Flag indicating the entry shares part of its name with the previous entry.
pub const XMIT_SAME_NAME: u8 = 1 << 5;

/// Flag indicating the name length uses a varint instead of 8-bit.
pub const XMIT_LONG_NAME: u8 = 1 << 6;

/// Flag indicating the entry has the same modification time as the previous entry.
pub const XMIT_SAME_TIME: u8 = 1 << 7;

// Extended flags (bits 8-15, stored as byte 1 of extended flags)

/// Extended flag: same rdev major as previous (bit 8, devices only).
pub const XMIT_SAME_RDEV_MAJOR: u8 = 1 << 0;

/// Extended flag: directory has no content to transfer (bit 8, directories only).
pub const XMIT_NO_CONTENT_DIR: u8 = 1 << 0;

/// Extended flag: entry has hardlink information (bit 9).
pub const XMIT_HLINKED: u8 = 1 << 1;

/// Extended flag: same device number as previous (bit 10, protocols 28-29).
pub const XMIT_SAME_DEV_PRE30: u8 = 1 << 2;

/// Extended flag: user name follows (bit 10, protocol 30+).
pub const XMIT_USER_NAME_FOLLOWS: u8 = 1 << 2;

/// Extended flag: rdev minor fits in 8 bits (bit 11, protocols 28-29).
pub const XMIT_RDEV_MINOR_8_PRE30: u8 = 1 << 3;

/// Extended flag: group name follows (bit 11, protocol 30+).
pub const XMIT_GROUP_NAME_FOLLOWS: u8 = 1 << 3;

/// Extended flag: hardlink first / I/O error end list (bit 12).
pub const XMIT_HLINK_FIRST: u8 = 1 << 4;

/// Extended flag: I/O error end list marker (bit 12, protocol 31+).
pub const XMIT_IO_ERROR_ENDLIST: u8 = 1 << 4;

/// Extended flag: mtime has nanoseconds (bit 13, protocol 31+).
pub const XMIT_MOD_NSEC: u8 = 1 << 5;

/// Extended flag: same atime as previous entry (bit 14).
pub const XMIT_SAME_ATIME: u8 = 1 << 6;

// Third byte of extended flags (bits 16-23, varint mode only)

/// Extended flag: creation time equals mtime (bit 17).
pub const XMIT_CRTIME_EQ_MTIME: u8 = 1 << 1;

// ============================================================================
// Flag Encoding
// ============================================================================

/// Encodes transmission flags to the wire format.
///
/// The encoding varies by protocol version and compatibility flags:
/// - **Varint mode** (VARINT_FLIST_FLAGS): Single varint containing all flag bits
/// - **Protocol 28+**: 1 byte, or 2 bytes if extended flags needed
/// - **Protocol < 28**: 1 byte only
///
/// # Arguments
///
/// * `writer` - Output stream
/// * `xflags` - Combined flag bits (byte 0 = primary, byte 1 = extended, byte 2 = extended16)
/// * `protocol_version` - Protocol version for encoding selection
/// * `use_varint_flags` - Whether VARINT_FLIST_FLAGS compat flag is set
/// * `is_dir` - Whether entry is a directory (affects handling of zero flags)
///
/// # Wire Format
///
/// | Mode | Format |
/// |------|--------|
/// | Varint | `varint(xflags)` or `varint(XMIT_EXTENDED_FLAGS)` if xflags=0 |
/// | Proto 28+ | `u8` or `u16 LE` if extended flags needed |
/// | Proto < 28 | `u8` (with XMIT_LONG_NAME if xflags=0 and not dir) |
///
/// # Examples
///
/// ```
/// use protocol::wire::file_entry::encode_flags;
///
/// let mut buf = Vec::new();
/// // Protocol 32, non-varint mode, file entry with mode compression
/// encode_flags(&mut buf, 0x02, 32, false, false).unwrap();
/// assert_eq!(buf, vec![0x02]); // Single byte for XMIT_SAME_MODE
/// ```
///
/// # Upstream Reference
///
/// See `flist.c:send_file_entry()` lines 545-575
pub fn encode_flags<W: Write>(
    writer: &mut W,
    xflags: u32,
    protocol_version: u8,
    use_varint_flags: bool,
    is_dir: bool,
) -> io::Result<()> {
    if use_varint_flags {
        // Varint mode: avoid xflags=0 which looks like end marker
        let flags_to_write = if xflags == 0 {
            XMIT_EXTENDED_FLAGS as u32
        } else {
            xflags
        };
        write_varint(writer, flags_to_write as i32)?;
    } else if protocol_version >= 28 {
        // Protocol 28-29: two-byte encoding if extended flags needed
        let mut flags_to_write = xflags;
        if flags_to_write == 0 && !is_dir {
            flags_to_write |= XMIT_TOP_DIR as u32;
        }

        if (flags_to_write & 0xFF00) != 0 || flags_to_write == 0 {
            flags_to_write |= XMIT_EXTENDED_FLAGS as u32;
            writer.write_all(&(flags_to_write as u16).to_le_bytes())?;
        } else {
            writer.write_all(&[flags_to_write as u8])?;
        }
    } else {
        // Protocol < 28: single byte
        let flags_to_write = if xflags == 0 && !is_dir {
            XMIT_LONG_NAME as u32
        } else {
            xflags
        };
        writer.write_all(&[flags_to_write as u8])?;
    }
    Ok(())
}

/// Encodes the end-of-list marker.
///
/// # Arguments
///
/// * `writer` - Output stream
/// * `use_varint_flags` - Whether VARINT_FLIST_FLAGS compat flag is set
/// * `use_safe_file_list` - Whether SAFE_FILE_LIST compat flag is set or protocol >= 31
/// * `io_error` - Optional I/O error code to transmit
///
/// # Wire Format
///
/// | Mode | Format |
/// |------|--------|
/// | Varint | `varint(0)` + `varint(io_error)` |
/// | Safe file list with error | `[XMIT_EXTENDED_FLAGS, XMIT_IO_ERROR_ENDLIST]` + `varint(error)` |
/// | Normal | `[0u8]` |
///
/// # Examples
///
/// ```
/// use protocol::wire::file_entry::encode_end_marker;
///
/// let mut buf = Vec::new();
/// encode_end_marker(&mut buf, false, false, None).unwrap();
/// assert_eq!(buf, vec![0u8]);
/// ```
pub fn encode_end_marker<W: Write>(
    writer: &mut W,
    use_varint_flags: bool,
    use_safe_file_list: bool,
    io_error: Option<i32>,
) -> io::Result<()> {
    if use_varint_flags {
        write_varint(writer, 0)?;
        write_varint(writer, io_error.unwrap_or(0))?;
        return Ok(());
    }

    if let Some(error) = io_error {
        if use_safe_file_list {
            writer.write_all(&[XMIT_EXTENDED_FLAGS, XMIT_IO_ERROR_ENDLIST])?;
            write_varint(writer, error)?;
            return Ok(());
        }
    }

    writer.write_all(&[0u8])
}

// ============================================================================
// Name Encoding
// ============================================================================

/// Encodes a file name with prefix compression.
///
/// The rsync protocol compresses file names by sharing common prefixes with
/// the previous entry. This function encodes the name suffix along with
/// compression metadata.
///
/// # Arguments
///
/// * `writer` - Output stream
/// * `name` - Full path bytes
/// * `same_len` - Number of bytes shared with previous entry (0-255)
/// * `xflags` - Transmission flags (used to check XMIT_SAME_NAME and XMIT_LONG_NAME)
/// * `protocol_version` - Protocol version (affects long name length encoding)
///
/// # Wire Format
///
/// ```text
/// [same_len: u8]     - Only if XMIT_SAME_NAME set
/// [suffix_len]       - u8, or varint30/fixed i32 if XMIT_LONG_NAME set
/// [suffix_bytes]     - The name portion after the shared prefix
/// ```
///
/// # Examples
///
/// ```
/// use protocol::wire::file_entry::{encode_name, XMIT_SAME_NAME};
///
/// let mut buf = Vec::new();
/// // Encoding "dir/file2.txt" when previous was "dir/file1.txt"
/// // same_len=9 ("dir/file1" shared prefix chars = 9 counting the "1")
/// // Actually for "dir/file1.txt" vs "dir/file2.txt", shared = 8 ("dir/file")
/// encode_name(&mut buf, b"dir/file2.txt", 8, XMIT_SAME_NAME as u32, 32).unwrap();
/// // same_len byte (8) + suffix_len byte (5) + "2.txt"
/// assert_eq!(buf.len(), 1 + 1 + 5);
/// ```
///
/// # Upstream Reference
///
/// See `flist.c:send_file_entry()` lines 580-610
pub fn encode_name<W: Write>(
    writer: &mut W,
    name: &[u8],
    same_len: usize,
    xflags: u32,
    protocol_version: u8,
) -> io::Result<()> {
    let suffix_len = name.len().saturating_sub(same_len);

    // Write same_len if XMIT_SAME_NAME is set
    if xflags & (XMIT_SAME_NAME as u32) != 0 {
        writer.write_all(&[same_len as u8])?;
    }

    // Write suffix length
    if xflags & (XMIT_LONG_NAME as u32) != 0 {
        // Long name: protocol-dependent encoding
        if protocol_version >= 30 {
            write_varint(writer, suffix_len as i32)?;
        } else {
            writer.write_all(&(suffix_len as i32).to_le_bytes())?;
        }
    } else {
        // Short name: single byte
        writer.write_all(&[suffix_len as u8])?;
    }

    // Write suffix bytes
    writer.write_all(&name[same_len..])
}

// ============================================================================
// Size Encoding
// ============================================================================

/// Encodes file size to the wire format.
///
/// # Arguments
///
/// * `writer` - Output stream
/// * `size` - File size in bytes
/// * `protocol_version` - Protocol version (affects encoding format)
///
/// # Wire Format
///
/// | Protocol | Format |
/// |----------|--------|
/// | >= 30 | varlong30 (min_bytes=3) |
/// | < 30 | longint (4 bytes, or 12 bytes if > 32-bit) |
///
/// # Examples
///
/// ```
/// use protocol::wire::file_entry::encode_size;
///
/// let mut buf = Vec::new();
/// encode_size(&mut buf, 1000, 32).unwrap();
/// // Modern protocol uses varlong30, compact for small values
/// ```
///
/// # Upstream Reference
///
/// See `flist.c:send_file_entry()` line 580: `write_varlong30(f, F_LENGTH(file), 3)`
pub fn encode_size<W: Write>(writer: &mut W, size: u64, protocol_version: u8) -> io::Result<()> {
    if protocol_version >= 30 {
        write_varlong(writer, size as i64, 3)
    } else {
        crate::write_longint(writer, size as i64)
    }
}

// ============================================================================
// Time Encoding
// ============================================================================

/// Encodes modification time to the wire format.
///
/// # Arguments
///
/// * `writer` - Output stream
/// * `mtime` - Modification time as seconds since Unix epoch
/// * `protocol_version` - Protocol version (affects encoding format)
///
/// # Wire Format
///
/// | Protocol | Format |
/// |----------|--------|
/// | >= 30 | varlong (min_bytes=4) |
/// | < 30 | Fixed 4-byte i32 LE |
///
/// # Examples
///
/// ```
/// use protocol::wire::file_entry::encode_mtime;
///
/// let mut buf = Vec::new();
/// encode_mtime(&mut buf, 1700000000, 32).unwrap();
/// ```
///
/// # Upstream Reference
///
/// See `flist.c:send_file_entry()` lines 582-584
pub fn encode_mtime<W: Write>(writer: &mut W, mtime: i64, protocol_version: u8) -> io::Result<()> {
    if protocol_version >= 30 {
        write_varlong(writer, mtime, 4)
    } else {
        writer.write_all(&(mtime as i32).to_le_bytes())
    }
}

/// Encodes modification time nanoseconds (protocol 31+).
///
/// # Arguments
///
/// * `writer` - Output stream
/// * `nsec` - Nanosecond component of modification time
///
/// # Wire Format
///
/// `varint(nsec)`
///
/// # Note
///
/// Only encode when `XMIT_MOD_NSEC` flag is set in xflags.
pub fn encode_mtime_nsec<W: Write>(writer: &mut W, nsec: u32) -> io::Result<()> {
    write_varint(writer, nsec as i32)
}

/// Encodes access time (for --atimes, non-directories only).
///
/// # Arguments
///
/// * `writer` - Output stream
/// * `atime` - Access time as seconds since Unix epoch
///
/// # Wire Format
///
/// `varlong(atime, 4)`
pub fn encode_atime<W: Write>(writer: &mut W, atime: i64) -> io::Result<()> {
    write_varlong(writer, atime, 4)
}

/// Encodes creation time (for --crtimes).
///
/// # Arguments
///
/// * `writer` - Output stream
/// * `crtime` - Creation time as seconds since Unix epoch
///
/// # Wire Format
///
/// `varlong(crtime, 4)`
///
/// # Note
///
/// Only encode when `XMIT_CRTIME_EQ_MTIME` flag is NOT set.
pub fn encode_crtime<W: Write>(writer: &mut W, crtime: i64) -> io::Result<()> {
    write_varlong(writer, crtime, 4)
}

// ============================================================================
// Mode Encoding
// ============================================================================

/// Encodes Unix mode bits to the wire format.
///
/// Mode is always encoded as a fixed 4-byte little-endian integer.
///
/// # Arguments
///
/// * `writer` - Output stream
/// * `mode` - Unix mode bits (file type + permissions)
///
/// # Wire Format
///
/// Fixed 4-byte i32 LE
///
/// # Note
///
/// Only encode when `XMIT_SAME_MODE` flag is NOT set.
///
/// # Examples
///
/// ```
/// use protocol::wire::file_entry::encode_mode;
///
/// let mut buf = Vec::new();
/// encode_mode(&mut buf, 0o100644).unwrap(); // Regular file, rw-r--r--
/// assert_eq!(buf.len(), 4);
/// ```
pub fn encode_mode<W: Write>(writer: &mut W, mode: u32) -> io::Result<()> {
    writer.write_all(&(mode as i32).to_le_bytes())
}

// ============================================================================
// UID/GID Encoding
// ============================================================================

/// Encodes a user ID to the wire format.
///
/// # Arguments
///
/// * `writer` - Output stream
/// * `uid` - User ID
/// * `protocol_version` - Protocol version (affects encoding format)
///
/// # Wire Format
///
/// | Protocol | Format |
/// |----------|--------|
/// | >= 30 | varint |
/// | < 30 | Fixed 4-byte i32 LE |
///
/// # Note
///
/// Only encode when preserve_uid is enabled and `XMIT_SAME_UID` flag is NOT set.
pub fn encode_uid<W: Write>(writer: &mut W, uid: u32, protocol_version: u8) -> io::Result<()> {
    if protocol_version >= 30 {
        write_varint(writer, uid as i32)
    } else {
        writer.write_all(&(uid as i32).to_le_bytes())
    }
}

/// Encodes a group ID to the wire format.
///
/// # Arguments
///
/// * `writer` - Output stream
/// * `gid` - Group ID
/// * `protocol_version` - Protocol version (affects encoding format)
///
/// # Wire Format
///
/// | Protocol | Format |
/// |----------|--------|
/// | >= 30 | varint |
/// | < 30 | Fixed 4-byte i32 LE |
///
/// # Note
///
/// Only encode when preserve_gid is enabled and `XMIT_SAME_GID` flag is NOT set.
pub fn encode_gid<W: Write>(writer: &mut W, gid: u32, protocol_version: u8) -> io::Result<()> {
    if protocol_version >= 30 {
        write_varint(writer, gid as i32)
    } else {
        writer.write_all(&(gid as i32).to_le_bytes())
    }
}

/// Encodes a user or group name (protocol 30+).
///
/// # Arguments
///
/// * `writer` - Output stream
/// * `name` - User or group name (truncated to 255 bytes)
///
/// # Wire Format
///
/// `u8(len)` + `name_bytes[0..len]`
///
/// # Note
///
/// Only encode when `XMIT_USER_NAME_FOLLOWS` or `XMIT_GROUP_NAME_FOLLOWS` flag is set.
pub fn encode_owner_name<W: Write>(writer: &mut W, name: &str) -> io::Result<()> {
    let name_bytes = name.as_bytes();
    let len = name_bytes.len().min(255) as u8;
    writer.write_all(&[len])?;
    writer.write_all(&name_bytes[..len as usize])
}

// ============================================================================
// Device Number Encoding
// ============================================================================

/// Encodes device numbers for block/character devices.
///
/// # Arguments
///
/// * `writer` - Output stream
/// * `major` - Device major number
/// * `minor` - Device minor number
/// * `xflags` - Transmission flags (checks XMIT_SAME_RDEV_MAJOR, XMIT_RDEV_MINOR_8_PRE30)
/// * `protocol_version` - Protocol version
///
/// # Wire Format (Protocol 28+)
///
/// - Major: varint30 (omitted if `XMIT_SAME_RDEV_MAJOR` set)
/// - Minor: varint (proto 30+) or byte/i32 (proto 28-29)
///
/// # Note
///
/// For special files (FIFOs, sockets) in protocol < 31, write dummy rdev (0, 0).
///
/// # Upstream Reference
///
/// See `flist.c:send_file_entry()` lines 640-680
pub fn encode_rdev<W: Write>(
    writer: &mut W,
    major: u32,
    minor: u32,
    xflags: u32,
    protocol_version: u8,
) -> io::Result<()> {
    // Write major if not same as previous
    if xflags & ((XMIT_SAME_RDEV_MAJOR as u32) << 8) == 0 {
        write_varint30_int(writer, major as i32, protocol_version)?;
    }

    // Write minor
    if protocol_version >= 30 {
        write_varint(writer, minor as i32)?;
    } else {
        // Protocol 28-29: check XMIT_RDEV_MINOR_8_PRE30 flag
        let minor_8_bit = (xflags & ((XMIT_RDEV_MINOR_8_PRE30 as u32) << 8)) != 0;
        if minor_8_bit {
            writer.write_all(&[minor as u8])?;
        } else {
            writer.write_all(&(minor as i32).to_le_bytes())?;
        }
    }

    Ok(())
}

// ============================================================================
// Symlink Target Encoding
// ============================================================================

/// Encodes symlink target path.
///
/// # Arguments
///
/// * `writer` - Output stream
/// * `target` - Target path bytes
/// * `protocol_version` - Protocol version
///
/// # Wire Format
///
/// `varint30(len)` + `target_bytes`
///
/// # Note
///
/// Only encode when preserve_links is enabled and entry is a symlink.
pub fn encode_symlink_target<W: Write>(
    writer: &mut W,
    target: &[u8],
    protocol_version: u8,
) -> io::Result<()> {
    write_varint30_int(writer, target.len() as i32, protocol_version)?;
    writer.write_all(target)
}

// ============================================================================
// Hardlink Encoding
// ============================================================================

/// Encodes hardlink index (protocol 30+).
///
/// # Arguments
///
/// * `writer` - Output stream
/// * `idx` - Hardlink index (reference to earlier entry in file list)
///
/// # Wire Format
///
/// `varint(idx)`
///
/// # Note
///
/// Only encode when `XMIT_HLINKED` is set but `XMIT_HLINK_FIRST` is NOT set.
/// The first occurrence of a hardlink group (leader) doesn't write an index.
pub fn encode_hardlink_idx<W: Write>(writer: &mut W, idx: u32) -> io::Result<()> {
    write_varint(writer, idx as i32)
}

/// Encodes hardlink device and inode (protocol 28-29).
///
/// In protocols before 30, hardlinks are identified by (dev, ino) pairs.
///
/// # Arguments
///
/// * `writer` - Output stream
/// * `dev` - Device number
/// * `ino` - Inode number
/// * `same_dev` - Whether `XMIT_SAME_DEV_PRE30` flag is set
///
/// # Wire Format
///
/// - If not same_dev: `longint(dev + 1)`
/// - Always: `longint(ino)`
///
/// # Upstream Reference
///
/// See `flist.c:send_file_entry()` lines 690-710
pub fn encode_hardlink_dev_ino<W: Write>(
    writer: &mut W,
    dev: i64,
    ino: i64,
    same_dev: bool,
) -> io::Result<()> {
    if !same_dev {
        // Write dev + 1 (upstream convention)
        crate::write_longint(writer, dev + 1)?;
    }
    // Always write ino
    crate::write_longint(writer, ino)
}

// ============================================================================
// Checksum Encoding
// ============================================================================

/// Encodes file checksum (for --checksum mode).
///
/// # Arguments
///
/// * `writer` - Output stream
/// * `checksum` - Checksum bytes (or None to write zeros)
/// * `csum_len` - Expected checksum length
///
/// # Wire Format
///
/// Raw bytes of length `csum_len`. If checksum is shorter, pads with zeros.
///
/// # Note
///
/// For regular files: actual checksum (or zeros if not computed)
/// For non-regular files (proto < 28 only): empty_sum (all zeros)
pub fn encode_checksum<W: Write>(
    writer: &mut W,
    checksum: Option<&[u8]>,
    csum_len: usize,
) -> io::Result<()> {
    if let Some(sum) = checksum {
        let len = sum.len().min(csum_len);
        writer.write_all(&sum[..len])?;
        // Pad with zeros if shorter
        if len < csum_len {
            let padding = vec![0u8; csum_len - len];
            writer.write_all(&padding)?;
        }
    } else {
        // No checksum: write zeros
        let zeros = vec![0u8; csum_len];
        writer.write_all(&zeros)?;
    }
    Ok(())
}

// ============================================================================
// Flag Calculation Helpers
// ============================================================================

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
/// # Arguments
///
/// * `mode` - Current entry's mode
/// * `prev_mode` - Previous entry's mode
/// * `mtime` - Current entry's mtime
/// * `prev_mtime` - Previous entry's mtime
/// * `uid` - Current entry's UID (or 0 if not preserving)
/// * `prev_uid` - Previous entry's UID
/// * `gid` - Current entry's GID (or 0 if not preserving)
/// * `prev_gid` - Previous entry's GID
/// * `same_len` - Common prefix length with previous name
/// * `suffix_len` - Length of name suffix
/// * `preserve_uid` - Whether UID preservation is enabled
/// * `preserve_gid` - Whether GID preservation is enabled
/// * `is_top_dir` - Whether this is a top-level directory
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
/// # Arguments
///
/// * `rdev_major` - Current entry's device major number
/// * `prev_rdev_major` - Previous entry's device major number
/// * `rdev_minor` - Current entry's device minor number
/// * `protocol_version` - Protocol version
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
/// # Arguments
///
/// * `hardlink_idx` - Hardlink index (Some(u32::MAX) for first/leader, Some(idx) for follower)
/// * `hardlink_dev` - Hardlink device (protocol 28-29)
/// * `prev_hardlink_dev` - Previous hardlink device
/// * `protocol_version` - Protocol version
/// * `is_dir` - Whether entry is a directory (directories don't have hardlinks)
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
/// # Arguments
///
/// * `atime` - Current entry's access time
/// * `prev_atime` - Previous entry's access time
/// * `crtime` - Current entry's creation time
/// * `mtime` - Current entry's modification time
/// * `mtime_nsec` - Nanosecond component of mtime
/// * `protocol_version` - Protocol version
/// * `preserve_atimes` - Whether atime preservation is enabled
/// * `preserve_crtimes` - Whether crtime preservation is enabled
/// * `is_dir` - Whether entry is a directory
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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read};

    // ------------------------------------------------------------------------
    // Flag Encoding Tests
    // ------------------------------------------------------------------------

    #[test]
    fn encode_flags_single_byte() {
        let mut buf = Vec::new();
        encode_flags(&mut buf, XMIT_SAME_MODE as u32, 32, false, false).unwrap();
        assert_eq!(buf, vec![XMIT_SAME_MODE]);
    }

    #[test]
    fn encode_flags_two_bytes_protocol_28() {
        let mut buf = Vec::new();
        let xflags = (XMIT_HLINKED as u32) << 8; // Extended flags set
        encode_flags(&mut buf, xflags, 28, false, false).unwrap();
        // Should write XMIT_EXTENDED_FLAGS in low byte
        assert_eq!(buf.len(), 2);
        assert_eq!(buf[0] & XMIT_EXTENDED_FLAGS, XMIT_EXTENDED_FLAGS);
    }

    #[test]
    fn encode_flags_varint_mode() {
        let mut buf = Vec::new();
        encode_flags(&mut buf, 0x123, 32, true, false).unwrap();
        // Should use varint encoding
        let mut cursor = Cursor::new(&buf);
        let decoded = crate::varint::read_varint(&mut cursor).unwrap();
        assert_eq!(decoded, 0x123);
    }

    #[test]
    fn encode_flags_zero_becomes_extended_in_varint_mode() {
        let mut buf = Vec::new();
        encode_flags(&mut buf, 0, 32, true, false).unwrap();
        let mut cursor = Cursor::new(&buf);
        let decoded = crate::varint::read_varint(&mut cursor).unwrap();
        assert_eq!(decoded, XMIT_EXTENDED_FLAGS as i32);
    }

    #[test]
    fn encode_flags_zero_for_file_uses_top_dir_in_protocol_28() {
        let mut buf = Vec::new();
        encode_flags(&mut buf, 0, 28, false, false).unwrap();
        // For files with zero flags, should use XMIT_TOP_DIR
        assert!(buf[0] & XMIT_TOP_DIR != 0 || buf[0] & XMIT_EXTENDED_FLAGS != 0);
    }

    #[test]
    fn encode_flags_zero_for_dir_stays_zero_in_protocol_28() {
        let mut buf = Vec::new();
        encode_flags(&mut buf, 0, 28, false, true).unwrap();
        // For directories with zero flags, extended flags bit should be set
        // to distinguish from end-of-list marker
        assert!(buf.len() == 2 || buf[0] == XMIT_EXTENDED_FLAGS);
    }

    // ------------------------------------------------------------------------
    // End Marker Tests
    // ------------------------------------------------------------------------

    #[test]
    fn encode_end_marker_simple() {
        let mut buf = Vec::new();
        encode_end_marker(&mut buf, false, false, None).unwrap();
        assert_eq!(buf, vec![0u8]);
    }

    #[test]
    fn encode_end_marker_varint() {
        let mut buf = Vec::new();
        encode_end_marker(&mut buf, true, false, None).unwrap();
        // Two varints: 0 and 0
        let mut cursor = Cursor::new(&buf);
        assert_eq!(crate::varint::read_varint(&mut cursor).unwrap(), 0);
        assert_eq!(crate::varint::read_varint(&mut cursor).unwrap(), 0);
    }

    #[test]
    fn encode_end_marker_varint_with_error() {
        let mut buf = Vec::new();
        encode_end_marker(&mut buf, true, false, Some(23)).unwrap();
        let mut cursor = Cursor::new(&buf);
        assert_eq!(crate::varint::read_varint(&mut cursor).unwrap(), 0);
        assert_eq!(crate::varint::read_varint(&mut cursor).unwrap(), 23);
    }

    #[test]
    fn encode_end_marker_safe_file_list_with_error() {
        let mut buf = Vec::new();
        encode_end_marker(&mut buf, false, true, Some(42)).unwrap();
        assert_eq!(buf[0], XMIT_EXTENDED_FLAGS);
        assert_eq!(buf[1], XMIT_IO_ERROR_ENDLIST);
        let mut cursor = Cursor::new(&buf[2..]);
        assert_eq!(crate::varint::read_varint(&mut cursor).unwrap(), 42);
    }

    // ------------------------------------------------------------------------
    // Name Encoding Tests
    // ------------------------------------------------------------------------

    #[test]
    fn encode_name_no_compression() {
        let mut buf = Vec::new();
        encode_name(&mut buf, b"test.txt", 0, 0, 32).unwrap();
        // suffix_len byte + "test.txt"
        assert_eq!(buf.len(), 1 + 8);
        assert_eq!(buf[0], 8); // suffix length
        assert_eq!(&buf[1..], b"test.txt");
    }

    #[test]
    fn encode_name_with_compression() {
        let mut buf = Vec::new();
        encode_name(&mut buf, b"dir/file2.txt", 8, XMIT_SAME_NAME as u32, 32).unwrap();
        // same_len byte + suffix_len byte + "2.txt"
        assert_eq!(buf.len(), 1 + 1 + 5);
        assert_eq!(buf[0], 8); // same_len
        assert_eq!(buf[1], 5); // suffix_len
        assert_eq!(&buf[2..], b"2.txt");
    }

    #[test]
    fn encode_name_long_name_modern() {
        let mut buf = Vec::new();
        let long_name = vec![b'a'; 300];
        encode_name(&mut buf, &long_name, 0, XMIT_LONG_NAME as u32, 32).unwrap();
        // varint(300) + 300 bytes
        let mut cursor = Cursor::new(&buf);
        let len = crate::varint::read_varint(&mut cursor).unwrap();
        assert_eq!(len, 300);
    }

    #[test]
    fn encode_name_long_name_legacy() {
        let mut buf = Vec::new();
        let long_name = vec![b'a'; 300];
        encode_name(&mut buf, &long_name, 0, XMIT_LONG_NAME as u32, 29).unwrap();
        // 4-byte length + 300 bytes
        assert_eq!(buf.len(), 4 + 300);
        let len = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(len, 300);
    }

    // ------------------------------------------------------------------------
    // Size Encoding Tests
    // ------------------------------------------------------------------------

    #[test]
    fn encode_size_modern() {
        let mut buf = Vec::new();
        encode_size(&mut buf, 1000, 32).unwrap();
        // varlong30 with min_bytes=3
        let mut cursor = Cursor::new(&buf);
        let decoded = crate::read_varlong(&mut cursor, 3).unwrap();
        assert_eq!(decoded, 1000);
    }

    #[test]
    fn encode_size_legacy() {
        let mut buf = Vec::new();
        encode_size(&mut buf, 1000, 29).unwrap();
        // longint: 4 bytes for small values
        assert_eq!(buf.len(), 4);
        let decoded = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(decoded, 1000);
    }

    #[test]
    fn encode_size_large_legacy() {
        let mut buf = Vec::new();
        let large = 0x1_0000_0000u64;
        encode_size(&mut buf, large, 29).unwrap();
        // longint: 4-byte marker + 8-byte value
        assert_eq!(buf.len(), 12);
    }

    // ------------------------------------------------------------------------
    // Mode Encoding Tests
    // ------------------------------------------------------------------------

    #[test]
    fn encode_mode_regular_file() {
        let mut buf = Vec::new();
        encode_mode(&mut buf, 0o100644).unwrap();
        assert_eq!(buf.len(), 4);
        let decoded = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(decoded as u32, 0o100644);
    }

    #[test]
    fn encode_mode_directory() {
        let mut buf = Vec::new();
        encode_mode(&mut buf, 0o040755).unwrap();
        let decoded = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(decoded as u32, 0o040755);
    }

    // ------------------------------------------------------------------------
    // UID/GID Encoding Tests
    // ------------------------------------------------------------------------

    #[test]
    fn encode_uid_modern() {
        let mut buf = Vec::new();
        encode_uid(&mut buf, 1000, 32).unwrap();
        let mut cursor = Cursor::new(&buf);
        let decoded = crate::varint::read_varint(&mut cursor).unwrap();
        assert_eq!(decoded, 1000);
    }

    #[test]
    fn encode_uid_legacy() {
        let mut buf = Vec::new();
        encode_uid(&mut buf, 1000, 29).unwrap();
        assert_eq!(buf.len(), 4);
        let decoded = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(decoded, 1000);
    }

    #[test]
    fn encode_gid_modern() {
        let mut buf = Vec::new();
        encode_gid(&mut buf, 500, 30).unwrap();
        let mut cursor = Cursor::new(&buf);
        let decoded = crate::varint::read_varint(&mut cursor).unwrap();
        assert_eq!(decoded, 500);
    }

    #[test]
    fn encode_owner_name_short() {
        let mut buf = Vec::new();
        encode_owner_name(&mut buf, "user").unwrap();
        assert_eq!(buf[0], 4); // length
        assert_eq!(&buf[1..], b"user");
    }

    #[test]
    fn encode_owner_name_truncated() {
        let mut buf = Vec::new();
        let long_name = "a".repeat(300);
        encode_owner_name(&mut buf, &long_name).unwrap();
        assert_eq!(buf[0], 255); // max length
        assert_eq!(buf.len(), 256);
    }

    // ------------------------------------------------------------------------
    // Device Number Encoding Tests
    // ------------------------------------------------------------------------

    #[test]
    fn encode_rdev_protocol_30() {
        let mut buf = Vec::new();
        encode_rdev(&mut buf, 8, 1, 0, 30).unwrap();
        // varint30(major) + varint(minor)
        let mut cursor = Cursor::new(&buf);
        let major = crate::varint::read_varint30_int(&mut cursor, 30).unwrap();
        let minor = crate::varint::read_varint(&mut cursor).unwrap();
        assert_eq!(major, 8);
        assert_eq!(minor, 1);
    }

    #[test]
    fn encode_rdev_same_major() {
        let mut buf = Vec::new();
        let xflags = (XMIT_SAME_RDEV_MAJOR as u32) << 8;
        encode_rdev(&mut buf, 8, 1, xflags, 30).unwrap();
        // Only minor written
        let mut cursor = Cursor::new(&buf);
        let minor = crate::varint::read_varint(&mut cursor).unwrap();
        assert_eq!(minor, 1);
    }

    #[test]
    fn encode_rdev_protocol_29_minor_8bit() {
        let mut buf = Vec::new();
        let xflags = (XMIT_RDEV_MINOR_8_PRE30 as u32) << 8;
        encode_rdev(&mut buf, 8, 5, xflags, 29).unwrap();
        // varint30(major) + u8(minor)
        // Find where minor is (after major)
        let minor_offset = buf.len() - 1;
        assert_eq!(buf[minor_offset], 5);
    }

    // ------------------------------------------------------------------------
    // Symlink Target Encoding Tests
    // ------------------------------------------------------------------------

    #[test]
    fn encode_symlink_target_simple() {
        let mut buf = Vec::new();
        encode_symlink_target(&mut buf, b"/target/path", 32).unwrap();
        // varint30(len) + bytes
        let mut cursor = Cursor::new(&buf);
        let len = crate::varint::read_varint30_int(&mut cursor, 32).unwrap();
        assert_eq!(len, 12);
    }

    #[test]
    fn encode_symlink_target_relative() {
        let mut buf = Vec::new();
        encode_symlink_target(&mut buf, b"../lib/libfoo.so", 32).unwrap();
        let mut cursor = Cursor::new(&buf);
        let len = crate::varint::read_varint30_int(&mut cursor, 32).unwrap();
        assert_eq!(len, 16);
        let mut target = vec![0u8; len as usize];
        cursor.read_exact(&mut target).unwrap();
        assert_eq!(&target, b"../lib/libfoo.so");
    }

    #[test]
    fn encode_symlink_target_empty() {
        let mut buf = Vec::new();
        encode_symlink_target(&mut buf, b"", 32).unwrap();
        let mut cursor = Cursor::new(&buf);
        let len = crate::varint::read_varint30_int(&mut cursor, 32).unwrap();
        assert_eq!(len, 0);
    }

    #[test]
    fn encode_symlink_target_with_spaces_and_unicode() {
        let target = "path/to/my file/\u{00e9}t\u{00e9}".as_bytes();
        let mut buf = Vec::new();
        encode_symlink_target(&mut buf, target, 32).unwrap();
        let mut cursor = Cursor::new(&buf);
        let len = crate::varint::read_varint30_int(&mut cursor, 32).unwrap();
        assert_eq!(len as usize, target.len());
        let mut decoded = vec![0u8; len as usize];
        cursor.read_exact(&mut decoded).unwrap();
        assert_eq!(&decoded, target);
    }

    #[test]
    fn encode_symlink_target_protocol_29_uses_fixed_int() {
        let target = b"/target";
        let mut buf = Vec::new();
        encode_symlink_target(&mut buf, target, 29).unwrap();
        // Protocol < 30: 4 bytes for length + target bytes
        assert_eq!(buf.len(), 4 + target.len());
        let len = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(len, 7);
        assert_eq!(&buf[4..], target);
    }

    #[test]
    fn encode_symlink_target_protocol_30_uses_varint() {
        let target = b"/target";
        let mut buf = Vec::new();
        encode_symlink_target(&mut buf, target, 30).unwrap();
        // Protocol 30+: varint (1 byte for small values) + target bytes
        assert!(buf.len() < 4 + target.len()); // More compact than fixed int
        assert!(buf.ends_with(target));
    }

    #[test]
    fn encode_symlink_target_long_path() {
        let target = vec![b'a'; 4096];
        let mut buf = Vec::new();
        encode_symlink_target(&mut buf, &target, 32).unwrap();
        let mut cursor = Cursor::new(&buf);
        let len = crate::varint::read_varint30_int(&mut cursor, 32).unwrap();
        assert_eq!(len, 4096);
    }

    #[test]
    fn encode_symlink_target_path_separators_preserved() {
        // Verify both forward and backslash are preserved as-is (no conversion)
        let target = b"dir/subdir\\file";
        let mut buf = Vec::new();
        encode_symlink_target(&mut buf, target, 32).unwrap();
        let mut cursor = Cursor::new(&buf);
        let len = crate::varint::read_varint30_int(&mut cursor, 32).unwrap();
        let mut decoded = vec![0u8; len as usize];
        cursor.read_exact(&mut decoded).unwrap();
        assert_eq!(&decoded, target);
    }

    // ------------------------------------------------------------------------
    // Hardlink Encoding Tests
    // ------------------------------------------------------------------------

    #[test]
    fn encode_hardlink_idx_simple() {
        let mut buf = Vec::new();
        encode_hardlink_idx(&mut buf, 5).unwrap();
        let mut cursor = Cursor::new(&buf);
        let idx = crate::varint::read_varint(&mut cursor).unwrap();
        assert_eq!(idx, 5);
    }

    #[test]
    fn encode_hardlink_dev_ino_different_dev() {
        let mut buf = Vec::new();
        encode_hardlink_dev_ino(&mut buf, 100, 12345, false).unwrap();
        // longint(dev+1) + longint(ino)
        let mut cursor = Cursor::new(&buf);
        let dev_plus_one = crate::read_longint(&mut cursor).unwrap();
        let ino = crate::read_longint(&mut cursor).unwrap();
        assert_eq!(dev_plus_one, 101); // dev + 1
        assert_eq!(ino, 12345);
    }

    #[test]
    fn encode_hardlink_dev_ino_same_dev() {
        let mut buf = Vec::new();
        encode_hardlink_dev_ino(&mut buf, 100, 12345, true).unwrap();
        // Only longint(ino)
        let mut cursor = Cursor::new(&buf);
        let ino = crate::read_longint(&mut cursor).unwrap();
        assert_eq!(ino, 12345);
    }

    // ------------------------------------------------------------------------
    // Checksum Encoding Tests
    // ------------------------------------------------------------------------

    #[test]
    fn encode_checksum_with_data() {
        let mut buf = Vec::new();
        let checksum = vec![0xAA, 0xBB, 0xCC, 0xDD];
        encode_checksum(&mut buf, Some(&checksum), 4).unwrap();
        assert_eq!(buf, checksum);
    }

    #[test]
    fn encode_checksum_padded() {
        let mut buf = Vec::new();
        let checksum = vec![0xAA, 0xBB];
        encode_checksum(&mut buf, Some(&checksum), 4).unwrap();
        assert_eq!(buf, vec![0xAA, 0xBB, 0x00, 0x00]);
    }

    #[test]
    fn encode_checksum_none() {
        let mut buf = Vec::new();
        encode_checksum(&mut buf, None, 4).unwrap();
        assert_eq!(buf, vec![0x00, 0x00, 0x00, 0x00]);
    }

    // ------------------------------------------------------------------------
    // Flag Calculation Tests
    // ------------------------------------------------------------------------

    #[test]
    fn calculate_name_prefix_len_full_match() {
        assert_eq!(calculate_name_prefix_len(b"test.txt", b"test.txt"), 8);
    }

    #[test]
    fn calculate_name_prefix_len_partial() {
        assert_eq!(
            calculate_name_prefix_len(b"dir/file1.txt", b"dir/file2.txt"),
            8
        );
    }

    #[test]
    fn calculate_name_prefix_len_no_match() {
        assert_eq!(calculate_name_prefix_len(b"abc", b"xyz"), 0);
    }

    #[test]
    fn calculate_name_prefix_len_capped_at_255() {
        let long = vec![b'a'; 300];
        assert_eq!(calculate_name_prefix_len(&long, &long), 255);
    }

    #[test]
    fn calculate_basic_flags_all_same() {
        let flags = calculate_basic_flags(
            0o100644, 0o100644, // same mode
            1000, 1000, // same mtime
            500, 500, // same uid
            600, 600, // same gid
            5, 3, // some prefix compression
            true, true, false,
        );
        assert!(flags & XMIT_SAME_MODE != 0);
        assert!(flags & XMIT_SAME_TIME != 0);
        assert!(flags & XMIT_SAME_UID != 0);
        assert!(flags & XMIT_SAME_GID != 0);
        assert!(flags & XMIT_SAME_NAME != 0);
    }

    #[test]
    fn calculate_basic_flags_all_different() {
        let flags = calculate_basic_flags(
            0o100644, 0o100755, // different mode
            1000, 2000, // different mtime
            500, 600, // different uid
            700, 800, // different gid
            0, 8, // no prefix compression
            true, true, false,
        );
        assert!(flags & XMIT_SAME_MODE == 0);
        assert!(flags & XMIT_SAME_TIME == 0);
        assert!(flags & XMIT_SAME_UID == 0);
        assert!(flags & XMIT_SAME_GID == 0);
        assert!(flags & XMIT_SAME_NAME == 0);
    }

    #[test]
    fn calculate_basic_flags_long_name() {
        let flags = calculate_basic_flags(
            0o100644, 0o100644, 1000, 1000, 0, 0, 0, 0, 0, 300, // suffix > 255
            false, false, false,
        );
        assert!(flags & XMIT_LONG_NAME != 0);
    }

    #[test]
    fn calculate_basic_flags_top_dir() {
        let flags = calculate_basic_flags(0o040755, 0, 0, 0, 0, 0, 0, 0, 0, 3, false, false, true);
        assert!(flags & XMIT_TOP_DIR != 0);
    }

    #[test]
    fn calculate_device_flags_same_major() {
        let flags = calculate_device_flags(8, 8, 1, 30);
        assert!(flags & XMIT_SAME_RDEV_MAJOR != 0);
    }

    #[test]
    fn calculate_device_flags_minor_8bit_proto29() {
        let flags = calculate_device_flags(8, 0, 5, 29);
        assert!(flags & XMIT_RDEV_MINOR_8_PRE30 != 0);
    }

    #[test]
    fn calculate_device_flags_minor_large_proto29() {
        let flags = calculate_device_flags(8, 0, 300, 29);
        assert!(flags & XMIT_RDEV_MINOR_8_PRE30 == 0);
    }

    #[test]
    fn calculate_hardlink_flags_proto30_first() {
        let flags = calculate_hardlink_flags(Some(u32::MAX), None, 0, 30, false);
        assert!(flags & XMIT_HLINKED != 0);
        assert!(flags & XMIT_HLINK_FIRST != 0);
    }

    #[test]
    fn calculate_hardlink_flags_proto30_follower() {
        let flags = calculate_hardlink_flags(Some(5), None, 0, 30, false);
        assert!(flags & XMIT_HLINKED != 0);
        assert!(flags & XMIT_HLINK_FIRST == 0);
    }

    #[test]
    fn calculate_hardlink_flags_proto29_same_dev() {
        let flags = calculate_hardlink_flags(None, Some(100), 100, 29, false);
        assert!(flags & XMIT_SAME_DEV_PRE30 != 0);
    }

    #[test]
    fn calculate_hardlink_flags_directory_ignored() {
        let flags = calculate_hardlink_flags(Some(5), None, 0, 30, true);
        assert!(flags == 0);
    }

    #[test]
    fn calculate_time_flags_same_atime() {
        let flags = calculate_time_flags(1000, 1000, 0, 0, 0, 31, true, false, false);
        assert!(flags & (XMIT_SAME_ATIME as u16) != 0);
    }

    #[test]
    fn calculate_time_flags_crtime_eq_mtime() {
        let flags = calculate_time_flags(0, 0, 5000, 5000, 0, 31, false, true, false);
        assert!(flags & ((XMIT_CRTIME_EQ_MTIME as u16) << 8) != 0);
    }

    #[test]
    fn calculate_time_flags_mtime_nsec() {
        let flags = calculate_time_flags(0, 0, 0, 1000, 123456, 31, false, false, false);
        assert!(flags & (XMIT_MOD_NSEC as u16) != 0);
    }

    #[test]
    fn calculate_time_flags_no_nsec_proto30() {
        let flags = calculate_time_flags(0, 0, 0, 1000, 123456, 30, false, false, false);
        assert!(flags & (XMIT_MOD_NSEC as u16) == 0);
    }

    // ------------------------------------------------------------------------
    // Mtime Encoding Tests
    // ------------------------------------------------------------------------

    #[test]
    fn encode_mtime_modern() {
        let mut buf = Vec::new();
        encode_mtime(&mut buf, 1700000000, 32).unwrap();
        let mut cursor = Cursor::new(&buf);
        let decoded = crate::read_varlong(&mut cursor, 4).unwrap();
        assert_eq!(decoded, 1700000000);
    }

    #[test]
    fn encode_mtime_legacy() {
        let mut buf = Vec::new();
        encode_mtime(&mut buf, 1700000000, 29).unwrap();
        assert_eq!(buf.len(), 4);
        let decoded = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(decoded, 1700000000);
    }

    #[test]
    fn test_encode_mtime_nsec() {
        let mut buf = Vec::new();
        super::encode_mtime_nsec(&mut buf, 123456789).unwrap();
        let mut cursor = Cursor::new(&buf);
        let decoded = crate::varint::read_varint(&mut cursor).unwrap();
        assert_eq!(decoded, 123456789);
    }

    #[test]
    fn encode_atime_simple() {
        let mut buf = Vec::new();
        encode_atime(&mut buf, 1700000001).unwrap();
        let mut cursor = Cursor::new(&buf);
        let decoded = crate::read_varlong(&mut cursor, 4).unwrap();
        assert_eq!(decoded, 1700000001);
    }

    #[test]
    fn encode_crtime_simple() {
        let mut buf = Vec::new();
        encode_crtime(&mut buf, 1600000000).unwrap();
        let mut cursor = Cursor::new(&buf);
        let decoded = crate::read_varlong(&mut cursor, 4).unwrap();
        assert_eq!(decoded, 1600000000);
    }

    // ------------------------------------------------------------------------
    // Round-trip Integration Tests
    // ------------------------------------------------------------------------

    #[test]
    fn roundtrip_flags_and_name() {
        let mut buf = Vec::new();

        // Write flags + name for "dir/file2.txt" with prefix compression
        let xflags = XMIT_SAME_NAME as u32 | XMIT_SAME_MODE as u32;
        encode_flags(&mut buf, xflags, 32, false, false).unwrap();
        encode_name(&mut buf, b"dir/file2.txt", 8, xflags, 32).unwrap();

        // Verify structure
        assert_eq!(buf[0], xflags as u8); // flags byte
        assert_eq!(buf[1], 8); // same_len
        assert_eq!(buf[2], 5); // suffix_len ("2.txt")
        assert_eq!(&buf[3..], b"2.txt");
    }

    #[test]
    fn roundtrip_full_entry_modern() {
        let mut buf = Vec::new();

        // Encode a complete file entry
        let xflags = 0u32; // All fields different from previous
        encode_flags(&mut buf, xflags, 32, false, false).unwrap();
        encode_name(&mut buf, b"test.txt", 0, xflags, 32).unwrap();
        encode_size(&mut buf, 1024, 32).unwrap();
        encode_mtime(&mut buf, 1700000000, 32).unwrap();
        encode_mode(&mut buf, 0o100644).unwrap();

        // Should have produced a valid byte sequence
        assert!(!buf.is_empty());
    }
}
