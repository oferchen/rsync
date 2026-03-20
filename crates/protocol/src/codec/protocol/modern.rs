//! Modern protocol codec for versions 30+.
//!
//! Uses variable-length encoding for bandwidth efficiency:
//! - File sizes: varlong with min_bytes=3
//! - Modification times: varlong with min_bytes=4
//! - Long name lengths: varint

use std::io::{self, Read, Write};

use crate::varint::{read_varint, read_varlong, write_varint, write_varlong};

use super::ProtocolCodec;

/// Protocol codec for modern versions (30+).
///
/// Uses variable-length encoding. Small values are more compact than their
/// fixed-size legacy equivalents, while large values use only as many bytes
/// as needed.
#[derive(Debug, Clone, Copy)]
pub struct ModernProtocolCodec {
    version: u8,
}

impl ModernProtocolCodec {
    /// Creates a new modern codec.
    ///
    /// # Panics
    ///
    /// Panics if `version < 30`. Use [`super::LegacyProtocolCodec`] for protocol 28-29.
    #[must_use]
    pub fn new(version: u8) -> Self {
        assert!(
            version >= 30,
            "ModernProtocolCodec requires protocol >= 30, got {version}"
        );
        Self { version }
    }
}

impl ProtocolCodec for ModernProtocolCodec {
    fn protocol_version(&self) -> u8 {
        self.version
    }

    fn write_file_size<W: Write + ?Sized>(&self, writer: &mut W, size: i64) -> io::Result<()> {
        write_varlong(writer, size, 3)
    }

    fn read_file_size<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<i64> {
        read_varlong(reader, 3)
    }

    fn write_mtime<W: Write + ?Sized>(&self, writer: &mut W, mtime: i64) -> io::Result<()> {
        write_varlong(writer, mtime, 4)
    }

    fn read_mtime<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<i64> {
        read_varlong(reader, 4)
    }

    fn write_long_name_len<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        len: usize,
    ) -> io::Result<()> {
        write_varint(writer, len as i32)
    }

    fn read_long_name_len<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<usize> {
        read_varint(reader).map(|v| v as usize)
    }
}
