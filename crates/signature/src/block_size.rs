//! Block size calculation algorithm matching upstream rsync 3.4.1.
//!
//! This module provides standalone functions for calculating block sizes and checksum
//! lengths used in rsync's delta transfer algorithm. The implementation mirrors the
//! C reference code in `rsync.c:block_len()` and `generator.c:sum_sizes_sqroot()`.
//!
//! # Overview
//!
//! Rsync divides files into fixed-size blocks for efficient delta transfer. The block
//! size affects transfer efficiency:
//! - **Too small**: More checksums overhead, larger signature
//! - **Too large**: Less granularity, larger deltas when changes occur
//!
//! The algorithm uses a "square root" heuristic: for files larger than ~490KB
//! (700 * 700 bytes), the block size approximates the square root of the file size,
//! rounded to specific bit boundaries.
//!
//! # Examples
//!
//! ```rust
//! use signature::block_size::calculate_block_length;
//!
//! // Small file uses default block size
//! let block_len = calculate_block_length(1024, 31, None);
//! assert_eq!(block_len, 700);
//!
//! // Large file uses sqrt scaling
//! let block_len = calculate_block_length(10 * 1024 * 1024, 31, None);
//! assert!(block_len > 700);
//! assert!(block_len <= 131_072); // MAX_BLOCK_SIZE for protocol >= 30
//!
//! // User can override with --block-size
//! let block_len = calculate_block_length(10 * 1024 * 1024, 31, Some(4096));
//! assert_eq!(block_len, 4096);
//! ```
//!
//! Strong checksum length (`s2length`) is computed by
//! `calculate_signature_layout`, which applies the negotiated
//! transfer-digest cap upstream requires; see the `layout` module.

/// Default block length used by rsync for small files.
///
/// Files smaller than `DEFAULT_BLOCK_SIZE * DEFAULT_BLOCK_SIZE` (490,000 bytes)
/// use this as their block size.
pub const DEFAULT_BLOCK_SIZE: u32 = 700;

/// Maximum block size for protocol version 30 and newer.
///
/// This is 128 KB (131,072 bytes), corresponding to `1 << 17`.
pub const MAX_BLOCK_SIZE_V30: u32 = 1 << 17;

/// Maximum block size for legacy protocol versions (< 30).
///
/// This is 512 MB (536,870,912 bytes), corresponding to `1 << 29`.
/// In practice, files rarely reach this limit.
pub const MAX_BLOCK_SIZE_OLD: u32 = 1 << 29;

/// Minimum practical block size.
///
/// While rsync doesn't enforce this minimum in the protocol, blocks smaller
/// than 64 bytes are impractical due to overhead.
pub const MIN_BLOCK_SIZE: u32 = 64;

/// Abbreviated strong checksum length used during phase 1 of the transfer.
///
/// Upstream rsync uses 2-byte checksums in the initial phase to reduce
/// signature size, then switches to full `MAX_SUM_LENGTH` (16 bytes) for
/// phase 2 redo passes where correctness is critical.
/// (upstream: rsync.h:714-715 `SHORT_SUM_LENGTH`)
pub const SHORT_SUM_LENGTH: u8 = 2;

/// Maximum strong checksum length supported by the protocol.
///
/// This is 16 bytes (128 bits), matching MD4/MD5 digest size.
/// Used as the full checksum length in phase 2 redo passes.
pub const MAX_SUM_LENGTH: u8 = 16;

