//! crates/protocol/src/codec/ndx.rs
//!
//! NDX (file-list index) encoding and decoding for the rsync protocol.
//!
//! This module implements NDX encoding using the Strategy pattern to handle
//! the different wire formats between protocol versions:
//!
//! - **Protocol < 30 (Legacy)**: Simple 4-byte little-endian signed integers
//! - **Protocol >= 30 (Modern)**: Delta-encoded byte-reduction format
//!
//! # Strategy Pattern
//!
//! The `NdxCodec` trait defines the encoding/decoding interface, with two
//! implementations:
//! - `LegacyNdxCodec`: Protocol 28-29 (4-byte LE integers)
//! - `ModernNdxCodec`: Protocol 30+ (delta encoding)
//!
//! Use `create_ndx_codec` to get the appropriate codec for a protocol version.
//!
//! # Wire Formats
//!
//! ## Legacy (Protocol < 30)
//!
//! All NDX values are 4-byte little-endian signed integers:
//! - Positive file indices: direct value
//! - NDX_DONE (-1): `[0xFF, 0xFF, 0xFF, 0xFF]`
//! - Other negative values: direct value
//!
//! ## Modern (Protocol >= 30)
//!
//! Delta-encoded format for bandwidth efficiency:
//! - `0x00`: NDX_DONE (-1)
//! - `0xFF prefix`: negative values (other than -1)
//! - `1-253`: delta-encoded positive index
//! - `0xFE prefix`: extended encoding for larger indices
//!
//! # Upstream Reference
//!
//! - `io.c:2243-2287` - `write_ndx()` function
//! - `io.c:2289-2318` - `read_ndx()` function
//! - `rsync.h:285-288` - NDX constant definitions

use std::io::{self, Read, Write};

/// NDX_DONE value indicating end of file requests.
///
/// Upstream: `rsync.h:285` - `#define NDX_DONE -1`
pub const NDX_DONE: i32 = -1;

/// NDX_FLIST_EOF value indicating end of file list(s).
///
/// Sent after the last incremental file list to signal no more file lists.
///
/// Upstream: `rsync.h:286` - `#define NDX_FLIST_EOF -2`
pub const NDX_FLIST_EOF: i32 = -2;

/// NDX_DEL_STATS value for delete statistics.
///
/// Upstream: `rsync.h:287` - `#define NDX_DEL_STATS -3`
pub const NDX_DEL_STATS: i32 = -3;

/// Offset for incremental file list directory indices.
///
/// Upstream: `rsync.h:288` - `#define NDX_FLIST_OFFSET -101`
pub const NDX_FLIST_OFFSET: i32 = -101;

// ============================================================================
// Strategy Pattern: NdxCodec trait and implementations
// ============================================================================

/// Strategy trait for NDX encoding/decoding.
///
/// Implementations provide protocol-version-specific wire formats for file
/// list indices. Use `create_ndx_codec` to get the appropriate implementation.
///
/// # Example
///
/// ```ignore
/// use protocol::ndx::{create_ndx_codec, NDX_DONE};
///
/// // Protocol 29: uses legacy 4-byte LE format
/// let mut codec = create_ndx_codec(29);
/// let mut buf = Vec::new();
/// codec.write_ndx(&mut buf, 5).unwrap();
/// assert_eq!(buf, vec![5, 0, 0, 0]); // 4-byte LE
///
/// // Protocol 32: uses modern delta encoding
/// let mut codec = create_ndx_codec(32);
/// let mut buf = Vec::new();
/// codec.write_ndx(&mut buf, 0).unwrap();
/// assert_eq!(buf, vec![0x01]); // delta from prev=-1: diff=1
/// ```
pub trait NdxCodec {
    /// Writes an NDX value to the given writer.
    ///
    /// The wire format depends on the codec implementation:
    /// - [`LegacyNdxCodec`]: 4-byte little-endian integer
    /// - [`ModernNdxCodec`]: delta-encoded byte-reduction format
    fn write_ndx<W: Write + ?Sized>(&mut self, writer: &mut W, ndx: i32) -> io::Result<()>;

    /// Writes NDX_DONE (-1) to signal end of file requests.
    ///
    /// This is a convenience method that handles NDX_DONE specifically:
    /// - Legacy: writes `[0xFF, 0xFF, 0xFF, 0xFF]`
    /// - Modern: writes `[0x00]`
    fn write_ndx_done<W: Write + ?Sized>(&mut self, writer: &mut W) -> io::Result<()>;

    /// Reads an NDX value from the given reader.
    fn read_ndx<R: Read + ?Sized>(&mut self, reader: &mut R) -> io::Result<i32>;

    /// Returns the protocol version this codec is configured for.
    fn protocol_version(&self) -> u8;
}

/// Legacy NDX codec for protocol versions < 30.
///
/// Uses simple 4-byte little-endian signed integers for all NDX values.
/// No delta encoding or byte reduction is applied.
///
/// # Wire Format
///
/// - All values are 4-byte little-endian signed integers
/// - NDX_DONE (-1) = `[0xFF, 0xFF, 0xFF, 0xFF]`
/// - NDX_FLIST_EOF (-2) = `[0xFE, 0xFF, 0xFF, 0xFF]`
/// - Positive index N = N as 4-byte LE
///
/// # Upstream Reference
///
/// `io.c:2246-2248`:
/// ```c
/// if (protocol_version < 30)
///     return read_int(f);
/// ```
#[derive(Debug, Clone)]
pub struct LegacyNdxCodec {
    protocol_version: u8,
}

impl LegacyNdxCodec {
    /// Creates a new legacy NDX codec.
    ///
    /// # Panics
    ///
    /// Panics if `protocol_version >= 30`. Use [`ModernNdxCodec`] for protocol 30+.
    #[must_use]
    pub fn new(protocol_version: u8) -> Self {
        assert!(
            protocol_version < 30,
            "LegacyNdxCodec is for protocol < 30, got {protocol_version}"
        );
        Self { protocol_version }
    }
}

impl NdxCodec for LegacyNdxCodec {
    fn write_ndx<W: Write + ?Sized>(&mut self, writer: &mut W, ndx: i32) -> io::Result<()> {
        writer.write_all(&ndx.to_le_bytes())
    }

    fn write_ndx_done<W: Write + ?Sized>(&mut self, writer: &mut W) -> io::Result<()> {
        // NDX_DONE = -1 = 0xFFFFFFFF as 4-byte LE
        writer.write_all(&NDX_DONE.to_le_bytes())
    }

