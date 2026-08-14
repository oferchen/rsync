#![deny(unsafe_code)]
//! The per-file `sum_head` that precedes every delta token stream.
//!
//! Upstream writes four little-endian 32-bit fields ahead of the tokens:
//!
//! | field       | meaning                                             |
//! |-------------|-----------------------------------------------------|
//! | `count`     | number of basis blocks the tokens may reference     |
//! | `blength`   | length of every block except possibly the last      |
//! | `s2length`  | width of the per-block strong sum (protocol >= 27)  |
//! | `remainder` | length of the final block, or 0 when it is full     |
//!
//! upstream: `io.c:write_sum_head()` / `io.c:read_sum_head()`.
//!
//! [`SumHead`] is the single owner of that geometry. Whoever emits a delta
//! body derives the head from the same [`SumHead`] it validates each block
//! match against, and whoever replays a body resolves every match token
//! through [`SumHead::block_span`]. A head that does not describe the body it
//! precedes therefore cannot be produced or accepted - which is what upstream
//! enforces at `receiver.c:414` before it indexes the block table.

use std::io::{self, Read, Write};

use thiserror::Error;

use crate::protocol_violation::protocol_violation;

/// Largest block length any protocol era may advertise, in bytes.
///
/// upstream: `rsync.h` `MAX_BLOCK_SIZE ((int32)1 << 29)`. Used as the
/// permissive ceiling so a header accepted by either protocol era decodes
/// here; the narrower protocol-30 ceiling is a sizing choice made by the
/// generator, not a decoding rule.
pub const MAX_BLOCK_SIZE: u32 = 1 << 29;

/// Largest strong-sum width a `sum_head` may advertise, in bytes.
///
/// upstream: `io.c:2056` bounds `s2length` by `xfer_sum_len`. SHA1 at 20
/// bytes is the widest transfer digest oc-rsync negotiates.
pub const MAX_STRONG_SUM_LEN: u32 = 20;

/// Wire bytes each signature block occupies: a 4-byte rolling sum plus a
/// strong sum of at most [`MAX_STRONG_SUM_LEN`].
const MAX_BLOCK_ENTRY_LEN: usize = 4 + MAX_STRONG_SUM_LEN as usize;

/// Errors raised when a `sum_head` cannot describe the body that follows it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum SumHeadError {
    /// A block-match token referenced a block the `sum_head` does not describe.
    ///
    /// upstream: `receiver.c:571` - `Invalid block index %d (count=%ld)`,
    /// which aborts the transfer with `RERR_PROTOCOL`.
    #[error("Invalid block index {index} (count={count})")]
    InvalidBlockIndex {
        /// The block index carried by the match token.
        index: i32,
        /// The block count advertised by the `sum_head`.
        count: i32,
    },

    /// The block count is negative, or large enough that sizing the block
    /// table from it would overflow.
    ///
    /// upstream: `io.c:2029` and `io.c:2039-2048`.
    #[error("invalid checksum count {count}")]
    InvalidCount {
        /// The rejected count.
        count: i32,
    },

    /// The block length is negative, past [`MAX_BLOCK_SIZE`], or zero while
    /// blocks are advertised - which would make every block empty.
    ///
    /// upstream: `io.c:2050-2054`.
    #[error("invalid block length {blength}")]
    InvalidBlockLength {
        /// The rejected block length.
        blength: i32,
    },

    /// The strong-sum width is negative or past [`MAX_STRONG_SUM_LEN`].
    ///
    /// upstream: `io.c:2056-2060`.
    #[error("invalid checksum length {s2length}")]
    InvalidStrongSumLength {
        /// The rejected strong-sum width.
        s2length: i32,
    },

    /// The trailing-block length is negative or wider than a whole block.
    ///
    /// upstream: `io.c:2061-2066`.
    #[error("invalid remainder length {remainder}")]
    InvalidRemainder {
        /// The rejected remainder.
        remainder: i32,
    },
}

impl From<SumHeadError> for io::Error {
    /// Maps a rejected `sum_head` onto the `RERR_PROTOCOL` (exit 2) path.
    ///
    /// upstream: `io.c:2032-2065` `read_sum_head()` calls
    /// `exit_cleanup(RERR_PROTOCOL)` on any out-of-range field.
    fn from(error: SumHeadError) -> Self {
        protocol_violation(format!("malformed sum_head: {error}"))
    }
}

