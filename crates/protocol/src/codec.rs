//! crates/protocol/src/codec.rs
//!
//! Protocol version-aware encoding/decoding using the Strategy pattern.
//!
//! This module provides `ProtocolCodec` trait implementations that encapsulate
//! the wire format differences between protocol versions:
//!
//! - **Protocol < 30 (Legacy)**: Fixed-size integers, longint encoding
//! - **Protocol >= 30 (Modern)**: Variable-length integers, varlong encoding
//!
//! # Strategy Pattern
//!
//! The `ProtocolCodec` trait defines the encoding/decoding interface, with two
//! implementations:
//! - `LegacyProtocolCodec`: Protocol 28-29 (fixed-size integers)
//! - `ModernProtocolCodec`: Protocol 30+ (variable-length encoding)
//!
//! Use `create_protocol_codec` to get the appropriate codec for a protocol version.
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

use std::io::{self, Read, Write};

use crate::varint::{read_varint, read_varlong, write_longint, write_varint, write_varlong};

// ============================================================================
// Strategy Pattern: ProtocolCodec trait
// ============================================================================

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

    // ========================================================================
    // Integer encoding
    // ========================================================================

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

    // ========================================================================
    // File size encoding (protocol-dependent)
    // ========================================================================

    /// Writes a file size value.
    ///
    /// - Protocol < 30: Uses longint (4 bytes for small values, 12 for large)
    /// - Protocol >= 30: Uses varlong with min_bytes=3
    fn write_file_size<W: Write + ?Sized>(&self, writer: &mut W, size: i64) -> io::Result<()>;

    /// Reads a file size value.
    fn read_file_size<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<i64>;

    // ========================================================================
    // Modification time encoding (protocol-dependent)
    // ========================================================================

    /// Writes a modification time value.
    ///
    /// - Protocol < 30: Uses 4-byte fixed integer
    /// - Protocol >= 30: Uses varlong with min_bytes=4
    fn write_mtime<W: Write + ?Sized>(&self, writer: &mut W, mtime: i64) -> io::Result<()>;

    /// Reads a modification time value.
    fn read_mtime<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<i64>;

    // ========================================================================
    // Long name length encoding (protocol-dependent)
    // ========================================================================

    /// Writes a long name suffix length (when XMIT_LONG_NAME is set).
    ///
    /// - Protocol < 30: Uses 4-byte fixed integer
    /// - Protocol >= 30: Uses varint
    fn write_long_name_len<W: Write + ?Sized>(&self, writer: &mut W, len: usize) -> io::Result<()>;

    /// Reads a long name suffix length.
    fn read_long_name_len<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<usize>;

    // ========================================================================
    // Variable-length integer (always available, but usage differs)
    // ========================================================================

    /// Writes a variable-length integer (varint).
    ///
    /// This is available in all protocol versions but used differently:
    /// - Protocol < 30: Only for specific fields (compat flags after negotiation)
    /// - Protocol >= 30: Used for many fields (sizes, lengths, flags)
    fn write_varint<W: Write + ?Sized>(&self, writer: &mut W, value: i32) -> io::Result<()> {
        write_varint(writer, value)
    }

    /// Reads a variable-length integer.
    fn read_varint<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<i32> {
        read_varint(reader)
    }
}

// ============================================================================
// Legacy codec implementation (Protocol 28-29)
// ============================================================================

/// Protocol codec for legacy versions (28-29).
///
/// Uses fixed-size integer encoding for most fields:
/// - File sizes: 4-byte longint (or 12 bytes for large values)
/// - Modification times: 4-byte fixed integer
/// - Long name lengths: 4-byte fixed integer
#[derive(Debug, Clone, Copy)]
pub struct LegacyProtocolCodec {
    version: u8,
}

impl LegacyProtocolCodec {
    /// Creates a new legacy codec.
    ///
    /// # Panics
    ///
    /// Panics if `version >= 30`. Use [`ModernProtocolCodec`] for protocol 30+.
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
        // Read 4 bytes first
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf)?;
        let value = i32::from_le_bytes(buf);

        if value != -1 {
            // Normal 32-bit value
            Ok(i64::from(value))
        } else {
            // 0xFFFFFFFF marker means full 64-bit value follows
            let mut buf64 = [0u8; 8];
            reader.read_exact(&mut buf64)?;
            Ok(i64::from_le_bytes(buf64))
        }
    }

    fn write_mtime<W: Write + ?Sized>(&self, writer: &mut W, mtime: i64) -> io::Result<()> {
        // Truncate to 32-bit for legacy protocol
        writer.write_all(&(mtime as i32).to_le_bytes())
    }

    fn read_mtime<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<i64> {
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf)?;
        Ok(i64::from(i32::from_le_bytes(buf)))
    }

    fn write_long_name_len<W: Write + ?Sized>(&self, writer: &mut W, len: usize) -> io::Result<()> {
        writer.write_all(&(len as i32).to_le_bytes())
    }

    fn read_long_name_len<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<usize> {
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf)?;
        Ok(i32::from_le_bytes(buf) as usize)
    }
}

