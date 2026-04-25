//! Parallel block-level checksum operations using rayon.
//!
//! Provides parallel computation of rolling checksums, strong digests,
//! and combined block signatures for in-memory data blocks.

use rayon::prelude::*;

use crate::rolling::RollingChecksum;
use crate::strong::StrongDigest;

use super::types::{BlockSignature, PARALLEL_BLOCK_THRESHOLD};

/// Computes strong digests for multiple data blocks, automatically choosing
/// parallel or sequential execution based on the block count.
///
/// When `blocks.len() >= PARALLEL_BLOCK_THRESHOLD`, computation is performed
/// in parallel using rayon. Otherwise, sequential iteration is used to avoid
/// thread-pool overhead.
///
/// # Example
///
/// ```rust
/// use checksums::parallel::compute_digests_auto;
/// use checksums::strong::Md5;
///
/// let blocks: Vec<&[u8]> = vec![b"block1", b"block2", b"block3"];
/// let digests = compute_digests_auto::<Md5, _>(&blocks);
/// assert_eq!(digests.len(), 3);
/// ```
pub fn compute_digests_auto<D, T>(blocks: &[T]) -> Vec<D::Digest>
where
    D: StrongDigest + Send,
    D::Seed: Default + Clone + Send + Sync,
    D::Digest: Send,
    T: AsRef<[u8]> + Sync,
{
    if blocks.len() >= PARALLEL_BLOCK_THRESHOLD {
        compute_digests_parallel::<D, T>(blocks)
    } else {
        blocks
            .iter()
            .map(|block| D::digest(block.as_ref()))
            .collect()
    }
}

/// Computes block signatures (rolling + strong checksums) for multiple blocks,
/// automatically choosing parallel or sequential execution based on block count.
///
/// When `blocks.len() >= PARALLEL_BLOCK_THRESHOLD`, computation is performed
/// in parallel using rayon. Otherwise, sequential iteration is used.
///
/// # Example
///
/// ```rust
/// use checksums::parallel::{compute_block_signatures_auto, BlockSignature};
/// use checksums::strong::Sha256;
///
/// let blocks: Vec<&[u8]> = vec![b"block1", b"block2", b"block3"];
/// let signatures = compute_block_signatures_auto::<Sha256, _>(&blocks);
/// assert_eq!(signatures.len(), 3);
/// ```
pub fn compute_block_signatures_auto<D, T>(blocks: &[T]) -> Vec<BlockSignature<D::Digest>>
where
    D: StrongDigest + Send,
    D::Seed: Default + Clone + Send + Sync,
    D::Digest: Send,
    T: AsRef<[u8]> + Sync,
{
    if blocks.len() >= PARALLEL_BLOCK_THRESHOLD {
        compute_block_signatures_parallel::<D, T>(blocks)
    } else {
        blocks
            .iter()
            .map(|block| {
                let data = block.as_ref();
                let mut rolling = RollingChecksum::new();
                rolling.update(data);
                BlockSignature {
                    rolling: rolling.value(),
                    strong: D::digest(data),
                }
            })
            .collect()
    }
}

/// Computes strong digests for multiple data blocks in parallel.
///
/// Each block is hashed independently using the specified digest algorithm.
/// This is useful for computing file block signatures during delta detection.
///
/// # Type Parameters
///
/// - `D`: The digest algorithm implementing [`StrongDigest`]
///
/// # Example
///
/// ```
/// use checksums::{parallel::compute_digests_parallel, strong::Md5};
///
/// let blocks: Vec<&[u8]> = vec![b"block1", b"block2", b"block3"];
/// let digests = compute_digests_parallel::<Md5, _>(&blocks);
/// assert_eq!(digests.len(), 3);
/// ```
pub fn compute_digests_parallel<D, T>(blocks: &[T]) -> Vec<D::Digest>
where
    D: StrongDigest + Send,
    D::Seed: Default + Clone + Send + Sync,
    D::Digest: Send,
    T: AsRef<[u8]> + Sync,
{
    // Ordering: digests must correspond 1:1 with blocks by position for signature assembly.
    // Preserved by par_iter().map().collect() (rayon preserves index order).
    // Violation produces wrong strong checksums per block, breaking delta matching.
    blocks
        .par_iter()
        .map(|block| D::digest(block.as_ref()))
        .collect()
}

/// Computes strong digests with a seed for multiple data blocks in parallel.
///
/// Similar to [`compute_digests_parallel`] but allows specifying a seed
/// value for algorithms that support seeded hashing (e.g., XXH64).
///
/// # Example
///
/// ```
/// use checksums::{parallel::compute_digests_with_seed_parallel, strong::Xxh64};
///
/// let blocks: Vec<&[u8]> = vec![b"block1", b"block2", b"block3"];
/// let seed = 42u64;
/// let digests = compute_digests_with_seed_parallel::<Xxh64, _>(&blocks, seed);
/// assert_eq!(digests.len(), 3);
/// ```
pub fn compute_digests_with_seed_parallel<D, T>(blocks: &[T], seed: D::Seed) -> Vec<D::Digest>
where
    D: StrongDigest + Send,
    D::Seed: Clone + Send + Sync,
    D::Digest: Send,
    T: AsRef<[u8]> + Sync,
{
    blocks
        .par_iter()
        .map(|block| D::digest_with_seed(seed.clone(), block.as_ref()))
        .collect()
}

