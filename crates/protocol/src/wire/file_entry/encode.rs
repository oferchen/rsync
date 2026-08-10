//! Wire format encoding functions for file list entries.
//!
//! Each function encodes a single field of the file entry wire format,
//! matching upstream rsync's `flist.c:send_file_entry()` behavior.

use std::io::{self, Write};

use crate::varint::{write_varint, write_varint30_int, write_varlong};

use super::constants::{
    XMIT_EXTENDED_FLAGS, XMIT_IO_ERROR_ENDLIST, XMIT_LONG_NAME, XMIT_RDEV_MINOR_8_PRE30,
    XMIT_SAME_NAME, XMIT_SAME_RDEV_MAJOR, XMIT_TOP_DIR,
};

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
/// // same_len=8 ("dir/file" shared prefix)
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

    if xflags & (XMIT_SAME_NAME as u32) != 0 {
        writer.write_all(&[same_len as u8])?;
    }

    if xflags & (XMIT_LONG_NAME as u32) != 0 {
        if protocol_version >= 30 {
            write_varint(writer, suffix_len as i32)?;
        } else {
            writer.write_all(&(suffix_len as i32).to_le_bytes())?;
        }
    } else {
        writer.write_all(&[suffix_len as u8])?;
    }

    writer.write_all(&name[same_len..])
}

/// Encodes file size to the wire format.
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

/// Encodes modification time to the wire format.
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
/// Only encode when `XMIT_MOD_NSEC` flag is set in xflags.
///
/// # Wire Format
///
/// `varint(nsec)`
pub fn encode_mtime_nsec<W: Write>(writer: &mut W, nsec: u32) -> io::Result<()> {
    write_varint(writer, nsec as i32)
}

/// Encodes access time (for --atimes, non-directories only).
///
/// # Wire Format
///
/// `varlong(atime, 4)`
pub fn encode_atime<W: Write>(writer: &mut W, atime: i64) -> io::Result<()> {
    write_varlong(writer, atime, 4)
}

/// Encodes creation time (for --crtimes).
///
/// Only encode when `XMIT_CRTIME_EQ_MTIME` flag is NOT set.
///
/// # Wire Format
///
/// `varlong(crtime, 4)`
pub fn encode_crtime<W: Write>(writer: &mut W, crtime: i64) -> io::Result<()> {
    write_varlong(writer, crtime, 4)
}

/// Encodes Unix mode bits to the wire format.
///
/// Mode is always encoded as a fixed 4-byte little-endian integer.
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

/// Encodes a user ID to the wire format.
///
/// Only encode when preserve_uid is enabled and `XMIT_SAME_UID` flag is NOT set.
///
/// # Wire Format
///
/// | Protocol | Format |
/// |----------|--------|
/// | >= 30 | varint |
/// | < 30 | Fixed 4-byte i32 LE |
pub fn encode_uid<W: Write>(writer: &mut W, uid: u32, protocol_version: u8) -> io::Result<()> {
    if protocol_version >= 30 {
        write_varint(writer, uid as i32)
    } else {
        writer.write_all(&(uid as i32).to_le_bytes())
    }
}

/// Encodes a group ID to the wire format.
///
/// Only encode when preserve_gid is enabled and `XMIT_SAME_GID` flag is NOT set.
///
/// # Wire Format
///
/// | Protocol | Format |
/// |----------|--------|
/// | >= 30 | varint |
/// | < 30 | Fixed 4-byte i32 LE |
pub fn encode_gid<W: Write>(writer: &mut W, gid: u32, protocol_version: u8) -> io::Result<()> {
    if protocol_version >= 30 {
        write_varint(writer, gid as i32)
    } else {
        writer.write_all(&(gid as i32).to_le_bytes())
    }
}

/// Encodes a user or group name (protocol 30+).
///
/// Only encode when `XMIT_USER_NAME_FOLLOWS` or `XMIT_GROUP_NAME_FOLLOWS` flag is set.
///
/// # Wire Format
///
/// `u8(len)` + `name_bytes[0..len]`
pub fn encode_owner_name<W: Write>(writer: &mut W, name: &str) -> io::Result<()> {
    let name_bytes = name.as_bytes();
    let len = name_bytes.len().min(255) as u8;
    writer.write_all(&[len])?;
    writer.write_all(&name_bytes[..len as usize])
}

/// Encodes device numbers for block/character devices.
///
/// For special files (FIFOs, sockets) in protocol < 31, write dummy rdev (0, 0).
///
/// # Wire Format (Protocol 28+)
///
/// - Major: varint30 (omitted if `XMIT_SAME_RDEV_MAJOR` set)
/// - Minor: varint (proto 30+) or byte/i32 (proto 28-29)
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
    if xflags & ((XMIT_SAME_RDEV_MAJOR as u32) << 8) == 0 {
        write_varint30_int(writer, major as i32, protocol_version)?;
    }

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

/// Encodes symlink target path.
///
/// Only encode when preserve_links is enabled and entry is a symlink.
///
/// # Wire Format
///
/// `varint30(len)` + `target_bytes`
pub fn encode_symlink_target<W: Write>(
    writer: &mut W,
    target: &[u8],
    protocol_version: u8,
) -> io::Result<()> {
    write_varint30_int(writer, target.len() as i32, protocol_version)?;
    writer.write_all(target)
}

/// Encodes hardlink index (protocol 30+).
///
/// Only encode when `XMIT_HLINKED` is set but `XMIT_HLINK_FIRST` is NOT set.
/// The first occurrence of a hardlink group (leader) does not write an index.
///
/// # Wire Format
///
/// `varint(idx)`
pub fn encode_hardlink_idx<W: Write>(writer: &mut W, idx: u32) -> io::Result<()> {
    // upstream: flist.c - indices are bounded by flist size (< 2^31),
    // so the as-i32 cast is safe. The decoder mirrors this with `as u32`.
    write_varint(writer, idx as i32)
}

/// Encodes hardlink device and inode (protocol 28-29).
///
/// In protocols before 30, hardlinks are identified by (dev, ino) pairs.
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
        // upstream: write dev + 1 (upstream convention)
        crate::write_longint(writer, dev + 1)?;
    }
    crate::write_longint(writer, ino)
}

/// Encodes file checksum (for --checksum mode).
///
/// For regular files: actual checksum (or zeros if not computed).
/// For non-regular files (proto < 28 only): empty_sum (all zeros).
///
/// # Wire Format
///
/// Raw bytes of length `csum_len`. If checksum is shorter, pads with zeros.
pub fn encode_checksum<W: Write>(
    writer: &mut W,
    checksum: Option<&[u8]>,
    csum_len: usize,
) -> io::Result<()> {
    if let Some(sum) = checksum {
        let len = sum.len().min(csum_len);
        writer.write_all(&sum[..len])?;
        if len < csum_len {
            let padding = vec![0u8; csum_len - len];
            writer.write_all(&padding)?;
        }
    } else {
        let zeros = vec![0u8; csum_len];
        writer.write_all(&zeros)?;
    }
    Ok(())
}
