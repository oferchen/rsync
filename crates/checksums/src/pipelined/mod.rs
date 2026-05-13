//! Pipelined checksum computation with double-buffering.
//!
//! This module provides a `DoubleBufferedReader` that overlaps I/O with checksum
//! computation by using two buffers in a producer-consumer pattern:
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────────────┐
//! │                    Double-Buffered Checksum Pipeline                     │
//! ├──────────────────────────────────────────────────────────────────────────┤
//! │                                                                          │
//! │  Time ->                                                                 │
//! │                                                                          │
//! │  Without pipelining (sequential):                                        │
//! │  ┌────────┐ ┌──────────────┐ ┌────────┐ ┌──────────────┐                │
//! │  │ Read 1 │ │ Checksum 1   │ │ Read 2 │ │ Checksum 2   │ ...            │
//! │  └────────┘ └──────────────┘ └────────┘ └──────────────┘                │
//! │                                                                          │
//! │  With pipelining (overlapped):                                           │
//! │  ┌────────┐ ┌────────┐ ┌────────┐                                       │
//! │  │ Read 1 │ │ Read 2 │ │ Read 3 │ ...                                   │
//! │  └────────┘ └──────────────┘ └──────────────┘                            │
//! │            │ Checksum 1   │ │ Checksum 2   │ ...                         │
//! │            └──────────────┘ └──────────────┘                            │
//! │                                                                          │
//! └──────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Strict Two-Buffer Invariant
//!
//! The reader pre-allocates exactly two buffers (A and B) and swaps their
//! roles on every iteration. A bounded `sync_channel(1)` limits the data
//! direction to one in-flight block, while the I/O thread blocks on
//! `recv()` until a recycled buffer arrives. This guarantees constant memory
//! usage of `2 * block_size` regardless of relative I/O and computation
//! speeds - no fallback allocations, no unbounded read-ahead.
//!
//! # Performance Benefits
//!
//! For CPU-intensive checksums (MD4/MD5/SHA1), pipelining can provide 20-40%
//! throughput improvement by hiding I/O latency behind computation:
//!
//! - Sequential: `total_time = n * (read_time + checksum_time)`
//! - Pipelined: `total_time ≈ n * max(read_time, checksum_time)`
//!
//! The benefit is maximized when:
//! - I/O and computation times are balanced
//! - Block sizes are large enough to amortize synchronization overhead
//! - The underlying storage is fast (SSD/NVMe)

mod checksums;
mod config;
pub mod reader;

