//! Stateful NDX delta encoding for protocol 30+.
//!
//! [`NdxState`] provides a standalone delta-encoding tracker with convenience
//! helpers for common NDX write operations.

use std::io::{self, Read, Write};

use super::constants::{NDX_DONE, NDX_FLIST_EOF};

/// State tracker for NDX delta encoding (protocol 30+).
///
/// This is the original implementation, kept for backward compatibility.
/// For new code, prefer using [`super::create_ndx_codec`] with the Strategy pattern.
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
    /// use [`super::LegacyNdxCodec`] or [`super::create_ndx_codec`].
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
    /// use [`super::LegacyNdxCodec`] or [`super::create_ndx_codec`].
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
/// **Note**: For protocol < 30, use [`super::LegacyNdxCodec`] or [`super::create_ndx_codec`].
pub fn write_ndx_flist_eof<W: Write>(writer: &mut W, state: &mut NdxState) -> io::Result<()> {
    state.write_ndx(writer, NDX_FLIST_EOF)
}

/// Convenience function to write NDX_DONE using protocol 30+ encoding.
///
/// **Note**: This writes `[0x00]` which is only correct for protocol 30+.
/// For protocol < 30, use [`super::LegacyNdxCodec`] or [`super::create_ndx_codec`].
pub fn write_ndx_done<W: Write>(writer: &mut W) -> io::Result<()> {
    writer.write_all(&[0x00])
}
