//! Block sizing heuristics that mirror upstream rsync's `generator.c:sum_sizes_sqroot()`.

use core::num::{NonZeroU8, NonZeroU32};

use protocol::ProtocolVersion;
use thiserror::Error;

use crate::block_size::MAX_SUM_LENGTH as SUM_LENGTH;

/// Default block length used by rsync when files are small.
const BLOCK_SIZE: u32 = 700;
/// Maximum block size supported by protocol versions 30 and newer.
const MAX_BLOCK_SIZE: u32 = 1 << 17;
/// Maximum block size accepted by legacy protocol versions (< 30).
const OLD_MAX_BLOCK_SIZE: u32 = 1 << 29;
/// Bias applied when computing strong checksum lengths for larger files.
const BLOCKSUM_BIAS: i32 = 10;

/// Default negotiated transfer-digest width, matching upstream's 16-byte digests
/// (MD5, MD4, XXH3-128) for which `MIN(SUM_LENGTH, xfer_sum_len)` is a no-op.
const DEFAULT_TRANSFER_DIGEST_LENGTH: NonZeroU8 = match NonZeroU8::new(SUM_LENGTH) {
    Some(len) => len,
    None => panic!("SUM_LENGTH must be non-zero"),
};

/// Parameters describing a file signature computation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignatureLayoutParams {
    file_length: u64,
    forced_block_length: Option<NonZeroU32>,
    protocol: ProtocolVersion,
    checksum_length: NonZeroU8,
    transfer_digest_length: NonZeroU8,
}

impl SignatureLayoutParams {
    /// Constructs layout parameters from file metadata and protocol settings.
    #[must_use]
    pub const fn new(
        file_length: u64,
        forced_block_length: Option<NonZeroU32>,
        protocol: ProtocolVersion,
        checksum_length: NonZeroU8,
    ) -> Self {
        Self {
            file_length,
            forced_block_length,
            protocol,
            checksum_length,
            transfer_digest_length: DEFAULT_TRANSFER_DIGEST_LENGTH,
        }
    }

    /// Sets the negotiated transfer-checksum digest width in bytes.
    ///
    /// The strong sum can be no longer than the negotiated digest: a short
    /// checksum (e.g. XXH64 / XXH3-64 = 8 bytes) makes the digest width less
    /// than [`SUM_LENGTH`], and the sender rejects a `s2length` wider than the
    /// digest it expects. Defaults to [`SUM_LENGTH`] (16), so the standard
    /// 16-byte digests (MD5, MD4, XXH3-128) leave the layout byte-identical to
    /// upstream. (upstream: generator.c:705 `max_s2length = MIN(SUM_LENGTH, xfer_sum_len)`)
    #[inline]
    #[must_use]
    pub const fn with_transfer_digest_length(mut self, transfer_digest_length: NonZeroU8) -> Self {
        self.transfer_digest_length = transfer_digest_length;
        self
    }

    /// Source file size in bytes.
    #[inline]
    #[must_use]
    pub const fn file_length(self) -> u64 {
        self.file_length
    }

    /// Caller-specified block length override, if any.
    #[inline]
    pub const fn forced_block_length(self) -> Option<NonZeroU32> {
        self.forced_block_length
    }

    /// Protocol version governing layout computation.
    #[inline]
    #[must_use]
    pub const fn protocol(self) -> ProtocolVersion {
        self.protocol
    }

    /// Negotiated strong checksum truncation length.
    #[inline]
    #[must_use]
    pub const fn checksum_length(self) -> NonZeroU8 {
        self.checksum_length
    }

    /// Negotiated transfer-checksum digest width in bytes.
    #[inline]
    #[must_use]
    pub const fn transfer_digest_length(self) -> NonZeroU8 {
        self.transfer_digest_length
    }
}

/// Describes the block layout and checksum characteristics of a file signature.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignatureLayout {
    block_length: NonZeroU32,
    remainder: u32,
    block_count: u64,
    strong_sum_length: NonZeroU8,
}

impl SignatureLayout {
    /// Creates a layout from raw components (for wire protocol reconstruction).
    #[must_use]
    pub const fn from_raw_parts(
        block_length: NonZeroU32,
        remainder: u32,
        block_count: u64,
        strong_sum_length: NonZeroU8,
    ) -> Self {
        Self {
            block_length,
            remainder,
            block_count,
            strong_sum_length,
        }
    }

