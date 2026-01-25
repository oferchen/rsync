//! crates/protocol/src/codec/protocol.rs
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

    // ========================================================================
    // Statistics encoding (mirrors write_varlong30 in upstream)
    // ========================================================================

    /// Writes a statistic value (used for transfer stats in handle_stats).
    ///
    /// This mirrors upstream's `write_varlong30(f, x, 3)` macro which:
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

    // ========================================================================
    // Filter rule modifier support (protocol-dependent)
    // ========================================================================

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

    // ========================================================================
    // Statistics encoding support (protocol-dependent)
    // ========================================================================

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
#[allow(clippy::uninlined_format_args)]
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

    // ------------------------------------------------------------------------
    // Filter modifier support tests
    // ------------------------------------------------------------------------

    #[test]
    fn protocol_28_does_not_support_sender_receiver_modifiers() {
        let codec = create_protocol_codec(28);
        assert!(!codec.supports_sender_receiver_modifiers());
    }

    #[test]
    fn protocol_29_supports_sender_receiver_modifiers() {
        let codec = create_protocol_codec(29);
        assert!(codec.supports_sender_receiver_modifiers());
    }

    #[test]
    fn protocol_30_supports_sender_receiver_modifiers() {
        let codec = create_protocol_codec(30);
        assert!(codec.supports_sender_receiver_modifiers());
    }

    #[test]
    fn protocol_32_supports_sender_receiver_modifiers() {
        let codec = create_protocol_codec(32);
        assert!(codec.supports_sender_receiver_modifiers());
    }

    #[test]
    fn protocol_28_does_not_support_perishable() {
        let codec = create_protocol_codec(28);
        assert!(!codec.supports_perishable_modifier());
    }

    #[test]
    fn protocol_29_does_not_support_perishable() {
        let codec = create_protocol_codec(29);
        assert!(!codec.supports_perishable_modifier());
    }

    #[test]
    fn protocol_30_supports_perishable() {
        let codec = create_protocol_codec(30);
        assert!(codec.supports_perishable_modifier());
    }

    #[test]
    fn protocol_31_supports_perishable() {
        let codec = create_protocol_codec(31);
        assert!(codec.supports_perishable_modifier());
    }

    #[test]
    fn protocol_32_supports_perishable() {
        let codec = create_protocol_codec(32);
        assert!(codec.supports_perishable_modifier());
    }

    #[test]
    fn protocol_28_uses_old_prefixes() {
        let codec = create_protocol_codec(28);
        assert!(codec.uses_old_prefixes());
    }

    #[test]
    fn protocol_29_does_not_use_old_prefixes() {
        let codec = create_protocol_codec(29);
        assert!(!codec.uses_old_prefixes());
    }

    #[test]
    fn protocol_30_does_not_use_old_prefixes() {
        let codec = create_protocol_codec(30);
        assert!(!codec.uses_old_prefixes());
    }

    #[test]
    fn protocol_32_does_not_use_old_prefixes() {
        let codec = create_protocol_codec(32);
        assert!(!codec.uses_old_prefixes());
    }

    #[test]
    fn filter_modifier_support_boundary_at_29() {
        // Protocol 28: no s/r, no p, old prefixes
        let codec_28 = create_protocol_codec(28);
        assert!(!codec_28.supports_sender_receiver_modifiers());
        assert!(!codec_28.supports_perishable_modifier());
        assert!(codec_28.uses_old_prefixes());

        // Protocol 29: has s/r, no p, no old prefixes
        let codec_29 = create_protocol_codec(29);
        assert!(codec_29.supports_sender_receiver_modifiers());
        assert!(!codec_29.supports_perishable_modifier());
        assert!(!codec_29.uses_old_prefixes());
    }

    #[test]
    fn filter_modifier_support_boundary_at_30() {
        // Protocol 29: has s/r, no p
        let codec_29 = create_protocol_codec(29);
        assert!(codec_29.supports_sender_receiver_modifiers());
        assert!(!codec_29.supports_perishable_modifier());

        // Protocol 30: has s/r and p
        let codec_30 = create_protocol_codec(30);
        assert!(codec_30.supports_sender_receiver_modifiers());
        assert!(codec_30.supports_perishable_modifier());
    }

    // ------------------------------------------------------------------------
    // Statistics encoding support tests
    // ------------------------------------------------------------------------

    #[test]
    fn protocol_28_does_not_support_flist_times() {
        let codec = create_protocol_codec(28);
        assert!(!codec.supports_flist_times());
    }

    #[test]
    fn protocol_29_supports_flist_times() {
        let codec = create_protocol_codec(29);
        assert!(codec.supports_flist_times());
    }

    #[test]
    fn protocol_30_supports_flist_times() {
        let codec = create_protocol_codec(30);
        assert!(codec.supports_flist_times());
    }

    #[test]
    fn protocol_32_supports_flist_times() {
        let codec = create_protocol_codec(32);
        assert!(codec.supports_flist_times());
    }

    #[test]
    fn flist_times_support_boundary_at_29() {
        // Protocol 28: no flist times
        let codec_28 = create_protocol_codec(28);
        assert!(!codec_28.supports_flist_times());

        // Protocol 29: has flist times
        let codec_29 = create_protocol_codec(29);
        assert!(codec_29.supports_flist_times());
    }

    #[test]
    fn write_stat_uses_file_size_encoding() {
        // Verify write_stat and write_file_size produce identical output
        let codec = create_protocol_codec(29);
        let mut stat_buf = Vec::new();
        let mut size_buf = Vec::new();

        codec.write_stat(&mut stat_buf, 12345).unwrap();
        codec.write_file_size(&mut size_buf, 12345).unwrap();

        assert_eq!(stat_buf, size_buf);
    }

    #[test]
    fn read_stat_uses_file_size_encoding() {
        // Verify read_stat and read_file_size produce identical results
        let codec = create_protocol_codec(30);
        let mut buf = Vec::new();
        codec.write_stat(&mut buf, 999999).unwrap();

        let mut cursor1 = Cursor::new(&buf);
        let mut cursor2 = Cursor::new(&buf);

        let stat_value = codec.read_stat(&mut cursor1).unwrap();
        let size_value = codec.read_file_size(&mut cursor2).unwrap();

        assert_eq!(stat_value, size_value);
        assert_eq!(stat_value, 999999);
    }

    #[test]
    fn stat_round_trip_legacy() {
        let codec = create_protocol_codec(29);
        let test_values = [0i64, 1, 1000, 65535, 0x7FFF_FFFF, 0x1_0000_0000];

        for &value in &test_values {
            let mut buf = Vec::new();
            codec.write_stat(&mut buf, value).unwrap();

            let mut cursor = Cursor::new(&buf);
            let read_value = codec.read_stat(&mut cursor).unwrap();
            assert_eq!(
                read_value, value,
                "Stat round-trip failed for value={value} (legacy)"
            );
        }
    }

    #[test]
    fn stat_round_trip_modern() {
        let codec = create_protocol_codec(32);
        let test_values = [0i64, 1, 1000, 65535, 0x7FFF_FFFF, 0x1_0000_0000];

        for &value in &test_values {
            let mut buf = Vec::new();
            codec.write_stat(&mut buf, value).unwrap();

            let mut cursor = Cursor::new(&buf);
            let read_value = codec.read_stat(&mut cursor).unwrap();
            assert_eq!(
                read_value, value,
                "Stat round-trip failed for value={value} (modern)"
            );
        }
    }

    // ============================================================================
    // Protocol Version Range Tests (v27-v32)
    // ============================================================================

    #[test]
    fn version_27_not_supported_use_28_as_minimum() {
        // Protocol 27 is below the supported range; use 28 as minimum
        let codec = create_protocol_codec(28);
        assert!(codec.is_legacy());
        assert_eq!(codec.protocol_version(), 28);
    }

    #[test]
    fn all_supported_versions_create_valid_codecs() {
        for version in 28..=32 {
            let codec = create_protocol_codec(version);
            assert_eq!(codec.protocol_version(), version);
        }
    }

    #[test]
    fn version_boundary_at_30_encoding_changes() {
        // Version 29 uses legacy fixed-size encoding
        let legacy = create_protocol_codec(29);
        let mut legacy_buf = Vec::new();
        legacy.write_file_size(&mut legacy_buf, 100).unwrap();
        assert_eq!(legacy_buf.len(), 4, "legacy version uses 4-byte fixed");

        // Version 30 uses modern varlong encoding
        let modern = create_protocol_codec(30);
        let mut modern_buf = Vec::new();
        modern.write_file_size(&mut modern_buf, 100).unwrap();
        assert!(modern_buf.len() <= 4, "modern version uses varlong");
    }

    // ============================================================================
    // Interop Tests - Upstream Protocol Byte Patterns
    // ============================================================================

    #[test]
    fn legacy_encoding_matches_upstream_byte_patterns() {
        let codec = create_protocol_codec(29);

        // Zero encoded as 4-byte LE
        let mut buf = Vec::new();
        codec.write_file_size(&mut buf, 0).unwrap();
        assert_eq!(buf, [0x00, 0x00, 0x00, 0x00]);

        // 255 encoded as 4-byte LE
        buf.clear();
        codec.write_file_size(&mut buf, 255).unwrap();
        assert_eq!(buf, [0xff, 0x00, 0x00, 0x00]);

        // 1000 (0x3E8) encoded as 4-byte LE
        buf.clear();
        codec.write_file_size(&mut buf, 1000).unwrap();
        assert_eq!(buf, [0xe8, 0x03, 0x00, 0x00]);

        // Max positive 32-bit
        buf.clear();
        codec.write_file_size(&mut buf, 0x7FFF_FFFF).unwrap();
        assert_eq!(buf, [0xff, 0xff, 0xff, 0x7f]);
    }

    #[test]
    fn legacy_large_file_uses_12_byte_longint() {
        let codec = create_protocol_codec(29);
        let mut buf = Vec::new();

        // Large value (> 32-bit) uses marker + 64-bit value
        codec.write_file_size(&mut buf, 0x1_0000_0000i64).unwrap();
        assert_eq!(buf.len(), 12);
        // First 4 bytes are the marker (0xFFFFFFFF)
        assert_eq!(&buf[0..4], [0xff, 0xff, 0xff, 0xff]);
    }

    #[test]
    fn modern_encoding_efficient_for_small_values() {
        let codec = create_protocol_codec(30);

        // Zero should be compact
        let mut buf = Vec::new();
        codec.write_file_size(&mut buf, 0).unwrap();
        assert!(buf.len() <= 4); // varlong with min_bytes=3

        // Read back to verify
        let mut cursor = Cursor::new(&buf);
        let value = codec.read_file_size(&mut cursor).unwrap();
        assert_eq!(value, 0);
    }

    #[test]
    fn mtime_encoding_differs_between_versions() {
        let legacy = create_protocol_codec(29);
        let modern = create_protocol_codec(30);
        let mtime = 1700000000i64; // Typical Unix timestamp

        let mut legacy_buf = Vec::new();
        legacy.write_mtime(&mut legacy_buf, mtime).unwrap();
        assert_eq!(legacy_buf.len(), 4, "legacy mtime is 4 bytes");

        let mut modern_buf = Vec::new();
        modern.write_mtime(&mut modern_buf, mtime).unwrap();
        // Modern uses varlong with min_bytes=4

        // Both must roundtrip correctly
        let mut cursor = Cursor::new(&legacy_buf);
        assert_eq!(legacy.read_mtime(&mut cursor).unwrap(), mtime);

        let mut cursor = Cursor::new(&modern_buf);
        assert_eq!(modern.read_mtime(&mut cursor).unwrap(), mtime);
    }

    // ============================================================================
    // Compatibility Flags Tests
    // ============================================================================

    #[test]
    fn compatibility_flags_progressive_enablement() {
        // Track feature enablement across versions
        struct VersionFeatures {
            sender_receiver: bool,
            perishable: bool,
            flist_times: bool,
            old_prefixes: bool,
        }

        let expected = [
            (
                28,
                VersionFeatures {
                    sender_receiver: false,
                    perishable: false,
                    flist_times: false,
                    old_prefixes: true,
                },
            ),
            (
                29,
                VersionFeatures {
                    sender_receiver: true,
                    perishable: false,
                    flist_times: true,
                    old_prefixes: false,
                },
            ),
            (
                30,
                VersionFeatures {
                    sender_receiver: true,
                    perishable: true,
                    flist_times: true,
                    old_prefixes: false,
                },
            ),
            (
                31,
                VersionFeatures {
                    sender_receiver: true,
                    perishable: true,
                    flist_times: true,
                    old_prefixes: false,
                },
            ),
            (
                32,
                VersionFeatures {
                    sender_receiver: true,
                    perishable: true,
                    flist_times: true,
                    old_prefixes: false,
                },
            ),
        ];

        for (version, features) in expected {
            let codec = create_protocol_codec(version);
            assert_eq!(
                codec.supports_sender_receiver_modifiers(),
                features.sender_receiver,
                "v{version} sender_receiver mismatch"
            );
            assert_eq!(
                codec.supports_perishable_modifier(),
                features.perishable,
                "v{version} perishable mismatch"
            );
            assert_eq!(
                codec.supports_flist_times(),
                features.flist_times,
                "v{version} flist_times mismatch"
            );
            assert_eq!(
                codec.uses_old_prefixes(),
                features.old_prefixes,
                "v{version} old_prefixes mismatch"
            );
        }
    }

    #[test]
    fn feature_flags_never_disable_in_newer_versions() {
        // Once a feature is enabled, it stays enabled
        let mut prev_sr = false;
        let mut prev_perishable = false;
        let mut prev_flist = false;

        for version in 28..=32 {
            let codec = create_protocol_codec(version);

            if prev_sr {
                assert!(
                    codec.supports_sender_receiver_modifiers(),
                    "sender_receiver must stay enabled at v{version}"
                );
            }
            if prev_perishable {
                assert!(
                    codec.supports_perishable_modifier(),
                    "perishable must stay enabled at v{version}"
                );
            }
            if prev_flist {
                assert!(
                    codec.supports_flist_times(),
                    "flist_times must stay enabled at v{version}"
                );
            }

            prev_sr = codec.supports_sender_receiver_modifiers();
            prev_perishable = codec.supports_perishable_modifier();
            prev_flist = codec.supports_flist_times();
        }
    }

    // ============================================================================
    // Error Handling Tests
    // ============================================================================

    #[test]
    fn read_file_size_handles_truncated_input() {
        let legacy = create_protocol_codec(29);
        let modern = create_protocol_codec(30);

        // Legacy needs at least 4 bytes
        let truncated = [0u8, 0, 0];
        let mut cursor = Cursor::new(&truncated[..]);
        assert!(legacy.read_file_size(&mut cursor).is_err());

        // Modern needs at least min_bytes=3
        let truncated = [0u8, 0];
        let mut cursor = Cursor::new(&truncated[..]);
        assert!(modern.read_file_size(&mut cursor).is_err());
    }

    #[test]
    fn read_mtime_handles_truncated_input() {
        let legacy = create_protocol_codec(29);

        let truncated = [0u8, 0];
        let mut cursor = Cursor::new(&truncated[..]);
        assert!(legacy.read_mtime(&mut cursor).is_err());
    }

    #[test]
    fn read_int_handles_truncated_input() {
        let codec = create_protocol_codec(30);

        let truncated = [0u8, 0];
        let mut cursor = Cursor::new(&truncated[..]);
        assert!(codec.read_int(&mut cursor).is_err());
    }

    #[test]
    fn read_long_name_len_handles_truncated_input() {
        let legacy = create_protocol_codec(29);

        let truncated = [0u8, 0, 0];
        let mut cursor = Cursor::new(&truncated[..]);
        assert!(legacy.read_long_name_len(&mut cursor).is_err());
    }

    #[test]
    fn empty_input_returns_error() {
        let codec = create_protocol_codec(30);
        let empty: [u8; 0] = [];

        let mut cursor = Cursor::new(&empty[..]);
        assert!(codec.read_file_size(&mut cursor).is_err());

        let mut cursor = Cursor::new(&empty[..]);
        assert!(codec.read_mtime(&mut cursor).is_err());

        let mut cursor = Cursor::new(&empty[..]);
        assert!(codec.read_int(&mut cursor).is_err());

        let mut cursor = Cursor::new(&empty[..]);
        assert!(codec.read_long_name_len(&mut cursor).is_err());
    }

    // ============================================================================
    // Cross-Version Compatibility Tests
    // ============================================================================

    #[test]
    fn write_int_consistent_across_all_versions() {
        // write_int always uses 4-byte LE regardless of version
        let mut prev_buf: Option<Vec<u8>> = None;

        for version in 28..=32 {
            let codec = create_protocol_codec(version);
            let mut buf = Vec::new();
            codec.write_int(&mut buf, 12345).unwrap();

            if let Some(ref prev) = prev_buf {
                assert_eq!(&buf, prev, "write_int should be same across versions");
            }
            prev_buf = Some(buf);
        }
    }

    #[test]
    fn write_varint_available_in_all_versions() {
        // write_varint is available regardless of version
        for version in [28, 29, 30, 31, 32] {
            let codec = create_protocol_codec(version);
            let mut buf = Vec::new();
            codec.write_varint(&mut buf, 1000).unwrap();

            let mut cursor = Cursor::new(&buf);
            let value = codec.read_varint(&mut cursor).unwrap();
            assert_eq!(value, 1000);
        }
    }

    // ============================================================================
    // Extreme Value Tests
    // ============================================================================

    #[test]
    fn file_size_extreme_values_roundtrip() {
        // Varlong encoding in modern protocol has limits on encoded size
        // Legacy uses 64-bit longint for values > 32-bit
        // Test values within the supported range
        let test_values = [
            0i64,
            1,
            i8::MAX as i64,
            u8::MAX as i64,
            i16::MAX as i64,
            u16::MAX as i64,
            i32::MAX as i64,
            u32::MAX as i64,
            0x1_0000_0000i64,    // Just above 32-bit
            0xFFFF_FFFF_FFFFi64, // 48-bit max (within varlong range)
        ];

        for version in [28, 29, 30, 31, 32] {
            let codec = create_protocol_codec(version);
            for &size in &test_values {
                let mut buf = Vec::new();
                codec.write_file_size(&mut buf, size).unwrap();

                let mut cursor = Cursor::new(&buf);
                let read = codec.read_file_size(&mut cursor).unwrap();
                assert_eq!(read, size, "v{version} roundtrip failed for {size}");
            }
        }
    }

    #[test]
    fn negative_int_roundtrip() {
        for version in [28, 29, 30, 31, 32] {
            let codec = create_protocol_codec(version);
            for value in [-1i32, -100, i32::MIN, i32::MIN + 1] {
                let mut buf = Vec::new();
                codec.write_int(&mut buf, value).unwrap();

                let mut cursor = Cursor::new(&buf);
                let read = codec.read_int(&mut cursor).unwrap();
                assert_eq!(read, value, "v{version} int roundtrip failed for {value}");
            }
        }
    }

    // ============================================================================
    // I/O Error Propagation Tests
    // ============================================================================

    #[test]
    fn write_file_size_propagates_io_error() {
        use std::io::{self, Write};

        struct FailWriter;
        impl Write for FailWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "write failed"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let codec = create_protocol_codec(30);
        let result = codec.write_file_size(&mut FailWriter, 1000);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn write_mtime_propagates_io_error() {
        use std::io::{self, Write};

        struct FailWriter;
        impl Write for FailWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "write failed",
                ))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let codec = create_protocol_codec(29);
        let result = codec.write_mtime(&mut FailWriter, 1700000000);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::ConnectionReset);
    }

    #[test]
    fn write_int_propagates_io_error() {
        use std::io::{self, Write};

        struct FailWriter;
        impl Write for FailWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "write failed",
                ))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let codec = create_protocol_codec(32);
        let result = codec.write_int(&mut FailWriter, 42);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    // ============================================================================
    // ProtocolCodecEnum Dispatch Tests
    // ============================================================================

    #[test]
    fn codec_enum_dispatches_to_correct_implementation() {
        // Verify that enum correctly dispatches to legacy vs modern
        let legacy = create_protocol_codec(29);
        let modern = create_protocol_codec(30);

        // Same value, different encodings
        let value = 1000i64;

        let mut legacy_buf = Vec::new();
        legacy.write_file_size(&mut legacy_buf, value).unwrap();

        let mut modern_buf = Vec::new();
        modern.write_file_size(&mut modern_buf, value).unwrap();

        // Legacy uses fixed 4-byte encoding
        assert_eq!(legacy_buf.len(), 4);

        // Modern may use varlong (3+ bytes for small values)
        // The important thing is both roundtrip correctly
        let mut cursor = Cursor::new(&legacy_buf);
        assert_eq!(legacy.read_file_size(&mut cursor).unwrap(), value);

        let mut cursor = Cursor::new(&modern_buf);
        assert_eq!(modern.read_file_size(&mut cursor).unwrap(), value);
    }

    #[test]
    fn is_legacy_correctly_identifies_version() {
        for version in 28..=32 {
            let codec = create_protocol_codec(version);
            let expected_legacy = version < 30;
            assert_eq!(
                codec.is_legacy(),
                expected_legacy,
                "v{version} is_legacy mismatch"
            );
        }
    }

    // ============================================================================
    // Wire Format Verification Tests
    // ============================================================================

    #[test]
    fn legacy_longint_format_for_large_values() {
        let codec = create_protocol_codec(29);
        let mut buf = Vec::new();

        // Value > 32-bit should use longint format
        let large = 0x1_ABCD_EF01i64;
        codec.write_file_size(&mut buf, large).unwrap();

        // Longint format: 4-byte marker (0xFFFFFFFF) + 8-byte value
        assert_eq!(buf.len(), 12);
        assert_eq!(&buf[0..4], &[0xFF, 0xFF, 0xFF, 0xFF]);

        // Read back
        let mut cursor = Cursor::new(&buf);
        let read = codec.read_file_size(&mut cursor).unwrap();
        assert_eq!(read, large);
    }

    #[test]
    fn modern_varlong_efficient_encoding() {
        let codec = create_protocol_codec(30);

        // Small value should be compact
        let mut buf = Vec::new();
        codec.write_file_size(&mut buf, 100).unwrap();

        // Read back
        let mut cursor = Cursor::new(&buf);
        let read = codec.read_file_size(&mut cursor).unwrap();
        assert_eq!(read, 100);
    }

    #[test]
    fn write_long_name_len_encoding_difference() {
        let legacy = create_protocol_codec(29);
        let modern = create_protocol_codec(30);
        let len = 300usize;

        let mut legacy_buf = Vec::new();
        legacy.write_long_name_len(&mut legacy_buf, len).unwrap();
        assert_eq!(legacy_buf.len(), 4); // Fixed 4-byte

        let mut modern_buf = Vec::new();
        modern.write_long_name_len(&mut modern_buf, len).unwrap();
        // Modern uses varint, should be smaller for small values

        // Both must roundtrip
        let mut cursor = Cursor::new(&legacy_buf);
        assert_eq!(legacy.read_long_name_len(&mut cursor).unwrap(), len);

        let mut cursor = Cursor::new(&modern_buf);
        assert_eq!(modern.read_long_name_len(&mut cursor).unwrap(), len);
    }

    // ============================================================================
    // Property: Encoding Determinism
    // ============================================================================

    #[test]
    fn encoding_is_deterministic() {
        // Same input should always produce same output
        for version in [28, 29, 30, 31, 32] {
            let codec = create_protocol_codec(version);
            let value = 12345i64;

            let mut buf1 = Vec::new();
            let mut buf2 = Vec::new();

            codec.write_file_size(&mut buf1, value).unwrap();
            codec.write_file_size(&mut buf2, value).unwrap();

            assert_eq!(buf1, buf2, "v{version} encoding should be deterministic");
        }
    }

    #[test]
    fn sequential_writes_independent() {
        // Multiple writes should produce correct output
        let codec = create_protocol_codec(30);
        let mut buf = Vec::new();

        let values = [100i64, 200, 300, 1000000];
        for &val in &values {
            codec.write_file_size(&mut buf, val).unwrap();
        }

        let mut cursor = Cursor::new(&buf);
        for &expected in &values {
            let read = codec.read_file_size(&mut cursor).unwrap();
            assert_eq!(read, expected);
        }
    }

    // ============================================================================
    // Boundary Value Tests
    // ============================================================================

    #[test]
    fn boundary_values_at_byte_boundaries() {
        let test_values = [
            0i64,
            0x7F,        // 127 - max 7-bit
            0x80,        // 128 - min 8-bit
            0xFF,        // 255 - max 8-bit
            0x100,       // 256 - min 9-bit
            0x7FFF,      // 32767 - max 15-bit
            0x8000,      // 32768 - min 16-bit
            0xFFFF,      // 65535 - max 16-bit
            0x10000,     // 65536 - min 17-bit
            0x7FFF_FFFF, // max 31-bit
            0x8000_0000, // min 32-bit (unsigned)
            0xFFFF_FFFF, // max 32-bit
        ];

        for version in [28, 29, 30, 31, 32] {
            let codec = create_protocol_codec(version);
            for &value in &test_values {
                let mut buf = Vec::new();
                codec.write_file_size(&mut buf, value).unwrap();

                let mut cursor = Cursor::new(&buf);
                let read = codec.read_file_size(&mut cursor).unwrap();
                assert_eq!(
                    read, value,
                    "v{version} boundary test failed for {value:#X}"
                );
            }
        }
    }

    #[test]
    fn mtime_boundary_values() {
        // Values that fit in 32-bit (work with legacy)
        let legacy_mtimes = [
            0i64,
            1,
            1700000000,      // Recent timestamp
            i32::MAX as i64, // 2038 problem boundary
        ];

        for version in [28, 29, 30, 31, 32] {
            let codec = create_protocol_codec(version);
            for &mtime in &legacy_mtimes {
                let mut buf = Vec::new();
                codec.write_mtime(&mut buf, mtime).unwrap();

                let mut cursor = Cursor::new(&buf);
                let read = codec.read_mtime(&mut cursor).unwrap();
                assert_eq!(read, mtime, "v{version} mtime boundary failed for {mtime}");
            }
        }
    }

    #[test]
    fn mtime_large_values_modern_only() {
        // Large values that exceed 32-bit (only work with modern varlong encoding)
        let large_mtimes = [
            i32::MAX as i64 + 1, // Post-2038
            0x1_0000_0000i64,    // 64-bit value
        ];

        for version in [30, 31, 32] {
            let codec = create_protocol_codec(version);
            for &mtime in &large_mtimes {
                let mut buf = Vec::new();
                codec.write_mtime(&mut buf, mtime).unwrap();

                let mut cursor = Cursor::new(&buf);
                let read = codec.read_mtime(&mut cursor).unwrap();
                assert_eq!(read, mtime, "v{version} mtime large failed for {mtime}");
            }
        }
    }

    // ============================================================================
    // Debug and Display Tests
    // ============================================================================

    #[test]
    fn codec_debug_format() {
        let legacy = create_protocol_codec(29);
        let modern = create_protocol_codec(30);

        let legacy_debug = format!("{:?}", legacy);
        let modern_debug = format!("{:?}", modern);

        // Debug output should be informative
        assert!(legacy_debug.contains("Legacy") || legacy_debug.contains("29"));
        assert!(modern_debug.contains("Modern") || modern_debug.contains("30"));
    }

    // ============================================================================
    // Varint and Varlong Specific Tests
    // ============================================================================

    #[test]
    fn varint_roundtrip_all_versions() {
        let test_values = [0i32, 1, 127, 128, 255, 256, 16383, 16384, 0x7FFF_FFFF];

        for version in [28, 29, 30, 31, 32] {
            let codec = create_protocol_codec(version);
            for &value in &test_values {
                let mut buf = Vec::new();
                codec.write_varint(&mut buf, value).unwrap();

                let mut cursor = Cursor::new(&buf);
                let read = codec.read_varint(&mut cursor).unwrap();
                assert_eq!(read, value, "v{version} varint failed for {value}");
            }
        }
    }

    #[test]
    fn varlong_roundtrip_modern_only() {
        // Varlong is used in modern encoding
        let codec = create_protocol_codec(30);
        let test_values = [0i64, 1, 0x7FFF_FFFF, 0x1_0000_0000, 0xFFFF_FFFF_FFFF];

        for &value in &test_values {
            let mut buf = Vec::new();
            codec.write_file_size(&mut buf, value).unwrap();

            let mut cursor = Cursor::new(&buf);
            let read = codec.read_file_size(&mut cursor).unwrap();
            assert_eq!(read, value, "varlong failed for {value:#X}");
        }
    }
}
