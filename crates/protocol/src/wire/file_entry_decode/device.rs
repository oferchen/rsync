#![deny(unsafe_code)]

use std::io::{self, Read};

use crate::varint::{read_int, read_varint, read_varint30_int};

use super::super::file_entry::{
    XMIT_RDEV_MINOR_8_PRE30, XMIT_SAME_RDEV_MAJOR, XMIT_SAME_RDEV_PRE28,
};

/// Decodes device numbers for block/character devices.
///
/// Returns `(major, minor)` device numbers.
///
/// # Wire Format (Protocol 28+)
///
/// - Major: varint30 (omitted if `XMIT_SAME_RDEV_MAJOR` set)
/// - Minor: varint (proto 30+) or byte/i32 (proto 28-29)
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
