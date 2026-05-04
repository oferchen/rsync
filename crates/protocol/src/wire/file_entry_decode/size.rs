#![deny(unsafe_code)]

use std::io::{self, Read};

use crate::varint::{read_longint, read_varlong};

/// Decodes file size from the wire format.
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