    fn read_ndx<R: Read + ?Sized>(&mut self, reader: &mut R) -> io::Result<i32> {
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf)?;
        Ok(i32::from_le_bytes(buf))
    }

    fn protocol_version(&self) -> u8 {
        self.protocol_version
    }
}

/// Modern NDX codec for protocol versions >= 30.
///
/// Uses delta-encoded byte-reduction format for bandwidth efficiency.
/// Tracks previous values to enable delta encoding.
///
/// # Wire Format
///
/// - `0x00`: NDX_DONE (-1)
/// - `0xFF prefix`: negative values (other than -1)
/// - `1-253`: delta from previous positive value
/// - `0xFE prefix`: extended encoding for larger deltas
///
/// # Upstream Reference
///
/// `io.c:2243-2287` - `write_ndx()` function
/// `io.c:2289-2318` - `read_ndx()` function
#[derive(Debug, Clone)]
pub struct ModernNdxCodec {
    protocol_version: u8,
    /// Previous positive index value for delta encoding.
    prev_positive: i32,
    /// Previous negative index value (as positive) for delta encoding.
    prev_negative: i32,
}

impl ModernNdxCodec {
    /// Creates a new modern NDX codec with upstream's initial values.
    ///
    /// # Panics
    ///
    /// Panics if `protocol_version < 30`. Use [`LegacyNdxCodec`] for protocol < 30.
    #[must_use]
    pub fn new(protocol_version: u8) -> Self {
        assert!(
            protocol_version >= 30,
            "ModernNdxCodec is for protocol >= 30, got {protocol_version}"
        );
        Self {
            protocol_version,
            prev_positive: -1,
            prev_negative: 1,
        }
    }
}

impl NdxCodec for ModernNdxCodec {
    fn write_ndx<W: Write + ?Sized>(&mut self, writer: &mut W, ndx: i32) -> io::Result<()> {
        let mut buf = [0u8; 6];
        let mut cnt = 0;

        // Compute diff and update state based on sign
        let (diff, ndx_positive) = if ndx >= 0 {
            let diff = ndx - self.prev_positive;
            self.prev_positive = ndx;
            (diff, ndx)
        } else if ndx == NDX_DONE {
            // NDX_DONE is sent as single-byte 0 with no side effects
            // Upstream io.c:2259-2262
            return writer.write_all(&[0x00]);
        } else {
            // All negative index bytes start with 0xFF
            // Upstream io.c:2263-2268
            buf[cnt] = 0xFF;
            cnt += 1;
            let ndx_abs = -ndx;
            let diff = ndx_abs - self.prev_negative;
            self.prev_negative = ndx_abs;
            (diff, ndx_abs)
        };

        // Encode the diff value
        // Upstream io.c:2270-2285
        if diff > 0 && diff < 0xFE {
            // Simple single-byte diff
            buf[cnt] = diff as u8;
            cnt += 1;
        } else if !(0..=0x7FFF).contains(&diff) {
            // Full 4-byte encoding with high bit set
            // Upstream io.c:2275-2280
            buf[cnt] = 0xFE;
            cnt += 1;
            buf[cnt] = ((ndx_positive >> 24) as u8) | 0x80;
            cnt += 1;
            buf[cnt] = ndx_positive as u8;
            cnt += 1;
            buf[cnt] = (ndx_positive >> 8) as u8;
            cnt += 1;
            buf[cnt] = (ndx_positive >> 16) as u8;
            cnt += 1;
        } else {
            // 2-byte diff encoding
            // Upstream io.c:2281-2284
            buf[cnt] = 0xFE;
            cnt += 1;
            buf[cnt] = (diff >> 8) as u8;
            cnt += 1;
            buf[cnt] = diff as u8;
            cnt += 1;
        }

        writer.write_all(&buf[..cnt])
    }

    fn write_ndx_done<W: Write + ?Sized>(&mut self, writer: &mut W) -> io::Result<()> {
        // NDX_DONE is always encoded as 0x00 for protocol 30+
        writer.write_all(&[0x00])
    }

    fn read_ndx<R: Read + ?Sized>(&mut self, reader: &mut R) -> io::Result<i32> {
        let mut b = [0u8; 4];
        reader.read_exact(&mut b[..1])?;

        let is_negative = if b[0] == 0xFF {
            reader.read_exact(&mut b[..1])?;
            true
        } else if b[0] == 0 {
            return Ok(NDX_DONE);
        } else {
            false
        };

        // Get the previous value based on sign
        let prev_val = if is_negative {
            self.prev_negative
        } else {
            self.prev_positive
        };

        let num = if b[0] == 0xFE {
            // Extended encoding
            // Upstream io.c:2305-2314
            reader.read_exact(&mut b[..1])?;
            if b[0] & 0x80 != 0 {
                // 4-byte full value
                // Upstream io.c:2307-2311
                let high = (b[0] & !0x80) as i32;
                reader.read_exact(&mut b[..3])?;
                (high << 24) | (b[0] as i32) | ((b[1] as i32) << 8) | ((b[2] as i32) << 16)
            } else {
                // 2-byte diff
                reader.read_exact(&mut b[1..2])?;
                let diff = ((b[0] as i32) << 8) | (b[1] as i32);
                prev_val + diff
            }
        } else {
            // Simple single-byte diff
            let diff = b[0] as i32;
            prev_val + diff
        };

        // Update the previous value tracker
        if is_negative {
            self.prev_negative = num;
        } else {
            self.prev_positive = num;
        }

        if is_negative { Ok(-num) } else { Ok(num) }
    }

    fn protocol_version(&self) -> u8 {
        self.protocol_version
    }
}

/// An NDX codec that handles both legacy and modern protocol formats.
///
/// This enum wraps both [`LegacyNdxCodec`] and [`ModernNdxCodec`] and dispatches
/// to the appropriate implementation based on the protocol version.
///
/// Use [`NdxCodecEnum::new`] or `create_ndx_codec` to create an instance.
#[derive(Debug, Clone)]
pub enum NdxCodecEnum {
    /// Legacy codec for protocol < 30
    Legacy(LegacyNdxCodec),
    /// Modern codec for protocol >= 30
    Modern(ModernNdxCodec),
}

impl NdxCodecEnum {
    /// Creates a new NDX codec for the given protocol version.
    ///
    /// Returns [`NdxCodecEnum::Legacy`] for protocol < 30 and
    /// [`NdxCodecEnum::Modern`] for protocol >= 30.
    #[must_use]
    pub fn new(protocol_version: u8) -> Self {
        if protocol_version < 30 {
            Self::Legacy(LegacyNdxCodec::new(protocol_version))
        } else {
            Self::Modern(ModernNdxCodec::new(protocol_version))
        }
    }
}

