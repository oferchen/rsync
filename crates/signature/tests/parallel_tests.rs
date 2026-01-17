//! Integration tests for parallel signature generation.
//!
//! These tests validate the parallel signature generation functionality,
//! ensuring it produces identical results to the sequential implementation
//! while leveraging multiple CPU cores for improved performance.
//!
//! ## Test Coverage
//!
//! ### Equivalence Testing
//! - Parallel and sequential produce identical signatures
//! - All checksum algorithms behave consistently
//! - Various file sizes and block configurations
//!
//! ### Auto-Selection Logic
//! - Threshold-based selection between parallel and sequential
//! - Edge cases around the threshold boundary
//!
//! ### Performance Characteristics
//! - Large file handling with parallel processing
//! - Memory usage patterns
//!
//! ## Upstream Reference
//!
//! While upstream rsync doesn't have a parallel signature generator,
//! this implementation must produce rsync-compatible signatures that
//! can be used for delta computation.

#![cfg(feature = "parallel")]

use checksums::RollingDigest;
use checksums::strong::Md5Seed;
use protocol::ProtocolVersion;
use signature::parallel::{
    PARALLEL_THRESHOLD_BYTES, generate_file_signature_auto, generate_file_signature_parallel,
};
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};
use std::io::Cursor;
use std::num::{NonZeroU8, NonZeroU32};

// ============================================================================
// Test Utilities
// ============================================================================

/// Creates layout params with common defaults for testing.
fn layout_params(file_len: u64, checksum_len: u8) -> SignatureLayoutParams {
    SignatureLayoutParams::new(
        file_len,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(checksum_len).expect("checksum length must be non-zero"),
    )
}

/// Creates layout params with a forced block size.
fn layout_params_with_block(
    file_len: u64,
    block_len: u32,
    checksum_len: u8,
) -> SignatureLayoutParams {
    SignatureLayoutParams::new(
        file_len,
        NonZeroU32::new(block_len),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(checksum_len).expect("checksum length must be non-zero"),
    )
}

/// Generates deterministic test data.
fn generate_test_data(size: usize) -> Vec<u8> {
    (0..size).map(|i| ((i * 17 + 31) % 256) as u8).collect()
}

// ============================================================================
// Parallel/Sequential Equivalence Tests
// ============================================================================

mod equivalence {
    //! Tests verifying parallel and sequential produce identical results.

    use super::*;

