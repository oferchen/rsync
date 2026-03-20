#![deny(unsafe_code)]

use std::io::{self, Read};

use crate::varint::{read_int, read_varint, read_varlong};

use super::super::file_entry::{
    XMIT_CRTIME_EQ_MTIME, XMIT_MOD_NSEC, XMIT_SAME_ATIME, XMIT_SAME_TIME,
};

/// Decodes modification time from the wire format.
///
/// Returns `Some(mtime)` with the decoded or inherited value.
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
/// Returns the nanosecond component if `XMIT_MOD_NSEC` flag is set, `None` if
/// mtime is inherited from the previous entry (caller must also inherit nsec),
/// or `Some(0)` for a new mtime without nanosecond precision.
///
/// # Wire Format
///
/// `varint(nsec)` - only present when `XMIT_MOD_NSEC` flag is set.
pub fn decode_mtime_nsec<R: Read>(reader: &mut R, flags: u32) -> io::Result<Option<u32>> {
    if flags & ((XMIT_MOD_NSEC as u32) << 8) != 0 {
        Ok(Some(read_varint(reader)? as u32))
    } else if flags & (XMIT_SAME_TIME as u32) != 0 {
        // mtime is inherited from the previous entry; callers must also
        // inherit the previous entry's nsec. Signal this with None.
        Ok(None)
    } else {
        // New mtime without XMIT_MOD_NSEC: upstream defines nsec = 0,
        // NOT "carry forward the previous nsec". Returning Some(0)
        // prevents callers from accidentally inheriting a stale nsec.
        // Upstream: flist.c recv_file_entry() -- nsec absent => 0.
        Ok(Some(0))
    }
}

/// Decodes access time (for --atimes, non-directories only).
///
/// Returns the decoded atime, or previous value if `XMIT_SAME_ATIME` is set.
///
/// # Wire Format
///
/// `varlong(atime, 4)` - only present when `XMIT_SAME_ATIME` is NOT set.
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
/// Returns the decoded crtime, or the current entry's mtime if `XMIT_CRTIME_EQ_MTIME` is set.
///
/// # Wire Format
///
/// `varlong(crtime, 4)` - only present when `XMIT_CRTIME_EQ_MTIME` is NOT set.
pub fn decode_crtime<R: Read>(reader: &mut R, flags: u32, mtime: i64) -> io::Result<Option<i64>> {
    if flags & ((XMIT_CRTIME_EQ_MTIME as u32) << 16) != 0 {
        Ok(Some(mtime))
    } else {
        Ok(Some(read_varlong(reader, 4)?))
    }
}