    /// Computed block size in bytes for signature generation.
    #[inline]
    #[must_use]
    pub const fn block_length(self) -> NonZeroU32 {
        self.block_length
    }

    /// Returns the trailing byte count that does not fill a complete block.
    #[inline]
    #[must_use]
    pub const fn remainder(self) -> u32 {
        self.remainder
    }

    /// Total number of blocks covering the source file.
    #[inline]
    #[must_use]
    pub const fn block_count(self) -> u64 {
        self.block_count
    }

    /// Strong checksum byte length per block.
    #[inline]
    #[must_use]
    pub const fn strong_sum_length(self) -> NonZeroU8 {
        self.strong_sum_length
    }

    /// Computes the total file size from the layout components.
    ///
    /// This is calculated as `(block_count - 1) * block_length + remainder`,
    /// or `block_count * block_length` if there's no remainder.
    #[inline]
    #[must_use]
    pub fn file_size(self) -> u64 {
        if self.block_count == 0 {
            return 0;
        }

        let block_len = u64::from(self.block_length.get());

        if self.remainder == 0 {
            self.block_count * block_len
        } else {
            (self.block_count - 1) * block_len + u64::from(self.remainder)
        }
    }
}

/// Errors produced when calculating signature layouts.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum SignatureLayoutError {
    /// File length exceeded [`i64::MAX`], which upstream rsync rejects.
    #[error("file length {length} exceeds i64::MAX")]
    FileTooLarge {
        /// Length in bytes of the file being processed.
        length: u64,
    },
    /// Number of blocks exceeded [`i32::MAX`].
    #[error("block count {blocks} derived from block length {block_length} exceeds i32::MAX")]
    BlockCountOverflow {
        /// Block length that triggered the overflow.
        block_length: u32,
        /// Block count produced by the sizing heuristic.
        blocks: u64,
    },
}

/// Calculates the signature layout for a file using rsync's heuristics.
#[doc(alias = "--block-size")]
#[doc(alias = "sum_sizes_sqroot")]
#[allow(clippy::cast_possible_truncation)]
pub fn calculate_signature_layout(
    params: SignatureLayoutParams,
) -> Result<SignatureLayout, SignatureLayoutError> {
    if params.file_length() > i64::MAX as u64 {
        return Err(SignatureLayoutError::FileTooLarge {
            length: params.file_length(),
        });
    }

    let mut block_length = match params.forced_block_length() {
        Some(length) => length.get(),
        None => derive_block_length(params.file_length(), params.protocol()),
    };

    let max_block = if params.protocol().as_u8() < 30 {
        OLD_MAX_BLOCK_SIZE
    } else {
        MAX_BLOCK_SIZE
    };

    if block_length > max_block {
        block_length = max_block;
    }

    let block_length_non_zero =
        NonZeroU32::new(block_length).expect("block length must be non-zero after clamping");

    let mut block_count = params.file_length() / u64::from(block_length);
    let remainder = (params.file_length() % u64::from(block_length)) as u32;
    if remainder != 0 {
        block_count = block_count.saturating_add(1);
    }

    if block_count > i32::MAX as u64 {
        return Err(SignatureLayoutError::BlockCountOverflow {
            block_length,
            blocks: block_count,
        });
    }

    let strong_sum_length = derive_strong_sum_length(
        params.file_length(),
        block_length,
        params.protocol(),
        params.checksum_length(),
        params.transfer_digest_length(),
    );

    Ok(SignatureLayout {
        block_length: block_length_non_zero,
        remainder,
        block_count,
        strong_sum_length,
    })
}

