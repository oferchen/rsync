#![deny(unsafe_code)]
//! File entry wire format decoding for the rsync protocol.
//!
//! This module provides low-level wire format decoding functions for file list entries,
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

use std::io::{self, Read};

use crate::varint::{read_int, read_longint, read_varint, read_varint30_int, read_varlong};

// Re-export flag constants from the encoding module
use super::file_entry::{
    XMIT_CRTIME_EQ_MTIME, XMIT_EXTENDED_FLAGS, XMIT_GROUP_NAME_FOLLOWS, XMIT_HLINK_FIRST,
    XMIT_HLINKED, XMIT_IO_ERROR_ENDLIST, XMIT_LONG_NAME, XMIT_MOD_NSEC, XMIT_RDEV_MINOR_8_PRE30,
    XMIT_SAME_ATIME, XMIT_SAME_DEV_PRE30, XMIT_SAME_GID, XMIT_SAME_MODE, XMIT_SAME_NAME,
    XMIT_SAME_RDEV_MAJOR, XMIT_SAME_RDEV_PRE28, XMIT_SAME_TIME, XMIT_SAME_UID,
    XMIT_USER_NAME_FOLLOWS,
};

// ============================================================================
// Flag Decoding
// ============================================================================

/// Decodes transmission flags from the wire format.
///
/// The decoding varies by protocol version and compatibility flags:
/// - **Varint mode** (VARINT_FLIST_FLAGS): Single varint containing all flag bits
/// - **Protocol 28+**: 1 byte, or 2 bytes if extended flags present
/// - **Protocol < 28**: 1 byte only
///
/// # Arguments
///
/// * `reader` - Input stream
/// * `protocol_version` - Protocol version for decoding selection
/// * `use_varint_flags` - Whether VARINT_FLIST_FLAGS compat flag is set
///
/// # Returns
///
/// Returns `(flags, is_end_marker)` where:
/// - `flags` is the decoded flag bits (u32)
/// - `is_end_marker` is true if this represents an end-of-list marker (flags == 0)
///
/// # Wire Format
///
/// | Mode | Format |
/// |------|--------|
/// | Varint | `varint(xflags)` where 0 means end-of-list |
/// | Proto 28+ | `u8` or `u16 LE` if XMIT_EXTENDED_FLAGS set |
/// | Proto < 28 | `u8` only |
///
/// # Examples
///
/// ```
/// use protocol::wire::file_entry_decode::decode_flags;
/// use std::io::Cursor;
///
/// let data = vec![0x02]; // XMIT_SAME_MODE
/// let mut cursor = Cursor::new(data);
/// let (flags, is_end) = decode_flags(&mut cursor, 32, false).unwrap();
/// assert_eq!(flags, 0x02);
/// assert!(!is_end);
/// ```
///
/// # Upstream Reference
///
/// See `flist.c:recv_file_entry()` lines 760-790
pub fn decode_flags<R: Read>(
    reader: &mut R,
    protocol_version: u8,
    use_varint_flags: bool,
) -> io::Result<(u32, bool)> {
    if use_varint_flags {
        // Varint mode: read single varint
        let flags = read_varint(reader)? as u32;

        // In varint mode:
        // - actual 0 means end-of-list
        // - XMIT_EXTENDED_FLAGS was written for flags=0 to avoid ambiguity
        if flags == 0 {
            // True end marker
            Ok((0, true))
        } else if flags == XMIT_EXTENDED_FLAGS as u32 {
            // Normal entry with all flags clear
            Ok((0, false))
        } else {
            // Normal entry with flags set
            Ok((flags, false))
        }
    } else if protocol_version >= 28 {
        // Protocol 28+: read first byte
        let mut first_byte = [0u8; 1];
        reader.read_exact(&mut first_byte)?;
        let flags0 = first_byte[0];

        if flags0 == 0 {
            // End-of-list marker
            return Ok((0, true));
        }

        // Check if extended flags follow
        if flags0 & XMIT_EXTENDED_FLAGS != 0 {
            // Read second byte
            let mut second_byte = [0u8; 1];
            reader.read_exact(&mut second_byte)?;
            let flags1 = second_byte[0];

            // Combine: low byte + extended byte
            let flags = (flags0 as u32) | ((flags1 as u32) << 8);
            Ok((flags, false))
        } else {
            Ok((flags0 as u32, false))
        }
    } else {
        // Protocol < 28: single byte only
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte)?;
        let flags = byte[0];

        if flags == 0 {
            Ok((0, true))
        } else {
            Ok((flags as u32, false))
        }
    }
}

/// Decodes the end-of-list marker and optional I/O error code.
///
/// # Arguments
///
/// * `reader` - Input stream
/// * `use_varint_flags` - Whether VARINT_FLIST_FLAGS compat flag is set
/// * `use_safe_file_list` - Whether SAFE_FILE_LIST compat flag is set or protocol >= 31
/// * `flags` - Already-read flags (for non-varint mode with IO_ERROR_ENDLIST)
///
/// # Returns
///
/// Returns optional I/O error code if present.
///
/// # Wire Format
///
/// | Mode | Format |
/// |------|--------|
/// | Varint | `varint(0)` + `varint(io_error)` |
/// | Safe file list with XMIT_IO_ERROR_ENDLIST | `varint(error)` |
/// | Normal | Nothing (flags == 0 is sufficient) |
///
/// # Examples
///
/// ```no_run
/// use protocol::wire::file_entry_decode::decode_end_marker;
/// use std::io::Cursor;
///
/// // Varint mode with error code 23
/// let data = vec![0x00, 0x17]; // varint(0), varint(23)
/// let mut cursor = Cursor::new(data);
/// let error = decode_end_marker(&mut cursor, true, false, 0).unwrap();
/// assert_eq!(error, Some(23));
/// ```
pub fn decode_end_marker<R: Read>(
    reader: &mut R,
    use_varint_flags: bool,
    use_safe_file_list: bool,
    flags: u32,
) -> io::Result<Option<i32>> {
    if use_varint_flags {
        // In varint mode, flags already read as 0
        // Read the error code varint
        let error = read_varint(reader)?;
        Ok(if error == 0 { None } else { Some(error) })
    } else if use_safe_file_list && (flags & ((XMIT_IO_ERROR_ENDLIST as u32) << 8)) != 0 {
        // Safe file list mode with IO_ERROR_ENDLIST flag
        let error = read_varint(reader)?;
        Ok(Some(error))
    } else {
        // Normal mode: no error code
        Ok(None)
    }
}

