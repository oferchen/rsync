#![allow(clippy::module_name_repetitions)]

//! # Overview
//!
//! The `delta` module hosts helpers that mirror upstream rsync's block-matching
//! heuristics. The initial implementation exposes [`calculate_signature_layout`],
//! which replicates the "square root" block-size calculation performed in
//! `generator.c:sun_sizes_sqroot()` (rsync 3.4.1). The function accepts the file
//! length, negotiated protocol version, and checksum parameters in order to
//! produce the block size, strong-checksum length, block count, and trailing
//! remainder that make up a file's signature descriptor. Future delta-transfer
//! stages reuse this information when computing rolling and strong checksums for
//! individual blocks.
//!
//! # Design
//!
//! The logic follows the upstream C implementation line-for-line while embracing
//! Rust's type system to surface invalid inputs as structured errors. The helper
//! takes a [`ProtocolVersion`] so call-sites do not need to translate negotiated
//! versions into ad-hoc integers, and the checksum length uses
//! [`NonZeroU8`] to reflect upstream's guarantee that the
//! value never reaches zero. Intermediate calculations use [`u128`] to avoid
//! overflow when comparing `block_length^2` against large file sizes. Overflow
//! scenarios—such as a block count that exceeds [`i32::MAX`]—are reported via
//! [`SignatureLayoutError`], allowing callers to surface canonical diagnostics.
//!
//! # Invariants
//!
//! - The computed block length is never zero and is clamped to the protocol
//!   specific maximum (`MAX_BLOCK_SIZE` for protocol ≥ 30, otherwise
//!   `OLD_MAX_BLOCK_SIZE`).
//! - For small files (≤ `700^2` bytes) the block length is fixed to 700 bytes,
//!   matching upstream behaviour.
//! - Strong checksum lengths honour the negotiated checksum length and the
//!   `BLOCKSUM_BIAS` heuristic from upstream rsync.
//! - Block counts that do not fit in a signed 32-bit integer surface a
//!   [`SignatureLayoutError::BlockCountOverflow`] instead of silently wrapping.
//!
//! # Errors
//!
//! [`calculate_signature_layout`] returns a [`SignatureLayoutError`] when the file
//! length exceeds [`i64::MAX`] or when the resulting block count no longer fits in
//! [`i32::MAX`], mirroring upstream guards.
//!
//! # Examples
//!
//! ```
//! use std::num::{NonZeroU32, NonZeroU8};
//! use engine::delta::{calculate_signature_layout, SignatureLayoutParams};
//! use protocol::ProtocolVersion;
//!
//! let params = SignatureLayoutParams::new(
//!     10 * 1024 * 1024,
//!     None,
//!     ProtocolVersion::NEWEST,
//!     NonZeroU8::new(16).unwrap(),
//! );
//!
//! let layout = calculate_signature_layout(params).expect("valid signature layout");
//!
//! assert_eq!(layout.block_length().get(), 3_232);
//! assert_eq!(layout.block_count(), 3_245);
//! assert_eq!(layout.strong_sum_length().get(), 16);
//! assert_eq!(layout.remainder(), 1_152);
//! ```
//!
//! # See also
//!
//! - [`crate::local_copy`] will integrate these helpers as the delta-transfer
//!   pipeline evolves.
//! - [`crate::delta::generator::DeltaGenerator`] exposes the delta-token
//!   generator that complements the layout helpers.
//! - [`crate::delta::script::apply_delta`] applies delta streams to recreate
//!   target payloads.
//! - Upstream `generator.c::sum_sizes_sqroot()` for the reference C
//!   implementation this module mirrors.

/// Delta-token generation pipeline.
pub mod generator;
/// Signature index used by the delta engine for fast lookups.
pub mod index;
/// Delta script representation and application helpers.
pub mod script;

pub use generator::{DeltaGenerator, generate_delta};
pub use index::DeltaSignatureIndex;
pub use script::{DeltaScript, DeltaToken, apply_delta};

use ::core::fmt;
use ::core::num::{NonZeroU8, NonZeroU32};