// ============================================================================
// Modern codec implementation (Protocol 30+)
// ============================================================================

/// Protocol codec for modern versions (30+).
///
/// Uses variable-length encoding for bandwidth efficiency:
/// - File sizes: varlong with min_bytes=3
/// - Modification times: varlong with min_bytes=4
/// - Long name lengths: varint
#[derive(Debug, Clone, Copy)]
pub struct ModernProtocolCodec {
    version: u8,
}

impl ModernProtocolCodec {
    /// Creates a new modern codec.
    ///
    /// # Panics
    ///
    /// Panics if `version < 30`. Use [`LegacyProtocolCodec`] for protocol 28-29.
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

    fn write_long_name_len<W: Write + ?Sized>(&self, writer: &mut W, len: usize) -> io::Result<()> {
        write_varint(writer, len as i32)
    }

    fn read_long_name_len<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<usize> {
        read_varint(reader).map(|v| v as usize)
    }
}

// ============================================================================
// Enum wrapper for dynamic dispatch
// ============================================================================

/// Enum wrapper for dynamic codec dispatch.
///
/// This allows selecting the appropriate codec at runtime based on protocol
/// version without boxing.
#[derive(Debug, Clone, Copy)]
pub enum ProtocolCodecEnum {
    /// Legacy codec for protocol 28-29.
    Legacy(LegacyProtocolCodec),
    /// Modern codec for protocol 30+.
    Modern(ModernProtocolCodec),
}

impl ProtocolCodec for ProtocolCodecEnum {
    fn protocol_version(&self) -> u8 {
        match self {
            Self::Legacy(c) => c.protocol_version(),
            Self::Modern(c) => c.protocol_version(),
        }
    }

    fn write_file_size<W: Write + ?Sized>(&self, writer: &mut W, size: i64) -> io::Result<()> {
        match self {
            Self::Legacy(c) => c.write_file_size(writer, size),
            Self::Modern(c) => c.write_file_size(writer, size),
        }
    }

    fn read_file_size<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<i64> {
        match self {
            Self::Legacy(c) => c.read_file_size(reader),
            Self::Modern(c) => c.read_file_size(reader),
        }
    }

    fn write_mtime<W: Write + ?Sized>(&self, writer: &mut W, mtime: i64) -> io::Result<()> {
        match self {
            Self::Legacy(c) => c.write_mtime(writer, mtime),
            Self::Modern(c) => c.write_mtime(writer, mtime),
        }
    }

    fn read_mtime<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<i64> {
        match self {
            Self::Legacy(c) => c.read_mtime(reader),
            Self::Modern(c) => c.read_mtime(reader),
        }
    }

    fn write_long_name_len<W: Write + ?Sized>(&self, writer: &mut W, len: usize) -> io::Result<()> {
        match self {
            Self::Legacy(c) => c.write_long_name_len(writer, len),
            Self::Modern(c) => c.write_long_name_len(writer, len),
        }
    }

    fn read_long_name_len<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<usize> {
        match self {
            Self::Legacy(c) => c.read_long_name_len(reader),
            Self::Modern(c) => c.read_long_name_len(reader),
        }
    }
}

// ============================================================================
// Factory function
// ============================================================================