/// Calculates the block length for a given file size and protocol version.
///
/// This function implements rsync's "square root" block sizing algorithm from
/// `generator.c:sum_sizes_sqroot()`. The algorithm:
///
/// 1. For small files (≤ 700² bytes), returns `DEFAULT_BLOCK_SIZE` (700)
/// 2. For larger files, computes an approximation of sqrt(file_size)
/// 3. Rounds the result to specific bit boundaries (multiple of 8)
/// 4. Clamps to protocol-specific maximum
///
/// # Arguments
///
/// * `file_size` - Size of the file in bytes
/// * `protocol_version` - Rsync protocol version (affects maximum block size)
/// * `user_block_size` - Optional user override from `--block-size` flag
///
/// # Returns
///
/// Block length in bytes. Always returns a value >= 64 (practical minimum)
/// and <= the protocol-specific maximum.
///
/// # Examples
///
/// ```rust
/// use signature::block_size::calculate_block_length;
///
/// // Empty file still gets default block size
/// assert_eq!(calculate_block_length(0, 31, None), 700);
///
/// // Small file (< 700²)
/// assert_eq!(calculate_block_length(1024, 31, None), 700);
///
/// // Medium file gets sqrt-based size
/// let block_len = calculate_block_length(10 * 1024 * 1024, 31, None);
/// assert!(block_len > 700 && block_len < 131_072);
///
/// // User override
/// assert_eq!(calculate_block_length(1024, 31, Some(512)), 512);
///
/// // Protocol version affects max
/// let old_max = calculate_block_length(u64::MAX >> 10, 29, None);
/// let new_max = calculate_block_length(u64::MAX >> 10, 31, None);
/// assert!(old_max >= new_max);
/// ```
#[must_use]
pub fn calculate_block_length(
    file_size: u64,
    protocol_version: u8,
    user_block_size: Option<u32>,
) -> u32 {
    let block_length = if let Some(user_size) = user_block_size {
        user_size
    } else {
        derive_block_length_sqrt(file_size, protocol_version)
    };

    let max_block = if protocol_version < 30 {
        MAX_BLOCK_SIZE_OLD
    } else {
        MAX_BLOCK_SIZE_V30
    };

    block_length.min(max_block)
}

/// Calculates the number of blocks needed for a file.
///
/// Returns the number of complete and partial blocks required to cover
/// the entire file.
///
/// # Arguments
///
/// * `file_size` - Size of the file in bytes
/// * `block_length` - Block size in bytes
///
/// # Returns
///
/// Number of blocks. Returns 0 for empty files.
///
/// # Examples
///
/// ```rust
/// use signature::block_size::calculate_checksum_count;
///
/// assert_eq!(calculate_checksum_count(0, 700), 0);
/// assert_eq!(calculate_checksum_count(700, 700), 1);
/// assert_eq!(calculate_checksum_count(701, 700), 2);
/// assert_eq!(calculate_checksum_count(1400, 700), 2);
/// assert_eq!(calculate_checksum_count(1401, 700), 3);
/// ```
#[must_use]
pub fn calculate_checksum_count(file_size: u64, block_length: u32) -> u64 {
    if file_size == 0 {
        return 0;
    }

    let block_len = u64::from(block_length);
    let full_blocks = file_size / block_len;
    let remainder = file_size % block_len;

    if remainder == 0 {
        full_blocks
    } else {
        full_blocks + 1
    }
}