// ============================================================================
// Name Decoding
// ============================================================================

/// Decodes a file name with prefix decompression.
///
/// The rsync protocol compresses file names by sharing common prefixes with
/// the previous entry. This function decodes the name suffix and reconstructs
/// the full name.
///
/// # Arguments
///
/// * `reader` - Input stream
/// * `flags` - Transmission flags (used to check XMIT_SAME_NAME and XMIT_LONG_NAME)
/// * `prev_name` - Previous entry's full name for prefix reconstruction
/// * `protocol_version` - Protocol version (affects long name length decoding)
///
/// # Returns
///
/// Returns the decoded full name as a `Vec<u8>`.
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
/// use protocol::wire::file_entry_decode::decode_name;
/// use protocol::wire::file_entry::XMIT_SAME_NAME;
/// use std::io::Cursor;
///
/// // Decoding "dir/file2.txt" when previous was "dir/file1.txt"
/// // same_len=8 ("dir/file") + suffix_len=5 + "2.txt"
/// let data = vec![8, 5, b'2', b'.', b't', b'x', b't'];
/// let mut cursor = Cursor::new(data);
/// let name = decode_name(&mut cursor, XMIT_SAME_NAME as u32, b"dir/file1.txt", 32).unwrap();
/// assert_eq!(name, b"dir/file2.txt");
/// ```
///
/// # Upstream Reference
///
/// See `flist.c:recv_file_entry()` lines 800-850
pub fn decode_name<R: Read>(
    reader: &mut R,
    flags: u32,
    prev_name: &[u8],
    protocol_version: u8,
) -> io::Result<Vec<u8>> {
    // Read same_len if XMIT_SAME_NAME is set
    let same_len = if flags & (XMIT_SAME_NAME as u32) != 0 {
        let mut buf = [0u8; 1];
        reader.read_exact(&mut buf)?;
        buf[0] as usize
    } else {
        0
    };

    // Read suffix length
    let suffix_len = if flags & (XMIT_LONG_NAME as u32) != 0 {
        // Long name: protocol-dependent encoding
        if protocol_version >= 30 {
            read_varint(reader)? as usize
        } else {
            read_int(reader)? as usize
        }
    } else {
        // Short name: single byte
        let mut buf = [0u8; 1];
        reader.read_exact(&mut buf)?;
        buf[0] as usize
    };

    // Read suffix bytes
    let mut suffix = vec![0u8; suffix_len];
    reader.read_exact(&mut suffix)?;

    // Reconstruct full name from prefix + suffix
    let mut name = Vec::with_capacity(same_len + suffix_len);
    if same_len > 0 {
        let prefix_len = same_len.min(prev_name.len());
        name.extend_from_slice(&prev_name[..prefix_len]);
    }
    name.extend_from_slice(&suffix);

    Ok(name)
}

// ============================================================================
// Size Decoding
// ============================================================================

/// Decodes file size from the wire format.
///
/// # Arguments
///
/// * `reader` - Input stream
/// * `protocol_version` - Protocol version (affects decoding format)
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
/// ```no_run
/// use protocol::wire::file_entry_decode::decode_size;
/// use std::io::Cursor;
///
/// // Modern protocol uses varlong30
/// let data = vec![0xE8, 0x03, 0x00]; // varlong30(1000) with min_bytes=3
/// let mut cursor = Cursor::new(data);
/// let size = decode_size(&mut cursor, 32).unwrap();
/// assert_eq!(size, 1000);
/// ```
///
/// # Upstream Reference
///
/// See `flist.c:recv_file_entry()` line 856: `file_length = read_varlong30(f, 3)`
pub fn decode_size<R: Read>(reader: &mut R, protocol_version: u8) -> io::Result<i64> {
    if protocol_version >= 30 {
        read_varlong(reader, 3)
    } else {
        read_longint(reader)
    }
}

// ============================================================================
// Time Decoding
// ============================================================================

/// Decodes modification time from the wire format.
///
/// # Arguments
///
/// * `reader` - Input stream
/// * `flags` - Transmission flags (checks XMIT_SAME_TIME)
/// * `prev_mtime` - Previous entry's mtime (returned if XMIT_SAME_TIME set)
/// * `protocol_version` - Protocol version (affects decoding format)
///
/// # Returns
///
/// Returns the decoded mtime, or `None` if XMIT_SAME_TIME is set.
///
/// # Wire Format
///
/// | Protocol | Format |
/// |----------|--------|
/// | >= 30 | varlong (min_bytes=4) |
/// | < 30 | Fixed 4-byte i32 LE |
///
/// # Note
///
/// Only decodes when `XMIT_SAME_TIME` flag is NOT set.
///
/// # Examples
///
/// ```
/// use protocol::wire::file_entry_decode::decode_mtime;
/// use std::io::Cursor;
///
/// let data = vec![0x00, 0x00, 0x5E, 0x40]; // Fixed i32 LE for protocol < 30
/// let mut cursor = Cursor::new(data);
/// let mtime = decode_mtime(&mut cursor, 0, 0, 29).unwrap();
/// assert!(mtime.is_some());
/// ```
///
/// # Upstream Reference
///
/// See `flist.c:recv_file_entry()` lines 858-862
pub fn decode_mtime<R: Read>(
    reader: &mut R,
    flags: u32,
    prev_mtime: i64,
    protocol_version: u8,
) -> io::Result<Option<i64>> {
    if flags & (XMIT_SAME_TIME as u32) != 0 {
        Ok(Some(prev_mtime))
    } else if protocol_version >= 30 {
        Ok(Some(read_varlong(reader, 4)?))
    } else {
        Ok(Some(read_int(reader)? as i64))
    }
}