/// Block geometry shared by a delta body and the `sum_head` that precedes it.
///
/// upstream: `rsync.h:200` `struct sum_struct`, serialised by
/// `io.c:write_sum_head()` and parsed by `io.c:read_sum_head()`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SumHead {
    count: u32,
    blength: u32,
    s2length: u32,
    remainder: u32,
}

impl SumHead {
    /// Serialised width of a `sum_head` for protocol >= 27, in bytes.
    ///
    /// Four 32-bit little-endian fields. Protocol 26 and older omit
    /// `s2length`, which oc-rsync does not negotiate.
    pub const WIRE_LEN: usize = 16;

    /// The all-zero head a whole-file transfer advertises.
    ///
    /// upstream: `io.c:write_sum_head()` substitutes a `static struct
    /// sum_struct null_sum` when the generator sent no sums, so every field -
    /// including `s2length` - goes out as zero.
    pub const WHOLE_FILE: Self = Self {
        count: 0,
        blength: 0,
        s2length: 0,
        remainder: 0,
    };

    /// Builds a head describing `count` basis blocks.
    ///
    /// `remainder` is the length of the final block when it is short, or 0
    /// when the basis divides evenly into `blength`-sized blocks.
    ///
    /// # Errors
    ///
    /// Returns the [`SumHeadError`] variant for whichever field cannot
    /// describe a real block layout.
    pub const fn with_blocks(
        count: u32,
        blength: u32,
        s2length: u32,
        remainder: u32,
    ) -> Result<Self, SumHeadError> {
        let head = Self {
            count,
            blength,
            s2length,
            remainder,
        };
        match head.check() {
            Ok(()) => Ok(head),
            Err(error) => Err(error),
        }
    }

    /// Applies upstream's `read_sum_head()` field checks.
    ///
    /// The fields size downstream allocations (`Vec::with_capacity(count)`,
    /// `vec![0u8; s2length]`), so an unbounded header from an authenticated
    /// but untrusted peer is a memory-exhaustion vector.
    const fn check(self) -> Result<(), SumHeadError> {
        // upstream: io.c:2029 - the field is signed on the wire, so a negative
        // count arrives here as a u32 above `i32::MAX` and is rejected.
        //
        // upstream: io.c:2039-2048 - the block table is sized `count *
        // (4 + s2length)`; bound the first factor so that product cannot
        // overflow on a 32-bit target either.
        if self.count > i32::MAX as u32 || self.count as usize > usize::MAX / MAX_BLOCK_ENTRY_LEN {
            return Err(SumHeadError::InvalidCount {
                count: self.count as i32,
            });
        }
        // upstream: io.c:2050-2054 - blength in (0, MAX_BLOCK_SIZE]; a zero
        // block length with blocks advertised would make every block empty.
        if self.blength > MAX_BLOCK_SIZE || (self.count > 0 && self.blength == 0) {
            return Err(SumHeadError::InvalidBlockLength {
                blength: self.blength as i32,
            });
        }
        // upstream: io.c:2056-2060 - s2length in [0, xfer_sum_len].
        if self.s2length > MAX_STRONG_SUM_LEN {
            return Err(SumHeadError::InvalidStrongSumLength {
                s2length: self.s2length as i32,
            });
        }
        // upstream: io.c:2061-2066 - remainder in [0, blength].
        if self.remainder > self.blength {
            return Err(SumHeadError::InvalidRemainder {
                remainder: self.remainder as i32,
            });
        }
        Ok(())
    }

    /// Rejects a head whose per-block strong sum is wider than the negotiated
    /// transfer digest `xfer_sum_len`.
    ///
    /// [`check`](Self::check) only bounds `s2length` by the static
    /// [`MAX_STRONG_SUM_LEN`] ceiling (SHA-1, the widest negotiable digest).
    /// A peer that negotiated a narrower transfer checksum (XXH64 / XXH3-64 =
    /// 8 bytes) can still advertise a legal-looking `s2length` up to 20; the
    /// reader would then consume `4 + s2length` bytes per block while the
    /// generator only wrote `4 + xfer_sum_len`, desyncing the signature-block
    /// stream. Gating on the negotiated width closes that gap while leaving a
    /// well-behaved header - whose `s2length` never exceeds `xfer_sum_len` -
    /// untouched.
    ///
    /// upstream: io.c:read_sum_head - `if (sum->s2length < 0 ||
    /// sum->s2length > xfer_sum_len) exit_cleanup(RERR_PROTOCOL)`.
    ///
    /// # Errors
    ///
    /// Returns [`SumHeadError::InvalidStrongSumLength`] when `s2length`
    /// exceeds `xfer_sum_len`.
    pub const fn ensure_s2length_within(self, xfer_sum_len: u32) -> Result<Self, SumHeadError> {
        if self.s2length > xfer_sum_len {
            return Err(SumHeadError::InvalidStrongSumLength {
                s2length: self.s2length as i32,
            });
        }
        Ok(self)
    }