/// Computes rolling checksums for multiple data blocks in parallel.
///
/// Each block gets its own rolling checksum computed independently.
/// Returns the packed 32-bit checksum values suitable for hash table lookups.
///
/// # Example
///
/// ```
/// use checksums::parallel::compute_rolling_checksums_parallel;
///
/// let blocks: Vec<&[u8]> = vec![b"block1", b"block2", b"block3"];
/// let checksums = compute_rolling_checksums_parallel(&blocks);
/// assert_eq!(checksums.len(), 3);
/// ```
pub fn compute_rolling_checksums_parallel<T>(blocks: &[T]) -> Vec<u32>
where
    T: AsRef<[u8]> + Sync,
{
    blocks
        .par_iter()
        .map(|block| {
            let mut checksum = RollingChecksum::new();
            checksum.update(block.as_ref());
            checksum.value()
        })
        .collect()
}

/// Computes both rolling and strong checksums for multiple blocks in parallel.
///
/// This is the primary function for building block signatures during
/// delta detection. Each block gets both a rolling checksum (for fast
/// hash table lookup) and a strong digest (for collision verification).
///
/// # Example
///
/// ```
/// use checksums::{parallel::compute_block_signatures_parallel, strong::Md5};
///
/// let blocks: Vec<&[u8]> = vec![b"block1", b"block2", b"block3"];
/// let signatures = compute_block_signatures_parallel::<Md5, _>(&blocks);
/// assert_eq!(signatures.len(), 3);
///
/// for sig in &signatures {
///     assert_ne!(sig.rolling, 0);
/// }
/// ```
pub fn compute_block_signatures_parallel<D, T>(blocks: &[T]) -> Vec<BlockSignature<D::Digest>>
where
    D: StrongDigest + Send,
    D::Seed: Default + Clone + Send + Sync,
    D::Digest: Send,
    T: AsRef<[u8]> + Sync,
{
    // Ordering: signatures must match block positions for delta reconstruction.
    // Preserved by par_iter().map().collect() (rayon preserves index order).
    // Violation pairs wrong rolling+strong checksums with block offsets.
    blocks
        .par_iter()
        .map(|block| {
            let data = block.as_ref();

            let mut rolling = RollingChecksum::new();
            rolling.update(data);

            BlockSignature {
                rolling: rolling.value(),
                strong: D::digest(data),
            }
        })
        .collect()
}

/// Processes data blocks in parallel, applying a custom function to each.
///
/// This is a generic parallel processor for custom checksum operations.
/// Use this when the built-in functions don't match your needs.
///
/// # Example
///
/// ```
/// use checksums::parallel::process_blocks_parallel;
/// use checksums::RollingChecksum;
///
/// let blocks: Vec<&[u8]> = vec![b"block1", b"block2", b"block3"];
///
/// // Custom processor that computes rolling checksum and length
/// let results: Vec<(u32, usize)> = process_blocks_parallel(&blocks, |block| {
///     let mut checksum = RollingChecksum::new();
///     checksum.update(block);
///     (checksum.value(), block.len())
/// });
/// assert_eq!(results.len(), 3);
/// assert_eq!(results[0].1, 6); // "block1" is 6 bytes
/// ```
pub fn process_blocks_parallel<T, R, F>(blocks: &[T], f: F) -> Vec<R>
where
    T: AsRef<[u8]> + Sync,
    R: Send,
    F: Fn(&[u8]) -> R + Sync + Send,
{
    blocks.par_iter().map(|block| f(block.as_ref())).collect()
}

/// Filters blocks in parallel based on their rolling checksum.
///
/// Returns indices of blocks whose rolling checksum matches the predicate.
/// Useful for finding candidate blocks during delta matching.
///
/// # Example
///
/// ```
/// use checksums::parallel::filter_blocks_by_checksum;
///
/// let blocks: Vec<&[u8]> = vec![b"block1", b"block2", b"block3"];
///
/// // Find blocks whose checksum is non-zero (all should match)
/// let matches = filter_blocks_by_checksum(&blocks, |checksum| checksum != 0);
/// assert_eq!(matches.len(), 3);
///
/// // Find blocks matching a specific mask - may return 0..3 results
/// let narrow = filter_blocks_by_checksum(&blocks, |checksum| {
///     (checksum & 0xFFFF0000) == 0
/// });
/// assert!(narrow.len() <= 3);
/// ```
pub fn filter_blocks_by_checksum<T, F>(blocks: &[T], predicate: F) -> Vec<usize>
where
    T: AsRef<[u8]> + Sync,
    F: Fn(u32) -> bool + Sync + Send,
{
    blocks
        .par_iter()
        .enumerate()
        .filter_map(|(i, block)| {
            let mut checksum = RollingChecksum::new();
            checksum.update(block.as_ref());
            if predicate(checksum.value()) {
                Some(i)
            } else {
                None
            }
        })
        .collect()
}