/// Derives a block length from file size using rsync's square-root heuristic.
///
/// Files at or below `BLOCK_SIZE²` use the default block size; larger files use
/// an approximation of `sqrt(file_length)` rounded down to a multiple of 8 and
/// clamped to the protocol-specific maximum.
fn derive_block_length(file_length: u64, protocol: ProtocolVersion) -> u32 {
    if file_length <= u64::from(BLOCK_SIZE).saturating_mul(u64::from(BLOCK_SIZE)) {
        return BLOCK_SIZE;
    }

    let max_block_length = if protocol.as_u8() < 30 {
        OLD_MAX_BLOCK_SIZE
    } else {
        MAX_BLOCK_SIZE
    };

    let mut c: u64 = 1;
    let mut l = file_length;
    while l >> 2 != 0 {
        c <<= 1;
        l >>= 2;
    }

    if c >= u64::from(max_block_length) {
        return max_block_length;
    }

    let mut block_length = 0u64;
    let mut current = c;
    while current >= 8 {
        block_length |= current;
        let candidate = u128::from(block_length);
        if u128::from(file_length) < candidate.saturating_mul(candidate) {
            block_length &= !current;
        }
        current >>= 1;
    }

    let block_length = block_length.max(u64::from(BLOCK_SIZE));
    block_length as u32
}