    /// Number of basis blocks a match token may reference.
    #[inline]
    #[must_use]
    pub const fn count(self) -> u32 {
        self.count
    }

    /// Length of every basis block except a short trailing one.
    #[inline]
    #[must_use]
    pub const fn blength(self) -> u32 {
        self.blength
    }

    /// Width of each per-block strong sum, in bytes.
    #[inline]
    #[must_use]
    pub const fn s2length(self) -> u32 {
        self.s2length
    }

    /// Length of the trailing basis block, or 0 when it is full-length.
    #[inline]
    #[must_use]
    pub const fn remainder(self) -> u32 {
        self.remainder
    }

    /// Whether the body may contain block-match tokens at all.
    #[inline]
    #[must_use]
    pub const fn describes_blocks(self) -> bool {
        self.count > 0
    }

    /// Total basis length this geometry covers.
    ///
    /// upstream: `receiver.c:289-291` - `sum.flength = count * blength;
    /// if (remainder) flength -= blength - remainder`. Append mode starts
    /// writing at this offset.
    #[inline]
    #[must_use]
    pub const fn flength(self) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let total = self.count as u64 * self.blength as u64;
        if self.remainder > 0 {
            total - self.blength as u64 + self.remainder as u64
        } else {
            total
        }
    }

    /// Resolves a match token's block index to a basis offset and length.
    ///
    /// This is the one place a block index becomes a byte range, so a head
    /// that does not describe the body it precedes fails here instead of
    /// indexing a block table that has no such entry.
    ///
    /// upstream: `receiver.c:414-422`
    ///
    /// ```text
    /// i = -(i+1);
    /// if (i < 0 || i >= sum.count) { ...Invalid block index... }
    /// offset2 = i * (OFF_T)sum.blength;
    /// len = sum.blength;
    /// if (i == (int)sum.count-1 && sum.remainder != 0)
    ///         len = sum.remainder;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`SumHeadError::InvalidBlockIndex`] when `index` is negative or
    /// not below [`SumHead::count`] - including every index against a
    /// whole-file head, which describes no blocks.
    pub const fn block_span(self, index: i32) -> Result<(u64, u32), SumHeadError> {
        if index < 0 || index as u32 >= self.count {
            return Err(SumHeadError::InvalidBlockIndex {
                index,
                count: self.count as i32,
            });
        }
        let index = index as u32;
        let offset = index as u64 * self.blength as u64;
        let len = if index == self.count - 1 && self.remainder != 0 {
            self.remainder
        } else {
            self.blength
        };
        Ok((offset, len))
    }

    /// Encodes the head into its 16 wire bytes.
    #[inline]
    #[must_use]
    pub const fn encode(self) -> [u8; Self::WIRE_LEN] {
        let count = (self.count as i32).to_le_bytes();
        let blength = (self.blength as i32).to_le_bytes();
        let s2length = (self.s2length as i32).to_le_bytes();
        let remainder = (self.remainder as i32).to_le_bytes();
        [
            count[0],
            count[1],
            count[2],
            count[3],
            blength[0],
            blength[1],
            blength[2],
            blength[3],
            s2length[0],
            s2length[1],
            s2length[2],
            s2length[3],
            remainder[0],
            remainder[1],
            remainder[2],
            remainder[3],
        ]
    }

    /// Decodes a head from its 16 wire bytes, rejecting impossible geometry.
    ///
    /// # Errors
    ///
    /// Returns the [`SumHeadError`] variant for whichever field upstream's
    /// `read_sum_head()` would reject.
    pub const fn decode(bytes: [u8; Self::WIRE_LEN]) -> Result<Self, SumHeadError> {
        // Fields are signed on the wire (write_int/read_int). A negative value
        // becomes a huge u32 and is caught by the ceilings in `check`.
        let head = Self {
            count: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            blength: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            s2length: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            remainder: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
        };
        match head.check() {
            Ok(()) => Ok(head),
            Err(error) => Err(error),
        }
    }

    /// Writes the head to `writer`.
    ///
    /// # Errors
    ///
    /// Propagates any write error from the underlying stream.
    #[inline]
    pub fn write<W: Write + ?Sized>(self, writer: &mut W) -> io::Result<()> {
        writer.write_all(&self.encode())
    }

    /// Reads a head from `reader`.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the 16 bytes cannot be read, or a
    /// `RERR_PROTOCOL`-tagged error when they do not describe a block layout.
    #[inline]
    pub fn read<R: Read + ?Sized>(reader: &mut R) -> io::Result<Self> {
        let mut buf = [0u8; Self::WIRE_LEN];
        reader.read_exact(&mut buf)?;
        Self::decode(buf).map_err(io::Error::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Upstream substitutes an all-zero `sum_struct` for a whole-file
    /// transfer, `s2length` included; advertising a non-zero strong-sum width
    /// there would describe blocks the body does not contain.
    #[test]
    fn whole_file_head_is_all_zero() {
        assert_eq!(SumHead::WHOLE_FILE.encode(), [0u8; SumHead::WIRE_LEN]);
        assert!(!SumHead::WHOLE_FILE.describes_blocks());
        assert_eq!(SumHead::WHOLE_FILE.flength(), 0);
    }

    /// A whole-file head must reject every block index: a body that emits one
    /// under this head is the stream that crashes upstream's receiver.
    #[test]
    fn whole_file_head_rejects_every_block_index() {
        assert_eq!(
            SumHead::WHOLE_FILE.block_span(0),
            Err(SumHeadError::InvalidBlockIndex { index: 0, count: 0 })
        );
    }

    /// The geometry upstream 3.4.4 writes for a 100 KiB basis, so the encoding
    /// is pinned to observed bytes rather than to our own writer.
    #[test]
    fn encodes_the_upstream_geometry_for_a_100k_basis() {
        let head = SumHead::with_blocks(147, 700, 2, 200).expect("valid geometry");
        assert_eq!(
            head.encode(),
            [
                0x93, 0, 0, 0, // count = 147
                0xbc, 0x02, 0, 0, // blength = 700
                0x02, 0, 0, 0, // s2length = 2
                0xc8, 0, 0, 0, // remainder = 200
            ]
        );
        assert_eq!(SumHead::decode(head.encode()), Ok(head));
        assert_eq!(head.flength(), 102_400);
    }

    /// Only the final block is short, and only when `remainder` is set.
    #[test]
    fn block_span_shortens_only_the_final_block() {
        let head = SumHead::with_blocks(147, 700, 2, 200).expect("valid geometry");
        assert_eq!(head.block_span(0), Ok((0, 700)));
        assert_eq!(head.block_span(145), Ok((145 * 700, 700)));
        assert_eq!(head.block_span(146), Ok((146 * 700, 200)));
        assert_eq!(
            head.block_span(147),
            Err(SumHeadError::InvalidBlockIndex {
                index: 147,
                count: 147,
            })
        );
        assert_eq!(
            head.block_span(-1),
            Err(SumHeadError::InvalidBlockIndex {
                index: -1,
                count: 147,
            })
        );
    }

    /// An evenly divided basis leaves every block full-length.
    #[test]
    fn zero_remainder_keeps_the_final_block_full_length() {
        let head = SumHead::with_blocks(4, 700, 2, 0).expect("valid geometry");
        assert_eq!(head.block_span(3), Ok((3 * 700, 700)));
        assert_eq!(head.flength(), 2800);
    }

    /// Blocks without a block length would each be empty, so the geometry is
    /// refused at both construction and decode.
    #[test]
    fn blocks_without_a_block_length_are_malformed() {
        assert_eq!(
            SumHead::with_blocks(147, 0, 2, 0),
            Err(SumHeadError::InvalidBlockLength { blength: 0 })
        );
        let mut bytes = [0u8; SumHead::WIRE_LEN];
        bytes[0] = 0x93;
        assert!(SumHead::decode(bytes).is_err());
    }

    /// Negative fields arrive as huge unsigned values and must be rejected by
    /// the same ceilings upstream applies.
    ///
    /// Each case starts from a header that is otherwise valid, so the rejection
    /// can only be attributed to the field under test - a blanket all-zero
    /// buffer would let a bad `count` be caught by the `blength` rule instead.
    #[test]
    fn negative_fields_are_rejected() {
        let valid = SumHead::with_blocks(4, 512, 16, 0).expect("valid geometry");
        let negative = (-1i32).to_le_bytes();
        let expected = [
            SumHeadError::InvalidCount { count: -1 },
            SumHeadError::InvalidBlockLength { blength: -1 },
            SumHeadError::InvalidStrongSumLength { s2length: -1 },
            SumHeadError::InvalidRemainder { remainder: -1 },
        ];
        for (field, want) in expected.into_iter().enumerate() {
            let mut bytes = valid.encode();
            bytes[field * 4..field * 4 + 4].copy_from_slice(&negative);
            assert_eq!(SumHead::decode(bytes), Err(want), "field {field}");
        }
    }

    #[test]
    fn out_of_range_fields_are_rejected() {
        assert_eq!(
            SumHead::with_blocks(1, MAX_BLOCK_SIZE + 1, 2, 0),
            Err(SumHeadError::InvalidBlockLength {
                blength: (MAX_BLOCK_SIZE + 1) as i32,
            })
        );
        assert_eq!(
            SumHead::with_blocks(1, 700, MAX_STRONG_SUM_LEN + 1, 0),
            Err(SumHeadError::InvalidStrongSumLength {
                s2length: (MAX_STRONG_SUM_LEN + 1) as i32,
            })
        );
        assert_eq!(
            SumHead::with_blocks(1, 700, 2, 701),
            Err(SumHeadError::InvalidRemainder { remainder: 701 })
        );
    }

    /// A header whose `s2length` exceeds the negotiated transfer digest width
    /// must be rejected: the block reader would consume `4 + s2length` bytes
    /// per block while the generator only wrote `4 + xfer_sum_len`, desyncing
    /// the signature-block stream. A width equal to or below the negotiated
    /// digest is the only geometry a well-behaved peer emits, so both are
    /// accepted unchanged.
    #[test]
    fn s2length_wider_than_negotiated_digest_is_rejected() {
        // XXH64 negotiates an 8-byte transfer digest; a header claiming a
        // 16-byte strong sum passes the static SHA-1 ceiling but must not pass
        // the negotiated one.
        let head = SumHead::with_blocks(4, 512, 16, 0).expect("valid geometry");
        assert_eq!(
            head.ensure_s2length_within(8),
            Err(SumHeadError::InvalidStrongSumLength { s2length: 16 })
        );
        // s2length == negotiated width is the widest a generator writes.
        assert_eq!(head.ensure_s2length_within(16), Ok(head));
        // s2length < negotiated width (e.g. a phase-1 short sum) is accepted.
        assert_eq!(head.ensure_s2length_within(20), Ok(head));

        let narrow = SumHead::with_blocks(4, 512, 8, 0).expect("valid geometry");
        assert_eq!(narrow.ensure_s2length_within(8), Ok(narrow));
        assert_eq!(
            narrow.ensure_s2length_within(4),
            Err(SumHeadError::InvalidStrongSumLength { s2length: 8 })
        );
    }

    #[test]
    fn round_trips_through_a_stream() {
        let head = SumHead::with_blocks(3, 1024, 16, 512).expect("valid geometry");
        let mut buf = Vec::new();
        head.write(&mut buf).expect("write");
        assert_eq!(buf.len(), SumHead::WIRE_LEN);
        assert_eq!(SumHead::read(&mut buf.as_slice()).expect("read"), head);
    }

    /// A rejected header maps to `RERR_PROTOCOL`, not a stream I/O error.
    #[test]
    fn reading_malformed_geometry_is_a_protocol_violation() {
        let mut bytes = [0u8; SumHead::WIRE_LEN];
        bytes[0] = 0x93;
        let error = SumHead::read(&mut bytes.as_slice()).expect_err("malformed");
        assert!(error.to_string().contains("malformed sum_head"));
        assert!(
            error
                .get_ref()
                .and_then(|inner| inner.downcast_ref::<crate::ProtocolViolation>())
                .is_some(),
            "a rejected sum_head must map to RERR_PROTOCOL, not RERR_STREAMIO"
        );
    }
}
