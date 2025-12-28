//! crates/signature/src/parallel.rs
//!
//! Parallel file signature generation using rayon.
//!
//! This module provides a parallel version of signature generation that
//! reads all blocks into memory and computes checksums concurrently.
//! Use this for large files where the CPU cost of checksum computation
//! outweighs the memory overhead of buffering blocks.

use std::io::Read;
use std::num::NonZeroUsize;

use rayon::prelude::*;
#[cfg(feature = "tracing")]
use tracing::instrument;

use checksums::RollingDigest;

use crate::algorithm::SignatureAlgorithm;
use crate::block::SignatureBlock;
use crate::file::FileSignature;
use crate::generation::SignatureError;
use crate::layout::SignatureLayout;

/// Generates an rsync-compatible file signature using parallel checksum computation.
///
/// This function reads all blocks into memory first, then computes rolling and
/// strong checksums in parallel using rayon. It's more efficient than the sequential
/// version for files with many blocks, but requires more memory.
///
/// # Memory Usage
///
/// This function allocates approximately `file_size` bytes to buffer all blocks.
/// For very large files or memory-constrained environments, use the sequential
/// [`generate_file_signature`](crate::generate_file_signature) instead.
///
/// # Performance
///
/// Parallel processing provides significant speedup when:
/// - The file has many blocks (typically 8+ blocks)
/// - Strong checksum computation is CPU-intensive (MD4, MD5, SHA1)
/// - Multiple CPU cores are available
///
/// For small files or fast checksums (XXH3), the overhead of parallelization
/// may outweigh the benefits.
///
/// # Errors
///
/// - Returns [`SignatureError::DigestLengthMismatch`] when the layout requests a
///   strong checksum length that exceeds the algorithm's digest width.
/// - Returns [`SignatureError::TooManyBlocks`] if the layout describes more blocks
///   than can be addressed on the current platform.
/// - Propagates any I/O error surfaced by the reader.
///
/// # Example
///
/// ```ignore
/// use signature::{
///     SignatureLayoutParams, calculate_signature_layout,
///     SignatureAlgorithm,
/// };
/// use signature::parallel::generate_file_signature_parallel;
/// use protocol::ProtocolVersion;
/// use std::io::Cursor;
/// use std::num::NonZeroU8;
///
/// let params = SignatureLayoutParams::new(
///     1_000_000,  // 1MB file
///     None,
///     ProtocolVersion::NEWEST,
///     NonZeroU8::new(16).unwrap(),
/// );
/// let layout = calculate_signature_layout(params).expect("layout");
/// let input = Cursor::new(vec![0u8; 1_000_000]);
///
/// let signature = generate_file_signature_parallel(
///     input,
///     layout,
///     SignatureAlgorithm::Md4,
/// ).expect("signature");
/// ```
#[cfg_attr(feature = "tracing", instrument(skip(reader), fields(algorithm = ?algorithm, block_count = layout.block_count()), name = "generate_signature_parallel"))]
pub fn generate_file_signature_parallel<R: Read>(
    mut reader: R,
    layout: SignatureLayout,
    algorithm: SignatureAlgorithm,
) -> Result<FileSignature, SignatureError> {
    let strong_len = usize::from(layout.strong_sum_length().get());
    if strong_len > algorithm.digest_len() {
        return Err(SignatureError::DigestLengthMismatch {
            algorithm,
            requested: NonZeroUsize::new(strong_len)
                .expect("strong digest length requested by layout must be non-zero"),
        });
    }

    let block_len = layout.block_length().get() as usize;
    let expected_blocks = layout.block_count();
    let expected_blocks_usize = usize::try_from(expected_blocks)
        .map_err(|_| SignatureError::TooManyBlocks(expected_blocks))?;

    if expected_blocks_usize == 0 {
        return Ok(FileSignature::new(layout, Vec::new(), 0));
    }

    // Phase 1: Read all blocks into memory
    let mut block_data: Vec<Vec<u8>> = Vec::with_capacity(expected_blocks_usize);
    let mut total_bytes: u64 = 0;

    for index in 0..expected_blocks_usize {
        let is_last = index + 1 == expected_blocks_usize;
        let target_len = if is_last && layout.remainder() != 0 {
            layout.remainder() as usize
        } else {
            block_len
        };

        let mut buffer = vec![0u8; target_len];
        reader.read_exact(&mut buffer)?;
        total_bytes = total_bytes.saturating_add(target_len as u64);
        block_data.push(buffer);
    }

    // Check for trailing data
    let mut extra = [0u8; 1];
    if reader.read(&mut extra)? != 0 {
        return Err(SignatureError::TrailingData { bytes: 1 });
    }

    // Phase 2: Compute checksums in parallel
    let blocks: Vec<SignatureBlock> = block_data
        .par_iter()
        .enumerate()
        .map(|(index, chunk)| {
            let rolling = RollingDigest::from_bytes(chunk);
            let mut strong = algorithm.compute_full(chunk);
            strong.truncate(strong_len);
            SignatureBlock::new(index as u64, rolling, strong)
        })
        .collect();

    Ok(FileSignature::new(layout, blocks, total_bytes))
}

