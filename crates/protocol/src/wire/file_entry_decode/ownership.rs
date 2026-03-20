#![deny(unsafe_code)]

use std::io::{self, Read};

use crate::varint::{read_int, read_varint};

use super::super::file_entry::{
    XMIT_GROUP_NAME_FOLLOWS, XMIT_SAME_GID, XMIT_SAME_UID, XMIT_USER_NAME_FOLLOWS,
};

/// Decodes a user ID from the wire format.
///
/// Returns `(uid, optional_name)` tuple.
///
/// # Wire Format
///
/// | Protocol | Format |
/// |----------|--------|
/// | >= 30 | varint + optional name (u8 len + bytes) |
/// | < 30 | Fixed 4-byte i32 LE |
pub fn decode_uid<R: Read>(
    reader: &mut R,
    flags: u32,
    prev_uid: u32,
    protocol_version: u8,
) -> io::Result<Option<(u32, Option<String>)>> {
    decode_owner_id(
        reader,
        flags,
        prev_uid,
        protocol_version,
        XMIT_SAME_UID,
        XMIT_USER_NAME_FOLLOWS,
    )
}

/// Decodes a group ID from the wire format.
///
/// Returns `(gid, optional_name)` tuple.
///
/// # Wire Format
///
/// | Protocol | Format |
/// |----------|--------|
/// | >= 30 | varint + optional name (u8 len + bytes) |
/// | < 30 | Fixed 4-byte i32 LE |
pub fn decode_gid<R: Read>(
    reader: &mut R,
    flags: u32,
    prev_gid: u32,
    protocol_version: u8,
) -> io::Result<Option<(u32, Option<String>)>> {
    decode_owner_id(
        reader,
        flags,
        prev_gid,
        protocol_version,
        XMIT_SAME_GID,
        XMIT_GROUP_NAME_FOLLOWS,
    )
}

/// Shared implementation for decoding a user or group ID from the wire format.
///
/// The only difference between UID and GID decoding is which flag constants
/// are checked: `same_flag` for reusing the previous value, and
/// `name_follows_flag` for reading an associated owner name.
fn decode_owner_id<R: Read>(
    reader: &mut R,
    flags: u32,
    prev_id: u32,
    protocol_version: u8,
    same_flag: u8,
    name_follows_flag: u8,
) -> io::Result<Option<(u32, Option<String>)>> {
    if flags & (same_flag as u32) != 0 {
        Ok(Some((prev_id, None)))
    } else {
        let id = if protocol_version >= 30 {
            read_varint(reader)? as u32
        } else {
            read_int(reader)? as u32
        };

        let name = if protocol_version >= 30 && (flags & ((name_follows_flag as u32) << 8)) != 0 {
            Some(decode_owner_name(reader)?)
        } else {
            None
        };

        Ok(Some((id, name)))
    }
}

/// Decodes a user or group name (protocol 30+).
///
/// Wire format: `u8(len)` + `name_bytes[0..len]`
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
