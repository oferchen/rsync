//! Legacy protocol codec for versions 28-29.
//!
//! Uses fixed-size integer encoding for most fields:
//! - File sizes: 4-byte longint (or 12 bytes for large values)
//! - Modification times: 4-byte fixed integer
//! - Long name lengths: 4-byte fixed integer

use std::io::{self, Read, Write};

use crate::varint::write_longint;

use super::ProtocolCodec;

/// Protocol codec for legacy versions (28-29).
///
/// Uses fixed-size integer encoding. File sizes use the longint format:
/// 4 bytes for values that fit in `i32`, or a 4-byte `0xFFFFFFFF` marker
/// followed by 8 bytes for larger values.
#[derive(Debug, Clone, Copy)]
pub struct LegacyProtocolCodec {
    version: u8,
}

impl LegacyProtocolCodec {
    /// Creates a new legacy codec.
    ///
    /// # Panics
    ///
    /// Panics if `version >= 30`. Use [`super::ModernProtocolCodec`] for protocol 30+.
    #[must_use]
    pub fn new(version: u8) -> Self {
        assert!(
            version < 30,
            "LegacyProtocolCodec requires protocol < 30, got {version}"
        );
        Self { version }
    }
}

impl ProtocolCodec for LegacyProtocolCodec {
    fn protocol_version(&self) -> u8 {
        self.version
    }

    fn write_file_size<W: Write + ?Sized>(&self, writer: &mut W, size: i64) -> io::Result<()> {
        write_longint(writer, size)
    }

    fn read_file_size<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<i64> {
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf)?;
        let value = i32::from_le_bytes(buf);

        if value != -1 {
            Ok(i64::from(value))
        } else {
            // 0xFFFFFFFF marker means full 64-bit value follows
            let mut buf64 = [0u8; 8];
            reader.read_exact(&mut buf64)?;
            Ok(i64::from_le_bytes(buf64))
        }
    }

    fn write_mtime<W: Write + ?Sized>(&self, writer: &mut W, mtime: i64) -> io::Result<()> {
        // upstream: flist.c uses write_uint() for proto < 30 (unsigned 32-bit)
        writer.write_all(&(mtime as u32).to_le_bytes())
    }

    fn read_mtime<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<i64> {
        // upstream: flist.c uses read_uint() for proto < 30 (unsigned 32-bit)
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf)?;
        Ok(i64::from(u32::from_le_bytes(buf)))
    }

    fn write_long_name_len<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        len: usize,
    ) -> io::Result<()> {
        writer.write_all(&(len as i32).to_le_bytes())
    }

    fn read_long_name_len<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<usize> {
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf)?;
        Ok(i32::from_le_bytes(buf) as usize)
    }
}
