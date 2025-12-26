//! crates/checksums/src/parallel.rs
//!
//! Parallel checksum computation utilities using rayon.
//!
//! This module provides parallel versions of checksum operations,
//! enabling concurrent digest computation for improved performance
//! when processing multiple data blocks.

use rayon::prelude::*;

use crate::rolling::RollingChecksum;
use crate::strong::StrongDigest;

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
/// ```ignore
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
/// ```ignore
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
/// ```ignore
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

/// Result of computing both rolling and strong checksums for a block.
#[derive(Clone, Debug)]
pub struct BlockSignature<D> {
    /// The rolling checksum (weak hash) for fast matching.
    pub rolling: u32,
    /// The strong digest for collision verification.
    pub strong: D,
}

/// Computes both rolling and strong checksums for multiple blocks in parallel.
///
/// This is the primary function for building block signatures during
/// delta detection. Each block gets both a rolling checksum (for fast
/// hash table lookup) and a strong digest (for collision verification).
///
/// # Example
///
/// ```ignore
/// use checksums::{parallel::compute_block_signatures_parallel, strong::Md5};
///
/// let blocks: Vec<&[u8]> = vec![b"block1", b"block2", b"block3"];
/// let signatures = compute_block_signatures_parallel::<Md5, _>(&blocks);
///
/// for sig in &signatures {
///     println!("Rolling: {:08x}, Strong: {:?}", sig.rolling, sig.strong.as_ref());
/// }
/// ```
pub fn compute_block_signatures_parallel<D, T>(blocks: &[T]) -> Vec<BlockSignature<D::Digest>>
where
    D: StrongDigest + Send,
    D::Seed: Default + Clone + Send + Sync,
    D::Digest: Send,
    T: AsRef<[u8]> + Sync,
{
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
/// ```ignore
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
/// ```ignore
/// use checksums::parallel::filter_blocks_by_checksum;
///
/// let blocks: Vec<&[u8]> = vec![b"block1", b"block2", b"block3"];
/// let target_mask = 0xFFFF0000u32;
/// let target_value = 0x12340000u32;
///
/// // Find blocks whose upper 16 bits match
/// let matches = filter_blocks_by_checksum(&blocks, |checksum| {
///     (checksum & target_mask) == target_value
/// });
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strong::{Md5, Sha256, Xxh64};

    fn create_test_blocks() -> Vec<Vec<u8>> {
        vec![
            b"block one content".to_vec(),
            b"block two content".to_vec(),
            b"block three content".to_vec(),
            b"block four content".to_vec(),
        ]
    }

    #[test]
    fn compute_digests_parallel_matches_sequential() {
        let blocks = create_test_blocks();

        let parallel_digests = compute_digests_parallel::<Md5, _>(&blocks);

        let sequential_digests: Vec<_> = blocks.iter().map(|b| Md5::digest(b.as_slice())).collect();

        assert_eq!(parallel_digests.len(), sequential_digests.len());
        for (p, s) in parallel_digests.iter().zip(sequential_digests.iter()) {
            assert_eq!(p.as_ref(), s.as_ref());
        }
    }

    #[test]
    fn compute_digests_with_seed_parallel_works() {
        let blocks = create_test_blocks();
        let seed = 12345u64;

        let parallel_digests = compute_digests_with_seed_parallel::<Xxh64, _>(&blocks, seed);

        let sequential_digests: Vec<_> = blocks
            .iter()
            .map(|b| Xxh64::digest(seed, b.as_slice()))
            .collect();

        assert_eq!(parallel_digests.len(), sequential_digests.len());
        for (p, s) in parallel_digests.iter().zip(sequential_digests.iter()) {
            assert_eq!(p.as_ref(), s.as_ref());
        }
    }

    #[test]
    fn compute_rolling_checksums_parallel_matches_sequential() {
        let blocks = create_test_blocks();

        let parallel_checksums = compute_rolling_checksums_parallel(&blocks);

        let sequential_checksums: Vec<_> = blocks
            .iter()
            .map(|b| {
                let mut checksum = RollingChecksum::new();
                checksum.update(b.as_slice());
                checksum.value()
            })
            .collect();

        assert_eq!(parallel_checksums, sequential_checksums);
    }

    #[test]
    fn compute_block_signatures_parallel_works() {
        let blocks = create_test_blocks();

        let signatures = compute_block_signatures_parallel::<Sha256, _>(&blocks);

        assert_eq!(signatures.len(), blocks.len());

        // Verify each signature matches sequential computation
        for (i, sig) in signatures.iter().enumerate() {
            let mut rolling = RollingChecksum::new();
            rolling.update(blocks[i].as_slice());
            assert_eq!(sig.rolling, rolling.value());

            let strong = Sha256::digest(blocks[i].as_slice());
            assert_eq!(sig.strong.as_ref(), strong.as_ref());
        }
    }

    #[test]
    fn process_blocks_parallel_with_custom_function() {
        let blocks = create_test_blocks();

        let results: Vec<(u32, usize)> = process_blocks_parallel(&blocks, |block| {
            let mut checksum = RollingChecksum::new();
            checksum.update(block);
            (checksum.value(), block.len())
        });

        assert_eq!(results.len(), blocks.len());

        for (i, (checksum, len)) in results.iter().enumerate() {
            assert_eq!(*len, blocks[i].len());

            let mut expected = RollingChecksum::new();
            expected.update(blocks[i].as_slice());
            assert_eq!(*checksum, expected.value());
        }
    }

    #[test]
    fn filter_blocks_by_checksum_works() {
        let blocks = create_test_blocks();

        // Get all checksums first
        let checksums = compute_rolling_checksums_parallel(&blocks);

        // Filter for blocks whose checksum has bit 0 set
        let matching_indices = filter_blocks_by_checksum(&blocks, |c| c & 1 == 1);

        // Verify results
        for &i in &matching_indices {
            assert!(checksums[i] & 1 == 1);
        }

        // Verify non-matching blocks
        for (i, &c) in checksums.iter().enumerate() {
            if c & 1 == 0 {
                assert!(!matching_indices.contains(&i));
            }
        }
    }

    #[test]
    fn parallel_handles_empty_input() {
        let blocks: Vec<Vec<u8>> = vec![];

        let digests = compute_digests_parallel::<Md5, _>(&blocks);
        assert!(digests.is_empty());

        let checksums = compute_rolling_checksums_parallel(&blocks);
        assert!(checksums.is_empty());

        let signatures = compute_block_signatures_parallel::<Md5, _>(&blocks);
        assert!(signatures.is_empty());
    }

    #[test]
    fn parallel_handles_single_block() {
        let blocks = vec![b"single block".to_vec()];

        let digests = compute_digests_parallel::<Md5, _>(&blocks);
        assert_eq!(digests.len(), 1);

        let expected = Md5::digest(b"single block");
        assert_eq!(digests[0].as_ref(), expected.as_ref());
    }
}