/// Decodes modification time nanoseconds (protocol 31+).
///
/// # Arguments
///
/// * `reader` - Input stream
/// * `flags` - Transmission flags (checks XMIT_MOD_NSEC)
///
/// # Returns
///
/// Returns nanosecond component if XMIT_MOD_NSEC flag is set, otherwise None.
///
/// # Wire Format
///
/// `varint(nsec)`
///
/// # Note
///
/// Only decode when `XMIT_MOD_NSEC` flag is set in xflags.
pub fn decode_mtime_nsec<R: Read>(reader: &mut R, flags: u32) -> io::Result<Option<u32>> {
    if flags & ((XMIT_MOD_NSEC as u32) << 8) != 0 {
        Ok(Some(read_varint(reader)? as u32))
    } else {
        Ok(None)
    }
}

/// Decodes access time (for --atimes, non-directories only).
///
/// # Arguments
///
/// * `reader` - Input stream
/// * `flags` - Transmission flags (checks XMIT_SAME_ATIME)
/// * `prev_atime` - Previous entry's atime (returned if XMIT_SAME_ATIME set)
///
/// # Returns
///
/// Returns the decoded atime, or previous value if same as previous.
///
/// # Wire Format
///
/// `varlong(atime, 4)`
///
/// # Note
///
/// Only decode when preserve_atimes is enabled and entry is not a directory.
pub fn decode_atime<R: Read>(
    reader: &mut R,
    flags: u32,
    prev_atime: i64,
) -> io::Result<Option<i64>> {
    if flags & ((XMIT_SAME_ATIME as u32) << 8) != 0 {
        Ok(Some(prev_atime))
    } else {
        Ok(Some(read_varlong(reader, 4)?))
    }
}

/// Decodes creation time (for --crtimes).
///
/// # Arguments
///
/// * `reader` - Input stream
/// * `flags` - Transmission flags (checks XMIT_CRTIME_EQ_MTIME)
/// * `mtime` - Current entry's mtime (returned if XMIT_CRTIME_EQ_MTIME set)
///
/// # Returns
///
/// Returns the decoded crtime, or mtime if equal.
///
/// # Wire Format
///
/// `varlong(crtime, 4)`
///
/// # Note
///
/// Only decode when `XMIT_CRTIME_EQ_MTIME` flag is NOT set.
pub fn decode_crtime<R: Read>(reader: &mut R, flags: u32, mtime: i64) -> io::Result<Option<i64>> {
    if flags & ((XMIT_CRTIME_EQ_MTIME as u32) << 16) != 0 {
        Ok(Some(mtime))
    } else {
        Ok(Some(read_varlong(reader, 4)?))
    }
}

// ============================================================================
// Mode Decoding
// ============================================================================

/// Decodes Unix mode bits from the wire format.
///
/// Mode is always encoded as a fixed 4-byte little-endian integer.
///
/// # Arguments
///
/// * `reader` - Input stream
/// * `flags` - Transmission flags (checks XMIT_SAME_MODE)
/// * `prev_mode` - Previous entry's mode (returned if XMIT_SAME_MODE set)
///
/// # Returns
///
/// Returns the decoded mode, or `None` if XMIT_SAME_MODE is set.
///
/// # Wire Format
///
/// Fixed 4-byte i32 LE
///
/// # Note
///
/// Only decode when `XMIT_SAME_MODE` flag is NOT set.
///
/// # Examples
///
/// ```
/// use protocol::wire::file_entry_decode::decode_mode;
/// use std::io::Cursor;
///
/// let data = vec![0xA4, 0x81, 0x00, 0x00]; // 0o100644 in LE
/// let mut cursor = Cursor::new(data);
/// let mode = decode_mode(&mut cursor, 0, 0).unwrap();
/// assert_eq!(mode.unwrap(), 0o100644);
/// ```
pub fn decode_mode<R: Read>(reader: &mut R, flags: u32, prev_mode: u32) -> io::Result<Option<u32>> {
    if flags & (XMIT_SAME_MODE as u32) != 0 {
        Ok(Some(prev_mode))
    } else {
        let mode = read_int(reader)? as u32;
        Ok(Some(mode))
    }
}

// ============================================================================
// UID/GID Decoding
// ============================================================================

/// Decodes a user ID from the wire format.
///
/// # Arguments
///
/// * `reader` - Input stream
/// * `flags` - Transmission flags (checks XMIT_SAME_UID, XMIT_USER_NAME_FOLLOWS)
/// * `prev_uid` - Previous entry's UID (returned if XMIT_SAME_UID set)
/// * `protocol_version` - Protocol version (affects decoding format)
///
/// # Returns
///
/// Returns `(uid, optional_name)` tuple, or `None` if XMIT_SAME_UID is set.
///
/// # Wire Format
///
/// | Protocol | Format |
/// |----------|--------|
/// | >= 30 | varint + optional name (u8 len + bytes) |
/// | < 30 | Fixed 4-byte i32 LE |
///
/// # Note
///
/// Only decode when preserve_uid is enabled and `XMIT_SAME_UID` flag is NOT set.
pub fn decode_uid<R: Read>(
    reader: &mut R,
    flags: u32,
    prev_uid: u32,
    protocol_version: u8,
) -> io::Result<Option<(u32, Option<String>)>> {
    if flags & (XMIT_SAME_UID as u32) != 0 {
        Ok(Some((prev_uid, None)))
    } else {
        let uid = if protocol_version >= 30 {
            read_varint(reader)? as u32
        } else {
            read_int(reader)? as u32
        };

        // Check if name follows (protocol 30+)
        let name =
            if protocol_version >= 30 && (flags & ((XMIT_USER_NAME_FOLLOWS as u32) << 8)) != 0 {
                Some(decode_owner_name(reader)?)
            } else {
                None
            };

        Ok(Some((uid, name)))
    }
}