    /// Parallel and sequential produce identical signatures for small files.
    #[test]
    fn parallel_matches_sequential_small_file() {
        let data = generate_test_data(1000);
        let params = layout_params(data.len() as u64, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let sequential =
            generate_file_signature(Cursor::new(data.clone()), layout, SignatureAlgorithm::Md4)
                .expect("sequential");

        let parallel =
            generate_file_signature_parallel(Cursor::new(data), layout, SignatureAlgorithm::Md4)
                .expect("parallel");

        assert_eq!(sequential.total_bytes(), parallel.total_bytes());
        assert_eq!(sequential.blocks().len(), parallel.blocks().len());

        for (seq, par) in sequential.blocks().iter().zip(parallel.blocks().iter()) {
            assert_eq!(seq.index(), par.index());
            assert_eq!(seq.rolling(), par.rolling());
            assert_eq!(seq.strong(), par.strong());
        }
    }

    /// Parallel and sequential produce identical signatures for large files.
    #[test]
    fn parallel_matches_sequential_large_file() {
        let size = PARALLEL_THRESHOLD_BYTES as usize * 2;
        let data = generate_test_data(size);
        let params = layout_params(data.len() as u64, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let sequential =
            generate_file_signature(Cursor::new(data.clone()), layout, SignatureAlgorithm::Md4)
                .expect("sequential");

        let parallel =
            generate_file_signature_parallel(Cursor::new(data), layout, SignatureAlgorithm::Md4)
                .expect("parallel");

        assert_eq!(sequential.total_bytes(), parallel.total_bytes());
        assert_eq!(sequential.blocks().len(), parallel.blocks().len());

        for (seq, par) in sequential.blocks().iter().zip(parallel.blocks().iter()) {
            assert_eq!(seq.index(), par.index());
            assert_eq!(seq.rolling(), par.rolling());
            assert_eq!(seq.strong(), par.strong());
        }
    }

    /// Parallel and sequential match for files with partial final block.
    #[test]
    fn parallel_matches_sequential_with_remainder() {
        let data = generate_test_data(2500);
        let params = layout_params_with_block(data.len() as u64, 700, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        assert_ne!(layout.remainder(), 0, "test requires non-zero remainder");

        let sequential =
            generate_file_signature(Cursor::new(data.clone()), layout, SignatureAlgorithm::Md4)
                .expect("sequential");

        let parallel =
            generate_file_signature_parallel(Cursor::new(data), layout, SignatureAlgorithm::Md4)
                .expect("parallel");

        assert_eq!(sequential.blocks().len(), parallel.blocks().len());

        for (seq, par) in sequential.blocks().iter().zip(parallel.blocks().iter()) {
            assert_eq!(seq.index(), par.index());
            assert_eq!(seq.rolling(), par.rolling());
            assert_eq!(seq.strong(), par.strong());
            assert_eq!(seq.len(), par.len());
        }
    }

    /// Parallel and sequential match for empty files.
    #[test]
    fn parallel_matches_sequential_empty_file() {
        let params = layout_params(0, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let sequential =
            generate_file_signature(Cursor::new(Vec::new()), layout, SignatureAlgorithm::Md4)
                .expect("sequential");

        let parallel = generate_file_signature_parallel(
            Cursor::new(Vec::new()),
            layout,
            SignatureAlgorithm::Md4,
        )
        .expect("parallel");

        assert!(sequential.blocks().is_empty());
        assert!(parallel.blocks().is_empty());
        assert_eq!(sequential.total_bytes(), 0);
        assert_eq!(parallel.total_bytes(), 0);
    }
}

// ============================================================================
// Algorithm Equivalence Tests
// ============================================================================

mod algorithm_equivalence {
    //! Tests verifying parallel works correctly with all algorithms.

    use super::*;

    /// Parallel matches sequential for MD4.
    #[test]
    fn md4_parallel_matches_sequential() {
        let data = generate_test_data(5000);
        let params = layout_params(data.len() as u64, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let sequential =
            generate_file_signature(Cursor::new(data.clone()), layout, SignatureAlgorithm::Md4)
                .expect("sequential");

        let parallel =
            generate_file_signature_parallel(Cursor::new(data), layout, SignatureAlgorithm::Md4)
                .expect("parallel");

        for (seq, par) in sequential.blocks().iter().zip(parallel.blocks().iter()) {
            assert_eq!(seq.strong(), par.strong());
        }
    }

    /// Parallel matches sequential for MD5.
    #[test]
    fn md5_parallel_matches_sequential() {
        let data = generate_test_data(5000);
        let params = layout_params(data.len() as u64, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let algorithm = SignatureAlgorithm::Md5 {
            seed_config: Md5Seed::none(),
        };

        let sequential = generate_file_signature(Cursor::new(data.clone()), layout, algorithm)
            .expect("sequential");

        let parallel = generate_file_signature_parallel(Cursor::new(data), layout, algorithm)
            .expect("parallel");

        for (seq, par) in sequential.blocks().iter().zip(parallel.blocks().iter()) {
            assert_eq!(seq.strong(), par.strong());
        }
    }

    /// Parallel matches sequential for MD5 with seed.
    #[test]
    fn md5_seeded_parallel_matches_sequential() {
        let data = generate_test_data(5000);
        let params = layout_params(data.len() as u64, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let algorithm = SignatureAlgorithm::Md5 {
            seed_config: Md5Seed::proper(12345),
        };

        let sequential = generate_file_signature(Cursor::new(data.clone()), layout, algorithm)
            .expect("sequential");

        let parallel = generate_file_signature_parallel(Cursor::new(data), layout, algorithm)
            .expect("parallel");

        for (seq, par) in sequential.blocks().iter().zip(parallel.blocks().iter()) {
            assert_eq!(seq.strong(), par.strong());
        }
    }

    /// Parallel matches sequential for SHA1.
    #[test]
    fn sha1_parallel_matches_sequential() {
        let data = generate_test_data(5000);
        let params = layout_params(data.len() as u64, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let sequential =
            generate_file_signature(Cursor::new(data.clone()), layout, SignatureAlgorithm::Sha1)
                .expect("sequential");

        let parallel =
            generate_file_signature_parallel(Cursor::new(data), layout, SignatureAlgorithm::Sha1)
                .expect("parallel");

        for (seq, par) in sequential.blocks().iter().zip(parallel.blocks().iter()) {
            assert_eq!(seq.strong(), par.strong());
        }
    }

    /// Parallel matches sequential for XXH64.
    #[test]
    fn xxh64_parallel_matches_sequential() {
        let data = generate_test_data(5000);
        let params = layout_params(data.len() as u64, 8);
        let layout = calculate_signature_layout(params).expect("layout");

        let algorithm = SignatureAlgorithm::Xxh64 { seed: 42 };

        let sequential = generate_file_signature(Cursor::new(data.clone()), layout, algorithm)
            .expect("sequential");

        let parallel = generate_file_signature_parallel(Cursor::new(data), layout, algorithm)
            .expect("parallel");

        for (seq, par) in sequential.blocks().iter().zip(parallel.blocks().iter()) {
            assert_eq!(seq.strong(), par.strong());
        }
    }

    /// Parallel matches sequential for XXH3/64.
    #[test]
    fn xxh3_parallel_matches_sequential() {
        let data = generate_test_data(5000);
        let params = layout_params(data.len() as u64, 8);
        let layout = calculate_signature_layout(params).expect("layout");

        let algorithm = SignatureAlgorithm::Xxh3 { seed: 999 };

        let sequential = generate_file_signature(Cursor::new(data.clone()), layout, algorithm)
            .expect("sequential");

        let parallel = generate_file_signature_parallel(Cursor::new(data), layout, algorithm)
            .expect("parallel");

        for (seq, par) in sequential.blocks().iter().zip(parallel.blocks().iter()) {
            assert_eq!(seq.strong(), par.strong());
        }
    }

    /// Parallel matches sequential for XXH3/128.
    #[test]
    fn xxh3_128_parallel_matches_sequential() {
        let data = generate_test_data(5000);
        let params = layout_params(data.len() as u64, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let algorithm = SignatureAlgorithm::Xxh3_128 { seed: 777 };

        let sequential = generate_file_signature(Cursor::new(data.clone()), layout, algorithm)
            .expect("sequential");

        let parallel = generate_file_signature_parallel(Cursor::new(data), layout, algorithm)
            .expect("parallel");

        for (seq, par) in sequential.blocks().iter().zip(parallel.blocks().iter()) {
            assert_eq!(seq.strong(), par.strong());
        }
    }
}

// ============================================================================
// Auto-Selection Tests
// ============================================================================

mod auto_selection {
    //! Tests for the automatic parallel/sequential selection logic.

    use super::*;

    /// Auto uses sequential for files below threshold.
    #[test]
    fn auto_uses_sequential_below_threshold() {
        let size = PARALLEL_THRESHOLD_BYTES as usize / 2;
        let data = generate_test_data(size);
        let params = layout_params(data.len() as u64, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        // Should complete without error and match sequential
        let auto_sig = generate_file_signature_auto(
            Cursor::new(data.clone()),
            layout,
            SignatureAlgorithm::Md4,
        )
        .expect("auto");

        let sequential =
            generate_file_signature(Cursor::new(data), layout, SignatureAlgorithm::Md4)
                .expect("sequential");

        assert_eq!(auto_sig.total_bytes(), sequential.total_bytes());
    }

    /// Auto uses parallel for files above threshold.
    #[test]
    fn auto_uses_parallel_above_threshold() {
        let size = PARALLEL_THRESHOLD_BYTES as usize * 2;
        let data = generate_test_data(size);
        let params = layout_params(data.len() as u64, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        // Should complete without error and match parallel
        let auto_sig = generate_file_signature_auto(
            Cursor::new(data.clone()),
            layout,
            SignatureAlgorithm::Md4,
        )
        .expect("auto");

        let parallel =
            generate_file_signature_parallel(Cursor::new(data), layout, SignatureAlgorithm::Md4)
                .expect("parallel");

        assert_eq!(auto_sig.total_bytes(), parallel.total_bytes());
    }

    /// Auto handles exactly threshold size correctly.
    #[test]
    fn auto_at_exact_threshold() {
        let size = PARALLEL_THRESHOLD_BYTES as usize;
        let data = generate_test_data(size);
        let params = layout_params(data.len() as u64, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let auto_sig =
            generate_file_signature_auto(Cursor::new(data), layout, SignatureAlgorithm::Md4)
                .expect("auto at threshold");

        assert_eq!(auto_sig.total_bytes(), size as u64);
    }

    /// Auto handles empty files.
    #[test]
    fn auto_empty_file() {
        let params = layout_params(0, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let auto_sig =
            generate_file_signature_auto(Cursor::new(Vec::new()), layout, SignatureAlgorithm::Md4)
                .expect("auto empty");

        assert!(auto_sig.blocks().is_empty());
        assert_eq!(auto_sig.total_bytes(), 0);
    }

    /// Auto handles single byte file.
    #[test]
    fn auto_single_byte() {
        let data = vec![0x42u8];
        let params = layout_params(1, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let auto_sig =
            generate_file_signature_auto(Cursor::new(data), layout, SignatureAlgorithm::Md4)
                .expect("auto single byte");

        assert_eq!(auto_sig.blocks().len(), 1);
        assert_eq!(auto_sig.total_bytes(), 1);
    }
}

// ============================================================================
// Error Handling Tests
// ============================================================================

mod error_handling {
    //! Tests for parallel error handling.

    use super::*;

    /// Parallel reports digest length mismatch.
    #[test]
    fn parallel_digest_length_mismatch() {
        let data = generate_test_data(1000);
        let params = layout_params(data.len() as u64, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        // XXH64 produces 8 bytes, but layout wants 16
        let result = generate_file_signature_parallel(
            Cursor::new(data),
            layout,
            SignatureAlgorithm::Xxh64 { seed: 0 },
        );

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("digest"));
    }

    /// Parallel detects trailing data.
    #[test]
    fn parallel_trailing_data() {
        let params = layout_params(100, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        // Provide more data than expected
        let data = vec![0u8; 150];
        let result =
            generate_file_signature_parallel(Cursor::new(data), layout, SignatureAlgorithm::Md4);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("trailing"));
    }

    /// Parallel handles truncated input.
    #[test]
    fn parallel_truncated_input() {
        let params = layout_params(1000, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        // Provide less data than expected
        let data = vec![0u8; 500];
        let result =
            generate_file_signature_parallel(Cursor::new(data), layout, SignatureAlgorithm::Md4);

        assert!(result.is_err());
    }

    /// Auto reports errors correctly.
    #[test]
    fn auto_reports_errors() {
        let params = layout_params(100, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        // Provide trailing data
        let data = vec![0u8; 150];
        let result =
            generate_file_signature_auto(Cursor::new(data), layout, SignatureAlgorithm::Md4);

        assert!(result.is_err());
    }
}

// ============================================================================
// Block Configuration Tests
// ============================================================================

mod block_configurations {
    //! Tests for various block size configurations.

    use super::*;

    /// Parallel handles many small blocks.
    #[test]
    fn parallel_many_small_blocks() {
        let data = generate_test_data(10000);
        let params = layout_params_with_block(data.len() as u64, 100, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        assert_eq!(layout.block_count(), 100);

        let sequential =
            generate_file_signature(Cursor::new(data.clone()), layout, SignatureAlgorithm::Md4)
                .expect("sequential");

        let parallel =
            generate_file_signature_parallel(Cursor::new(data), layout, SignatureAlgorithm::Md4)
                .expect("parallel");

        assert_eq!(sequential.blocks().len(), parallel.blocks().len());
        for (seq, par) in sequential.blocks().iter().zip(parallel.blocks().iter()) {
            assert_eq!(seq.rolling(), par.rolling());
            assert_eq!(seq.strong(), par.strong());
        }
    }

    /// Parallel handles few large blocks.
    #[test]
    fn parallel_few_large_blocks() {
        let data = generate_test_data(10000);
        let params = layout_params_with_block(data.len() as u64, 5000, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        assert_eq!(layout.block_count(), 2);

        let sequential =
            generate_file_signature(Cursor::new(data.clone()), layout, SignatureAlgorithm::Md4)
                .expect("sequential");

        let parallel =
            generate_file_signature_parallel(Cursor::new(data), layout, SignatureAlgorithm::Md4)
                .expect("parallel");

        assert_eq!(sequential.blocks().len(), parallel.blocks().len());
        for (seq, par) in sequential.blocks().iter().zip(parallel.blocks().iter()) {
            assert_eq!(seq.rolling(), par.rolling());
            assert_eq!(seq.strong(), par.strong());
        }
    }

    /// Parallel handles single block.
    #[test]
    fn parallel_single_block() {
        let data = generate_test_data(500);
        let params = layout_params(data.len() as u64, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        assert_eq!(layout.block_count(), 1);

        let sequential =
            generate_file_signature(Cursor::new(data.clone()), layout, SignatureAlgorithm::Md4)
                .expect("sequential");

        let parallel =
            generate_file_signature_parallel(Cursor::new(data), layout, SignatureAlgorithm::Md4)
                .expect("parallel");

        assert_eq!(
            sequential.blocks()[0].rolling(),
            parallel.blocks()[0].rolling()
        );
        assert_eq!(
            sequential.blocks()[0].strong(),
            parallel.blocks()[0].strong()
        );
    }
}

// ============================================================================
// Rolling Checksum Consistency Tests
// ============================================================================

mod rolling_checksum_consistency {
    //! Tests verifying rolling checksums are computed correctly.

    use super::*;

    /// Parallel computes correct rolling checksums.
    #[test]
    fn parallel_rolling_checksum_correctness() {
        let data = generate_test_data(2000);
        let params = layout_params_with_block(data.len() as u64, 500, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let parallel = generate_file_signature_parallel(
            Cursor::new(data.clone()),
            layout,
            SignatureAlgorithm::Md4,
        )
        .expect("parallel");

        // Verify each block's rolling checksum manually
        for (i, block) in parallel.blocks().iter().enumerate() {
            let start = i * 500;
            let end = if i == parallel.blocks().len() - 1 {
                data.len()
            } else {
                start + 500
            };
            let expected = RollingDigest::from_bytes(&data[start..end]);
            assert_eq!(
                block.rolling(),
                expected,
                "block {i} rolling checksum mismatch"
            );
        }
    }
}

// ============================================================================
// Performance Tests
// ============================================================================

mod performance {
    //! Tests for performance characteristics.

    use super::*;

    /// Parallel handles large files efficiently.
    #[test]
    fn parallel_large_file_performance() {
        let size = 5 * 1024 * 1024; // 5 MB
        let data = generate_test_data(size);
        let params = layout_params(data.len() as u64, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let start = std::time::Instant::now();

        let signature =
            generate_file_signature_parallel(Cursor::new(data), layout, SignatureAlgorithm::Md4)
                .expect("signature");

        let elapsed = start.elapsed();

        assert_eq!(signature.total_bytes(), size as u64);

        // Should complete in reasonable time
        assert!(
            elapsed.as_secs() < 10,
            "parallel signature took too long: {elapsed:?}"
        );
    }

    /// Parallel threshold is sensible.
    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn parallel_threshold_is_reasonable() {
        // Threshold should be at least a few blocks worth
        assert!(PARALLEL_THRESHOLD_BYTES >= 64 * 1024);
        // But not so large that parallel is rarely used
        assert!(PARALLEL_THRESHOLD_BYTES <= 1024 * 1024);
    }
}

// ============================================================================
// Determinism Tests
// ============================================================================

mod determinism {
    //! Tests verifying parallel produces deterministic results.

    use super::*;

    /// Parallel produces identical results on repeated runs.
    #[test]
    fn parallel_is_deterministic() {
        let data = generate_test_data(10000);
        let params = layout_params(data.len() as u64, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let sig1 = generate_file_signature_parallel(
            Cursor::new(data.clone()),
            layout,
            SignatureAlgorithm::Md4,
        )
        .expect("sig1");

        let sig2 = generate_file_signature_parallel(
            Cursor::new(data.clone()),
            layout,
            SignatureAlgorithm::Md4,
        )
        .expect("sig2");

        let sig3 =
            generate_file_signature_parallel(Cursor::new(data), layout, SignatureAlgorithm::Md4)
                .expect("sig3");

        assert_eq!(sig1.blocks().len(), sig2.blocks().len());
        assert_eq!(sig2.blocks().len(), sig3.blocks().len());

        for i in 0..sig1.blocks().len() {
            assert_eq!(sig1.blocks()[i].strong(), sig2.blocks()[i].strong());
            assert_eq!(sig2.blocks()[i].strong(), sig3.blocks()[i].strong());
        }
    }

    /// Block indices are always sequential after parallel processing.
    #[test]
    fn parallel_preserves_block_order() {
        let data = generate_test_data(50000);
        let params = layout_params_with_block(data.len() as u64, 500, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let signature =
            generate_file_signature_parallel(Cursor::new(data), layout, SignatureAlgorithm::Md4)
                .expect("signature");

        for (expected_idx, block) in signature.blocks().iter().enumerate() {
            assert_eq!(
                block.index(),
                expected_idx as u64,
                "block indices must be sequential"
            );
        }
    }
}