use protocol::ProtocolVersion;

/// Default block length used by rsync when files are small.
const BLOCK_SIZE: u32 = 700;
/// Maximum block size supported by protocol versions 30 and newer.
const MAX_BLOCK_SIZE: u32 = 1 << 17;
/// Maximum block size accepted by legacy protocol versions (< 30).
const OLD_MAX_BLOCK_SIZE: u32 = 1 << 29;
/// Bias applied when computing strong checksum lengths for larger files.
const BLOCKSUM_BIAS: i32 = 10;
/// Maximum strong checksum length supported by the protocol.
const SUM_LENGTH: u8 = 16;

/// Parameters describing a file signature computation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignatureLayoutParams {
    file_length: u64,
    forced_block_length: Option<NonZeroU32>,
    protocol: ProtocolVersion,
    checksum_length: NonZeroU8,
}

impl SignatureLayoutParams {
    /// Creates a new descriptor.
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
        }
    }

    /// Returns the file length in bytes.
    #[must_use]
    pub const fn file_length(self) -> u64 {
        self.file_length
    }

    /// Returns the optional caller-specified block length.
    #[must_use]
    pub const fn forced_block_length(self) -> Option<NonZeroU32> {
        self.forced_block_length
    }

    /// Returns the negotiated protocol version.
    #[must_use]
    pub const fn protocol(self) -> ProtocolVersion {
        self.protocol
    }

    /// Returns the negotiated checksum length.
    #[must_use]
    pub const fn checksum_length(self) -> NonZeroU8 {
        self.checksum_length
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

    /// Returns the block length in bytes.
    #[must_use]
    pub const fn block_length(self) -> NonZeroU32 {
        self.block_length
    }

    /// Returns the trailing byte count that does not fill a complete block.
    #[must_use]
    pub const fn remainder(self) -> u32 {
        self.remainder
    }

    /// Returns the number of blocks in the layout.
    #[must_use]
    pub const fn block_count(self) -> u64 {
        self.block_count
    }

    /// Returns the strong checksum length in bytes.
    #[must_use]
    pub const fn strong_sum_length(self) -> NonZeroU8 {
        self.strong_sum_length
    }
}

/// Errors produced when calculating signature layouts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignatureLayoutError {
    /// File length exceeded [`i64::MAX`], which upstream rsync rejects.
    FileTooLarge {
        /// Length in bytes of the file being processed.
        length: u64,
    },
    /// Number of blocks exceeded [`i32::MAX`].
    BlockCountOverflow {
        /// Block length that triggered the overflow.
        block_length: u32,
        /// Block count produced by the sizing heuristic.
        blocks: u64,
    },
}

impl fmt::Display for SignatureLayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SignatureLayoutError::FileTooLarge { length } => {
                write!(f, "file length {length} exceeds i64::MAX")
            }
            SignatureLayoutError::BlockCountOverflow {
                block_length,
                blocks,
            } => {
                write!(
                    f,
                    "block count {blocks} derived from block length {block_length} exceeds i32::MAX"
                )
            }
        }
    }
}

impl std::error::Error for SignatureLayoutError {}

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
    );

    Ok(SignatureLayout {
        block_length: block_length_non_zero,
        remainder,
        block_count,
        strong_sum_length,
    })
}

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

fn derive_strong_sum_length(
    file_length: u64,
    block_length: u32,
    protocol: ProtocolVersion,
    checksum_length: NonZeroU8,
) -> NonZeroU8 {
    if protocol.as_u8() < 27 {
        return checksum_length;
    }

    if checksum_length.get() == SUM_LENGTH {
        return checksum_length;
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

    let mut strong_len = (bias + 1 - 32 + 7) / 8;
    let min_len = i32::from(checksum_length.get());
    if strong_len < min_len {
        strong_len = min_len;
    }
    let max_len = i32::from(SUM_LENGTH);
    if strong_len > max_len {
        strong_len = max_len;
    }

    NonZeroU8::new(strong_len as u8).expect("strong checksum length must be non-zero")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::num::NonZeroU8;
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
}