/// Decodes a group ID from the wire format.
///
/// # Arguments
///
/// * `reader` - Input stream
/// * `flags` - Transmission flags (checks XMIT_SAME_GID, XMIT_GROUP_NAME_FOLLOWS)
/// * `prev_gid` - Previous entry's GID (returned if XMIT_SAME_GID set)
/// * `protocol_version` - Protocol version (affects decoding format)
///
/// # Returns
///
/// Returns `(gid, optional_name)` tuple, or `None` if XMIT_SAME_GID is set.
///
/// # Wire Format
///
/// | Protocol | Format |
/// |----------|--------|
/// | >= 30 | varint + optional name (u8 len + bytes) |
/// | < 30 | Fixed 4-byte i32 LE |
///
/// # Note
///
/// Only decode when preserve_gid is enabled and `XMIT_SAME_GID` flag is NOT set.
pub fn decode_gid<R: Read>(
    reader: &mut R,
    flags: u32,
    prev_gid: u32,
    protocol_version: u8,
) -> io::Result<Option<(u32, Option<String>)>> {
    if flags & (XMIT_SAME_GID as u32) != 0 {
        Ok(Some((prev_gid, None)))
    } else {
        let gid = if protocol_version >= 30 {
            read_varint(reader)? as u32
        } else {
            read_int(reader)? as u32
        };

        // Check if name follows (protocol 30+)
        let name =
            if protocol_version >= 30 && (flags & ((XMIT_GROUP_NAME_FOLLOWS as u32) << 8)) != 0 {
                Some(decode_owner_name(reader)?)
            } else {
                None
            };

        Ok(Some((gid, name)))
    }
}

/// Decodes a user or group name (protocol 30+).
///
/// # Arguments
///
/// * `reader` - Input stream
///
/// # Wire Format
///
/// `u8(len)` + `name_bytes[0..len]`
///
/// # Note
///
/// Only decode when `XMIT_USER_NAME_FOLLOWS` or `XMIT_GROUP_NAME_FOLLOWS` flag is set.
fn decode_owner_name<R: Read>(reader: &mut R) -> io::Result<String> {
    let mut len_buf = [0u8; 1];
    reader.read_exact(&mut len_buf)?;
    let len = len_buf[0] as usize;

    let mut name_bytes = vec![0u8; len];
    reader.read_exact(&mut name_bytes)?;

    String::from_utf8(name_bytes).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid UTF-8 in owner name: {e}"),
        )
    })
}

// ============================================================================
// Device Number Decoding
// ============================================================================

/// Decodes device numbers for block/character devices.
///
/// # Arguments
///
/// * `reader` - Input stream
/// * `flags` - Transmission flags (checks XMIT_SAME_RDEV_MAJOR, XMIT_RDEV_MINOR_8_PRE30)
/// * `prev_rdev_major` - Previous device major number (used if XMIT_SAME_RDEV_MAJOR set)
/// * `protocol_version` - Protocol version
///
/// # Returns
///
/// Returns `(major, minor)` device numbers.
///
/// # Wire Format (Protocol 28+)
///
/// - Major: varint30 (omitted if `XMIT_SAME_RDEV_MAJOR` set)
/// - Minor: varint (proto 30+) or byte/i32 (proto 28-29)
///
/// # Note
///
/// For special files (FIFOs, sockets) in protocol < 31, dummy rdev (0, 0) is read.
///
/// # Upstream Reference
///
/// See `flist.c:recv_file_entry()` lines 910-945
pub fn decode_rdev<R: Read>(
    reader: &mut R,
    flags: u32,
    prev_rdev_major: u32,
    protocol_version: u8,
) -> io::Result<(u32, u32)> {
    // Read major if not same as previous
    let major = if flags & ((XMIT_SAME_RDEV_MAJOR as u32) << 8) != 0 {
        prev_rdev_major
    } else if protocol_version >= 28 {
        read_varint30_int(reader, protocol_version)? as u32
    } else {
        // Protocol < 28: use XMIT_SAME_RDEV_PRE28 flag
        if flags & (XMIT_SAME_RDEV_PRE28 as u32) != 0 {
            prev_rdev_major
        } else {
            read_varint30_int(reader, protocol_version)? as u32
        }
    };

    // Read minor
    let minor = if protocol_version >= 30 {
        read_varint(reader)? as u32
    } else if protocol_version >= 28 {
        // Protocol 28-29: check XMIT_RDEV_MINOR_8_PRE30 flag
        let minor_8_bit = (flags & ((XMIT_RDEV_MINOR_8_PRE30 as u32) << 8)) != 0;
        if minor_8_bit {
            let mut buf = [0u8; 1];
            reader.read_exact(&mut buf)?;
            buf[0] as u32
        } else {
            read_int(reader)? as u32
        }
    } else {
        read_varint30_int(reader, protocol_version)? as u32
    };

    Ok((major, minor))
}

// ============================================================================
// Symlink Target Decoding
// ============================================================================

/// Decodes symlink target path.
///
/// # Arguments
///
/// * `reader` - Input stream
/// * `protocol_version` - Protocol version
///
/// # Wire Format
///
/// `varint30(len)` + `target_bytes`
///
/// # Note
///
/// Only decode when preserve_links is enabled and entry is a symlink.
pub fn decode_symlink_target<R: Read>(reader: &mut R, protocol_version: u8) -> io::Result<Vec<u8>> {
    let len = read_varint30_int(reader, protocol_version)? as usize;
    let mut target = vec![0u8; len];
    reader.read_exact(&mut target)?;
    Ok(target)
}

// ============================================================================
// Hardlink Decoding
// ============================================================================

/// Decodes hardlink index (protocol 30+).
///
/// # Arguments
///
/// * `reader` - Input stream
/// * `flags` - Transmission flags (checks XMIT_HLINKED and XMIT_HLINK_FIRST)
///
/// # Returns
///
/// Returns hardlink index, or `None` if this is the first occurrence (leader).
///
/// # Wire Format
///
/// `varint(idx)`
///
/// # Note
///
/// Only decode when `XMIT_HLINKED` is set but `XMIT_HLINK_FIRST` is NOT set.
/// The first occurrence of a hardlink group (leader) doesn't have an index.
pub fn decode_hardlink_idx<R: Read>(reader: &mut R, flags: u32) -> io::Result<Option<i32>> {
    if flags & ((XMIT_HLINKED as u32) << 8) != 0 {
        if flags & ((XMIT_HLINK_FIRST as u32) << 8) != 0 {
            // First occurrence (leader) - no index follows
            Ok(None)
        } else {
            // Follower - read index
            Ok(Some(read_varint(reader)?))
        }
    } else {
        Ok(None)
    }
}

