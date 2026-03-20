//! Parallel checksum computation utilities using rayon.
//!
//! This module provides parallel versions of checksum operations,
//! enabling concurrent digest computation for improved performance
//! when processing multiple data blocks or files.
//!
//! # File Hashing
//!
//! The module provides functions for hashing multiple files concurrently:
//!
//! ```ignore
//! use checksums::parallel::{hash_files_parallel, FileHashResult};
//! use checksums::strong::Sha256;
//! use std::path::PathBuf;
//!
//! let files = vec![
//!     PathBuf::from("/path/to/file1"),
//!     PathBuf::from("/path/to/file2"),
//! ];
//!
//! let results = hash_files_parallel::<Sha256>(&files, 64 * 1024);
//!
//! for result in results {
//!     match result.digest {
//!         Ok(digest) => println!("{}: {:?}", result.path.display(), digest.as_ref()),
//!         Err(e) => eprintln!("{}: error - {}", result.path.display(), e),
//!     }
//! }
//! ```

mod blocks;
mod files;
mod types;

// Re-export all public items to preserve the existing API.

pub use blocks::{
    compute_block_signatures_auto, compute_block_signatures_parallel, compute_digests_auto,
    compute_digests_parallel, compute_digests_with_seed_parallel,
    compute_rolling_checksums_parallel, filter_blocks_by_checksum, process_blocks_parallel,
};

pub use files::{
    ParallelFileHasher, compute_file_signatures_parallel, hash_files_parallel,
    hash_files_parallel_with_config, hash_files_with_seed_parallel,
};

