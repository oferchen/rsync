#![deny(unsafe_code)]

use std::io::{self, Read};

use crate::varint::read_varint;

use super::super::file_entry::{XMIT_EXTENDED_FLAGS, XMIT_IO_ERROR_ENDLIST};

/// Decodes transmission flags from the wire format.
///
/// The decoding varies by protocol version and compatibility flags:
/// - **Varint mode** (VARINT_FLIST_FLAGS): Single varint containing all flag bits
/// - **Protocol 28+**: 1 byte, or 2 bytes if extended flags present
/// - **Protocol < 28**: 1 byte only
///
/// Returns `(flags, is_end_marker)` where `flags` is the decoded flag bits (u32)
/// and `is_end_marker` is true if this represents an end-of-list marker (flags == 0).
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
        let flags = read_varint(reader)? as u32;

        // In varint mode:
        // - actual 0 means end-of-list
        // - XMIT_EXTENDED_FLAGS was written for flags=0 to avoid ambiguity
        if flags == 0 {
            Ok((0, true))
        } else if flags == XMIT_EXTENDED_FLAGS as u32 {
            Ok((0, false))
        } else {
            Ok((flags, false))
        }
    } else if protocol_version >= 28 {
        let mut first_byte = [0u8; 1];
        reader.read_exact(&mut first_byte)?;
        let flags0 = first_byte[0];

        if flags0 == 0 {
            return Ok((0, true));
        }

        if flags0 & XMIT_EXTENDED_FLAGS != 0 {
            let mut second_byte = [0u8; 1];
            reader.read_exact(&mut second_byte)?;
            let flags1 = second_byte[0];

            let flags = (flags0 as u32) | ((flags1 as u32) << 8);

            // Detect the safe-file-list IO-error end-of-list sentinel.
            // Wire: [XMIT_EXTENDED_FLAGS, XMIT_IO_ERROR_ENDLIST] + varint(err).
            // Without this, decode_end_marker is never called; the error
            // varint leaks into the next entry parse, corrupting the flist.
            // Upstream: flist.c recv_file_entry() XMIT_IO_ERROR_ENDLIST branch.
            // The sentinel is exactly [XMIT_EXTENDED_FLAGS, XMIT_IO_ERROR_ENDLIST].
            // XMIT_IO_ERROR_ENDLIST shares its bit with XMIT_HLINK_FIRST; a bitmask
            // test would fire on any hardlink-leader or atime-inherit entry that also
            // has bit 0x1000 set. Exact equality is the only safe check.
            if flags == (XMIT_EXTENDED_FLAGS as u32) | ((XMIT_IO_ERROR_ENDLIST as u32) << 8) {
                return Ok((flags, true));
            }

            Ok((flags, false))
        } else {
            Ok((flags0 as u32, false))
        }
    } else {
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
        let error = read_varint(reader)?;
        Ok(if error == 0 { None } else { Some(error) })
    } else if use_safe_file_list && (flags & ((XMIT_IO_ERROR_ENDLIST as u32) << 8)) != 0 {
        let error = read_varint(reader)?;
        Ok(Some(error))
    } else {
        Ok(None)
    }
}

/// Returns `true` when the flag word from `decode_flags` is a
/// safe-file-list IO-error end-of-list sentinel rather than a file entry.
///
/// After `decode_flags` returns `(flags, true)`, callers must check this
/// to distinguish `flags == 0` (normal end) from the IO-error sentinel
/// (which needs `decode_end_marker` to consume the trailing error varint).
///
/// Upstream: `flist.c:recv_file_entry()` XMIT_IO_ERROR_ENDLIST branch.
#[must_use]
pub fn is_io_error_end_marker(flags: u32) -> bool {
    // Must match exact sentinel, not just the bit: XMIT_HLINK_FIRST shares
    // the same bit position as XMIT_IO_ERROR_ENDLIST.
    flags == (XMIT_EXTENDED_FLAGS as u32) | ((XMIT_IO_ERROR_ENDLIST as u32) << 8)
}
