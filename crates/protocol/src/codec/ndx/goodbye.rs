//! Stateless goodbye sentinel read/write helpers.
//!
//! These functions handle the NDX_DONE goodbye handshake at the end of a
//! transfer, using the correct wire format for the given protocol version.

use std::io::{self, Read, Write};

use super::constants::{NDX_DONE, NDX_DONE_LEGACY_BYTES, NDX_DONE_MODERN_BYTE};

/// Writes the goodbye NDX_DONE using the correct wire format for the given protocol version.
///
/// - Protocol < 30: writes 4-byte LE `[0xFF, 0xFF, 0xFF, 0xFF]` (same as `write_int(-1)`)
/// - Protocol >= 30: writes single byte `[0x00]` (modern varint encoding)
///
/// This is a stateless helper for code paths that need to send a goodbye sentinel
/// without maintaining an `NdxCodecEnum` instance. For codec-based writes, prefer
/// `NdxCodec::write_ndx_done()`.
///
/// # Upstream Reference
///
/// - `main.c:875-906` - `read_final_goodbye()` reads the goodbye sentinel
/// - `main.c:883` - protocol < 29 uses `read_int(f_in)` (4-byte LE)
/// - `main.c:885-886` - protocol >= 29 uses `read_ndx_and_attrs()` (still 4-byte LE for 29)
pub fn write_goodbye<W: Write>(writer: &mut W, protocol_version: u8) -> io::Result<()> {
    if protocol_version < 30 {
        writer.write_all(&NDX_DONE_LEGACY_BYTES)
    } else {
        writer.write_all(&[NDX_DONE_MODERN_BYTE])
    }
}

/// Reads a goodbye NDX_DONE using the correct wire format for the given protocol version.
///
/// - Protocol < 30: reads 4-byte LE integer and validates it equals -1
/// - Protocol >= 30: reads single byte and validates it equals 0x00
///
/// Returns `Ok(())` if the goodbye sentinel was read successfully, or an error if
/// the read value doesn't match the expected NDX_DONE encoding.
///
/// This is a stateless helper for code paths that need to read a goodbye sentinel
/// without maintaining an `NdxCodecEnum` instance. For codec-based reads, prefer
/// `NdxCodec::read_ndx()` and check for `NDX_DONE`.
///
/// # Upstream Reference
///
/// - `main.c:883` - protocol < 29: `i = read_int(f_in)` then checks `i != NDX_DONE`
/// - `main.c:885-886` - protocol >= 29: `read_ndx_and_attrs()` which calls `read_ndx()`
pub fn read_goodbye<R: Read>(reader: &mut R, protocol_version: u8) -> io::Result<()> {
    if protocol_version < 30 {
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf)?;
        let value = i32::from_le_bytes(buf);
        if value != NDX_DONE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "expected goodbye NDX_DONE (-1) as 4-byte LE, got {value} (protocol {protocol_version})"
                ),
            ));
        }
        Ok(())
    } else {
        let mut buf = [0u8; 1];
        reader.read_exact(&mut buf)?;
        if buf[0] != NDX_DONE_MODERN_BYTE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "expected goodbye NDX_DONE (0x00), got 0x{:02X} (protocol {protocol_version})",
                    buf[0]
                ),
            ));
        }
        Ok(())
    }
}