/// Computes the strong checksum length for a given file and block size.
///
/// Behavior depends on the transfer phase:
/// - **Phase 1** (`checksum_length = SHORT_SUM_LENGTH = 2`): dynamically computes
///   a length between 2-16 bytes using a bias heuristic based on file and block sizes.
///   Smaller files get shorter checksums, reducing signature overhead.
/// - **Phase 2 redo** (`checksum_length = SUM_LENGTH = 16`): returns the full
///   negotiated digest width (`max_s2length`), ensuring maximal collision
///   resistance for retransmissions without exceeding the negotiated digest.
///
/// The result is capped by `max_s2length = MIN(SUM_LENGTH, transfer_digest_length)`:
/// a narrower negotiated transfer digest (XXH64 / XXH3-64 = 8 bytes) bounds the
/// strong sum so the wire `sum_head` never advertises a `s2length` wider than the
/// checksum the sender expects.
///
/// (upstream: generator.c:697-750 `sum_sizes_sqroot()`, specifically
/// generator.c:705 `max_s2length = MIN(SUM_LENGTH, xfer_sum_len)`)
fn derive_strong_sum_length(
    file_length: u64,
    block_length: u32,
    protocol: ProtocolVersion,
    checksum_length: NonZeroU8,
    transfer_digest_length: NonZeroU8,
) -> NonZeroU8 {
    if protocol.as_u8() < 27 {
        return checksum_length;
    }

    // upstream: generator.c:705 `max_s2length = MIN(SUM_LENGTH, xfer_sum_len)`.
    let max_s2length = i32::from(SUM_LENGTH.min(transfer_digest_length.get()));

    // upstream: generator.c:738-740 - a full-length phase csum yields the whole
    // negotiated digest, not an unconditional SUM_LENGTH.
    if checksum_length.get() == SUM_LENGTH {
        return NonZeroU8::new(max_s2length as u8).expect("max_s2length floors at 1");
    }

    let mut bias = BLOCKSUM_BIAS;
    let mut l = file_length;
    while l >> 1 != 0 {
        l >>= 1;
        bias += 2;
    }

    let mut current = block_length;
    while current >> 1 != 0 && bias > 0 {
        current >>= 1;
        bias -= 1;
    }

    // upstream: generator.c:747-749 - `MAX(s2length, csum_length)` floors the
    // computed length, then `MIN(s2length, max_s2length)` caps it last, so the
    // negotiated digest width wins even below the phase csum length.
    let mut strong_len = (bias + 1 - 32 + 7) / 8;
    let min_len = i32::from(checksum_length.get());
    if strong_len < min_len {
        strong_len = min_len;
    }
    if strong_len > max_s2length {
        strong_len = max_s2length;
    }

    NonZeroU8::new(strong_len as u8).expect("strong checksum length must be non-zero")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_size::SHORT_SUM_LENGTH;
    use core::num::NonZeroU8;
    use std::convert::TryFrom;

    fn params(
        length: u64,
        forced: Option<u32>,
        protocol: u8,
        checksum: u8,
    ) -> SignatureLayoutParams {
        SignatureLayoutParams::new(
            length,
            forced.and_then(NonZeroU32::new),
            ProtocolVersion::try_from(protocol).expect("supported protocol"),
            NonZeroU8::new(checksum).expect("checksum length must be non-zero"),
        )
    }

    /// A narrower negotiated transfer digest (XXH64 / XXH3-64 = 8 bytes) must cap
    /// the `sum_head` `s2length`, mirroring upstream generator.c:705
    /// `max_s2length = MIN(SUM_LENGTH, xfer_sum_len)`. The sender rejects an
    /// `s2length` wider than the digest it expects, so an uncapped 16-byte sum
    /// desyncs the wire. WHY: the strong-sum byte on the wire must equal what a
    /// real upstream generator writes for the same negotiated digest.
    #[test]
    fn phase2_strong_sum_capped_by_narrow_transfer_digest() {
        let digest8 = NonZeroU8::new(8).expect("digest length");
        let capped = calculate_signature_layout(
            params(1 << 20, None, 32, SUM_LENGTH).with_transfer_digest_length(digest8),
        )
        .expect("layout");
        // upstream returns max_s2length = MIN(16, 8) = 8, not a full 16-byte sum.
        assert_eq!(capped.strong_sum_length().get(), 8);

        // The default 16-byte digests (MD5, MD4, XXH3-128) stay byte-identical.
        let full =
            calculate_signature_layout(params(1 << 20, None, 32, SUM_LENGTH)).expect("layout");
        assert_eq!(full.strong_sum_length().get(), SUM_LENGTH);
    }

    /// The heuristic (phase-1) branch also caps at the negotiated digest width.
    /// A large file that naturally yields a strong sum wider than the negotiated
    /// 8-byte digest must be clamped to 8, not to SUM_LENGTH. Uses protocol 29
    /// (`OLD_MAX_BLOCK_SIZE`) so a large forced block is not clamped to 2^17,
    /// letting the computed s2length exceed 8.
    #[test]
    fn phase1_strong_sum_capped_by_narrow_transfer_digest() {
        let digest8 = NonZeroU8::new(8).expect("digest length");
        let uncapped =
            calculate_signature_layout(params(1 << 57, Some(1 << 27), 29, SHORT_SUM_LENGTH))
                .expect("layout");
        assert!(uncapped.strong_sum_length().get() > 8);

        let capped = calculate_signature_layout(
            params(1 << 57, Some(1 << 27), 29, SHORT_SUM_LENGTH)
                .with_transfer_digest_length(digest8),
        )
        .expect("layout");
        assert_eq!(capped.strong_sum_length().get(), 8);
    }

    #[test]
    fn small_files_use_default_block_size() {
        let layout = calculate_signature_layout(params(32, None, 32, 16)).expect("layout");
        assert_eq!(layout.block_length().get(), 700);
        assert_eq!(layout.block_count(), 1);
        assert_eq!(layout.remainder(), 32);
        assert_eq!(layout.strong_sum_length().get(), 16);
    }

    #[test]
    fn block_length_scales_with_file_size() {
        let layout =
            calculate_signature_layout(params(10 * 1024 * 1024, None, 32, 16)).expect("layout");
        assert_eq!(layout.block_length().get(), 3_232);
        assert_eq!(layout.remainder(), 1_152);
        assert_eq!(layout.block_count(), 3_245);
        assert_eq!(layout.strong_sum_length().get(), 16);
    }

    #[test]
    fn large_files_clamp_to_protocol_maximum() {
        let layout = calculate_signature_layout(params(1u64 << 34, None, 32, 16)).expect("layout");
        assert_eq!(layout.block_length().get(), 131_072);
        assert_eq!(layout.block_count(), 131_072);
        assert_eq!(layout.remainder(), 0);
    }

    #[test]
    fn legacy_protocols_allow_larger_block_lengths() {
        let layout = calculate_signature_layout(params(1u64 << 34, None, 29, 16)).expect("layout");
        assert!(layout.block_length().get() >= 131_072);
        assert_eq!(layout.strong_sum_length().get(), 16);
    }

    #[test]
    fn checksum_length_respects_bias_heuristic() {
        let layout = calculate_signature_layout(params(1_048_576, None, 32, 2)).expect("layout");
        assert_eq!(layout.block_length().get(), 1_024);
        assert_eq!(layout.strong_sum_length().get(), 2);
    }

    #[test]
    fn forced_block_length_is_honoured() {
        let layout =
            calculate_signature_layout(params(50_000, Some(4_096), 32, 16)).expect("layout");
        assert_eq!(layout.block_length().get(), 4_096);
        assert_eq!(layout.block_count(), 13);
        assert_eq!(layout.remainder(), 848);
    }

    #[test]
    fn block_count_overflow_is_reported() {
        let params = params(
            (i32::MAX as u64 + 1) * u64::from(BLOCK_SIZE),
            Some(BLOCK_SIZE),
            32,
            16,
        );
        let error = calculate_signature_layout(params).expect_err("overflow");
        match error {
            SignatureLayoutError::BlockCountOverflow { block_length, .. } => {
                assert_eq!(block_length, BLOCK_SIZE);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn file_length_overflow_is_reported() {
        let params = params(u64::MAX, None, 32, 16);
        let error = calculate_signature_layout(params).expect_err("overflow");
        assert!(matches!(error, SignatureLayoutError::FileTooLarge { .. }));
    }

    #[test]
    fn sum_length_derive_strong_phase1_is_dynamic() {
        // Phase 1 (SHORT_SUM_LENGTH = 2): the heuristic widens the result for
        // large files, exceeding the minimum.
        use crate::block_size::SHORT_SUM_LENGTH;

        let checksum_len = NonZeroU8::new(SHORT_SUM_LENGTH).unwrap();
        let protocol = ProtocolVersion::try_from(31u8).unwrap();
        let digest = NonZeroU8::new(SUM_LENGTH).unwrap();
        let result =
            derive_strong_sum_length(100 * 1024 * 1024, 10_240, protocol, checksum_len, digest);

        assert!(result.get() >= SHORT_SUM_LENGTH);
        assert!(result.get() <= SUM_LENGTH);
        assert!(
            result.get() > SHORT_SUM_LENGTH,
            "large file should exceed SHORT_SUM_LENGTH"
        );
    }

    #[test]
    fn sum_length_derive_strong_phase2_redo_returns_max() {
        // Phase 2 redo (SUM_LENGTH = 16) short-circuits and returns 16 for any
        // file/block-size combination.
        let checksum_len = NonZeroU8::new(SUM_LENGTH).unwrap();
        let protocol = ProtocolVersion::try_from(31u8).unwrap();
        let digest = NonZeroU8::new(SUM_LENGTH).unwrap();

        for &(file_len, block_len) in &[
            (1024u64, 700u32),
            (10 * 1024 * 1024, 3232),
            (1u64 << 30, 32768),
        ] {
            let result =
                derive_strong_sum_length(file_len, block_len, protocol, checksum_len, digest);
            assert_eq!(
                result.get(),
                SUM_LENGTH,
                "redo must return SUM_LENGTH for file_len={file_len}"
            );
        }
    }

    #[test]
    fn sum_length_phase_toggle_produces_different_layouts() {
        // Same file, two checksum requests: block layout is identical, only the
        // strong checksum length differs.
        use crate::block_size::SHORT_SUM_LENGTH;

        let phase1 = calculate_signature_layout(params(1024, None, 31, SHORT_SUM_LENGTH))
            .expect("phase1 layout");
        let phase2 =
            calculate_signature_layout(params(1024, None, 31, SUM_LENGTH)).expect("phase2 layout");

        assert_eq!(phase1.block_length(), phase2.block_length());
        assert_eq!(phase1.block_count(), phase2.block_count());
        assert_eq!(phase1.remainder(), phase2.remainder());

        assert_eq!(phase1.strong_sum_length().get(), SHORT_SUM_LENGTH);
        assert_eq!(phase2.strong_sum_length().get(), SUM_LENGTH);
        assert!(phase1.strong_sum_length() < phase2.strong_sum_length());
    }

    #[test]
    fn signature_layout_params_accessors() {
        let p = params(100, Some(512), 31, 8);
        assert_eq!(p.file_length(), 100);
        assert_eq!(p.forced_block_length().unwrap().get(), 512);
        assert_eq!(p.protocol().as_u8(), 31);
        assert_eq!(p.checksum_length().get(), 8);
    }

    #[test]
    fn signature_layout_from_raw_parts() {
        let layout = SignatureLayout::from_raw_parts(
            NonZeroU32::new(1024).unwrap(),
            100,
            10,
            NonZeroU8::new(16).unwrap(),
        );
        assert_eq!(layout.block_length().get(), 1024);
        assert_eq!(layout.remainder(), 100);
        assert_eq!(layout.block_count(), 10);
        assert_eq!(layout.strong_sum_length().get(), 16);
    }
}
