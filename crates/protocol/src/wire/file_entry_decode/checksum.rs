#![deny(unsafe_code)]

use std::io::{self, Read};

/// Decodes file checksum (for --checksum mode).
///
/// Reads raw bytes of length `checksum_len` from the wire.
/// For regular files this is the actual checksum (or zeros if not computed).
/// For non-regular files (proto < 28 only) this is `empty_sum` (all zeros).
pub fn decode_checksum<R: Read>(reader: &mut R, checksum_len: usize) -> io::Result<Vec<u8>> {
    let mut checksum = vec![0u8; checksum_len];
    reader.read_exact(&mut checksum)?;
    Ok(checksum)
}