/// Creates the appropriate protocol codec for the given version.
///
/// - Protocol 28-29: Returns [`LegacyProtocolCodec`]
/// - Protocol 30+: Returns [`ModernProtocolCodec`]
///
/// # Example
///
/// ```ignore
/// use protocol::codec::create_protocol_codec;
///
/// let legacy = create_protocol_codec(29);
/// assert!(legacy.is_legacy());
///
/// let modern = create_protocol_codec(32);
/// assert!(!modern.is_legacy());
/// ```
#[must_use]
pub fn create_protocol_codec(protocol_version: u8) -> ProtocolCodecEnum {
    if protocol_version < 30 {
        ProtocolCodecEnum::Legacy(LegacyProtocolCodec::new(protocol_version))
    } else {
        ProtocolCodecEnum::Modern(ModernProtocolCodec::new(protocol_version))
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // ------------------------------------------------------------------------
    // Factory tests
    // ------------------------------------------------------------------------

    #[test]
    fn factory_creates_legacy_for_protocol_28() {
        let codec = create_protocol_codec(28);
        assert!(matches!(codec, ProtocolCodecEnum::Legacy(_)));
        assert_eq!(codec.protocol_version(), 28);
        assert!(codec.is_legacy());
    }

    #[test]
    fn factory_creates_legacy_for_protocol_29() {
        let codec = create_protocol_codec(29);
        assert!(matches!(codec, ProtocolCodecEnum::Legacy(_)));
        assert_eq!(codec.protocol_version(), 29);
        assert!(codec.is_legacy());
    }

    #[test]
    fn factory_creates_modern_for_protocol_30() {
        let codec = create_protocol_codec(30);
        assert!(matches!(codec, ProtocolCodecEnum::Modern(_)));
        assert_eq!(codec.protocol_version(), 30);
        assert!(!codec.is_legacy());
    }

    #[test]
    fn factory_creates_modern_for_protocol_32() {
        let codec = create_protocol_codec(32);
        assert!(matches!(codec, ProtocolCodecEnum::Modern(_)));
        assert_eq!(codec.protocol_version(), 32);
        assert!(!codec.is_legacy());
    }

    // ------------------------------------------------------------------------
    // Legacy codec panic tests
    // ------------------------------------------------------------------------

    #[test]
    #[should_panic(expected = "LegacyProtocolCodec requires protocol < 30")]
    fn legacy_codec_panics_for_protocol_30() {
        let _ = LegacyProtocolCodec::new(30);
    }

    #[test]
    #[should_panic(expected = "ModernProtocolCodec requires protocol >= 30")]
    fn modern_codec_panics_for_protocol_29() {
        let _ = ModernProtocolCodec::new(29);
    }

    // ------------------------------------------------------------------------
    // File size encoding tests
    // ------------------------------------------------------------------------

    #[test]
    fn legacy_file_size_small_value() {
        let codec = create_protocol_codec(29);
        let mut buf = Vec::new();
        codec.write_file_size(&mut buf, 1000).unwrap();

        // Legacy uses 4-byte fixed for small values
        assert_eq!(buf.len(), 4);
        assert_eq!(buf, vec![0xe8, 0x03, 0x00, 0x00]); // 1000 in LE

        let mut cursor = Cursor::new(&buf);
        let value = codec.read_file_size(&mut cursor).unwrap();
        assert_eq!(value, 1000);
    }

    #[test]
    fn legacy_file_size_large_value() {
        let codec = create_protocol_codec(29);
        let mut buf = Vec::new();
        let large_value = 0x1_0000_0000i64; // > 32-bit
        codec.write_file_size(&mut buf, large_value).unwrap();

        // Legacy uses 4-byte marker + 8-byte value for large values
        assert_eq!(buf.len(), 12);
        assert_eq!(&buf[0..4], &[0xff, 0xff, 0xff, 0xff]); // marker

        let mut cursor = Cursor::new(&buf);
        let value = codec.read_file_size(&mut cursor).unwrap();
        assert_eq!(value, large_value);
    }

    #[test]
    fn modern_file_size_small_value() {
        let codec = create_protocol_codec(32);
        let mut buf = Vec::new();
        codec.write_file_size(&mut buf, 1000).unwrap();

        // Modern uses varlong with min_bytes=3, should be compact
        assert!(buf.len() <= 4); // varlong is typically more compact

        let mut cursor = Cursor::new(&buf);
        let value = codec.read_file_size(&mut cursor).unwrap();
        assert_eq!(value, 1000);
    }

    #[test]
    fn modern_file_size_large_value() {
        let codec = create_protocol_codec(32);
        let mut buf = Vec::new();
        let large_value = 0x1_0000_0000i64;
        codec.write_file_size(&mut buf, large_value).unwrap();

        let mut cursor = Cursor::new(&buf);
        let value = codec.read_file_size(&mut cursor).unwrap();
        assert_eq!(value, large_value);
    }

    // ------------------------------------------------------------------------
    // Mtime encoding tests
    // ------------------------------------------------------------------------

    #[test]
    fn legacy_mtime_encoding() {
        let codec = create_protocol_codec(29);
        let mut buf = Vec::new();
        let mtime = 1700000000i64; // Typical Unix timestamp

        codec.write_mtime(&mut buf, mtime).unwrap();

        // Legacy uses 4-byte fixed
        assert_eq!(buf.len(), 4);

        let mut cursor = Cursor::new(&buf);
        let value = codec.read_mtime(&mut cursor).unwrap();
        assert_eq!(value, mtime);
    }

    #[test]
    fn modern_mtime_encoding() {
        let codec = create_protocol_codec(32);
        let mut buf = Vec::new();
        let mtime = 1700000000i64;

        codec.write_mtime(&mut buf, mtime).unwrap();

        // Modern uses varlong with min_bytes=4
        let mut cursor = Cursor::new(&buf);
        let value = codec.read_mtime(&mut cursor).unwrap();
        assert_eq!(value, mtime);
    }

    // ------------------------------------------------------------------------
    // Long name length encoding tests
    // ------------------------------------------------------------------------

    #[test]
    fn legacy_long_name_len_encoding() {
        let codec = create_protocol_codec(29);
        let mut buf = Vec::new();
        let len = 300usize;

        codec.write_long_name_len(&mut buf, len).unwrap();

        // Legacy uses 4-byte fixed
        assert_eq!(buf.len(), 4);

        let mut cursor = Cursor::new(&buf);
        let value = codec.read_long_name_len(&mut cursor).unwrap();
        assert_eq!(value, len);
    }

    #[test]
    fn modern_long_name_len_encoding() {
        let codec = create_protocol_codec(32);
        let mut buf = Vec::new();
        let len = 300usize;

        codec.write_long_name_len(&mut buf, len).unwrap();

        // Modern uses varint, should be 2 bytes for 300
        assert!(buf.len() <= 2);

        let mut cursor = Cursor::new(&buf);
        let value = codec.read_long_name_len(&mut cursor).unwrap();
        assert_eq!(value, len);
    }

    // ------------------------------------------------------------------------
    // Integer encoding tests (same across versions)
    // ------------------------------------------------------------------------

    #[test]
    fn write_int_is_always_4_bytes() {
        for version in [28, 29, 30, 31, 32] {
            let codec = create_protocol_codec(version);
            let mut buf = Vec::new();
            codec.write_int(&mut buf, 12345).unwrap();
            assert_eq!(buf.len(), 4);
            assert_eq!(buf, vec![0x39, 0x30, 0x00, 0x00]); // 12345 in LE
        }
    }

    #[test]
    fn read_int_round_trip() {
        for version in [28, 29, 30, 31, 32] {
            let codec = create_protocol_codec(version);
            let mut buf = Vec::new();
            codec.write_int(&mut buf, -1).unwrap();

            let mut cursor = Cursor::new(&buf);
            let value = codec.read_int(&mut cursor).unwrap();
            assert_eq!(value, -1);
        }
    }

    // ------------------------------------------------------------------------
    // Cross-version round-trip tests
    // ------------------------------------------------------------------------

    #[test]
    fn file_size_round_trip_all_versions() {
        let test_sizes = [0i64, 1, 255, 256, 65535, 65536, 0x7FFF_FFFF, 0x1_0000_0000];

        for version in [28, 29, 30, 31, 32] {
            let codec = create_protocol_codec(version);
            for &size in &test_sizes {
                let mut buf = Vec::new();
                codec.write_file_size(&mut buf, size).unwrap();

                let mut cursor = Cursor::new(&buf);
                let value = codec.read_file_size(&mut cursor).unwrap();
                assert_eq!(
                    value, size,
                    "Round-trip failed for size={size} protocol={version}"
                );
            }
        }
    }

    #[test]
    fn mtime_round_trip_all_versions() {
        let test_mtimes = [0i64, 1, 1700000000, 0x7FFF_FFFF];

        for version in [28, 29, 30, 31, 32] {
            let codec = create_protocol_codec(version);
            for &mtime in &test_mtimes {
                let mut buf = Vec::new();
                codec.write_mtime(&mut buf, mtime).unwrap();

                let mut cursor = Cursor::new(&buf);
                let value = codec.read_mtime(&mut cursor).unwrap();
                assert_eq!(
                    value, mtime,
                    "Round-trip failed for mtime={mtime} protocol={version}"
                );
            }
        }
    }
}