impl NdxCodec for NdxCodecEnum {
    fn write_ndx<W: Write + ?Sized>(&mut self, writer: &mut W, ndx: i32) -> io::Result<()> {
        match self {
            Self::Legacy(codec) => codec.write_ndx(writer, ndx),
            Self::Modern(codec) => codec.write_ndx(writer, ndx),
        }
    }

    fn write_ndx_done<W: Write + ?Sized>(&mut self, writer: &mut W) -> io::Result<()> {
        match self {
            Self::Legacy(codec) => codec.write_ndx_done(writer),
            Self::Modern(codec) => codec.write_ndx_done(writer),
        }
    }

    fn read_ndx<R: Read + ?Sized>(&mut self, reader: &mut R) -> io::Result<i32> {
        match self {
            Self::Legacy(codec) => codec.read_ndx(reader),
            Self::Modern(codec) => codec.read_ndx(reader),
        }
    }

    fn protocol_version(&self) -> u8 {
        match self {
            Self::Legacy(codec) => codec.protocol_version(),
            Self::Modern(codec) => codec.protocol_version(),
        }
    }
}

/// Creates an NDX codec appropriate for the given protocol version.
///
/// This is a convenience function that creates an [`NdxCodecEnum`].
///
/// # Returns
///
/// - [`NdxCodecEnum::Legacy`] for protocol < 30
/// - [`NdxCodecEnum::Modern`] for protocol >= 30
///
/// # Example
///
/// ```ignore
/// use protocol::ndx::create_ndx_codec;
///
/// let mut legacy_codec = create_ndx_codec(29);
/// assert_eq!(legacy_codec.protocol_version(), 29);
///
/// let mut modern_codec = create_ndx_codec(32);
/// assert_eq!(modern_codec.protocol_version(), 32);
/// ```
#[must_use]
pub fn create_ndx_codec(protocol_version: u8) -> NdxCodecEnum {
    NdxCodecEnum::new(protocol_version)
}

// ============================================================================
// Legacy compatibility: NdxState (kept for backward compatibility)
// ============================================================================

/// State tracker for NDX delta encoding (protocol 30+).
///
/// This is the original implementation, kept for backward compatibility.
/// For new code, prefer using `create_ndx_codec` with the Strategy pattern.
///
/// # Upstream Reference
///
/// `io.c:2245` - `static int32 prev_positive = -1, prev_negative = 1;`
#[derive(Debug, Clone)]
pub struct NdxState {
    /// Previous positive index value for delta encoding.
    prev_positive: i32,
    /// Previous negative index value (as positive) for delta encoding.
    prev_negative: i32,
}

impl Default for NdxState {
    fn default() -> Self {
        Self::new()
    }
}

impl NdxState {
    /// Creates a new NDX state with upstream's initial values.
    ///
    /// Upstream: `static int32 prev_positive = -1, prev_negative = 1;`
    #[must_use]
    pub const fn new() -> Self {
        Self {
            prev_positive: -1,
            prev_negative: 1,
        }
    }

    /// Writes an NDX value using the byte-reduction method (protocol 30+).
    ///
    /// **Note**: This method always uses protocol 30+ encoding. For protocol < 30,
    /// use [`LegacyNdxCodec`] or `create_ndx_codec`.
    pub fn write_ndx<W: Write>(&mut self, writer: &mut W, ndx: i32) -> io::Result<()> {
        let mut buf = [0u8; 6];
        let mut cnt = 0;

        let (diff, ndx_positive) = if ndx >= 0 {
            let diff = ndx - self.prev_positive;
            self.prev_positive = ndx;
            (diff, ndx)
        } else if ndx == NDX_DONE {
            return writer.write_all(&[0x00]);
        } else {
            buf[cnt] = 0xFF;
            cnt += 1;
            let ndx_abs = -ndx;
            let diff = ndx_abs - self.prev_negative;
            self.prev_negative = ndx_abs;
            (diff, ndx_abs)
        };

        if diff > 0 && diff < 0xFE {
            buf[cnt] = diff as u8;
            cnt += 1;
        } else if !(0..=0x7FFF).contains(&diff) {
            buf[cnt] = 0xFE;
            cnt += 1;
            buf[cnt] = ((ndx_positive >> 24) as u8) | 0x80;
            cnt += 1;
            buf[cnt] = ndx_positive as u8;
            cnt += 1;
            buf[cnt] = (ndx_positive >> 8) as u8;
            cnt += 1;
            buf[cnt] = (ndx_positive >> 16) as u8;
            cnt += 1;
        } else {
            buf[cnt] = 0xFE;
            cnt += 1;
            buf[cnt] = (diff >> 8) as u8;
            cnt += 1;
            buf[cnt] = diff as u8;
            cnt += 1;
        }

        writer.write_all(&buf[..cnt])
    }

    /// Reads an NDX value using the byte-reduction method (protocol 30+).
    ///
    /// **Note**: This method always uses protocol 30+ decoding. For protocol < 30,
    /// use [`LegacyNdxCodec`] or `create_ndx_codec`.
    pub fn read_ndx<R: Read>(&mut self, reader: &mut R) -> io::Result<i32> {
        let mut b = [0u8; 4];
        reader.read_exact(&mut b[..1])?;

        let is_negative = if b[0] == 0xFF {
            reader.read_exact(&mut b[..1])?;
            true
        } else if b[0] == 0 {
            return Ok(NDX_DONE);
        } else {
            false
        };

        let prev_val = if is_negative {
            self.prev_negative
        } else {
            self.prev_positive
        };

        let num = if b[0] == 0xFE {
            reader.read_exact(&mut b[..1])?;
            if b[0] & 0x80 != 0 {
                let high = (b[0] & !0x80) as i32;
                reader.read_exact(&mut b[..3])?;
                (high << 24) | (b[0] as i32) | ((b[1] as i32) << 8) | ((b[2] as i32) << 16)
            } else {
                reader.read_exact(&mut b[1..2])?;
                let diff = ((b[0] as i32) << 8) | (b[1] as i32);
                prev_val + diff
            }
        } else {
            let diff = b[0] as i32;
            prev_val + diff
        };

        if is_negative {
            self.prev_negative = num;
        } else {
            self.prev_positive = num;
        }

        if is_negative { Ok(-num) } else { Ok(num) }
    }
}

/// Convenience function to write NDX_FLIST_EOF using protocol 30+ encoding.
///
/// **Note**: For protocol < 30, use [`LegacyNdxCodec`] or `create_ndx_codec`.
pub fn write_ndx_flist_eof<W: Write>(writer: &mut W, state: &mut NdxState) -> io::Result<()> {
    state.write_ndx(writer, NDX_FLIST_EOF)
}

