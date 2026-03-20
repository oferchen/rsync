#![deny(unsafe_code)]

use std::io::{self, Read};

use crate::varint::read_varint30_int;

/// Maximum allowed symlink target length on receive.
///
/// Matches upstream rsync's `MAXPATHLEN` (4096). Without this cap, a malicious
/// sender could claim an arbitrarily large symlink target, causing unbounded
/// memory allocation on the receiver.
///
/// upstream: rsync.h `MAXPATHLEN`
pub const MAX_SYMLINK_TARGET_LEN: usize = 4096;

/// Decodes symlink target path.
///
/// # Wire Format
///
/// `varint30(len)` + `target_bytes`
///
/// # Errors
///
/// Returns `InvalidData` if the target length exceeds [`MAX_SYMLINK_TARGET_LEN`].
pub fn decode_symlink_target<R: Read>(reader: &mut R, protocol_version: u8) -> io::Result<Vec<u8>> {
    let len = read_varint30_int(reader, protocol_version)? as usize;
    if len > MAX_SYMLINK_TARGET_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("symlink target length {len} exceeds maximum {MAX_SYMLINK_TARGET_LEN}"),
        ));
    }
    let mut target = vec![0u8; len];
    reader.read_exact(&mut target)?;
    Ok(target)
}
