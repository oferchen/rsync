//! NDX codec trait and protocol-versioned implementations.
//!
//! Provides the Strategy pattern for NDX encoding/decoding across protocol
//! versions, with [`LegacyNdxCodec`] (protocol < 30) and [`ModernNdxCodec`]
//! (protocol >= 30).

use std::io::{self, Read, Write};

use super::constants::NDX_DONE;

/// Strategy trait for NDX encoding/decoding.
///
/// Implementations provide protocol-version-specific wire formats for file
/// list indices. Use [`create_ndx_codec`] to get the appropriate implementation.
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

    fn protocol_version(&self) -> u8 {
        self.protocol_version
    }
}

/// An NDX codec that handles both legacy and modern protocol formats.
///
/// This enum wraps both [`LegacyNdxCodec`] and [`ModernNdxCodec`] and dispatches
/// to the appropriate implementation based on the protocol version.
///
/// Use [`NdxCodecEnum::new`] or [`create_ndx_codec`] to create an instance.
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
/// Returns [`NdxCodecEnum::Legacy`] for protocol < 30 and
/// [`NdxCodecEnum::Modern`] for protocol >= 30.
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

/// NDX codec wrapper that asserts strictly monotonic positive file indices.
///
/// Wraps an [`NdxCodecEnum`] and adds a `debug_assert!` in `write_ndx` to
/// verify that each positive NDX (file index) is strictly greater than the
/// previous one. Negative sentinel values (NDX_DONE, NDX_FLIST_EOF, etc.)
/// are excluded from this check since they are control signals, not file
/// indices.
///
/// Use this at wire-emission points (generator/receiver transfer loops) to
/// catch ordering violations from parallel processing before they become
/// protocol errors.
///
/// In release builds, the assertion is compiled out and this wrapper has
/// zero overhead.
#[derive(Debug, Clone)]
pub struct MonotonicNdxWriter {
    /// Inner codec that performs the actual wire encoding.
    inner: NdxCodecEnum,
    /// Tracks the last positive NDX written for monotonicity verification.
    #[cfg(debug_assertions)]
    last_positive: Option<i32>,
}

impl MonotonicNdxWriter {
    /// Creates a new monotonic NDX writer for the given protocol version.
    #[must_use]
    pub fn new(protocol_version: u8) -> Self {
        Self {
            inner: NdxCodecEnum::new(protocol_version),
            #[cfg(debug_assertions)]
            last_positive: None,
        }
    }
}

impl NdxCodec for MonotonicNdxWriter {
    fn write_ndx<W: Write + ?Sized>(&mut self, writer: &mut W, ndx: i32) -> io::Result<()> {
        // Only check positive file indices - negative values are sentinels
        // (NDX_DONE, NDX_FLIST_EOF, NDX_DEL_STATS, NDX_FLIST_OFFSET).
        #[cfg(debug_assertions)]
        if ndx >= 0 {
            if let Some(prev) = self.last_positive {
                debug_assert!(
                    ndx > prev,
                    "NDX monotonicity violation: emitted {ndx} after {prev} - \
                     file indices must be strictly increasing on the wire"
                );
            }
            self.last_positive = Some(ndx);
        }
        self.inner.write_ndx(writer, ndx)
    }

    fn write_ndx_done<W: Write + ?Sized>(&mut self, writer: &mut W) -> io::Result<()> {
        self.inner.write_ndx_done(writer)
    }

    fn read_ndx<R: Read + ?Sized>(&mut self, reader: &mut R) -> io::Result<i32> {
        self.inner.read_ndx(reader)
    }

    fn protocol_version(&self) -> u8 {
        self.inner.protocol_version()
    }
}
