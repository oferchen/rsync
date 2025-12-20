// crates/protocol/src/ndx.rs
//! NDX (file-list index) encoding and decoding for the rsync protocol.
//!
//! This module implements the byte-reduction method for file-list indices
//! as defined in upstream rsync io.c:2243-2318 (write_ndx/read_ndx).
//!
//! # Wire Format
//!
//! For protocol 30+, NDX values use delta encoding:
//! - `0x00`: NDX_DONE (-1) - signals end of file requests
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

/// State tracker for NDX delta encoding.
///
/// The rsync protocol uses delta encoding for file indices, where each value
/// is encoded relative to the previous value of the same sign. This struct
/// tracks the previous values to enable correct encoding/decoding.
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

    /// Writes an NDX value using the byte-reduction method.
    ///
    /// # Protocol 30+ Encoding (upstream io.c:2243-2287)
    ///
    /// - NDX_DONE (-1): single byte 0x00
    /// - Other negative: 0xFF prefix, then delta-encoded absolute value
    /// - Positive: delta-encoded from previous positive
    ///
    /// Delta encoding:
    /// - diff 1-253: single byte
    /// - diff 254-32767 or 0: 0xFE + 2 bytes
    /// - diff < 0 or > 32767: 0xFE + 4 bytes (high bit set in first byte)
    pub fn write_ndx<W: Write>(&mut self, writer: &mut W, ndx: i32) -> io::Result<()> {
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

    /// Reads an NDX value using the byte-reduction method.
    ///
    /// # Protocol 30+ Decoding (upstream io.c:2289-2318)
    ///
    /// - 0x00: NDX_DONE (-1)
    /// - 0xFF: negative value follows
    /// - 0x01-0xFD: delta from previous positive
    /// - 0xFE: extended encoding follows
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
}

/// Convenience function to write NDX_FLIST_EOF.
///
/// This is commonly used after sending the file list to signal that there
/// are no more incremental file lists when INC_RECURSE is enabled.
///
/// # Upstream Reference
///
/// `flist.c:2541` - `write_ndx(f, NDX_FLIST_EOF);`
pub fn write_ndx_flist_eof<W: Write>(writer: &mut W, state: &mut NdxState) -> io::Result<()> {
    state.write_ndx(writer, NDX_FLIST_EOF)
}

/// Convenience function to write NDX_DONE.
pub fn write_ndx_done<W: Write>(writer: &mut W) -> io::Result<()> {
    // NDX_DONE is always encoded as 0x00, no state tracking needed
    writer.write_all(&[0x00])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

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
