//! Protocol version-aware encoding/decoding using the Strategy pattern.
//!
//! This module provides [`ProtocolCodec`] trait implementations that encapsulate
//! the wire format differences between protocol versions:
//!
//! - **Protocol < 30 (Legacy)**: Fixed-size integers, longint encoding
//! - **Protocol >= 30 (Modern)**: Variable-length integers, varlong encoding
//!
//! # Strategy Pattern
//!
//! The [`ProtocolCodec`] trait defines the encoding/decoding interface, with two
//! implementations:
//! - [`LegacyProtocolCodec`]: Protocol 28-29 (fixed-size integers)
//! - [`ModernProtocolCodec`]: Protocol 30+ (variable-length encoding)
//!
//! Use [`create_protocol_codec`] to get the appropriate codec for a protocol version.
//!
//! # Example
//!
//! ```ignore
//! use protocol::codec::create_protocol_codec;
//!
//! // Protocol 29: uses fixed-size encoding
//! let codec = create_protocol_codec(29);
//! let mut buf = Vec::new();
//! codec.write_file_size(&mut buf, 1000).unwrap();
//! assert_eq!(buf.len(), 4); // 4-byte fixed
//!
//! // Protocol 32: uses variable-length encoding
//! let codec = create_protocol_codec(32);
//! let mut buf = Vec::new();
//! codec.write_file_size(&mut buf, 1000).unwrap();
//! assert!(buf.len() < 4); // varlong is more compact
//! ```

mod dispatch;
mod legacy;
mod modern;

#[cfg(test)]
#[allow(clippy::uninlined_format_args)]
mod tests;

use std::io::{self, Read, Write};

use crate::varint::{read_varint, write_varint};

pub use dispatch::{ProtocolCodecEnum, create_protocol_codec};
pub use legacy::LegacyProtocolCodec;
pub use modern::ModernProtocolCodec;

/// Strategy trait for protocol version-aware encoding/decoding.
///
/// Implementations provide version-specific wire formats for common protocol
/// data types. Use [`create_protocol_codec`] to get the appropriate implementation.
///
/// # Wire Format Differences
///
/// | Data Type | Protocol < 30 | Protocol >= 30 |
/// |-----------|---------------|----------------|
/// | File size | 4-byte fixed (longint) | varlong min_bytes=3 |
/// | Mtime | 4-byte fixed | varlong min_bytes=4 |
/// | Long name length | 4-byte fixed | varint |
/// | Flags (with compat) | 1-2 byte fixed | varint |
pub trait ProtocolCodec: Send + Sync {
    /// Returns the protocol version this codec is configured for.
    fn protocol_version(&self) -> u8;

    /// Returns true if this is a legacy codec (protocol < 30).
    fn is_legacy(&self) -> bool {
        self.protocol_version() < 30
    }

    /// Writes a 32-bit integer.
    ///
    /// All protocol versions use 4-byte little-endian for plain integers.
    fn write_int<W: Write + ?Sized>(&self, writer: &mut W, value: i32) -> io::Result<()> {
        writer.write_all(&value.to_le_bytes())
    }

    /// Reads a 32-bit integer.
    fn read_int<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<i32> {
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf)?;
        Ok(i32::from_le_bytes(buf))
    }

    /// Writes a file size value.
    ///
    /// - Protocol < 30: Uses longint (4 bytes for small values, 12 for large)
    /// - Protocol >= 30: Uses varlong with min_bytes=3
    fn write_file_size<W: Write + ?Sized>(&self, writer: &mut W, size: i64) -> io::Result<()>;

    /// Reads a file size value.
    fn read_file_size<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<i64>;

    /// Writes a modification time value.
    ///
    /// - Protocol < 30: Uses 4-byte fixed integer
    /// - Protocol >= 30: Uses varlong with min_bytes=4
    fn write_mtime<W: Write + ?Sized>(&self, writer: &mut W, mtime: i64) -> io::Result<()>;

    /// Reads a modification time value.
    fn read_mtime<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<i64>;

    /// Writes a long name suffix length (when XMIT_LONG_NAME is set).
    ///
    /// - Protocol < 30: Uses 4-byte fixed integer
    /// - Protocol >= 30: Uses varint
    fn write_long_name_len<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        len: usize,
    ) -> io::Result<()>;

    /// Reads a long name suffix length.
    fn read_long_name_len<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<usize>;

    /// Writes a variable-length integer (varint).
    ///
    /// Available in all protocol versions but used differently:
    /// - Protocol < 30: Only for specific fields (compat flags after negotiation)
    /// - Protocol >= 30: Used for many fields (sizes, lengths, flags)
    fn write_varint<W: Write + ?Sized>(&self, writer: &mut W, value: i32) -> io::Result<()> {
        write_varint(writer, value)
    }

    /// Reads a variable-length integer.
    fn read_varint<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<i32> {
        read_varint(reader)
    }

    /// Writes a statistic value (used for transfer stats in handle_stats).
    ///
    /// Mirrors upstream's `write_varlong30(f, x, 3)` macro which:
    /// - Protocol < 30: Uses longint (4 bytes, or 12 bytes for large values)
    /// - Protocol >= 30: Uses varlong with min_bytes=3
    ///
    /// The encoding is identical to `write_file_size` but this method is provided
    /// for semantic clarity when encoding transfer statistics.
    fn write_stat<W: Write + ?Sized>(&self, writer: &mut W, value: i64) -> io::Result<()> {
        self.write_file_size(writer, value)
    }

    /// Reads a statistic value.
    fn read_stat<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<i64> {
        self.read_file_size(reader)
    }

    /// Returns `true` if this protocol supports sender/receiver side modifiers (`s`, `r`).
    ///
    /// - Protocol < 29: Returns `false`
    /// - Protocol >= 29: Returns `true`
    ///
    /// # Upstream Reference
    ///
    /// `exclude.c:1530` - `legal_len = for_xfer && protocol_version < 29 ? 1 : MAX_RULE_PREFIX-1`
    /// `exclude.c:1567-1571` - Sender/receiver modifier support gated by protocol >= 29
    fn supports_sender_receiver_modifiers(&self) -> bool {
        self.protocol_version() >= 29
    }

    /// Returns `true` if this protocol supports the perishable modifier (`p`).
    ///
    /// - Protocol < 30: Returns `false`
    /// - Protocol >= 30: Returns `true`
    ///
    /// # Upstream Reference
    ///
    /// `exclude.c:1350` - `protocol_version >= 30 ? FILTRULE_PERISHABLE : 0`
    /// `exclude.c:1574` - Perishable modifier gated by protocol >= 30
    fn supports_perishable_modifier(&self) -> bool {
        self.protocol_version() >= 30
    }

    /// Returns `true` if this protocol uses old-style prefixes (protocol < 29).
    ///
    /// Old prefixes have restricted modifier support and different parsing rules.
    ///
    /// # Upstream Reference
    ///
    /// `exclude.c:1675` - `xflags = protocol_version >= 29 ? 0 : XFLG_OLD_PREFIXES`
    fn uses_old_prefixes(&self) -> bool {
        self.protocol_version() < 29
    }

    /// Returns `true` if this protocol supports file list timing statistics.
    ///
    /// - Protocol < 29: Returns `false` (no flist_buildtime/flist_xfertime)
    /// - Protocol >= 29: Returns `true`
    ///
    /// # Upstream Reference
    ///
    /// `main.c` - handle_stats() sends flist times only for protocol >= 29
    fn supports_flist_times(&self) -> bool {
        self.protocol_version() >= 29
    }
}