pub use checksums::{BlockChecksums, PipelinedChecksumIterator, compute_checksums_pipelined};
pub use config::PipelineConfig;
pub use reader::DoubleBufferedReader;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RollingDigest;
    use crate::strong::Md5;
    use std::io::Cursor;

    #[test]
    fn double_buffered_reader_basic() {
        let data = vec![0xAB; 256 * 1024];
        let config = PipelineConfig::default().with_block_size(64 * 1024);
        let mut reader = DoubleBufferedReader::new(Cursor::new(data.clone()), config);

        let mut total_bytes = 0;
        let mut block_count = 0;

        while let Some(block) = reader.next_block().unwrap() {
            total_bytes += block.len();
            block_count += 1;
            assert!(block.iter().all(|&b| b == 0xAB));
        }

        assert_eq!(total_bytes, data.len());
        assert_eq!(block_count, 4);
    }

    #[test]
    fn double_buffered_reader_small_file_sync() {
        let data = vec![0xCD; 64 * 1024];
        let config = PipelineConfig::default()
            .with_block_size(32 * 1024)
            .with_min_file_size(128 * 1024);

        let mut reader = DoubleBufferedReader::with_size_hint(
            Cursor::new(data.clone()),
            config,
            Some(64 * 1024),
        );

        assert!(!reader.is_pipelined());

        let mut total_bytes = 0;
        while let Some(block) = reader.next_block().unwrap() {
            total_bytes += block.len();
        }
        assert_eq!(total_bytes, data.len());
    }

    #[test]
    fn double_buffered_reader_pipelined_mode() {
        let data = vec![0xEF; 512 * 1024];
        let config = PipelineConfig::default()
            .with_block_size(64 * 1024)
            .with_min_file_size(128 * 1024);

        let reader =
            DoubleBufferedReader::with_size_hint(Cursor::new(data), config, Some(512 * 1024));

        assert!(reader.is_pipelined());
    }

    #[test]
    fn double_buffered_reader_empty_input() {
        let data: Vec<u8> = vec![];
        let config = PipelineConfig::default();
        let mut reader = DoubleBufferedReader::new(Cursor::new(data), config);

        assert!(reader.next_block().unwrap().is_none());
    }

    #[test]
    fn double_buffered_reader_partial_last_block() {
        let data = vec![0x12; 100 * 1024];
        let config = PipelineConfig::default().with_block_size(64 * 1024);
        let mut reader = DoubleBufferedReader::new(Cursor::new(data.clone()), config);

        let block1 = reader.next_block().unwrap().unwrap();
        assert_eq!(block1.len(), 64 * 1024);

        let block2 = reader.next_block().unwrap().unwrap();
        assert_eq!(block2.len(), 36 * 1024);

        assert!(reader.next_block().unwrap().is_none());
    }

    #[test]
    fn double_buffered_reader_disabled_pipelining() {
        let data = vec![0x34; 512 * 1024];
        let config = PipelineConfig::default()
            .with_block_size(64 * 1024)
            .with_enabled(false);

        let reader = DoubleBufferedReader::new(Cursor::new(data), config);

        assert!(!reader.is_pipelined());
    }

    #[test]
    fn compute_checksums_pipelined_basic() {
        let data = vec![0x56; 256 * 1024];
        let config = PipelineConfig::default().with_block_size(64 * 1024);

        let checksums = compute_checksums_pipelined::<Md5, _>(
            Cursor::new(data.clone()),
            config,
            Some(256 * 1024),
        )
        .unwrap();

        assert_eq!(checksums.len(), 4);

        for (i, cs) in checksums.iter().enumerate() {
            let start = i * 64 * 1024;
            let end = (start + 64 * 1024).min(data.len());
            let block = &data[start..end];

            let expected_rolling = RollingDigest::from_bytes(block);
            let expected_strong = Md5::digest(block);

            assert_eq!(cs.rolling, expected_rolling);
            assert_eq!(cs.strong.as_ref(), expected_strong.as_ref());
            assert_eq!(cs.len, block.len());
        }
    }

    #[test]
    fn compute_checksums_pipelined_empty() {
        let data: Vec<u8> = vec![];
        let config = PipelineConfig::default();

        let checksums =
            compute_checksums_pipelined::<Md5, _>(Cursor::new(data), config, Some(0)).unwrap();

        assert!(checksums.is_empty());
    }

    #[test]
    fn pipelined_iterator_basic() {
        let data = vec![0x78; 128 * 1024];
        let config = PipelineConfig::default().with_block_size(32 * 1024);

        let mut iter: PipelinedChecksumIterator<Md5, _> =
            PipelinedChecksumIterator::new(Cursor::new(data.clone()), config);

        let mut count = 0;
        while let Some(cs) = iter.next_block_checksums().unwrap() {
            assert_eq!(cs.len, 32 * 1024);
            count += 1;
        }

        assert_eq!(count, 4);
    }

    #[test]
    fn pipeline_config_builder() {
        let config = PipelineConfig::new()
            .with_block_size(128 * 1024)
            .with_min_file_size(512 * 1024)
            .with_enabled(false);

        assert_eq!(config.block_size, 128 * 1024);
        assert_eq!(config.min_file_size, 512 * 1024);
        assert!(!config.enabled);
    }

    #[test]
    fn block_checksums_clone_debug() {
        let cs = BlockChecksums {
            rolling: RollingDigest::from_bytes(b"test"),
            strong: [0u8; 16],
            len: 4,
        };

        let cloned = cs.clone();
        assert_eq!(cloned.rolling, cs.rolling);
        assert_eq!(cloned.strong, cs.strong);
        assert_eq!(cloned.len, cs.len);

        let debug = format!("{cs:?}");
        assert!(debug.contains("BlockChecksums"));
    }

    #[test]
    fn multiple_reads_same_content() {
        let data = vec![0x9A; 256 * 1024];
        let config = PipelineConfig::default().with_block_size(64 * 1024);

        let pipelined =
            compute_checksums_pipelined::<Md5, _>(Cursor::new(data.clone()), config, None).unwrap();

        let sync_config = config.with_enabled(false);
        let sequential =
            compute_checksums_pipelined::<Md5, _>(Cursor::new(data), sync_config, None).unwrap();

        assert_eq!(pipelined.len(), sequential.len());
        for (p, s) in pipelined.iter().zip(sequential.iter()) {
            assert_eq!(p.rolling, s.rolling);
            assert_eq!(p.strong.as_ref(), s.strong.as_ref());
            assert_eq!(p.len, s.len);
        }
    }

    #[test]
    fn handles_exact_block_boundary() {
        let data = vec![0xBC; 128 * 1024];
        let config = PipelineConfig::default().with_block_size(64 * 1024);

        let mut reader = DoubleBufferedReader::new(Cursor::new(data), config);

        let block1 = reader.next_block().unwrap().unwrap();
        assert_eq!(block1.len(), 64 * 1024);

        let block2 = reader.next_block().unwrap().unwrap();
        assert_eq!(block2.len(), 64 * 1024);

        assert!(reader.next_block().unwrap().is_none());
    }

    #[test]
    fn handles_very_small_blocks() {
        let data = vec![0xDE; 1000];
        let config = PipelineConfig::default()
            .with_block_size(100)
            .with_min_file_size(0);

        let checksums =
            compute_checksums_pipelined::<Md5, _>(Cursor::new(data), config, Some(1000)).unwrap();

        assert_eq!(checksums.len(), 10);
    }

    #[test]
    fn reader_thread_cleanup_on_drop() {
        let data = vec![0xF0; 1024 * 1024];
        let config = PipelineConfig::default().with_block_size(64 * 1024);

        {
            let mut reader =
                DoubleBufferedReader::with_size_hint(Cursor::new(data), config, Some(1024 * 1024));

            let _ = reader.next_block().unwrap();
        }
    }

    /// Verifies that pipelined (double-buffered) checksums produce identical
    /// results to sequential (single-buffer) checksums across various file
    /// sizes, including smaller-than-block, exact-block, multi-block, and
    /// partial-last-block cases.
    #[test]
    fn pipelined_matches_sequential_various_sizes() {
        let block_size = 64 * 1024;
        let sizes: &[usize] = &[
            0,                  // empty
            1,                  // single byte
            100,                // smaller than block
            block_size - 1,     // one less than block
            block_size,         // exact block boundary
            block_size + 1,     // one more than block
            block_size * 3,     // exact multiple
            block_size * 3 + 7, // partial last block
            block_size * 10,    // many blocks
        ];

        for &size in sizes {
            let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();

            let pipelined_config = PipelineConfig::default()
                .with_block_size(block_size)
                .with_min_file_size(0)
                .with_enabled(true);
            let sequential_config = pipelined_config.with_enabled(false);

            let pipelined = compute_checksums_pipelined::<Md5, _>(
                Cursor::new(data.clone()),
                pipelined_config,
                Some(size as u64),
            )
            .unwrap();

            let sequential = compute_checksums_pipelined::<Md5, _>(
                Cursor::new(data),
                sequential_config,
                Some(size as u64),
            )
            .unwrap();

            assert_eq!(
                pipelined.len(),
                sequential.len(),
                "block count mismatch for size {size}"
            );
            for (i, (p, s)) in pipelined.iter().zip(sequential.iter()).enumerate() {
                assert_eq!(
                    p.rolling, s.rolling,
                    "rolling mismatch at block {i} for size {size}"
                );
                assert_eq!(
                    p.strong.as_ref(),
                    s.strong.as_ref(),
                    "strong mismatch at block {i} for size {size}"
                );
                assert_eq!(p.len, s.len, "len mismatch at block {i} for size {size}");
            }
        }
    }

    /// Exercises the sync-mode buffer reuse path by reading multiple blocks
    /// in synchronous mode and verifying all data is correct.
    #[test]
    fn sync_mode_reuses_buffer() {
        let data: Vec<u8> = (0..200).map(|i| (i % 256) as u8).collect();
        let config = PipelineConfig::default()
            .with_block_size(50)
            .with_enabled(false);

        let mut reader = DoubleBufferedReader::new(Cursor::new(data.clone()), config);

        let mut collected = Vec::new();
        while let Some(block) = reader.next_block().unwrap() {
            collected.extend_from_slice(block);
        }
        assert_eq!(collected, data);
    }

    /// Verifies that many blocks worth of data are correctly read and
    /// checksummed in pipelined mode with buffer recycling.
    #[test]
    fn many_blocks_pipelined_recycling() {
        let block_size = 1024;
        let num_blocks = 100;
        let data: Vec<u8> = (0..block_size * num_blocks)
            .map(|i| (i % 256) as u8)
            .collect();

        let config = PipelineConfig::default()
            .with_block_size(block_size)
            .with_min_file_size(0);

        let checksums = compute_checksums_pipelined::<Md5, _>(
            Cursor::new(data.clone()),
            config,
            Some(data.len() as u64),
        )
        .unwrap();

        assert_eq!(checksums.len(), num_blocks);

        for (i, cs) in checksums.iter().enumerate() {
            let start = i * block_size;
            let end = start + block_size;
            let block = &data[start..end];

            let expected_rolling = RollingDigest::from_bytes(block);
            let expected_strong = Md5::digest(block);

            assert_eq!(
                cs.rolling, expected_rolling,
                "rolling mismatch at block {i}"
            );
            assert_eq!(
                cs.strong.as_ref(),
                expected_strong.as_ref(),
                "strong mismatch at block {i}"
            );
        }
    }

    /// Verifies data integrity when file size is smaller than one block and
    /// pipelining is forced.
    #[test]
    fn single_byte_pipelined() {
        let data = vec![0x42u8; 1];
        let config = PipelineConfig::default()
            .with_block_size(64 * 1024)
            .with_min_file_size(0);

        let checksums =
            compute_checksums_pipelined::<Md5, _>(Cursor::new(data.clone()), config, Some(1))
                .unwrap();

        assert_eq!(checksums.len(), 1);
        assert_eq!(checksums[0].len, 1);
        assert_eq!(checksums[0].rolling, RollingDigest::from_bytes(&data));
    }
}