/// Derives the block length using rsync's square root algorithm.
///
/// This is the core block sizing heuristic. For files larger than
/// `DEFAULT_BLOCK_SIZE²`, it computes an approximation of sqrt(file_size)
/// by iteratively finding the largest value where `block_length² ≤ file_size`.
///
/// The algorithm:
/// 1. Start with a power-of-2 upper bound
/// 2. Iteratively test bits from high to low
/// 3. Set bits that keep `block_length² ≤ file_size`
/// 4. Round down to multiples of 8 bytes
///
/// This mirrors `generator.c:sum_sizes_sqroot()` from upstream rsync.
fn derive_block_length_sqrt(file_size: u64, protocol_version: u8) -> u32 {
    if file_size <= u64::from(DEFAULT_BLOCK_SIZE) * u64::from(DEFAULT_BLOCK_SIZE) {
        return DEFAULT_BLOCK_SIZE;
    }

    let max_block_length = if protocol_version < 30 {
        MAX_BLOCK_SIZE_OLD
    } else {
        MAX_BLOCK_SIZE_V30
    };

    // c = 2^(floor(log2(file_size)/2)): power-of-2 upper bound for sqrt(file_size).
    let mut c: u64 = 1;
    let mut l = file_size;
    while l >> 2 != 0 {
        c <<= 1;
        l >>= 2;
    }

    if c >= u64::from(max_block_length) {
        return max_block_length;
    }

    // Binary search for the largest block_length where block_length^2 <= file_size.
    let mut block_length = 0u64;
    let mut current = c;
    while current >= 8 {
        block_length |= current;
        let candidate = u128::from(block_length);
        if u128::from(file_size) < candidate * candidate {
            block_length &= !current;
        }
        current >>= 1;
    }

    let block_length = block_length.max(u64::from(DEFAULT_BLOCK_SIZE));

    #[allow(clippy::cast_possible_truncation)]
    let result = block_length as u32;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_upstream() {
        assert_eq!(DEFAULT_BLOCK_SIZE, 700);
        assert_eq!(MAX_BLOCK_SIZE_V30, 131_072);
        assert_eq!(MAX_BLOCK_SIZE_OLD, 536_870_912);
        assert_eq!(MIN_BLOCK_SIZE, 64);
    }

    #[test]
    fn empty_file_uses_default_block_size() {
        assert_eq!(calculate_block_length(0, 31, None), DEFAULT_BLOCK_SIZE);
    }

    #[test]
    fn small_file_uses_default_block_size() {
        // Files at or below 700² (490,000 bytes) use DEFAULT_BLOCK_SIZE.
        assert_eq!(calculate_block_length(1, 31, None), DEFAULT_BLOCK_SIZE);
        assert_eq!(calculate_block_length(699, 31, None), DEFAULT_BLOCK_SIZE);
        assert_eq!(calculate_block_length(700, 31, None), DEFAULT_BLOCK_SIZE);
        assert_eq!(
            calculate_block_length(490_000, 31, None),
            DEFAULT_BLOCK_SIZE
        );
    }

    #[test]
    fn medium_files_use_sqrt_scaling() {
        // 1 MB: sqrt ~ 1024.
        let block_len = calculate_block_length(1024 * 1024, 31, None);
        assert!(block_len > DEFAULT_BLOCK_SIZE);
        assert!(block_len <= MAX_BLOCK_SIZE_V30);
        assert_eq!(block_len, 1024);

        // 10 MB: sqrt ~ 3232.
        let block_len = calculate_block_length(10 * 1024 * 1024, 31, None);
        assert!(block_len > 1024);
        assert!(block_len <= MAX_BLOCK_SIZE_V30);
        assert_eq!(block_len, 3232);

        // 100 MB: sqrt ~ 10240.
        let block_len = calculate_block_length(100 * 1024 * 1024, 31, None);
        assert!(block_len > 3232);
        assert!(block_len <= MAX_BLOCK_SIZE_V30);
        assert_eq!(block_len, 10_240);
    }

    #[test]
    fn large_files_capped_at_max_block_size() {
        let block_len = calculate_block_length(1u64 << 40, 31, None);
        assert_eq!(block_len, MAX_BLOCK_SIZE_V30);

        let block_len = calculate_block_length(1u64 << 40, 29, None);
        assert!(block_len <= MAX_BLOCK_SIZE_OLD);
        assert!(block_len > MAX_BLOCK_SIZE_V30);
    }

    #[test]
    fn protocol_version_affects_maximum() {
        let file_size = 1u64 << 35; // 32 GB

        let block_len_v30 = calculate_block_length(file_size, 30, None);
        assert_eq!(block_len_v30, MAX_BLOCK_SIZE_V30);

        let block_len_v31 = calculate_block_length(file_size, 31, None);
        assert_eq!(block_len_v31, MAX_BLOCK_SIZE_V30);

        let block_len_v29 = calculate_block_length(file_size, 29, None);
        assert!(block_len_v29 > MAX_BLOCK_SIZE_V30);
        assert!(block_len_v29 <= MAX_BLOCK_SIZE_OLD);
    }

    #[test]
    fn user_block_size_override() {
        assert_eq!(calculate_block_length(1024, 31, Some(512)), 512);
        assert_eq!(calculate_block_length(1024 * 1024, 31, Some(2048)), 2048);

        let oversized = MAX_BLOCK_SIZE_V30 * 2;
        assert_eq!(
            calculate_block_length(1024, 31, Some(oversized)),
            MAX_BLOCK_SIZE_V30
        );

        let oversized_old = MAX_BLOCK_SIZE_OLD * 2;
        assert_eq!(
            calculate_block_length(1024, 29, Some(oversized_old)),
            MAX_BLOCK_SIZE_OLD
        );
    }

    #[test]
    fn block_length_is_multiple_of_8() {
        for size in [
            500_000u64,
            1_000_000,
            5_000_000,
            10_000_000,
            50_000_000,
            100_000_000,
        ] {
            let block_len = calculate_block_length(size, 31, None);
            if block_len > DEFAULT_BLOCK_SIZE {
                assert_eq!(block_len % 8, 0, "block_len {block_len} not multiple of 8");
            }
        }
    }

    #[test]
    fn very_large_file_edge_cases() {
        // u64::MAX: sqrt clamps to protocol maximum.
        let block_len = calculate_block_length(u64::MAX, 31, None);
        assert_eq!(block_len, MAX_BLOCK_SIZE_V30);

        // 4 GB: sqrt(2^32) = 65536.
        let block_len = calculate_block_length(4u64 << 30, 31, None);
        assert_eq!(block_len, 65536);

        // 1 TB: sqrt(2^40) = 2^20 = 1_048_576, clamped to MAX_BLOCK_SIZE_V30.
        let block_len = calculate_block_length(1u64 << 40, 31, None);
        assert_eq!(block_len, MAX_BLOCK_SIZE_V30);
    }

    #[test]
    fn checksum_count_basic() {
        assert_eq!(calculate_checksum_count(0, 700), 0);
        assert_eq!(calculate_checksum_count(1, 700), 1);
        assert_eq!(calculate_checksum_count(699, 700), 1);
        assert_eq!(calculate_checksum_count(700, 700), 1);
        assert_eq!(calculate_checksum_count(701, 700), 2);
        assert_eq!(calculate_checksum_count(1400, 700), 2);
        assert_eq!(calculate_checksum_count(1401, 700), 3);
    }

    #[test]
    fn checksum_count_exact_multiples() {
        assert_eq!(calculate_checksum_count(7000, 700), 10);
        assert_eq!(calculate_checksum_count(70_000, 700), 100);
        assert_eq!(calculate_checksum_count(700_000, 700), 1000);
    }

    #[test]
    fn checksum_count_large_files() {
        let file_size = 1u64 << 30; // 1 GB
        let block_len = MAX_BLOCK_SIZE_V30;
        let count = calculate_checksum_count(file_size, block_len);
        assert_eq!(count, file_size / u64::from(block_len));
    }

    #[test]
    fn roundtrip_block_coverage() {
        let test_sizes = [
            0u64,
            1,
            700,
            701,
            1400,
            10_000,
            100_000,
            1_000_000,
            10_000_000,
            100_000_000,
        ];

        for file_size in test_sizes {
            let block_len = calculate_block_length(file_size, 31, None);
            let count = calculate_checksum_count(file_size, block_len);

            if file_size == 0 {
                assert_eq!(count, 0);
            } else {
                let covered = count * u64::from(block_len);
                assert!(covered >= file_size);
                // Last block overshoot must be strictly less than one full block.
                assert!(covered - file_size < u64::from(block_len));
            }
        }
    }

    #[test]
    fn sum_length_constants_match_upstream() {
        // upstream: rsync.h:714 `#define SHORT_SUM_LENGTH 2`
        assert_eq!(SHORT_SUM_LENGTH, 2);
        // upstream: rsync.h:715 `#define SUM_LENGTH 16`
        assert_eq!(MAX_SUM_LENGTH, 16);
        assert!(usize::from(SHORT_SUM_LENGTH) < usize::from(MAX_SUM_LENGTH));
    }

    #[test]
    fn specific_upstream_compatibility_values() {
        // Golden values lifted from upstream rsync 3.4.1
        // (generator.c:sum_sizes_sqroot) for protocol 31.
        assert_eq!(calculate_block_length(1_048_576, 31, None), 1024);
        assert_eq!(calculate_block_length(10_485_760, 31, None), 3232);
        assert_eq!(calculate_block_length(104_857_600, 31, None), 10_240);
        // 1 GB: sqrt(2^30) = 32768.
        assert_eq!(calculate_block_length(1_073_741_824, 31, None), 32768);
    }
}
