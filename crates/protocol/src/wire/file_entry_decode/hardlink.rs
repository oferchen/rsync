#![deny(unsafe_code)]

use std::io::{self, Read};

use crate::varint::{read_longint, read_varint};

use super::super::file_entry::{XMIT_HLINK_FIRST, XMIT_HLINKED, XMIT_SAME_DEV_PRE30};

/// Decodes hardlink index (protocol 30+).
///
/// Returns the hardlink index, or `None` if this is the first occurrence (leader).
/// Only decode when `XMIT_HLINKED` is set but `XMIT_HLINK_FIRST` is NOT set.
/// The first occurrence of a hardlink group (leader) does not have an index.
///
/// # Wire Format
///
/// `varint(idx)`
pub fn decode_hardlink_idx<R: Read>(reader: &mut R, flags: u32) -> io::Result<Option<u32>> {
    if flags & ((XMIT_HLINKED as u32) << 8) != 0 {
        if flags & ((XMIT_HLINK_FIRST as u32) << 8) != 0 {
            Ok(None)
        } else {
            // Cast i32 bits to u32 to preserve the full index space;
            // upstream C uses unsigned int for hlink_flist indices.
            Ok(Some(read_varint(reader)? as u32))
        }
    } else {
        Ok(None)
    }
}

/// Decodes hardlink device and inode (protocol 28-29).
///
/// In protocols before 30, hardlinks are identified by (dev, ino) pairs.
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
