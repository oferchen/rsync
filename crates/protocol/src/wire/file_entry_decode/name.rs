#![deny(unsafe_code)]

use std::io::{self, Read};

use crate::varint::{read_int, read_varint};

use super::super::file_entry::{XMIT_LONG_NAME, XMIT_SAME_NAME};

/// Decodes a file name with prefix decompression.
///
/// The rsync protocol compresses file names by sharing common prefixes with
/// the previous entry. This function decodes the name suffix and reconstructs
/// the full name.
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
    let same_len = if flags & (XMIT_SAME_NAME as u32) != 0 {
        let mut buf = [0u8; 1];
        reader.read_exact(&mut buf)?;
        buf[0] as usize
    } else {
        0
    };

    let suffix_len = if flags & (XMIT_LONG_NAME as u32) != 0 {
        if protocol_version >= 30 {
            read_varint(reader)? as usize
        } else {
            read_int(reader)? as usize
        }
    } else {
        let mut buf = [0u8; 1];
        reader.read_exact(&mut buf)?;
        buf[0] as usize
    };

    let mut suffix = vec![0u8; suffix_len];
    reader.read_exact(&mut suffix)?;

    let mut name = Vec::with_capacity(same_len + suffix_len);
    if same_len > 0 {
        let prefix_len = same_len.min(prev_name.len());
        name.extend_from_slice(&prev_name[..prefix_len]);
    }
    name.extend_from_slice(&suffix);

    Ok(name)
}
