//! crates/protocol/src/ndx.rs
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
}