/// Decodes hardlink device and inode (protocol 28-29).
///
/// In protocols before 30, hardlinks are identified by (dev, ino) pairs.
///
/// # Arguments
///
/// * `reader` - Input stream
/// * `flags` - Transmission flags (checks XMIT_SAME_DEV_PRE30)
/// * `prev_dev` - Previous device number (used if XMIT_SAME_DEV_PRE30 set)
///
/// # Returns
///
/// Returns `(dev, ino)` pair.
///
/// # Wire Format
///
/// - If not same_dev: `longint(dev + 1)`
/// - Always: `longint(ino)`
///
/// # Upstream Reference
///
/// See `flist.c:recv_file_entry()` lines 950-975
pub fn decode_hardlink_dev_ino<R: Read>(
    reader: &mut R,
    flags: u32,
    prev_dev: i64,
) -> io::Result<(i64, i64)> {
    let dev = if flags & ((XMIT_SAME_DEV_PRE30 as u32) << 8) != 0 {
        prev_dev
    } else {
        // Read dev + 1 and subtract 1 (upstream convention)
        read_longint(reader)? - 1
    };

    let ino = read_longint(reader)?;

    Ok((dev, ino))
}

// ============================================================================
// Checksum Decoding
// ============================================================================

/// Decodes file checksum (for --checksum mode).
///
/// # Arguments
///
/// * `reader` - Input stream
/// * `checksum_len` - Expected checksum length
///
/// # Wire Format
///
/// Raw bytes of length `checksum_len`.
///
/// # Note
///
/// For regular files: actual checksum (or zeros if not computed)
/// For non-regular files (proto < 28 only): empty_sum (all zeros)
pub fn decode_checksum<R: Read>(reader: &mut R, checksum_len: usize) -> io::Result<Vec<u8>> {
    let mut checksum = vec![0u8; checksum_len];
    reader.read_exact(&mut checksum)?;
    Ok(checksum)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::file_entry::{
        encode_atime, encode_checksum, encode_crtime, encode_end_marker, encode_flags, encode_gid,
        encode_hardlink_dev_ino, encode_hardlink_idx, encode_mode, encode_mtime, encode_mtime_nsec,
        encode_name, encode_owner_name, encode_rdev, encode_size, encode_symlink_target,
        encode_uid,
    };
    use std::io::Cursor;

    // ------------------------------------------------------------------------
    // Flag Decoding Tests
    // ------------------------------------------------------------------------

    #[test]
    fn decode_flags_single_byte() {
        let mut buf = Vec::new();
        encode_flags(&mut buf, XMIT_SAME_MODE as u32, 32, false, false).unwrap();

        let mut cursor = Cursor::new(&buf);
        let (flags, is_end) = decode_flags(&mut cursor, 32, false).unwrap();
        assert_eq!(flags, XMIT_SAME_MODE as u32);
        assert!(!is_end);
    }

    #[test]
    fn decode_flags_two_bytes_protocol_28() {
        let mut buf = Vec::new();
        let xflags = (XMIT_HLINKED as u32) << 8; // Extended flags set
        encode_flags(&mut buf, xflags, 28, false, false).unwrap();

        let mut cursor = Cursor::new(&buf);
        let (flags, is_end) = decode_flags(&mut cursor, 28, false).unwrap();
        assert!(!is_end);
        assert!(flags & ((XMIT_HLINKED as u32) << 8) != 0);
    }

    #[test]
    fn decode_flags_varint_mode() {
        let mut buf = Vec::new();
        encode_flags(&mut buf, 0x123, 32, true, false).unwrap();

        let mut cursor = Cursor::new(&buf);
        let (flags, is_end) = decode_flags(&mut cursor, 32, true).unwrap();
        assert_eq!(flags, 0x123);
        assert!(!is_end);
    }

    #[test]
    fn decode_flags_end_marker_varint() {
        let mut buf = Vec::new();
        // Write an actual end marker (varint 0) not encode_flags with xflags=0
        encode_end_marker(&mut buf, true, false, None).unwrap();

        let mut cursor = Cursor::new(&buf);
        let (flags, is_end) = decode_flags(&mut cursor, 32, true).unwrap();
        assert!(is_end);
        // End marker returns flags=0
        assert_eq!(flags, 0);
    }

    #[test]
    fn decode_flags_end_marker_normal() {
        let data = vec![0u8];
        let mut cursor = Cursor::new(&data);
        let (flags, is_end) = decode_flags(&mut cursor, 32, false).unwrap();
        assert_eq!(flags, 0);
        assert!(is_end);
    }

    // ------------------------------------------------------------------------
    // End Marker Tests
    // ------------------------------------------------------------------------

    #[test]
    fn roundtrip_end_marker_simple() {
        let mut buf = Vec::new();
        encode_end_marker(&mut buf, false, false, None).unwrap();

        let mut cursor = Cursor::new(&buf);
        let error = decode_end_marker(&mut cursor, false, false, 0).unwrap();
        assert_eq!(error, None);
    }

    #[test]
    fn roundtrip_end_marker_varint() {
        let mut buf = Vec::new();
        encode_end_marker(&mut buf, true, false, None).unwrap();

        let mut cursor = Cursor::new(&buf);
        let (_, _) = decode_flags(&mut cursor, 32, true).unwrap(); // Read the 0 flags
        // Error code is in the buffer, not read yet
        let error = read_varint(&mut cursor).unwrap();
        assert_eq!(error, 0);
    }

    #[test]
    fn roundtrip_end_marker_varint_with_error() {
        let mut buf = Vec::new();
        encode_end_marker(&mut buf, true, false, Some(23)).unwrap();

        let mut cursor = Cursor::new(&buf);
        // In varint mode, must first decode the flags (which reads the 0)
        let (flags, is_end) = decode_flags(&mut cursor, 32, true).unwrap();
        assert!(is_end);
        assert_eq!(flags, 0);
        // Then decode_end_marker reads the error code
        let error = decode_end_marker(&mut cursor, true, false, 0).unwrap();
        assert_eq!(error, Some(23));
    }

    #[test]
    fn roundtrip_end_marker_safe_file_list_with_error() {
        let mut buf = Vec::new();
        encode_end_marker(&mut buf, false, true, Some(42)).unwrap();

        let mut cursor = Cursor::new(&buf);
        let (flags, _) = decode_flags(&mut cursor, 31, false).unwrap();
        let error = decode_end_marker(&mut cursor, false, true, flags).unwrap();
        assert_eq!(error, Some(42));
    }

    // ------------------------------------------------------------------------
    // Name Decoding Tests
    // ------------------------------------------------------------------------

    #[test]
    fn roundtrip_name_no_compression() {
        let mut buf = Vec::new();
        encode_name(&mut buf, b"test.txt", 0, 0, 32).unwrap();

        let mut cursor = Cursor::new(&buf);
        let name = decode_name(&mut cursor, 0, b"", 32).unwrap();
        assert_eq!(name, b"test.txt");
    }

    #[test]
    fn roundtrip_name_with_compression() {
        let mut buf = Vec::new();
        encode_name(&mut buf, b"dir/file2.txt", 8, XMIT_SAME_NAME as u32, 32).unwrap();

        let mut cursor = Cursor::new(&buf);
        let name = decode_name(&mut cursor, XMIT_SAME_NAME as u32, b"dir/file1.txt", 32).unwrap();
        assert_eq!(name, b"dir/file2.txt");
    }

    #[test]
    fn roundtrip_name_long_name_modern() {
        let mut buf = Vec::new();
        let long_name = vec![b'a'; 300];
        encode_name(&mut buf, &long_name, 0, XMIT_LONG_NAME as u32, 32).unwrap();

        let mut cursor = Cursor::new(&buf);
        let name = decode_name(&mut cursor, XMIT_LONG_NAME as u32, b"", 32).unwrap();
        assert_eq!(name, long_name);
    }

    #[test]
    fn roundtrip_name_long_name_legacy() {
        let mut buf = Vec::new();
        let long_name = vec![b'a'; 300];
        encode_name(&mut buf, &long_name, 0, XMIT_LONG_NAME as u32, 29).unwrap();

        let mut cursor = Cursor::new(&buf);
        let name = decode_name(&mut cursor, XMIT_LONG_NAME as u32, b"", 29).unwrap();
        assert_eq!(name, long_name);
    }

    // ------------------------------------------------------------------------
    // Size Decoding Tests
    // ------------------------------------------------------------------------

    #[test]
    fn roundtrip_size_modern() {
        let mut buf = Vec::new();
        encode_size(&mut buf, 1000, 32).unwrap();

        let mut cursor = Cursor::new(&buf);
        let size = decode_size(&mut cursor, 32).unwrap();
        assert_eq!(size, 1000);
    }

    #[test]
    fn roundtrip_size_legacy() {
        let mut buf = Vec::new();
        encode_size(&mut buf, 1000, 29).unwrap();

        let mut cursor = Cursor::new(&buf);
        let size = decode_size(&mut cursor, 29).unwrap();
        assert_eq!(size, 1000);
    }

    #[test]
    fn roundtrip_size_large_legacy() {
        let mut buf = Vec::new();
        let large = 0x1_0000_0000u64;
        encode_size(&mut buf, large, 29).unwrap();

        let mut cursor = Cursor::new(&buf);
        let size = decode_size(&mut cursor, 29).unwrap();
        assert_eq!(size, large as i64);
    }

    // ------------------------------------------------------------------------
    // Time Decoding Tests
    // ------------------------------------------------------------------------

    #[test]
    fn roundtrip_mtime_modern() {
        let mut buf = Vec::new();
        encode_mtime(&mut buf, 1700000000, 32).unwrap();

        let mut cursor = Cursor::new(&buf);
        let mtime = decode_mtime(&mut cursor, 0, 0, 32).unwrap();
        assert_eq!(mtime, Some(1700000000));
    }

    #[test]
    fn roundtrip_mtime_legacy() {
        let mut buf = Vec::new();
        encode_mtime(&mut buf, 1700000000, 29).unwrap();

        let mut cursor = Cursor::new(&buf);
        let mtime = decode_mtime(&mut cursor, 0, 0, 29).unwrap();
        assert_eq!(mtime, Some(1700000000));
    }

    #[test]
    fn roundtrip_mtime_same_as_previous() {
        let mut cursor = Cursor::new(Vec::new());
        let mtime = decode_mtime(&mut cursor, XMIT_SAME_TIME as u32, 1600000000, 32).unwrap();
        assert_eq!(mtime, Some(1600000000));
    }

    #[test]
    fn roundtrip_mtime_nsec() {
        let mut buf = Vec::new();
        encode_mtime_nsec(&mut buf, 123456789).unwrap();

        let mut cursor = Cursor::new(&buf);
        let flags = (XMIT_MOD_NSEC as u32) << 8;
        let nsec = decode_mtime_nsec(&mut cursor, flags).unwrap();
        assert_eq!(nsec, Some(123456789));
    }

    #[test]
    fn roundtrip_atime() {
        let mut buf = Vec::new();
        encode_atime(&mut buf, 1700000001).unwrap();

        let mut cursor = Cursor::new(&buf);
        let atime = decode_atime(&mut cursor, 0, 0).unwrap();
        assert_eq!(atime, Some(1700000001));
    }

    #[test]
    fn roundtrip_atime_same_as_previous() {
        let mut cursor = Cursor::new(Vec::new());
        let flags = (XMIT_SAME_ATIME as u32) << 8;
        let atime = decode_atime(&mut cursor, flags, 1600000000).unwrap();
        assert_eq!(atime, Some(1600000000));
    }

    #[test]
    fn roundtrip_crtime() {
        let mut buf = Vec::new();
        encode_crtime(&mut buf, 1600000000).unwrap();

        let mut cursor = Cursor::new(&buf);
        let crtime = decode_crtime(&mut cursor, 0, 0).unwrap();
        assert_eq!(crtime, Some(1600000000));
    }

    #[test]
    fn roundtrip_crtime_eq_mtime() {
        let mut cursor = Cursor::new(Vec::new());
        let flags = (XMIT_CRTIME_EQ_MTIME as u32) << 16;
        let crtime = decode_crtime(&mut cursor, flags, 1700000000).unwrap();
        assert_eq!(crtime, Some(1700000000));
    }

    // ------------------------------------------------------------------------
    // Mode Decoding Tests
    // ------------------------------------------------------------------------

    #[test]
    fn roundtrip_mode_regular_file() {
        let mut buf = Vec::new();
        encode_mode(&mut buf, 0o100644).unwrap();

        let mut cursor = Cursor::new(&buf);
        let mode = decode_mode(&mut cursor, 0, 0).unwrap();
        assert_eq!(mode, Some(0o100644));
    }

    #[test]
    fn roundtrip_mode_same_as_previous() {
        let mut cursor = Cursor::new(Vec::new());
        let mode = decode_mode(&mut cursor, XMIT_SAME_MODE as u32, 0o100755).unwrap();
        assert_eq!(mode, Some(0o100755));
    }

    // ------------------------------------------------------------------------
    // UID/GID Decoding Tests
    // ------------------------------------------------------------------------

    #[test]
    fn roundtrip_uid_modern() {
        let mut buf = Vec::new();
        encode_uid(&mut buf, 1000, 32).unwrap();

        let mut cursor = Cursor::new(&buf);
        let result = decode_uid(&mut cursor, 0, 0, 32).unwrap();
        assert_eq!(result, Some((1000, None)));
    }

    #[test]
    fn roundtrip_uid_legacy() {
        let mut buf = Vec::new();
        encode_uid(&mut buf, 1000, 29).unwrap();

        let mut cursor = Cursor::new(&buf);
        let result = decode_uid(&mut cursor, 0, 0, 29).unwrap();
        assert_eq!(result, Some((1000, None)));
    }

    #[test]
    fn roundtrip_uid_same_as_previous() {
        let mut cursor = Cursor::new(Vec::new());
        let result = decode_uid(&mut cursor, XMIT_SAME_UID as u32, 500, 32).unwrap();
        assert_eq!(result, Some((500, None)));
    }

    #[test]
    fn roundtrip_uid_with_name() {
        let mut buf = Vec::new();
        encode_uid(&mut buf, 1000, 32).unwrap();
        encode_owner_name(&mut buf, "testuser").unwrap();

        let mut cursor = Cursor::new(&buf);
        let flags = (XMIT_USER_NAME_FOLLOWS as u32) << 8;
        let result = decode_uid(&mut cursor, flags, 0, 32).unwrap();
        assert_eq!(result, Some((1000, Some("testuser".to_string()))));
    }

    #[test]
    fn roundtrip_gid_modern() {
        let mut buf = Vec::new();
        encode_gid(&mut buf, 500, 30).unwrap();

        let mut cursor = Cursor::new(&buf);
        let result = decode_gid(&mut cursor, 0, 0, 30).unwrap();
        assert_eq!(result, Some((500, None)));
    }

    #[test]
    fn roundtrip_gid_with_name() {
        let mut buf = Vec::new();
        encode_gid(&mut buf, 500, 32).unwrap();
        encode_owner_name(&mut buf, "testgroup").unwrap();

        let mut cursor = Cursor::new(&buf);
        let flags = (XMIT_GROUP_NAME_FOLLOWS as u32) << 8;
        let result = decode_gid(&mut cursor, flags, 0, 32).unwrap();
        assert_eq!(result, Some((500, Some("testgroup".to_string()))));
    }

    // ------------------------------------------------------------------------
    // Device Number Decoding Tests
    // ------------------------------------------------------------------------

    #[test]
    fn roundtrip_rdev_protocol_30() {
        let mut buf = Vec::new();
        encode_rdev(&mut buf, 8, 1, 0, 30).unwrap();

        let mut cursor = Cursor::new(&buf);
        let (major, minor) = decode_rdev(&mut cursor, 0, 0, 30).unwrap();
        assert_eq!(major, 8);
        assert_eq!(minor, 1);
    }

    #[test]
    fn roundtrip_rdev_same_major() {
        let mut buf = Vec::new();
        let xflags = (XMIT_SAME_RDEV_MAJOR as u32) << 8;
        encode_rdev(&mut buf, 8, 1, xflags, 30).unwrap();

        let mut cursor = Cursor::new(&buf);
        let (major, minor) = decode_rdev(&mut cursor, xflags, 8, 30).unwrap();
        assert_eq!(major, 8);
        assert_eq!(minor, 1);
    }

    #[test]
    fn roundtrip_rdev_protocol_29_minor_8bit() {
        let mut buf = Vec::new();
        let xflags = (XMIT_RDEV_MINOR_8_PRE30 as u32) << 8;
        encode_rdev(&mut buf, 8, 5, xflags, 29).unwrap();

        let mut cursor = Cursor::new(&buf);
        let (major, minor) = decode_rdev(&mut cursor, xflags, 0, 29).unwrap();
        assert_eq!(major, 8);
        assert_eq!(minor, 5);
    }

    // ------------------------------------------------------------------------
    // Symlink Target Decoding Tests
    // ------------------------------------------------------------------------

    #[test]
    fn roundtrip_symlink_target() {
        let mut buf = Vec::new();
        encode_symlink_target(&mut buf, b"/target/path", 32).unwrap();

        let mut cursor = Cursor::new(&buf);
        let target = decode_symlink_target(&mut cursor, 32).unwrap();
        assert_eq!(target, b"/target/path");
    }

    #[test]
    fn roundtrip_symlink_target_relative() {
        let mut buf = Vec::new();
        encode_symlink_target(&mut buf, b"../lib/libfoo.so", 32).unwrap();

        let mut cursor = Cursor::new(&buf);
        let target = decode_symlink_target(&mut cursor, 32).unwrap();
        assert_eq!(target, b"../lib/libfoo.so");
    }

    #[test]
    fn roundtrip_symlink_target_empty() {
        let mut buf = Vec::new();
        encode_symlink_target(&mut buf, b"", 32).unwrap();

        let mut cursor = Cursor::new(&buf);
        let target = decode_symlink_target(&mut cursor, 32).unwrap();
        assert_eq!(target, b"");
    }

    #[test]
    fn roundtrip_symlink_target_protocol_29() {
        let mut buf = Vec::new();
        encode_symlink_target(&mut buf, b"/usr/bin/python3", 29).unwrap();

        let mut cursor = Cursor::new(&buf);
        let target = decode_symlink_target(&mut cursor, 29).unwrap();
        assert_eq!(target, b"/usr/bin/python3");
    }

    #[test]
    fn roundtrip_symlink_target_protocol_30() {
        let mut buf = Vec::new();
        encode_symlink_target(&mut buf, b"/usr/bin/python3", 30).unwrap();

        let mut cursor = Cursor::new(&buf);
        let target = decode_symlink_target(&mut cursor, 30).unwrap();
        assert_eq!(target, b"/usr/bin/python3");
    }

    #[test]
    fn roundtrip_symlink_target_all_protocols() {
        let target = b"../relative/link/target";
        for proto in [28u8, 29, 30, 31, 32] {
            let mut buf = Vec::new();
            encode_symlink_target(&mut buf, target, proto).unwrap();

            let mut cursor = Cursor::new(&buf);
            let decoded = decode_symlink_target(&mut cursor, proto).unwrap();
            assert_eq!(decoded, target, "roundtrip failed for protocol {proto}");
            // Verify all bytes consumed
            assert_eq!(cursor.position() as usize, buf.len());
        }
    }

    #[test]
    fn roundtrip_symlink_target_with_unicode() {
        let target = "\u{65e5}\u{672c}\u{8a9e}/\u{30d5}\u{30a1}\u{30a4}\u{30eb}".as_bytes();
        let mut buf = Vec::new();
        encode_symlink_target(&mut buf, target, 32).unwrap();

        let mut cursor = Cursor::new(&buf);
        let decoded = decode_symlink_target(&mut cursor, 32).unwrap();
        assert_eq!(decoded, target);
    }

    #[test]
    fn roundtrip_symlink_target_binary_data() {
        // All byte values 0x01..=0xFF
        let target: Vec<u8> = (1u8..=255).collect();
        let mut buf = Vec::new();
        encode_symlink_target(&mut buf, &target, 32).unwrap();

        let mut cursor = Cursor::new(&buf);
        let decoded = decode_symlink_target(&mut cursor, 32).unwrap();
        assert_eq!(decoded, target);
    }

    #[test]
    fn roundtrip_symlink_target_long() {
        let target = vec![b'x'; 4096];
        for proto in [29u8, 30, 32] {
            let mut buf = Vec::new();
            encode_symlink_target(&mut buf, &target, proto).unwrap();

            let mut cursor = Cursor::new(&buf);
            let decoded = decode_symlink_target(&mut cursor, proto).unwrap();
            assert_eq!(decoded, target, "long target failed for protocol {proto}");
        }
    }

    #[test]
    fn roundtrip_symlink_target_path_separators_preserved() {
        // Backslash and forward slash should both be preserved
        let target = b"dir/subdir\\file";
        let mut buf = Vec::new();
        encode_symlink_target(&mut buf, target, 32).unwrap();

        let mut cursor = Cursor::new(&buf);
        let decoded = decode_symlink_target(&mut cursor, 32).unwrap();
        assert_eq!(decoded, target);
    }

    #[test]
    fn decode_symlink_target_known_bytes_proto29() {
        // Protocol 29: fixed 4-byte LE int (3) + "tgt"
        let data = vec![0x03, 0x00, 0x00, 0x00, b't', b'g', b't'];
        let mut cursor = Cursor::new(&data);
        let target = decode_symlink_target(&mut cursor, 29).unwrap();
        assert_eq!(target, b"tgt");
    }

    #[test]
    fn decode_symlink_target_known_bytes_proto30() {
        // Protocol 30: varint (0x03) + "tgt"
        let data = vec![0x03, b't', b'g', b't'];
        let mut cursor = Cursor::new(&data);
        let target = decode_symlink_target(&mut cursor, 30).unwrap();
        assert_eq!(target, b"tgt");
    }

    // ------------------------------------------------------------------------
    // Hardlink Decoding Tests
    // ------------------------------------------------------------------------

    #[test]
    fn roundtrip_hardlink_idx_follower() {
        let mut buf = Vec::new();
        encode_hardlink_idx(&mut buf, 5).unwrap();

        let mut cursor = Cursor::new(&buf);
        let flags = (XMIT_HLINKED as u32) << 8;
        let idx = decode_hardlink_idx(&mut cursor, flags).unwrap();
        assert_eq!(idx, Some(5));
    }

    #[test]
    fn roundtrip_hardlink_idx_leader() {
        let mut cursor = Cursor::new(Vec::new());
        let flags = ((XMIT_HLINKED as u32) << 8) | ((XMIT_HLINK_FIRST as u32) << 8);
        let idx = decode_hardlink_idx(&mut cursor, flags).unwrap();
        assert_eq!(idx, None);
    }

    #[test]
    fn roundtrip_hardlink_dev_ino_different_dev() {
        let mut buf = Vec::new();
        encode_hardlink_dev_ino(&mut buf, 100, 12345, false).unwrap();

        let mut cursor = Cursor::new(&buf);
        let (dev, ino) = decode_hardlink_dev_ino(&mut cursor, 0, 0).unwrap();
        assert_eq!(dev, 100);
        assert_eq!(ino, 12345);
    }

    #[test]
    fn roundtrip_hardlink_dev_ino_same_dev() {
        let mut buf = Vec::new();
        encode_hardlink_dev_ino(&mut buf, 100, 12345, true).unwrap();

        let mut cursor = Cursor::new(&buf);
        let flags = (XMIT_SAME_DEV_PRE30 as u32) << 8;
        let (dev, ino) = decode_hardlink_dev_ino(&mut cursor, flags, 100).unwrap();
        assert_eq!(dev, 100);
        assert_eq!(ino, 12345);
    }

    // ------------------------------------------------------------------------
    // Checksum Decoding Tests
    // ------------------------------------------------------------------------

    #[test]
    fn roundtrip_checksum() {
        let mut buf = Vec::new();
        let checksum = vec![0xAA, 0xBB, 0xCC, 0xDD];
        encode_checksum(&mut buf, Some(&checksum), 4).unwrap();

        let mut cursor = Cursor::new(&buf);
        let decoded = decode_checksum(&mut cursor, 4).unwrap();
        assert_eq!(decoded, checksum);
    }

    #[test]
    fn roundtrip_checksum_zeros() {
        let mut buf = Vec::new();
        encode_checksum(&mut buf, None, 4).unwrap();

        let mut cursor = Cursor::new(&buf);
        let decoded = decode_checksum(&mut cursor, 4).unwrap();
        assert_eq!(decoded, vec![0x00, 0x00, 0x00, 0x00]);
    }
}