/// Minimum file size (in bytes) where parallel signature generation is beneficial.
///
/// Files smaller than this threshold should use the sequential
/// [`generate_file_signature`](crate::generate_file_signature) function
/// to avoid parallelization overhead.
///
/// This value is based on typical block sizes and the overhead of rayon's
/// work-stealing scheduler. For most systems, parallel processing becomes
/// beneficial when there are at least 4-8 blocks to process.
pub const PARALLEL_THRESHOLD_BYTES: u64 = 256 * 1024; // 256 KB

/// Generates a file signature, automatically choosing parallel or sequential mode.
///
/// This function selects the optimal implementation based on file size:
/// - Files >= [`PARALLEL_THRESHOLD_BYTES`]: Uses parallel checksum computation
/// - Smaller files: Uses sequential processing to avoid overhead
///
/// # Example
///
/// ```ignore
/// use signature::{SignatureLayoutParams, calculate_signature_layout, SignatureAlgorithm};
/// use signature::parallel::generate_file_signature_auto;
/// use protocol::ProtocolVersion;
/// use std::fs::File;
/// use std::num::NonZeroU8;
///
/// let file = File::open("large_file.bin")?;
/// let metadata = file.metadata()?;
/// let params = SignatureLayoutParams::new(
///     metadata.len(),
///     None,
///     ProtocolVersion::NEWEST,
///     NonZeroU8::new(16).unwrap(),
/// );
/// let layout = calculate_signature_layout(params)?;
///
/// // Automatically uses parallel mode for large files
/// let signature = generate_file_signature_auto(file, layout, SignatureAlgorithm::Md4)?;
/// ```
pub fn generate_file_signature_auto<R: Read>(
    reader: R,
    layout: SignatureLayout,
    algorithm: SignatureAlgorithm,
) -> Result<FileSignature, SignatureError> {
    // Calculate file size from layout components
    let block_len = u64::from(layout.block_length().get());
    let file_size = if layout.remainder() != 0 {
        // Last block is partial: (count-1) full blocks + remainder
        layout
            .block_count()
            .saturating_sub(1)
            .saturating_mul(block_len)
            .saturating_add(u64::from(layout.remainder()))
    } else {
        // All blocks are full
        layout.block_count().saturating_mul(block_len)
    };

    if file_size >= PARALLEL_THRESHOLD_BYTES {
        generate_file_signature_parallel(reader, layout, algorithm)
    } else {
        crate::generate_file_signature(reader, layout, algorithm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{SignatureLayoutParams, calculate_signature_layout};
    use checksums::strong::Md5Seed;
    use protocol::ProtocolVersion;
    use std::io::Cursor;
    use std::num::{NonZeroU8, NonZeroU32};

    fn layout(len: u64, checksum_len: u8) -> SignatureLayout {
        calculate_signature_layout(SignatureLayoutParams::new(
            len,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(checksum_len).expect("checksum length"),
        ))
        .expect("layout")
    }

    #[test]
    fn parallel_matches_sequential_single_block() {
        let sig_layout = layout(11, 16);
        let input_seq = Cursor::new(b"hello world".to_vec());
        let input_par = Cursor::new(b"hello world".to_vec());

        let sequential =
            crate::generate_file_signature(input_seq, sig_layout, SignatureAlgorithm::Md4)
                .expect("sequential signature");
        let parallel =
            generate_file_signature_parallel(input_par, sig_layout, SignatureAlgorithm::Md4)
                .expect("parallel signature");

        assert_eq!(sequential.blocks().len(), parallel.blocks().len());
        assert_eq!(sequential.total_bytes(), parallel.total_bytes());

        for (seq_block, par_block) in sequential.blocks().iter().zip(parallel.blocks().iter()) {
            assert_eq!(seq_block.index(), par_block.index());
            assert_eq!(seq_block.rolling(), par_block.rolling());
            assert_eq!(seq_block.strong(), par_block.strong());
        }
    }

    #[test]
    fn parallel_matches_sequential_multiple_blocks() {
        let mut data = vec![0u8; 1_024 + 111];
        for (index, byte) in data.iter_mut().enumerate() {
            *byte = (index % 251) as u8;
        }
        let sig_layout = calculate_signature_layout(SignatureLayoutParams::new(
            data.len() as u64,
            NonZeroU32::new(512),
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        ))
        .expect("layout");

        let algorithm = SignatureAlgorithm::Md5 {
            seed_config: Md5Seed::none(),
        };

        let sequential = crate::generate_file_signature(
            Cursor::new(data.clone()),
            sig_layout,
            algorithm,
        )
        .expect("sequential signature");

        let parallel = generate_file_signature_parallel(
            Cursor::new(data),
            sig_layout,
            algorithm,
        )
        .expect("parallel signature");

        assert_eq!(sequential.blocks().len(), parallel.blocks().len());
        assert_eq!(sequential.total_bytes(), parallel.total_bytes());

        for (seq_block, par_block) in sequential.blocks().iter().zip(parallel.blocks().iter()) {
            assert_eq!(seq_block.index(), par_block.index());
            assert_eq!(seq_block.rolling(), par_block.rolling());
            assert_eq!(seq_block.strong(), par_block.strong());
        }
    }

    #[test]
    fn parallel_handles_empty_file() {
        let sig_layout = layout(0, 16);
        let signature = generate_file_signature_parallel(
            Cursor::new(Vec::new()),
            sig_layout,
            SignatureAlgorithm::Md4,
        )
        .expect("signature");

        assert!(signature.blocks().is_empty());
        assert_eq!(signature.total_bytes(), 0);
    }

    #[test]
    fn parallel_detects_trailing_data() {
        let sig_layout = layout(32, 16);
        let mut data = vec![0u8; 33];
        data[32] = 1;

        let error = generate_file_signature_parallel(
            Cursor::new(data),
            sig_layout,
            SignatureAlgorithm::Md4,
        )
        .expect_err("trailing byte should be detected");

        assert!(matches!(error, SignatureError::TrailingData { .. }));
    }

    #[test]
    fn parallel_reports_digest_length_mismatch() {
        let sig_layout = layout(256, 16);
        let result = generate_file_signature_parallel(
            Cursor::new(vec![0u8; 256]),
            sig_layout,
            SignatureAlgorithm::Xxh64 { seed: 0 },
        );

        let error = result.expect_err("xxh64 cannot provide 16-byte digests");
        assert!(matches!(
            error,
            SignatureError::DigestLengthMismatch { .. }
        ));
    }

    #[test]
    fn auto_uses_sequential_for_small_files() {
        let sig_layout = layout(100, 16);
        let signature = generate_file_signature_auto(
            Cursor::new(vec![0u8; 100]),
            sig_layout,
            SignatureAlgorithm::Md4,
        )
        .expect("signature");

        assert_eq!(signature.total_bytes(), 100);
    }

    #[test]
    fn auto_uses_parallel_for_large_files() {
        let size = PARALLEL_THRESHOLD_BYTES as usize + 1000;
        let sig_layout = calculate_signature_layout(SignatureLayoutParams::new(
            size as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        ))
        .expect("layout");

        let signature = generate_file_signature_auto(
            Cursor::new(vec![0u8; size]),
            sig_layout,
            SignatureAlgorithm::Md4,
        )
        .expect("signature");

        assert_eq!(signature.total_bytes(), size as u64);
    }

    #[test]
    fn parallel_with_sha1() {
        let sig_layout = layout(1000, 16);
        let data = vec![42u8; 1000];

        let sequential = crate::generate_file_signature(
            Cursor::new(data.clone()),
            sig_layout,
            SignatureAlgorithm::Sha1,
        )
        .expect("sequential");

        let parallel = generate_file_signature_parallel(
            Cursor::new(data),
            sig_layout,
            SignatureAlgorithm::Sha1,
        )
        .expect("parallel");

        assert_eq!(sequential.blocks().len(), parallel.blocks().len());
        for (seq, par) in sequential.blocks().iter().zip(parallel.blocks().iter()) {
            assert_eq!(seq.strong(), par.strong());
        }
    }

    #[test]
    fn parallel_with_xxh3() {
        let sig_layout = layout(500, 8);
        let data = vec![0xAB; 500];

        let sequential = crate::generate_file_signature(
            Cursor::new(data.clone()),
            sig_layout,
            SignatureAlgorithm::Xxh3 { seed: 12345 },
        )
        .expect("sequential");

        let parallel = generate_file_signature_parallel(
            Cursor::new(data),
            sig_layout,
            SignatureAlgorithm::Xxh3 { seed: 12345 },
        )
        .expect("parallel");

        assert_eq!(sequential.blocks().len(), parallel.blocks().len());
        for (seq, par) in sequential.blocks().iter().zip(parallel.blocks().iter()) {
            assert_eq!(seq.rolling(), par.rolling());
            assert_eq!(seq.strong(), par.strong());
        }
    }

    #[test]
    fn parallel_threshold_constant_is_reasonable() {
        // Threshold should be at least a few blocks worth
        assert!(PARALLEL_THRESHOLD_BYTES >= 64 * 1024);
        // But not so large that we never use parallel mode
        assert!(PARALLEL_THRESHOLD_BYTES <= 1024 * 1024);
    }
}