pub use types::{
    BlockSignature, FileHashConfig, FileHashResult, FileSignatureResult, PARALLEL_BLOCK_THRESHOLD,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strong::{Md5, Sha256, Xxh3, Xxh64};
    use std::io::Write;
    use tempfile::TempDir;

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
        use crate::rolling::RollingChecksum;

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
        use crate::rolling::RollingChecksum;

        let blocks = create_test_blocks();

        let signatures = compute_block_signatures_parallel::<Sha256, _>(&blocks);

        assert_eq!(signatures.len(), blocks.len());

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
        use crate::rolling::RollingChecksum;

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

        let checksums = compute_rolling_checksums_parallel(&blocks);

        let matching_indices = filter_blocks_by_checksum(&blocks, |c| c & 1 == 1);

        for &i in &matching_indices {
            assert!(checksums[i] & 1 == 1);
        }

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

    fn create_test_files(dir: &TempDir) -> Vec<std::path::PathBuf> {
        let files: Vec<(&str, &[u8])> = vec![
            ("file1.txt", b"Content of file one"),
            ("file2.txt", b"Content of file two"),
            ("file3.txt", b"Content of file three with more data"),
            ("empty.txt", b""),
        ];

        files
            .into_iter()
            .map(|(name, content)| {
                let path = dir.path().join(name);
                let mut file = std::fs::File::create(&path).unwrap();
                file.write_all(content).unwrap();
                path
            })
            .collect()
    }

    #[test]
    fn hash_files_parallel_basic() {
        let dir = TempDir::new().unwrap();
        let files = create_test_files(&dir);

        let results = hash_files_parallel::<Md5>(&files, 64 * 1024);

        assert_eq!(results.len(), files.len());

        for (result, path) in results.iter().zip(files.iter()) {
            assert_eq!(&result.path, path);
            assert!(result.digest.is_ok());

            let content = std::fs::read(path).unwrap();
            let expected = Md5::digest(&content);
            assert_eq!(result.digest.as_ref().unwrap().as_ref(), expected.as_ref());
            assert_eq!(result.size, content.len() as u64);
        }
    }

    #[test]
    fn hash_files_parallel_with_sha256() {
        let dir = TempDir::new().unwrap();
        let files = create_test_files(&dir);

        let results = hash_files_parallel::<Sha256>(&files, 32 * 1024);

        for result in &results {
            assert!(result.digest.is_ok());

            let content = std::fs::read(&result.path).unwrap();
            let expected = Sha256::digest(&content);
            assert_eq!(result.digest.as_ref().unwrap().as_ref(), expected.as_ref());
        }
    }

    #[test]
    fn hash_files_parallel_handles_missing_file() {
        let dir = TempDir::new().unwrap();
        let mut files = create_test_files(&dir);
        files.push(dir.path().join("nonexistent.txt"));

        let results = hash_files_parallel::<Md5>(&files, 64 * 1024);

        assert_eq!(results.len(), files.len());

        let last_result = results.last().unwrap();
        assert!(last_result.digest.is_err());
        assert_eq!(last_result.size, 0);
    }

    #[test]
    fn hash_files_parallel_empty_list() {
        let files: Vec<std::path::PathBuf> = vec![];
        let results = hash_files_parallel::<Md5>(&files, 64 * 1024);
        assert!(results.is_empty());
    }

    #[test]
    fn hash_files_parallel_single_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("single.txt");
        let content = b"single file content";
        std::fs::write(&path, content).unwrap();

        let results = hash_files_parallel::<Sha256>(&[path.clone()], 64 * 1024);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, path);
        assert!(results[0].digest.is_ok());
        assert_eq!(results[0].size, content.len() as u64);

        let expected = Sha256::digest(content);
        assert_eq!(
            results[0].digest.as_ref().unwrap().as_ref(),
            expected.as_ref()
        );
    }

    #[test]
    fn hash_files_with_seed_parallel_works() {
        let dir = TempDir::new().unwrap();
        let files = create_test_files(&dir);
        let seed = 0xDEADBEEFu64;

        let results = hash_files_with_seed_parallel::<Xxh64>(&files, seed, 64 * 1024);

        for result in &results {
            assert!(result.digest.is_ok());

            let content = std::fs::read(&result.path).unwrap();
            let expected = Xxh64::digest(seed, &content);
            assert_eq!(result.digest.as_ref().unwrap().as_ref(), expected.as_ref());
        }
    }

    #[test]
    fn hash_files_with_different_seeds_produces_different_digests() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("seeded.txt");
        std::fs::write(&path, b"test content").unwrap();

        let files = vec![path];

        let results1 = hash_files_with_seed_parallel::<Xxh64>(&files, 1u64, 64 * 1024);
        let results2 = hash_files_with_seed_parallel::<Xxh64>(&files, 2u64, 64 * 1024);

        assert_ne!(
            results1[0].digest.as_ref().unwrap().as_ref(),
            results2[0].digest.as_ref().unwrap().as_ref()
        );
    }

    #[test]
    fn hash_files_parallel_with_config_custom_buffer() {
        let dir = TempDir::new().unwrap();
        let files = create_test_files(&dir);

        let config = FileHashConfig::new()
            .with_buffer_size(4096)
            .with_max_memory_file_size(512);

        let results = hash_files_parallel_with_config::<Md5>(&files, &config);

        for result in &results {
            assert!(result.digest.is_ok());

            let content = std::fs::read(&result.path).unwrap();
            let expected = Md5::digest(&content);
            assert_eq!(result.digest.as_ref().unwrap().as_ref(), expected.as_ref());
        }
    }

    #[test]
    fn hash_files_parallel_large_file_streaming() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("large.bin");

        let size = 2 * 1024 * 1024; // 2 MB
        let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        std::fs::write(&path, &data).unwrap();

        let config = FileHashConfig::new()
            .with_buffer_size(64 * 1024)
            .with_max_memory_file_size(1024 * 1024); // 1 MB threshold

        let results = hash_files_parallel_with_config::<Sha256>(&[path], &config);

        assert_eq!(results.len(), 1);
        assert!(results[0].digest.is_ok());
        assert_eq!(results[0].size, size as u64);

        let expected = Sha256::digest(&data);
        assert_eq!(
            results[0].digest.as_ref().unwrap().as_ref(),
            expected.as_ref()
        );
    }

    #[test]
    fn compute_file_signatures_parallel_basic() {
        use crate::rolling::RollingChecksum;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sigtest.bin");

        let data: Vec<u8> = (0..10000).map(|i| (i % 256) as u8).collect();
        std::fs::write(&path, &data).unwrap();

        let block_size = 1024;
        let results =
            compute_file_signatures_parallel::<Md5>(&[path.clone()], block_size, 64 * 1024);

        assert_eq!(results.len(), 1);
        let result = &results[0];
        assert!(result.signatures.is_ok());

        let signatures = result.signatures.as_ref().unwrap();
        let expected_blocks = data.len().div_ceil(block_size);
        assert_eq!(signatures.len(), expected_blocks);
        assert_eq!(result.block_count, expected_blocks);
        assert_eq!(result.size, data.len() as u64);

        for (i, sig) in signatures.iter().enumerate() {
            let start = i * block_size;
            let end = (start + block_size).min(data.len());
            let block = &data[start..end];

            let mut expected_rolling = RollingChecksum::new();
            expected_rolling.update(block);
            assert_eq!(sig.rolling, expected_rolling.value());

            let expected_strong = Md5::digest(block);
            assert_eq!(sig.strong.as_ref(), expected_strong.as_ref());
        }
    }

    #[test]
    fn compute_file_signatures_parallel_multiple_files() {
        let dir = TempDir::new().unwrap();
        let files = create_test_files(&dir);

        let results = compute_file_signatures_parallel::<Sha256>(&files, 16, 4096);

        assert_eq!(results.len(), files.len());

        for result in &results {
            assert!(result.signatures.is_ok());
        }
    }

    #[test]
    fn compute_file_signatures_handles_empty_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("empty.bin");
        std::fs::write(&path, b"").unwrap();

        let results = compute_file_signatures_parallel::<Md5>(&[path], 1024, 4096);

        assert_eq!(results.len(), 1);
        assert!(results[0].signatures.is_ok());
        assert!(results[0].signatures.as_ref().unwrap().is_empty());
        assert_eq!(results[0].size, 0);
        assert_eq!(results[0].block_count, 0);
    }

    #[test]
    fn parallel_file_hasher_batching() {
        let dir = TempDir::new().unwrap();

        let files: Vec<std::path::PathBuf> = (0..10)
            .map(|i| {
                let path = dir.path().join(format!("file{i}.txt"));
                std::fs::write(&path, format!("Content of file {i}")).unwrap();
                path
            })
            .collect();

        let config = FileHashConfig::default();
        let mut hasher = ParallelFileHasher::<Md5>::new(&files, config, 3);

        assert_eq!(hasher.total(), 10);
        assert_eq!(hasher.remaining(), 10);
        assert_eq!(hasher.processed(), 0);

        let batch1 = hasher.next_batch().unwrap();
        assert_eq!(batch1.len(), 3);
        assert_eq!(hasher.processed(), 3);
        assert_eq!(hasher.remaining(), 7);

        let batch2 = hasher.next_batch().unwrap();
        assert_eq!(batch2.len(), 3);
        assert_eq!(hasher.processed(), 6);

        let batch3 = hasher.next_batch().unwrap();
        assert_eq!(batch3.len(), 3);
        assert_eq!(hasher.processed(), 9);

        let batch4 = hasher.next_batch().unwrap();
        assert_eq!(batch4.len(), 1);
        assert_eq!(hasher.processed(), 10);

        assert!(hasher.next_batch().is_none());
    }

    #[test]
    fn file_hash_config_builder() {
        let config = FileHashConfig::new()
            .with_buffer_size(128 * 1024)
            .with_min_file_size(1024)
            .with_max_memory_file_size(4 * 1024 * 1024);

        assert_eq!(config.buffer_size, 128 * 1024);
        assert_eq!(config.min_file_size, 1024);
        assert_eq!(config.max_memory_file_size, 4 * 1024 * 1024);
    }

    #[test]
    fn file_hash_result_includes_size() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sized.txt");
        let content = b"precise content length";
        std::fs::write(&path, content).unwrap();

        let results = hash_files_parallel::<Md5>(&[path], 4096);

        assert_eq!(results[0].size, content.len() as u64);
    }

    #[test]
    fn parallel_file_hashing_preserves_order() {
        let dir = TempDir::new().unwrap();

        let files: Vec<std::path::PathBuf> = (0..20)
            .map(|i| {
                let path = dir.path().join(format!("ordered{i:02}.txt"));
                std::fs::write(&path, format!("Unique content {i}")).unwrap();
                path
            })
            .collect();

        let results = hash_files_parallel::<Sha256>(&files, 4096);

        for (result, expected_path) in results.iter().zip(files.iter()) {
            assert_eq!(&result.path, expected_path);
        }
    }

    #[test]
    fn parallel_file_hashing_with_xxh3() {
        let dir = TempDir::new().unwrap();
        let files = create_test_files(&dir);

        let results = hash_files_parallel::<Xxh3>(&files, 64 * 1024);

        for result in &results {
            assert!(result.digest.is_ok());

            let content = std::fs::read(&result.path).unwrap();
            let expected = Xxh3::digest(0, &content);
            assert_eq!(result.digest.as_ref().unwrap().as_ref(), expected.as_ref());
        }
    }

    #[test]
    fn mmap_hash_matches_buffered_hash() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("mmap_test.bin");

        // File above MMAP_THRESHOLD (64 KB) triggers the mmap path
        let size = 128 * 1024;
        let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();

        let results = hash_files_parallel::<Sha256>(&[path], 64 * 1024);

        assert_eq!(results.len(), 1);
        assert!(results[0].digest.is_ok());

        let expected = Sha256::digest(&data);
        assert_eq!(
            results[0].digest.as_ref().unwrap().as_ref(),
            expected.as_ref()
        );
    }

    #[test]
    fn mmap_signatures_match_buffered_signatures() {
        use crate::rolling::RollingChecksum;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("mmap_sig_test.bin");

        // File above MMAP_THRESHOLD triggers mmap-based signature computation
        let size: usize = 128 * 1024;
        let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();

        let block_size = 8192;
        let results = compute_file_signatures_parallel::<Md5>(&[path], block_size, 64 * 1024);

        assert_eq!(results.len(), 1);
        let sigs = results[0].signatures.as_ref().unwrap();
        let expected_blocks = size.div_ceil(block_size);
        assert_eq!(sigs.len(), expected_blocks);

        for (i, sig) in sigs.iter().enumerate() {
            let start = i * block_size;
            let end = (start + block_size).min(data.len());
            let block = &data[start..end];

            let mut expected_rolling = RollingChecksum::new();
            expected_rolling.update(block);
            assert_eq!(sig.rolling, expected_rolling.value());

            let expected_strong = Md5::digest(block);
            assert_eq!(sig.strong.as_ref(), expected_strong.as_ref());
        }
    }

    #[test]
    fn mmap_seeded_hash_matches_buffered() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("mmap_seeded.bin");

        let size = 128 * 1024;
        let data: Vec<u8> = (0..size).map(|i| (i % 199) as u8).collect();
        std::fs::write(&path, &data).unwrap();

        let seed = 0xCAFEBABEu64;
        let results = hash_files_with_seed_parallel::<Xxh64>(&[path], seed, 64 * 1024);

        assert_eq!(results.len(), 1);
        assert!(results[0].digest.is_ok());

        let expected = Xxh64::digest(seed, &data);
        assert_eq!(
            results[0].digest.as_ref().unwrap().as_ref(),
            expected.as_ref()
        );
    }
}
