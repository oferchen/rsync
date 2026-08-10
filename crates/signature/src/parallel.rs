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
/// ```
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
///
/// assert!(signature.blocks().len() > 0);
/// assert_eq!(signature.total_bytes(), 1_000_000);
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

    let mut extra = [0u8; 1];
    if reader.read(&mut extra)? != 0 {
        return Err(SignatureError::TrailingData { bytes: 1 });
    }

    // Rayon distributes chunks of blocks across threads. Within each chunk, the SIMD
    // batch API processes multiple blocks through multi-lane hashing (e.g., 4-16 lanes
    // for MD4/MD5 on AVX2/AVX-512). This combines thread-level parallelism with
    // data-level parallelism for maximum throughput.
    const BATCH_SIZE: usize = 16;

    // Ordering: block indices must match sequential file offsets for delta reconstruction.
    // Preserved by par_chunks() + enumerate() assigning base_index from chunk position.
    // Violation produces wrong block indices, causing corrupted file reconstruction.
    let blocks: Vec<SignatureBlock> = block_data
        .par_chunks(BATCH_SIZE)
        .enumerate()
        .flat_map_iter(|(chunk_idx, chunk)| {
            let base_index = chunk_idx * BATCH_SIZE;
            let rolling_digests: Vec<RollingDigest> = chunk
                .iter()
                .map(|data| RollingDigest::from_bytes(data))
                .collect();
            let batch_slices: Vec<&[u8]> = chunk.iter().map(|v| v.as_slice()).collect();
            let strong_digests = algorithm.compute_truncated_batch(&batch_slices, strong_len);

            rolling_digests
                .into_iter()
                .zip(strong_digests)
                .enumerate()
                .map(move |(i, (rolling, strong))| {
                    SignatureBlock::new((base_index + i) as u64, rolling, strong)
                })
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
/// ```
/// use signature::{SignatureLayoutParams, calculate_signature_layout, SignatureAlgorithm};
/// use signature::parallel::generate_file_signature_auto;
/// use protocol::ProtocolVersion;
/// use std::io::Cursor;
/// use std::num::NonZeroU8;
///
/// let data = vec![0u8; 512 * 1024]; // 512 KB - above parallel threshold
/// let params = SignatureLayoutParams::new(
///     data.len() as u64,
///     None,
///     ProtocolVersion::NEWEST,
///     NonZeroU8::new(16).unwrap(),
/// );
/// let layout = calculate_signature_layout(params).unwrap();
///
/// // Automatically uses parallel mode for large inputs
/// let signature = generate_file_signature_auto(
///     Cursor::new(data),
///     layout,
///     SignatureAlgorithm::Md4,
/// ).unwrap();
///
/// assert!(signature.blocks().len() > 0);
/// ```
pub fn generate_file_signature_auto<R: Read>(
    reader: R,
    layout: SignatureLayout,
    algorithm: SignatureAlgorithm,
) -> Result<FileSignature, SignatureError> {
    let block_len = u64::from(layout.block_length().get());
    let file_size = if layout.remainder() != 0 {
        // Last block is partial: (count-1) full blocks + remainder
        layout
            .block_count()
            .saturating_sub(1)
            .saturating_mul(block_len)
            .saturating_add(u64::from(layout.remainder()))
    } else {
        layout.block_count().saturating_mul(block_len)
    };

    if file_size >= PARALLEL_THRESHOLD_BYTES {
        generate_file_signature_parallel(reader, layout, algorithm)
    } else {
        crate::generate_file_signature(reader, layout, algorithm)
    }
}

/// Bounded-memory parallel signature generation.
///
/// Reads the basis in windows of at most `window_blocks` blocks and computes
/// each window's rolling and strong checksums across the rayon thread pool
/// (sized to `available_parallelism()` unless `--rayon-threads` overrode the
/// global pool). Unlike [`generate_file_signature_parallel`], which buffers the
/// entire file in memory, this caps resident block buffers at roughly
/// [`WINDOW_BYTE_BUDGET`] bytes regardless of basis size - so large files scale
/// across cores without the peak-RSS cost of a full-file slurp.
///
/// Output is byte-identical to the sequential
/// [`generate_file_signature`](crate::generate_file_signature): per-block
/// rolling and strong sums depend only on that block's bytes (independent of
/// how blocks are batched across threads), block indices are strictly
/// increasing - preserved by `par_chunks().enumerate()` within each window and
/// by appending windows in order - the final block is recorded at the layout
/// remainder length, and trailing data past the expected block count is an
/// error.
///
/// # Errors
///
/// Same as [`generate_file_signature_parallel`]: digest-length mismatch,
/// too-many-blocks, trailing data, or any reader I/O error.
#[cfg_attr(feature = "tracing", instrument(skip(reader), fields(algorithm = ?algorithm, block_count = layout.block_count()), name = "generate_signature_windowed"))]
pub fn generate_file_signature_windowed<R: Read>(
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

    // One window must hold enough blocks to feed every worker a full SIMD
    // batch, but a large block size must not blow up resident memory - so the
    // window is also capped by a fixed byte budget.
    let threads = rayon::current_num_threads().max(1);
    let budget_blocks = (WINDOW_BYTE_BUDGET / block_len.max(1)).max(BATCH_SIZE);
    let window_blocks = threads
        .saturating_mul(BATCH_SIZE)
        .max(BATCH_SIZE)
        .min(budget_blocks)
        .min(expected_blocks_usize);

    let mut blocks: Vec<SignatureBlock> = Vec::with_capacity(expected_blocks_usize);
    let mut total_bytes: u64 = 0;
    let mut window_data: Vec<Vec<u8>> = Vec::with_capacity(window_blocks);
    let mut base_index = 0usize;

    while base_index < expected_blocks_usize {
        let window_end = (base_index + window_blocks).min(expected_blocks_usize);
        window_data.clear();
        for block_index in base_index..window_end {
            let is_last = block_index + 1 == expected_blocks_usize;
            let target_len = if is_last && layout.remainder() != 0 {
                layout.remainder() as usize
            } else {
                block_len
            };
            let mut buf = vec![0u8; target_len];
            reader.read_exact(&mut buf)?;
            total_bytes = total_bytes.saturating_add(target_len as u64);
            window_data.push(buf);
        }

        // Compute the window's blocks across cores; chunk position assigns the
        // global block index, so collect() yields them in order (mirrors
        // generate_file_signature_parallel).
        let window_out: Vec<SignatureBlock> = window_data
            .par_chunks(BATCH_SIZE)
            .enumerate()
            .flat_map_iter(|(chunk_idx, chunk)| {
                let chunk_base = base_index + chunk_idx * BATCH_SIZE;
                let rolling: Vec<RollingDigest> =
                    chunk.iter().map(|d| RollingDigest::from_bytes(d)).collect();
                let slices: Vec<&[u8]> = chunk.iter().map(|v| v.as_slice()).collect();
                let strong = algorithm.compute_truncated_batch(&slices, strong_len);
                rolling
                    .into_iter()
                    .zip(strong)
                    .enumerate()
                    .map(move |(i, (r, s))| SignatureBlock::new((chunk_base + i) as u64, r, s))
            })
            .collect();
        blocks.extend(window_out);
        base_index = window_end;
    }

    let mut extra = [0u8; 1];
    if reader.read(&mut extra)? != 0 {
        return Err(SignatureError::TrailingData { bytes: 1 });
    }

    Ok(FileSignature::new(layout, blocks, total_bytes))
}

/// SIMD batch width shared by the parallel signature generators: the number of
/// blocks `compute_truncated_batch` hashes per multi-lane call.
const BATCH_SIZE: usize = 16;

/// Upper bound on the resident block buffers held by
/// [`generate_file_signature_windowed`] for a single window, irrespective of
/// core count or block size. Caps peak RSS for large-basis parallel signatures.
const WINDOW_BYTE_BUDGET: usize = 64 * 1024 * 1024;

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

    /// Deterministic, non-trivial bytes so each block hashes distinctly.
    fn fill(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn windowed_matches_sequential_across_sizes_and_algorithms() {
        // Spans: empty, sub-block, exact single block, several blocks, a
        // non-block-multiple (exercises the short final block), and >256 KB
        // (the size where the production path would prefer parallel). The
        // number of internal windows varies with the host core count, but the
        // output must be byte-identical to the sequential generator regardless.
        let sizes = [0u64, 7, 1024, 4096, 5000, 64 * 1024 + 123, 300 * 1024 + 7];
        let algorithm = SignatureAlgorithm::Md4;
        for &size in &sizes {
            let data = fill(size as usize);
            let sig_layout = layout(size, 16);
            let sequential =
                crate::generate_file_signature(Cursor::new(data.clone()), sig_layout, algorithm)
                    .expect("sequential signature");
            let windowed =
                generate_file_signature_windowed(Cursor::new(data), sig_layout, algorithm)
                    .expect("windowed signature");
            assert_eq!(
                sequential, windowed,
                "windowed signature diverged from sequential at size={size}",
            );
        }
    }

    #[test]
    fn windowed_rejects_trailing_data() {
        // The layout describes 1024 bytes but the reader yields more; the
        // trailing-data guard must fire exactly as the sequential path does.
        let sig_layout = layout(1024, 16);
        let oversized = fill(1024 + 16);
        let err = generate_file_signature_windowed(
            Cursor::new(oversized),
            sig_layout,
            SignatureAlgorithm::Md4,
        )
        .expect_err("trailing data must be rejected");
        assert!(matches!(err, SignatureError::TrailingData { .. }));
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

        let sequential =
            crate::generate_file_signature(Cursor::new(data.clone()), sig_layout, algorithm)
                .expect("sequential signature");

        let parallel = generate_file_signature_parallel(Cursor::new(data), sig_layout, algorithm)
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
        assert!(matches!(error, SignatureError::DigestLengthMismatch { .. }));
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
    #[allow(clippy::assertions_on_constants)]
    fn parallel_threshold_constant_is_reasonable() {
        // Lower bound: at least a few blocks. Upper bound: still small enough
        // that realistic transfers exercise the parallel path.
        assert!(PARALLEL_THRESHOLD_BYTES >= 64 * 1024);
        assert!(PARALLEL_THRESHOLD_BYTES <= 1024 * 1024);
    }
}