#[cfg(test)]
mod property_tests {
    use super::*;
    use crate::strong::Md5;
    use proptest::prelude::*;
    use std::io::Cursor;

    proptest! {
        /// Property: for any random data and block size, pipelined checksums
        /// must be identical to sequential checksums.
        #[test]
        fn pipelined_equals_sequential(
            data in prop::collection::vec(any::<u8>(), 0..512 * 1024),
            block_size in (64usize..=128 * 1024).prop_map(|s| s.max(1)),
        ) {
            let pipelined_config = PipelineConfig::default()
                .with_block_size(block_size)
                .with_min_file_size(0)
                .with_enabled(true);
            let sequential_config = pipelined_config.with_enabled(false);

            let pipelined = compute_checksums_pipelined::<Md5, _>(
                Cursor::new(data.clone()),
                pipelined_config,
                Some(data.len() as u64),
            )
            .unwrap();

            let sequential = compute_checksums_pipelined::<Md5, _>(
                Cursor::new(data.clone()),
                sequential_config,
                Some(data.len() as u64),
            )
            .unwrap();

            prop_assert_eq!(pipelined.len(), sequential.len());
            for (p, s) in pipelined.iter().zip(sequential.iter()) {
                prop_assert_eq!(p.rolling, s.rolling);
                prop_assert_eq!(p.strong.as_ref(), s.strong.as_ref());
                prop_assert_eq!(p.len, s.len);
            }
        }

        /// Property: collected data from the double-buffered reader equals
        /// the original input data, regardless of pipelining mode.
        #[test]
        fn reader_collects_all_data(
            data in prop::collection::vec(any::<u8>(), 0..256 * 1024),
            block_size in (64usize..=64 * 1024).prop_map(|s| s.max(1)),
            pipelined in any::<bool>(),
        ) {
            let config = PipelineConfig::default()
                .with_block_size(block_size)
                .with_min_file_size(0)
                .with_enabled(pipelined);

            let mut reader = DoubleBufferedReader::with_size_hint(
                Cursor::new(data.clone()),
                config,
                Some(data.len() as u64),
            );

            let mut collected = Vec::new();
            while let Some(block) = reader.next_block().unwrap() {
                collected.extend_from_slice(block);
            }

            prop_assert_eq!(collected, data);
        }
    }
}