/// Convenience function to write NDX_DONE using protocol 30+ encoding.
///
/// **Note**: This writes `[0x00]` which is only correct for protocol 30+.
/// For protocol < 30, use [`LegacyNdxCodec`] or `create_ndx_codec`.
pub fn write_ndx_done<W: Write>(writer: &mut W) -> io::Result<()> {
    writer.write_all(&[0x00])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // ========================================================================
    // Strategy Pattern Tests
    // ========================================================================

    #[test]
    fn test_legacy_codec_writes_4_byte_le() {
        let mut codec = LegacyNdxCodec::new(29);
        let mut buf = Vec::new();

        codec.write_ndx(&mut buf, 5).unwrap();
        assert_eq!(buf, vec![5, 0, 0, 0], "positive index should be 4-byte LE");

        buf.clear();
        codec.write_ndx(&mut buf, 0).unwrap();
        assert_eq!(buf, vec![0, 0, 0, 0], "zero should be 4-byte LE");

        buf.clear();
        codec.write_ndx(&mut buf, 1000).unwrap();
        assert_eq!(
            buf,
            vec![0xE8, 0x03, 0x00, 0x00],
            "1000 should be 4-byte LE"
        );
    }

    #[test]
    fn test_legacy_codec_writes_ndx_done_as_4_bytes() {
        let mut codec = LegacyNdxCodec::new(28);
        let mut buf = Vec::new();

        codec.write_ndx_done(&mut buf).unwrap();
        assert_eq!(
            buf,
            vec![0xFF, 0xFF, 0xFF, 0xFF],
            "NDX_DONE should be -1 as 4-byte LE"
        );
    }

    #[test]
    fn test_legacy_codec_reads_4_byte_le() {
        let mut codec = LegacyNdxCodec::new(29);

        // Read positive value
        let data = vec![5u8, 0, 0, 0];
        let mut cursor = Cursor::new(&data);
        assert_eq!(codec.read_ndx(&mut cursor).unwrap(), 5);

        // Read NDX_DONE
        let data = vec![0xFFu8, 0xFF, 0xFF, 0xFF];
        let mut cursor = Cursor::new(&data);
        assert_eq!(codec.read_ndx(&mut cursor).unwrap(), NDX_DONE);

        // Read large value
        let data = vec![0xE8u8, 0x03, 0x00, 0x00];
        let mut cursor = Cursor::new(&data);
        assert_eq!(codec.read_ndx(&mut cursor).unwrap(), 1000);
    }

    #[test]
    fn test_legacy_codec_roundtrip() {
        let mut codec = LegacyNdxCodec::new(29);
        let values = [0, 1, 5, 100, 1000, 50000, NDX_DONE, NDX_FLIST_EOF];

        let mut buf = Vec::new();
        for &v in &values {
            codec.write_ndx(&mut buf, v).unwrap();
        }

        let mut cursor = Cursor::new(&buf);
        for &expected in &values {
            assert_eq!(codec.read_ndx(&mut cursor).unwrap(), expected);
        }
    }

    #[test]
    fn test_modern_codec_writes_delta_encoded() {
        let mut codec = ModernNdxCodec::new(32);
        let mut buf = Vec::new();

        // First positive: prev=-1, ndx=0, diff=1
        codec.write_ndx(&mut buf, 0).unwrap();
        assert_eq!(buf, vec![0x01], "first index 0 should be delta 1");

        buf.clear();
        // Second: prev=0, ndx=1, diff=1
        codec.write_ndx(&mut buf, 1).unwrap();
        assert_eq!(buf, vec![0x01], "sequential index should be delta 1");

        buf.clear();
        // Third: prev=1, ndx=5, diff=4
        codec.write_ndx(&mut buf, 5).unwrap();
        assert_eq!(buf, vec![0x04], "index 5 should be delta 4");
    }

    #[test]
    fn test_modern_codec_writes_ndx_done_as_single_byte() {
        let mut codec = ModernNdxCodec::new(30);
        let mut buf = Vec::new();

        codec.write_ndx_done(&mut buf).unwrap();
        assert_eq!(buf, vec![0x00], "NDX_DONE should be single byte 0x00");
    }

    #[test]
    fn test_modern_codec_roundtrip() {
        let mut write_codec = ModernNdxCodec::new(32);
        let mut buf = Vec::new();

        for ndx in [0, 1, 2, 5, 100, 253, 254, 500, 10000] {
            write_codec.write_ndx(&mut buf, ndx).unwrap();
        }

        let mut cursor = Cursor::new(&buf);
        let mut read_codec = ModernNdxCodec::new(32);

        for expected in [0, 1, 2, 5, 100, 253, 254, 500, 10000] {
            assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), expected);
        }
    }

    #[test]
    fn test_create_ndx_codec_selects_correct_implementation() {
        let legacy = create_ndx_codec(29);
        assert_eq!(legacy.protocol_version(), 29);

        let modern = create_ndx_codec(32);
        assert_eq!(modern.protocol_version(), 32);
    }

    #[test]
    fn test_codec_factory_protocol_boundary() {
        // Protocol 29: should be legacy
        let mut codec29 = create_ndx_codec(29);
        let mut buf = Vec::new();
        codec29.write_ndx(&mut buf, 5).unwrap();
        assert_eq!(buf.len(), 4, "protocol 29 should use 4-byte format");

        // Protocol 30: should be modern
        let mut codec30 = create_ndx_codec(30);
        buf.clear();
        codec30.write_ndx(&mut buf, 0).unwrap();
        assert_eq!(buf.len(), 1, "protocol 30 should use delta format");
    }

    #[test]
    #[should_panic(expected = "LegacyNdxCodec is for protocol < 30")]
    fn test_legacy_codec_panics_for_protocol_30() {
        let _ = LegacyNdxCodec::new(30);
    }

    #[test]
    #[should_panic(expected = "ModernNdxCodec is for protocol >= 30")]
    fn test_modern_codec_panics_for_protocol_29() {
        let _ = ModernNdxCodec::new(29);
    }

    // ========================================================================
    // Legacy NdxState Tests (backward compatibility)
    // ========================================================================

    #[test]
    fn test_ndx_done_encoding() {
        let mut buf = Vec::new();
        let mut state = NdxState::new();
        state.write_ndx(&mut buf, NDX_DONE).unwrap();
        assert_eq!(buf, vec![0x00]);
    }

    #[test]
    fn test_ndx_flist_eof_encoding() {
        // NDX_FLIST_EOF (-2) should encode as [0xFF, 0x01]
        // Because: -(-2) = 2, diff = 2 - 1 = 1, so 0xFF prefix + 0x01
        let mut buf = Vec::new();
        let mut state = NdxState::new();
        state.write_ndx(&mut buf, NDX_FLIST_EOF).unwrap();
        assert_eq!(buf, vec![0xFF, 0x01]);
    }

    #[test]
    fn test_positive_index_first() {
        // First positive index: prev_positive starts at -1
        // ndx=0: diff = 0 - (-1) = 1, encoded as single byte 0x01
        let mut buf = Vec::new();
        let mut state = NdxState::new();
        state.write_ndx(&mut buf, 0).unwrap();
        assert_eq!(buf, vec![0x01]);
    }

    #[test]
    fn test_positive_index_sequence() {
        let mut buf = Vec::new();
        let mut state = NdxState::new();

        // First: ndx=0, diff=1
        state.write_ndx(&mut buf, 0).unwrap();
        assert_eq!(buf, vec![0x01]);

        buf.clear();
        // Second: ndx=1, diff=1
        state.write_ndx(&mut buf, 1).unwrap();
        assert_eq!(buf, vec![0x01]);

        buf.clear();
        // Third: ndx=5, diff=4
        state.write_ndx(&mut buf, 5).unwrap();
        assert_eq!(buf, vec![0x04]);
    }

    #[test]
    fn test_roundtrip_positive() {
        let mut buf = Vec::new();
        let mut write_state = NdxState::new();

        for ndx in [0, 1, 2, 5, 100, 253, 254, 500, 10000, 50000] {
            write_state.write_ndx(&mut buf, ndx).unwrap();
        }

        let mut cursor = Cursor::new(&buf);
        let mut read_state = NdxState::new();

        for expected in [0, 1, 2, 5, 100, 253, 254, 500, 10000, 50000] {
            let ndx = read_state.read_ndx(&mut cursor).unwrap();
            assert_eq!(ndx, expected);
        }
    }

    #[test]
    fn test_roundtrip_negative() {
        let mut buf = Vec::new();
        let mut write_state = NdxState::new();

        state_write_ndx(&mut write_state, &mut buf, NDX_DONE).unwrap();
        state_write_ndx(&mut write_state, &mut buf, NDX_FLIST_EOF).unwrap();
        state_write_ndx(&mut write_state, &mut buf, NDX_DEL_STATS).unwrap();

        let mut cursor = Cursor::new(&buf);
        let mut read_state = NdxState::new();

        assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), NDX_DONE);
        assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), NDX_FLIST_EOF);
        assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), NDX_DEL_STATS);
    }

    fn state_write_ndx(state: &mut NdxState, buf: &mut Vec<u8>, ndx: i32) -> io::Result<()> {
        state.write_ndx(buf, ndx)
    }

    #[test]
    fn test_ndx_done_roundtrip() {
        let mut buf = Vec::new();
        write_ndx_done(&mut buf).unwrap();
        assert_eq!(buf, vec![0x00]);

        let mut cursor = Cursor::new(&buf);
        let mut state = NdxState::new();
        assert_eq!(state.read_ndx(&mut cursor).unwrap(), NDX_DONE);
    }

    #[test]
    fn test_large_index_encoding() {
        // Test extended 2-byte diff encoding (diff >= 254 but <= 32767)
        let mut buf = Vec::new();
        let mut state = NdxState::new();

        // ndx=253 after prev=-1 means diff=254, needs 0xFE prefix
        state.write_ndx(&mut buf, 253).unwrap();
        // First byte 0xFE, then 2-byte diff: 254 = 0x00FE
        assert_eq!(buf[0], 0xFE);
        assert_eq!(buf.len(), 3);
    }

    #[test]
    fn test_very_large_index_encoding() {
        // Test extended 4-byte full value encoding
        let mut buf = Vec::new();
        let mut state = NdxState::new();

        // Use a very large index that requires full encoding
        let large_ndx = 0x01_00_00_00; // 16 million+
        state.write_ndx(&mut buf, large_ndx).unwrap();

        // Should be 0xFE + 4 bytes (high bit set in first byte)
        assert_eq!(buf[0], 0xFE);
        assert!(buf[1] & 0x80 != 0);
        assert_eq!(buf.len(), 5);
    }

    // ========================================================================
    // Sign Transition Edge Cases
    // ========================================================================

    #[test]
    fn test_sign_transition_positive_to_negative_to_positive() {
        // Test: positive sequence, then negative (NDX_DONE), then positive again
        // This verifies the state tracks prev_positive and prev_negative separately
        let mut buf = Vec::new();
        let mut write_state = NdxState::new();

        // Write positive sequence: 0, 1, 2
        write_state.write_ndx(&mut buf, 0).unwrap();
        write_state.write_ndx(&mut buf, 1).unwrap();
        write_state.write_ndx(&mut buf, 2).unwrap();

        // Write NDX_DONE (-1) - should NOT affect prev_positive
        write_state.write_ndx(&mut buf, NDX_DONE).unwrap();

        // Write positive again: 3, 4 (should continue from prev_positive=2)
        write_state.write_ndx(&mut buf, 3).unwrap();
        write_state.write_ndx(&mut buf, 4).unwrap();

        // Read back and verify
        let mut cursor = Cursor::new(&buf);
        let mut read_state = NdxState::new();

        assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), 0);
        assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), 1);
        assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), 2);
        assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), NDX_DONE);
        assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), 3);
        assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), 4);
    }

    #[test]
    fn test_alternating_positive_negative_values() {
        // Test: alternating between positive and negative values
        let mut buf = Vec::new();
        let mut write_state = NdxState::new();

        let sequence = [0, NDX_DONE, 5, NDX_FLIST_EOF, 10, NDX_DEL_STATS, 15];

        for &val in &sequence {
            write_state.write_ndx(&mut buf, val).unwrap();
        }

        let mut cursor = Cursor::new(&buf);
        let mut read_state = NdxState::new();

        for &expected in &sequence {
            assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), expected);
        }
    }

    #[test]
    fn test_negative_sequence_tracks_prev_negative() {
        // Test: multiple negative values in sequence use delta encoding
        let mut buf = Vec::new();
        let mut write_state = NdxState::new();

        // Write sequence of negative values (other than NDX_DONE)
        // NDX_FLIST_EOF = -2, NDX_DEL_STATS = -3, NDX_FLIST_OFFSET = -101
        write_state.write_ndx(&mut buf, NDX_FLIST_EOF).unwrap();
        write_state.write_ndx(&mut buf, NDX_DEL_STATS).unwrap();
        write_state.write_ndx(&mut buf, NDX_FLIST_OFFSET).unwrap();

        let mut cursor = Cursor::new(&buf);
        let mut read_state = NdxState::new();

        assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), NDX_FLIST_EOF);
        assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), NDX_DEL_STATS);
        assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), NDX_FLIST_OFFSET);
    }

    // ========================================================================
    // State Isolation Tests
    // ========================================================================

    #[test]
    fn test_codec_instances_have_independent_state() {
        // Test: two codec instances should not share state
        let mut codec1 = ModernNdxCodec::new(32);
        let mut codec2 = ModernNdxCodec::new(32);

        let mut buf1 = Vec::new();
        let mut buf2 = Vec::new();

        // Codec1: writes 0, 1, 2 (prev_positive progresses: -1, 0, 1)
        codec1.write_ndx(&mut buf1, 0).unwrap();
        codec1.write_ndx(&mut buf1, 1).unwrap();
        codec1.write_ndx(&mut buf1, 2).unwrap();

        // Codec2: writes 100, 101 (prev_positive progresses: -1, 100)
        // This should be independent of codec1's state
        codec2.write_ndx(&mut buf2, 100).unwrap();
        codec2.write_ndx(&mut buf2, 101).unwrap();

        // Read back codec1's data
        let mut cursor1 = Cursor::new(&buf1);
        let mut read_codec1 = ModernNdxCodec::new(32);
        assert_eq!(read_codec1.read_ndx(&mut cursor1).unwrap(), 0);
        assert_eq!(read_codec1.read_ndx(&mut cursor1).unwrap(), 1);
        assert_eq!(read_codec1.read_ndx(&mut cursor1).unwrap(), 2);

        // Read back codec2's data
        let mut cursor2 = Cursor::new(&buf2);
        let mut read_codec2 = ModernNdxCodec::new(32);
        assert_eq!(read_codec2.read_ndx(&mut cursor2).unwrap(), 100);
        assert_eq!(read_codec2.read_ndx(&mut cursor2).unwrap(), 101);
    }

    // ========================================================================
    // Encoding Boundary Tests
    // ========================================================================

    #[test]
    fn test_delta_boundary_at_253() {
        // Test: diff=253 should use single-byte encoding (max single byte)
        let mut buf = Vec::new();
        let mut state = NdxState::new();

        // ndx=252 after prev=-1 means diff=253
        state.write_ndx(&mut buf, 252).unwrap();
        assert_eq!(buf.len(), 1, "diff=253 should be single byte");
        assert_eq!(buf[0], 253);
    }

    #[test]
    fn test_delta_boundary_at_254() {
        // Test: diff=254 should trigger 2-byte encoding
        let mut buf = Vec::new();
        let mut state = NdxState::new();

        // ndx=253 after prev=-1 means diff=254
        state.write_ndx(&mut buf, 253).unwrap();
        assert_eq!(buf[0], 0xFE, "diff=254 needs 0xFE prefix");
        assert_eq!(buf.len(), 3, "diff=254 should be 3 bytes (prefix + 2)");
    }

    #[test]
    fn test_large_gap_encoding() {
        // Test: large gap between consecutive values
        let mut buf = Vec::new();
        let mut write_state = NdxState::new();

        // First write index 100
        write_state.write_ndx(&mut buf, 100).unwrap();
        buf.clear();

        // Now write index 50000 (gap of 49900)
        write_state.write_ndx(&mut buf, 50000).unwrap();

        // Verify extended encoding is used
        assert_eq!(buf[0], 0xFE, "large gap needs extended encoding");

        // Roundtrip test
        let mut write_state2 = NdxState::new();
        let mut buf2 = Vec::new();
        write_state2.write_ndx(&mut buf2, 100).unwrap();
        write_state2.write_ndx(&mut buf2, 50000).unwrap();

        let mut cursor = Cursor::new(&buf2);
        let mut read_state = NdxState::new();
        assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), 100);
        assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), 50000);
    }

    #[test]
    fn test_4byte_encoding_for_very_large_diff() {
        // Test: diff > 32767 requires 4-byte encoding
        let mut buf = Vec::new();
        let mut state = NdxState::new();

        // ndx = 40000 after prev=-1 means diff = 40001
        state.write_ndx(&mut buf, 40000).unwrap();

        assert_eq!(buf[0], 0xFE, "large diff needs 0xFE prefix");
        // High bit set indicates 4-byte format
        assert!(buf[1] & 0x80 != 0, "4-byte format should have high bit set");
        assert_eq!(buf.len(), 5, "4-byte format: prefix + 4 bytes");
    }

    // ========================================================================
    // NDX Constants Edge Cases
    // ========================================================================

    #[test]
    fn test_all_negative_constants() {
        let mut buf = Vec::new();
        let mut write_state = NdxState::new();

        // Write all defined negative constants
        let negatives = [NDX_DONE, NDX_FLIST_EOF, NDX_DEL_STATS, NDX_FLIST_OFFSET];

        for &val in &negatives {
            write_state.write_ndx(&mut buf, val).unwrap();
        }

        let mut cursor = Cursor::new(&buf);
        let mut read_state = NdxState::new();

        for &expected in &negatives {
            assert_eq!(
                read_state.read_ndx(&mut cursor).unwrap(),
                expected,
                "failed for constant {expected}"
            );
        }
    }

    #[test]
    fn test_ndx_flist_offset_roundtrip() {
        // NDX_FLIST_OFFSET (-101) is used for incremental file lists
        let mut buf = Vec::new();
        let mut write_state = NdxState::new();

        write_state.write_ndx(&mut buf, NDX_FLIST_OFFSET).unwrap();

        let mut cursor = Cursor::new(&buf);
        let mut read_state = NdxState::new();

        assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), NDX_FLIST_OFFSET);
    }

    // ========================================================================
    // Protocol Version Range Tests (v27-v32)
    // ========================================================================

    #[test]
    fn test_all_versions_create_valid_ndx_codecs() {
        for version in 28..=32 {
            let codec = create_ndx_codec(version);
            assert_eq!(codec.protocol_version(), version);
        }
    }

    #[test]
    fn test_version_boundary_at_30_for_ndx() {
        // Version 29 uses legacy 4-byte encoding
        let mut legacy = create_ndx_codec(29);
        let mut legacy_buf = Vec::new();
        legacy.write_ndx(&mut legacy_buf, 0).unwrap();
        assert_eq!(legacy_buf.len(), 4);

        // Version 30 uses delta-encoded format
        let mut modern = create_ndx_codec(30);
        let mut modern_buf = Vec::new();
        modern.write_ndx(&mut modern_buf, 0).unwrap();
        assert_eq!(modern_buf.len(), 1); // Delta encoding is more compact
    }

    // ========================================================================
    // Interop Tests - Upstream NDX Byte Patterns
    // ========================================================================

    #[test]
    fn test_legacy_ndx_upstream_byte_patterns() {
        let mut codec = LegacyNdxCodec::new(29);

        // Zero as 4-byte LE
        let mut buf = Vec::new();
        codec.write_ndx(&mut buf, 0).unwrap();
        assert_eq!(buf, [0x00, 0x00, 0x00, 0x00]);

        // 255 as 4-byte LE
        buf.clear();
        codec.write_ndx(&mut buf, 255).unwrap();
        assert_eq!(buf, [0xff, 0x00, 0x00, 0x00]);

        // NDX_DONE (-1) as 4-byte LE
        buf.clear();
        codec.write_ndx(&mut buf, NDX_DONE).unwrap();
        assert_eq!(buf, [0xff, 0xff, 0xff, 0xff]);

        // NDX_FLIST_EOF (-2) as 4-byte LE
        buf.clear();
        codec.write_ndx(&mut buf, NDX_FLIST_EOF).unwrap();
        assert_eq!(buf, [0xfe, 0xff, 0xff, 0xff]);
    }

    #[test]
    fn test_modern_ndx_done_is_single_byte_zero() {
        // NDX_DONE is always 0x00 in protocol 30+
        let mut codec = ModernNdxCodec::new(30);
        let mut buf = Vec::new();
        codec.write_ndx(&mut buf, NDX_DONE).unwrap();
        assert_eq!(buf, [0x00]);
    }

    #[test]
    fn test_modern_ndx_first_positive_is_delta_one() {
        // First positive after initial prev=-1: diff=1
        let mut codec = ModernNdxCodec::new(30);
        let mut buf = Vec::new();
        codec.write_ndx(&mut buf, 0).unwrap();
        assert_eq!(buf, [0x01]); // delta from -1 to 0 is 1
    }

    #[test]
    fn test_modern_ndx_sequential_indices() {
        let mut codec = ModernNdxCodec::new(30);
        let mut buf = Vec::new();

        // Each sequential index has delta=1
        for ndx in 0..5 {
            codec.write_ndx(&mut buf, ndx).unwrap();
        }

        // Should be all 0x01 bytes
        assert_eq!(buf, [0x01, 0x01, 0x01, 0x01, 0x01]);
    }

    #[test]
    fn test_modern_ndx_negative_prefix_0xff() {
        // All negative values (except -1) start with 0xFF prefix
        let mut codec = ModernNdxCodec::new(30);
        let mut buf = Vec::new();

        codec.write_ndx(&mut buf, NDX_FLIST_EOF).unwrap(); // -2
        assert_eq!(buf[0], 0xFF);

        buf.clear();
        let mut codec2 = ModernNdxCodec::new(30);
        codec2.write_ndx(&mut buf, NDX_DEL_STATS).unwrap(); // -3
        assert_eq!(buf[0], 0xFF);
    }

    // ========================================================================
    // Error Handling Tests
    // ========================================================================

    #[test]
    fn test_legacy_ndx_read_truncated() {
        let mut codec = LegacyNdxCodec::new(29);
        let truncated = [0u8, 0, 0]; // Only 3 bytes, need 4
        let mut cursor = Cursor::new(&truncated[..]);
        assert!(codec.read_ndx(&mut cursor).is_err());
    }

    #[test]
    fn test_modern_ndx_read_truncated_extended() {
        let mut codec = ModernNdxCodec::new(30);

        // Extended encoding starts with 0xFE but is incomplete
        let truncated = [0xFE, 0x00]; // Missing bytes for 2-byte diff
        let mut cursor = Cursor::new(&truncated[..]);
        assert!(codec.read_ndx(&mut cursor).is_err());
    }

    #[test]
    fn test_modern_ndx_read_truncated_4byte() {
        let mut codec = ModernNdxCodec::new(30);

        // 4-byte encoding with high bit set but incomplete
        let truncated = [0xFE, 0x80, 0x00, 0x00]; // Missing last byte
        let mut cursor = Cursor::new(&truncated[..]);
        assert!(codec.read_ndx(&mut cursor).is_err());
    }

    #[test]
    fn test_empty_input_returns_error() {
        let mut legacy = LegacyNdxCodec::new(29);
        let mut modern = ModernNdxCodec::new(30);
        let empty: [u8; 0] = [];

        let mut cursor = Cursor::new(&empty[..]);
        assert!(legacy.read_ndx(&mut cursor).is_err());

        let mut cursor = Cursor::new(&empty[..]);
        assert!(modern.read_ndx(&mut cursor).is_err());
    }

    // ========================================================================
    // Cross-Version Compatibility Tests
    // ========================================================================

    #[test]
    fn test_all_versions_roundtrip_ndx_constants() {
        let constants = [NDX_DONE, NDX_FLIST_EOF, NDX_DEL_STATS, NDX_FLIST_OFFSET];

        for version in [28, 29, 30, 31, 32] {
            let mut write_codec = create_ndx_codec(version);
            let mut buf = Vec::new();

            for &ndx in &constants {
                write_codec.write_ndx(&mut buf, ndx).unwrap();
            }

            let mut read_codec = create_ndx_codec(version);
            let mut cursor = Cursor::new(&buf);

            for &expected in &constants {
                let read = read_codec.read_ndx(&mut cursor).unwrap();
                assert_eq!(read, expected, "v{version} roundtrip failed for {expected}");
            }
        }
    }

    #[test]
    fn test_all_versions_roundtrip_positive_sequence() {
        let indices: Vec<i32> = (0..100).collect();

        for version in [28, 29, 30, 31, 32] {
            let mut write_codec = create_ndx_codec(version);
            let mut buf = Vec::new();

            for &ndx in &indices {
                write_codec.write_ndx(&mut buf, ndx).unwrap();
            }

            let mut read_codec = create_ndx_codec(version);
            let mut cursor = Cursor::new(&buf);

            for &expected in &indices {
                let read = read_codec.read_ndx(&mut cursor).unwrap();
                assert_eq!(read, expected, "v{version} roundtrip failed for index {expected}");
            }
        }
    }

    // ========================================================================
    // Extended Encoding Boundary Tests
    // ========================================================================

    #[test]
    fn test_modern_single_byte_max_diff_253() {
        // Diff values 1-253 use single-byte encoding
        let mut codec = ModernNdxCodec::new(30);
        let mut buf = Vec::new();

        // ndx=252 after prev=-1 means diff=253 (max single byte)
        codec.write_ndx(&mut buf, 252).unwrap();
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0], 253);
    }

    #[test]
    fn test_modern_two_byte_at_diff_254() {
        // Diff >= 254 triggers 0xFE prefix
        let mut codec = ModernNdxCodec::new(30);
        let mut buf = Vec::new();

        // ndx=253 after prev=-1 means diff=254
        codec.write_ndx(&mut buf, 253).unwrap();
        assert_eq!(buf[0], 0xFE);
        assert_eq!(buf.len(), 3); // 0xFE + 2 bytes for diff
    }

    #[test]
    fn test_modern_two_byte_max_diff_32767() {
        // 2-byte encoding handles diff up to 32767 (0x7FFF)
        let mut codec = ModernNdxCodec::new(30);
        let mut buf = Vec::new();

        // First write to set prev_positive
        codec.write_ndx(&mut buf, 0).unwrap();
        buf.clear();

        // Write 32767 (diff = 32767 from prev=0)
        codec.write_ndx(&mut buf, 32767).unwrap();
        assert_eq!(buf[0], 0xFE);
        // High bit not set means 2-byte diff
        assert!(buf[1] & 0x80 == 0);
    }

    #[test]
    fn test_modern_four_byte_for_large_diff() {
        // Diff > 32767 requires 4-byte full value encoding
        let mut codec = ModernNdxCodec::new(30);
        let mut buf = Vec::new();

        // Large value that requires full encoding
        codec.write_ndx(&mut buf, 0x01_00_00_00).unwrap();
        assert_eq!(buf[0], 0xFE);
        // High bit set indicates 4-byte format
        assert!(buf[1] & 0x80 != 0);
        assert_eq!(buf.len(), 5);
    }

    // ========================================================================
    // NdxCodecEnum Dispatch Tests
    // ========================================================================

    #[test]
    fn test_ndx_codec_enum_dispatches_correctly() {
        // Legacy via enum
        let mut legacy_enum = NdxCodecEnum::new(29);
        let mut buf = Vec::new();
        legacy_enum.write_ndx(&mut buf, 5).unwrap();
        assert_eq!(buf.len(), 4, "enum should use legacy 4-byte format");

        // Modern via enum
        let mut modern_enum = NdxCodecEnum::new(30);
        buf.clear();
        modern_enum.write_ndx(&mut buf, 0).unwrap();
        assert_eq!(buf.len(), 1, "enum should use modern delta format");
    }

    #[test]
    fn test_ndx_codec_enum_protocol_version() {
        for version in [28, 29, 30, 31, 32] {
            let codec = NdxCodecEnum::new(version);
            assert_eq!(codec.protocol_version(), version);
        }
    }

    #[test]
    fn test_create_ndx_codec_matches_direct_construction() {
        // Factory function should produce same behavior as direct construction
        let factory = create_ndx_codec(29);
        let direct = NdxCodecEnum::Legacy(LegacyNdxCodec::new(29));
        assert_eq!(factory.protocol_version(), direct.protocol_version());

        let factory = create_ndx_codec(30);
        let direct = NdxCodecEnum::Modern(ModernNdxCodec::new(30));
        assert_eq!(factory.protocol_version(), direct.protocol_version());
    }

    // ========================================================================
    // NdxState Legacy Compatibility Tests
    // ========================================================================

    #[test]
    fn test_ndx_state_default_equals_new() {
        let default_state = NdxState::default();
        let new_state = NdxState::new();

        // Both should produce same output for same input
        let mut default_buf = Vec::new();
        let mut new_buf = Vec::new();

        let mut d = default_state.clone();
        let mut n = new_state.clone();

        d.write_ndx(&mut default_buf, 0).unwrap();
        n.write_ndx(&mut new_buf, 0).unwrap();

        assert_eq!(default_buf, new_buf);
    }

    #[test]
    fn test_write_ndx_done_helper() {
        let mut buf = Vec::new();
        write_ndx_done(&mut buf).unwrap();
        assert_eq!(buf, [0x00]);
    }

    #[test]
    fn test_write_ndx_flist_eof_helper() {
        let mut buf = Vec::new();
        let mut state = NdxState::new();
        write_ndx_flist_eof(&mut buf, &mut state).unwrap();
        // NDX_FLIST_EOF (-2) with prev_negative=1: diff = 2-1 = 1
        assert_eq!(buf, [0xFF, 0x01]);
    }

    #[test]
    fn test_ndx_state_clone_independence() {
        let mut state = NdxState::new();
        let mut buf = Vec::new();
        state.write_ndx(&mut buf, 0).unwrap();
        state.write_ndx(&mut buf, 1).unwrap();

        // Clone after writing
        let mut cloned = state.clone();

        // Write different values to each
        let mut orig_buf = Vec::new();
        let mut clone_buf = Vec::new();

        state.write_ndx(&mut orig_buf, 10).unwrap();
        cloned.write_ndx(&mut clone_buf, 10).unwrap();

        // Both should produce same output since they have same prev state
        assert_eq!(orig_buf, clone_buf);
    }

    // ========================================================================
    // Extreme Value Tests
    // ========================================================================

    #[test]
    fn test_ndx_extreme_positive_values() {
        let extreme_values = [
            0i32,
            1,
            127,
            128,
            253,
            254,
            255,
            256,
            32767,
            32768,
            65535,
            65536,
            0x7FFF_FFFF, // i32::MAX
        ];

        for version in [28, 29, 30, 31, 32] {
            let mut write_codec = create_ndx_codec(version);
            let mut buf = Vec::new();

            for &ndx in &extreme_values {
                write_codec.write_ndx(&mut buf, ndx).unwrap();
            }

            let mut read_codec = create_ndx_codec(version);
            let mut cursor = Cursor::new(&buf);

            for &expected in &extreme_values {
                let read = read_codec.read_ndx(&mut cursor).unwrap();
                assert_eq!(read, expected, "v{version} failed for {expected}");
            }
        }
    }

    #[test]
    fn test_ndx_large_gaps() {
        // Test with large gaps between values
        let values = [0i32, 10000, 20000, 100000, 1000000];

        for version in [28, 29, 30, 31, 32] {
            let mut write_codec = create_ndx_codec(version);
            let mut buf = Vec::new();

            for &ndx in &values {
                write_codec.write_ndx(&mut buf, ndx).unwrap();
            }

            let mut read_codec = create_ndx_codec(version);
            let mut cursor = Cursor::new(&buf);

            for &expected in &values {
                let read = read_codec.read_ndx(&mut cursor).unwrap();
                assert_eq!(read, expected, "v{version} failed for {expected}");
            }
        }
    }
}
