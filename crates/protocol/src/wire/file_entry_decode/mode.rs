#![deny(unsafe_code)]

use std::io::{self, Read};

use crate::varint::read_int;

use super::super::file_entry::XMIT_SAME_MODE;

/// Decodes Unix mode bits from the wire format.
///
/// Mode is always encoded as a fixed 4-byte little-endian integer.
/// Returns the previous mode if `XMIT_SAME_MODE` is set.
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
