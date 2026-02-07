//! SIMD acceleration parity tests.
//!
//! These tests verify that SIMD-accelerated implementations produce identical
//! results to the scalar/reference implementations for all hash algorithms.
//! This ensures correctness across different CPU feature levels (SSE2, AVX2,
//! AVX-512, NEON) and prevents regressions when optimizing SIMD code paths.

// =============================================================================
// MD5 SIMD Batch vs Scalar Parity
// =============================================================================

#[cfg(test)]
mod md5_simd_parity {
    use crate::simd_batch;
    use crate::strong::Md5;

    fn to_hex(bytes: &[u8]) -> String {
        use std::fmt::Write;
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            write!(s, "{b:02x}").unwrap();
        }
        s
    }

    // ========================================================================
    // RFC 1321 Test Vectors (MD5)
    // ========================================================================

    /// Verify SIMD batch MD5 produces correct RFC 1321 test vector results.
    #[test]
    fn simd_md5_rfc1321_test_vectors() {
        let vectors: &[(&[u8], &str)] = &[
            (b"", "d41d8cd98f00b204e9800998ecf8427e"),
            (b"a", "0cc175b9c0f1b6a831c399e269772661"),
            (b"abc", "900150983cd24fb0d6963f7d28e17f72"),
            (b"message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
            (
                b"abcdefghijklmnopqrstuvwxyz",
                "c3fcd3d76192e4007dfb496cca67e13b",
            ),
            (
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
                "d174ab98d277d9f5a5611c2c9f419d9f",
            ),
            (
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                "57edf4a22be3c955ac49da2e2107b67a",
            ),
        ];

        let inputs: Vec<&[u8]> = vectors.iter().map(|(input, _)| *input).collect();
        let batch_results = simd_batch::digest_batch(&inputs);

        for (i, (_, expected_hex)) in vectors.iter().enumerate() {
            assert_eq!(
                to_hex(&batch_results[i]),
                *expected_hex,
                "SIMD MD5 batch mismatch for RFC 1321 vector index {i}"
            );
        }
    }

    /// Verify SIMD batch results match the strong::Md5 reference for RFC vectors.
    #[test]
    fn simd_md5_batch_matches_strong_md5_rfc_vectors() {
        let inputs: Vec<&[u8]> = vec![
            b"",
            b"a",
            b"abc",
            b"message digest",
            b"abcdefghijklmnopqrstuvwxyz",
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
            b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
        ];

        let batch_results = simd_batch::digest_batch(&inputs);

        for (i, input) in inputs.iter().enumerate() {
            let reference = Md5::digest(input);
            assert_eq!(
                batch_results[i],
                reference,
                "SIMD MD5 batch[{i}] does not match strong::Md5 for input {:?}",
                String::from_utf8_lossy(input)
            );
        }
    }

    // ========================================================================
    // SIMD Lane Boundary Tests (MD5)
    // ========================================================================

    /// Test with data sizes at SIMD lane boundaries (16, 32, 64, 128, 256, 512 bytes).
    #[test]
    fn simd_md5_lane_boundary_sizes() {
        let boundary_sizes: &[usize] = &[16, 32, 64, 128, 256, 512, 1024];

        for &size in boundary_sizes {
            let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            let inputs = vec![data.as_slice()];

            let batch_result = simd_batch::digest_batch(&inputs);
            let reference = Md5::digest(&data);

            assert_eq!(
                batch_result[0], reference,
                "SIMD MD5 mismatch at lane boundary size {size}"
            );
        }
    }

    /// Test with data sizes at MD5 block boundaries (55, 56, 63, 64, 65, 119, 120, 128).
    #[test]
    fn simd_md5_block_boundary_sizes() {
        let boundary_sizes: &[usize] =
            &[0, 1, 55, 56, 57, 63, 64, 65, 119, 120, 121, 127, 128, 129];

        for &size in boundary_sizes {
            let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            let inputs = vec![data.as_slice()];

            let batch_result = simd_batch::digest_batch(&inputs);
            let reference = Md5::digest(&data);

            assert_eq!(
                batch_result[0], reference,
                "SIMD MD5 mismatch at block boundary size {size}"
            );
        }
    }

    // ========================================================================
    // Batch Size Tests (MD5) - exercise full and partial SIMD lanes
    // ========================================================================

    /// Test with exactly 4 inputs (SSE2/NEON full lane).
    #[test]
    fn simd_md5_batch_4_inputs_full_lane() {
        let inputs: Vec<Vec<u8>> = (0..4)
            .map(|i| {
                (0..100 + i * 50)
                    .map(|j| ((i * 37 + j) % 256) as u8)
                    .collect()
            })
            .collect();
        let input_refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();

        let batch_results = simd_batch::digest_batch(&input_refs);

        for (i, input) in inputs.iter().enumerate() {
            let reference = Md5::digest(input);
            assert_eq!(
                batch_results[i], reference,
                "SIMD MD5 batch of 4 mismatch at index {i}"
            );
        }
    }

    /// Test with exactly 8 inputs (AVX2 full lane).
    #[test]
    fn simd_md5_batch_8_inputs_full_lane() {
        let inputs: Vec<Vec<u8>> = (0..8)
            .map(|i| {
                (0..200 + i * 30)
                    .map(|j| ((i * 53 + j) % 256) as u8)
                    .collect()
            })
            .collect();
        let input_refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();

        let batch_results = simd_batch::digest_batch(&input_refs);

        for (i, input) in inputs.iter().enumerate() {
            let reference = Md5::digest(input);
            assert_eq!(
                batch_results[i], reference,
                "SIMD MD5 batch of 8 mismatch at index {i}"
            );
        }
    }

    /// Test with exactly 16 inputs (AVX-512 full lane).
    #[test]
    fn simd_md5_batch_16_inputs_full_lane() {
        let inputs: Vec<Vec<u8>> = (0..16)
            .map(|i| {
                (0..150 + i * 20)
                    .map(|j| ((i * 71 + j) % 256) as u8)
                    .collect()
            })
            .collect();
        let input_refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();

        let batch_results = simd_batch::digest_batch(&input_refs);

        for (i, input) in inputs.iter().enumerate() {
            let reference = Md5::digest(input);
            assert_eq!(
                batch_results[i], reference,
                "SIMD MD5 batch of 16 mismatch at index {i}"
            );
        }
    }

    /// Test with partial batches (1, 3, 5, 7, 9, 13, 15, 17 inputs).
    #[test]
    fn simd_md5_batch_partial_lanes() {
        for count in [1, 2, 3, 5, 6, 7, 9, 10, 13, 15, 17, 19, 32, 33] {
            let inputs: Vec<Vec<u8>> = (0..count)
                .map(|i| {
                    (0..(50 + i * 10))
                        .map(|j| ((i * 41 + j) % 256) as u8)
                        .collect()
                })
                .collect();
            let input_refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();

            let batch_results = simd_batch::digest_batch(&input_refs);

            assert_eq!(
                batch_results.len(),
                count,
                "Batch count mismatch for {count} inputs"
            );
            for (i, input) in inputs.iter().enumerate() {
                let reference = Md5::digest(input);
                assert_eq!(
                    batch_results[i], reference,
                    "SIMD MD5 partial batch ({count} inputs) mismatch at index {i}"
                );
            }
        }
    }

    /// Test with empty batch.
    #[test]
    fn simd_md5_batch_empty() {
        let inputs: Vec<&[u8]> = vec![];
        let batch_results = simd_batch::digest_batch(&inputs);
        assert!(batch_results.is_empty());
    }

    // ========================================================================
    // Large Data Tests (MD5)
    // ========================================================================

    /// Test with large inputs that span many MD5 blocks.
    #[test]
    fn simd_md5_large_inputs() {
        let sizes = [1024, 4096, 8192, 65536, 100_000];

        for &size in &sizes {
            let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            let inputs = vec![data.as_slice()];

            let batch_result = simd_batch::digest_batch(&inputs);
            let reference = Md5::digest(&data);

            assert_eq!(
                batch_result[0], reference,
                "SIMD MD5 mismatch for large input size {size}"
            );
        }
    }

    /// Test batch of large inputs for SIMD parity.
    #[test]
    fn simd_md5_batch_large_varied_inputs() {
        let inputs: Vec<Vec<u8>> = vec![
            vec![0xAA; 10_000],
            vec![0xBB; 20_000],
            vec![0xCC; 5_000],
            vec![0xDD; 15_000],
            vec![0xEE; 8_192],
            vec![0xFF; 4_096],
            vec![0x11; 65_536],
            vec![0x22; 1_024],
        ];
        let input_refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();

        let batch_results = simd_batch::digest_batch(&input_refs);

        for (i, input) in inputs.iter().enumerate() {
            let reference = Md5::digest(input);
            assert_eq!(
                batch_results[i], reference,
                "SIMD MD5 large varied batch mismatch at index {i}"
            );
        }
    }

    // ========================================================================
    // All-Same and Special Pattern Tests (MD5)
    // ========================================================================

    /// Test with identical inputs in a batch (verifies lane independence).
    #[test]
    fn simd_md5_batch_identical_inputs() {
        let data = b"identical input for all lanes";
        let inputs: Vec<&[u8]> = vec![data.as_slice(); 16];

        let batch_results = simd_batch::digest_batch(&inputs);
        let reference = Md5::digest(data);

        for (i, result) in batch_results.iter().enumerate() {
            assert_eq!(
                *result, reference,
                "SIMD MD5 identical inputs mismatch at lane {i}"
            );
        }
    }

    /// Test with all-zero inputs.
    #[test]
    fn simd_md5_batch_all_zeros() {
        let sizes = [0, 1, 64, 128, 1000];
        for &size in &sizes {
            let data = vec![0u8; size];
            let inputs = vec![data.as_slice()];

            let batch_result = simd_batch::digest_batch(&inputs);
            let reference = Md5::digest(&data);

            assert_eq!(
                batch_result[0], reference,
                "SIMD MD5 all-zeros mismatch for size {size}"
            );
        }
    }

    /// Test with all-0xFF inputs.
    #[test]
    fn simd_md5_batch_all_ff() {
        let sizes = [0, 1, 64, 128, 1000];
        for &size in &sizes {
            let data = vec![0xFF; size];
            let inputs = vec![data.as_slice()];

            let batch_result = simd_batch::digest_batch(&inputs);
            let reference = Md5::digest(&data);

            assert_eq!(
                batch_result[0], reference,
                "SIMD MD5 all-0xFF mismatch for size {size}"
            );
        }
    }

    // ========================================================================
    // Random Data Parity (MD5)
    // ========================================================================

    /// Test with pseudo-random data at various sizes.
    #[test]
    fn simd_md5_random_data_parity() {
        use rand::Rng;
        let mut rng = rand::thread_rng();

        let sizes = [
            0, 1, 15, 16, 17, 31, 32, 33, 55, 56, 63, 64, 65, 127, 128, 129, 255, 256, 512, 1000,
            4096, 10_000,
        ];

        for &size in &sizes {
            let data: Vec<u8> = (0..size).map(|_| rng.r#gen()).collect();
            let inputs = vec![data.as_slice()];

            let batch_result = simd_batch::digest_batch(&inputs);
            let reference = Md5::digest(&data);

            assert_eq!(
                batch_result[0], reference,
                "SIMD MD5 random data mismatch for size {size}"
            );
        }
    }

    /// Test batch of random data (multiple inputs at once).
    #[test]
    fn simd_md5_batch_random_data_parity() {
        use rand::Rng;
        let mut rng = rand::thread_rng();

        // Generate 20 random inputs with varying sizes
        let inputs: Vec<Vec<u8>> = (0..20)
            .map(|_| {
                let size = rng.gen_range(0..5000);
                (0..size).map(|_| rng.r#gen()).collect()
            })
            .collect();
        let input_refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();

        let batch_results = simd_batch::digest_batch(&input_refs);

        for (i, input) in inputs.iter().enumerate() {
            let reference = Md5::digest(input);
            assert_eq!(
                batch_results[i],
                reference,
                "SIMD MD5 random batch mismatch at index {i} (size={})",
                input.len()
            );
        }
    }

    // ========================================================================
    // Backend Reporting Tests (MD5)
    // ========================================================================

    /// Verify the active backend reports correctly and produces correct results.
    #[test]
    fn simd_md5_active_backend_produces_correct_results() {
        let backend = simd_batch::active_backend();
        eprintln!(
            "Active SIMD MD5 backend: {:?} ({} lanes)",
            backend,
            backend.lanes()
        );

        // Regardless of backend, results must match scalar
        let data = b"backend correctness check";
        let simd_result = simd_batch::digest(data);
        let reference = Md5::digest(data);
        assert_eq!(simd_result, reference);
    }

    /// Verify parallel_lanes returns a sensible value.
    #[test]
    fn simd_md5_parallel_lanes_valid() {
        let lanes = simd_batch::parallel_lanes();
        assert!(
            [1, 4, 8, 16].contains(&lanes),
            "Unexpected lane count: {lanes}"
        );
    }
}

// =============================================================================
// MD4 SIMD Batch vs Scalar Parity
// =============================================================================

#[cfg(test)]
mod md4_simd_parity {
    use crate::simd_batch::md4;
    use crate::strong::Md4;

    fn to_hex(bytes: &[u8]) -> String {
        use std::fmt::Write;
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            write!(s, "{b:02x}").unwrap();
        }
        s
    }

    // ========================================================================
    // RFC 1320 Test Vectors (MD4)
    // ========================================================================

    /// Verify SIMD batch MD4 produces correct RFC 1320 test vector results.
    #[test]
    fn simd_md4_rfc1320_test_vectors() {
        let vectors: &[(&[u8], &str)] = &[
            (b"", "31d6cfe0d16ae931b73c59d7e0c089c0"),
            (b"a", "bde52cb31de33e46245e05fbdbd6fb24"),
            (b"abc", "a448017aaf21d8525fc10ae87aa6729d"),
            (b"message digest", "d9130a8164549fe818874806e1c7014b"),
            (
                b"abcdefghijklmnopqrstuvwxyz",
                "d79e1c308aa5bbcdeea8ed63df412da9",
            ),
            (
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
                "043f8582f241db351ce627e153e7f0e4",
            ),
            (
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                "e33b4ddc9c38f2199c3e7b164fcc0536",
            ),
        ];

        let inputs: Vec<&[u8]> = vectors.iter().map(|(input, _)| *input).collect();
        let batch_results = md4::digest_batch(&inputs);

        for (i, (_, expected_hex)) in vectors.iter().enumerate() {
            assert_eq!(
                to_hex(&batch_results[i]),
                *expected_hex,
                "SIMD MD4 batch mismatch for RFC 1320 vector index {i}"
            );
        }
    }

    /// Verify SIMD batch results match the strong::Md4 reference for RFC vectors.
    #[test]
    fn simd_md4_batch_matches_strong_md4_rfc_vectors() {
        let inputs: Vec<&[u8]> = vec![
            b"",
            b"a",
            b"abc",
            b"message digest",
            b"abcdefghijklmnopqrstuvwxyz",
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
            b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
        ];

        let batch_results = md4::digest_batch(&inputs);

        for (i, input) in inputs.iter().enumerate() {
            let reference = Md4::digest(input);
            assert_eq!(
                batch_results[i],
                reference,
                "SIMD MD4 batch[{i}] does not match strong::Md4 for input {:?}",
                String::from_utf8_lossy(input)
            );
        }
    }

    // ========================================================================
    // SIMD Lane Boundary Tests (MD4)
    // ========================================================================

    /// Test with data sizes at SIMD lane boundaries.
    #[test]
    fn simd_md4_lane_boundary_sizes() {
        let boundary_sizes: &[usize] = &[16, 32, 64, 128, 256, 512, 1024];

        for &size in boundary_sizes {
            let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            let inputs = vec![data.as_slice()];

            let batch_result = md4::digest_batch(&inputs);
            let reference = Md4::digest(&data);

            assert_eq!(
                batch_result[0], reference,
                "SIMD MD4 mismatch at lane boundary size {size}"
            );
        }
    }

    /// Test with data sizes at MD4 block boundaries (55, 56, 63, 64, 65).
    #[test]
    fn simd_md4_block_boundary_sizes() {
        let boundary_sizes: &[usize] =
            &[0, 1, 55, 56, 57, 63, 64, 65, 119, 120, 121, 127, 128, 129];

        for &size in boundary_sizes {
            let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            let inputs = vec![data.as_slice()];

            let batch_result = md4::digest_batch(&inputs);
            let reference = Md4::digest(&data);

            assert_eq!(
                batch_result[0], reference,
                "SIMD MD4 mismatch at block boundary size {size}"
            );
        }
    }

    // ========================================================================
    // Batch Size Tests (MD4)
    // ========================================================================

    /// Test with exactly 4 inputs (SSE2/NEON full lane).
    #[test]
    fn simd_md4_batch_4_inputs_full_lane() {
        let inputs: Vec<Vec<u8>> = (0..4)
            .map(|i| {
                (0..100 + i * 50)
                    .map(|j| ((i * 37 + j) % 256) as u8)
                    .collect()
            })
            .collect();
        let input_refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();

        let batch_results = md4::digest_batch(&input_refs);

        for (i, input) in inputs.iter().enumerate() {
            let reference = Md4::digest(input);
            assert_eq!(
                batch_results[i], reference,
                "SIMD MD4 batch of 4 mismatch at index {i}"
            );
        }
    }

    /// Test with exactly 8 inputs (AVX2 full lane).
    #[test]
    fn simd_md4_batch_8_inputs_full_lane() {
        let inputs: Vec<Vec<u8>> = (0..8)
            .map(|i| {
                (0..200 + i * 30)
                    .map(|j| ((i * 53 + j) % 256) as u8)
                    .collect()
            })
            .collect();
        let input_refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();

        let batch_results = md4::digest_batch(&input_refs);

        for (i, input) in inputs.iter().enumerate() {
            let reference = Md4::digest(input);
            assert_eq!(
                batch_results[i], reference,
                "SIMD MD4 batch of 8 mismatch at index {i}"
            );
        }
    }

    /// Test with exactly 16 inputs (AVX-512 full lane).
    #[test]
    fn simd_md4_batch_16_inputs_full_lane() {
        let inputs: Vec<Vec<u8>> = (0..16)
            .map(|i| {
                (0..150 + i * 20)
                    .map(|j| ((i * 71 + j) % 256) as u8)
                    .collect()
            })
            .collect();
        let input_refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();

        let batch_results = md4::digest_batch(&input_refs);

        for (i, input) in inputs.iter().enumerate() {
            let reference = Md4::digest(input);
            assert_eq!(
                batch_results[i], reference,
                "SIMD MD4 batch of 16 mismatch at index {i}"
            );
        }
    }

    /// Test with partial batches.
    #[test]
    fn simd_md4_batch_partial_lanes() {
        for count in [1, 2, 3, 5, 6, 7, 9, 10, 13, 15, 17, 19, 32, 33] {
            let inputs: Vec<Vec<u8>> = (0..count)
                .map(|i| {
                    (0..(50 + i * 10))
                        .map(|j| ((i * 41 + j) % 256) as u8)
                        .collect()
                })
                .collect();
            let input_refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();

            let batch_results = md4::digest_batch(&input_refs);

            assert_eq!(
                batch_results.len(),
                count,
                "Batch count mismatch for {count} inputs"
            );
            for (i, input) in inputs.iter().enumerate() {
                let reference = Md4::digest(input);
                assert_eq!(
                    batch_results[i], reference,
                    "SIMD MD4 partial batch ({count} inputs) mismatch at index {i}"
                );
            }
        }
    }

    /// Test with empty batch.
    #[test]
    fn simd_md4_batch_empty() {
        let inputs: Vec<&[u8]> = vec![];
        let batch_results = md4::digest_batch(&inputs);
        assert!(batch_results.is_empty());
    }

    // ========================================================================
    // Large Data Tests (MD4)
    // ========================================================================

    /// Test with large inputs that span many MD4 blocks.
    #[test]
    fn simd_md4_large_inputs() {
        let sizes = [1024, 4096, 8192, 65536, 100_000];

        for &size in &sizes {
            let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            let inputs = vec![data.as_slice()];

            let batch_result = md4::digest_batch(&inputs);
            let reference = Md4::digest(&data);

            assert_eq!(
                batch_result[0], reference,
                "SIMD MD4 mismatch for large input size {size}"
            );
        }
    }

    /// Test batch of large inputs for SIMD parity.
    #[test]
    fn simd_md4_batch_large_varied_inputs() {
        let inputs: Vec<Vec<u8>> = vec![
            vec![0xAA; 10_000],
            vec![0xBB; 20_000],
            vec![0xCC; 5_000],
            vec![0xDD; 15_000],
            vec![0xEE; 8_192],
            vec![0xFF; 4_096],
            vec![0x11; 65_536],
            vec![0x22; 1_024],
        ];
        let input_refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();

        let batch_results = md4::digest_batch(&input_refs);

        for (i, input) in inputs.iter().enumerate() {
            let reference = Md4::digest(input);
            assert_eq!(
                batch_results[i], reference,
                "SIMD MD4 large varied batch mismatch at index {i}"
            );
        }
    }

    // ========================================================================
    // Special Pattern Tests (MD4)
    // ========================================================================

    /// Test with identical inputs in a batch (verifies lane independence).
    #[test]
    fn simd_md4_batch_identical_inputs() {
        let data = b"identical input for all lanes";
        let inputs: Vec<&[u8]> = vec![data.as_slice(); 16];

        let batch_results = md4::digest_batch(&inputs);
        let reference = Md4::digest(data);

        for (i, result) in batch_results.iter().enumerate() {
            assert_eq!(
                *result, reference,
                "SIMD MD4 identical inputs mismatch at lane {i}"
            );
        }
    }

    /// Test with all-zero and all-0xFF inputs.
    #[test]
    fn simd_md4_batch_extreme_byte_values() {
        let sizes = [0, 1, 64, 128, 1000];
        for &size in &sizes {
            // All zeros
            let zeros = vec![0u8; size];
            let batch_z = md4::digest_batch(&[zeros.as_slice()]);
            assert_eq!(batch_z[0], Md4::digest(&zeros), "MD4 zeros size {size}");

            // All 0xFF
            let ones = vec![0xFF; size];
            let batch_f = md4::digest_batch(&[ones.as_slice()]);
            assert_eq!(batch_f[0], Md4::digest(&ones), "MD4 0xFF size {size}");
        }
    }

    // ========================================================================
    // Random Data Parity (MD4)
    // ========================================================================

    /// Test with pseudo-random data at various sizes.
    #[test]
    fn simd_md4_random_data_parity() {
        use rand::Rng;
        let mut rng = rand::thread_rng();

        let sizes = [
            0, 1, 15, 16, 17, 31, 32, 33, 55, 56, 63, 64, 65, 127, 128, 129, 255, 256, 512, 1000,
            4096, 10_000,
        ];

        for &size in &sizes {
            let data: Vec<u8> = (0..size).map(|_| rng.r#gen()).collect();
            let inputs = vec![data.as_slice()];

            let batch_result = md4::digest_batch(&inputs);
            let reference = Md4::digest(&data);

            assert_eq!(
                batch_result[0], reference,
                "SIMD MD4 random data mismatch for size {size}"
            );
        }
    }

    /// Test batch of random data (multiple inputs at once).
    #[test]
    fn simd_md4_batch_random_data_parity() {
        use rand::Rng;
        let mut rng = rand::thread_rng();

        let inputs: Vec<Vec<u8>> = (0..20)
            .map(|_| {
                let size = rng.gen_range(0..5000);
                (0..size).map(|_| rng.r#gen()).collect()
            })
            .collect();
        let input_refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();

        let batch_results = md4::digest_batch(&input_refs);

        for (i, input) in inputs.iter().enumerate() {
            let reference = Md4::digest(input);
            assert_eq!(
                batch_results[i],
                reference,
                "SIMD MD4 random batch mismatch at index {i} (size={})",
                input.len()
            );
        }
    }

    // ========================================================================
    // MD4 Single Digest via SIMD scalar matches strong::Md4
    // ========================================================================

    /// Verify the single-digest function in the SIMD module matches strong::Md4.
    #[test]
    fn simd_md4_single_digest_matches_strong() {
        let test_data: &[&[u8]] = &[
            b"",
            b"a",
            b"abc",
            b"message digest",
            b"The quick brown fox jumps over the lazy dog",
            &vec![0xAB; 1024],
            &vec![0xCD; 65536],
        ];

        for data in test_data {
            let simd_scalar = md4::digest(data);
            let reference = Md4::digest(data);
            assert_eq!(
                simd_scalar,
                reference,
                "MD4 single digest mismatch for size {}",
                data.len()
            );
        }
    }
}

// =============================================================================
// XXH3 SIMD vs Scalar Parity (one-shot SIMD vs streaming scalar)
// =============================================================================

#[cfg(test)]
mod xxh3_simd_parity {
    use crate::strong::{Xxh3, Xxh3_128, Xxh64};

    // ========================================================================
    // XXH3-64: one-shot (potentially SIMD) vs streaming (xxhash-rust scalar)
    // ========================================================================

    /// Verify XXH3-64 one-shot matches streaming for empty input.
    #[test]
    fn simd_xxh3_64_parity_empty() {
        let seed = 0u64;
        let one_shot = Xxh3::digest(seed, b"");

        let mut streaming = Xxh3::new(seed);
        streaming.update(b"");
        let streamed = streaming.finalize();

        assert_eq!(one_shot, streamed, "XXH3-64 empty input parity failed");
    }

    /// Verify XXH3-64 one-shot matches streaming across various sizes.
    #[test]
    fn simd_xxh3_64_parity_various_sizes() {
        let sizes: &[usize] = &[
            0, 1, 2, 3, 4, 7, 8, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 129, 240, 241, 255,
            256, 512, 1024, 4096, 8192, 16384, 65536,
        ];

        for &size in sizes {
            let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            for &seed in &[0u64, 1, 42, 0xDEADBEEF, u64::MAX] {
                let one_shot = Xxh3::digest(seed, &data);

                let mut streaming = Xxh3::new(seed);
                streaming.update(&data);
                let streamed = streaming.finalize();

                assert_eq!(
                    one_shot, streamed,
                    "XXH3-64 parity failed for size={size}, seed={seed}"
                );
            }
        }
    }

    /// Verify XXH3-64 at SIMD lane boundaries specifically.
    #[test]
    fn simd_xxh3_64_parity_lane_boundaries() {
        // XXH3 uses 256-byte stripes internally (4 x 64-byte lanes)
        let lane_sizes: &[usize] = &[16, 32, 64, 128, 240, 256, 512, 768, 1024, 1536, 2048, 4096];
        let seed = 12345u64;

        for &size in lane_sizes {
            let data: Vec<u8> = (0..size).map(|i| ((i * 7 + 3) % 256) as u8).collect();

            let one_shot = Xxh3::digest(seed, &data);

            let mut streaming = Xxh3::new(seed);
            streaming.update(&data);
            let streamed = streaming.finalize();

            assert_eq!(
                one_shot, streamed,
                "XXH3-64 lane boundary parity failed for size={size}"
            );
        }
    }

    /// Verify XXH3-64 one-shot matches streaming with chunked updates.
    #[test]
    fn simd_xxh3_64_parity_chunked_streaming() {
        let data: Vec<u8> = (0..10_000).map(|i| (i % 256) as u8).collect();
        let seed = 999u64;

        let one_shot = Xxh3::digest(seed, &data);

        // Stream in various chunk sizes
        for chunk_size in [1, 7, 16, 64, 128, 256, 1000, 4096] {
            let mut streaming = Xxh3::new(seed);
            for chunk in data.chunks(chunk_size) {
                streaming.update(chunk);
            }
            let streamed = streaming.finalize();

            assert_eq!(
                one_shot, streamed,
                "XXH3-64 chunked parity failed for chunk_size={chunk_size}"
            );
        }
    }

    // ========================================================================
    // XXH3-128: one-shot (potentially SIMD) vs streaming (xxhash-rust scalar)
    // ========================================================================

    /// Verify XXH3-128 one-shot matches streaming for empty input.
    #[test]
    fn simd_xxh3_128_parity_empty() {
        let seed = 0u64;
        let one_shot = Xxh3_128::digest(seed, b"");

        let mut streaming = Xxh3_128::new(seed);
        streaming.update(b"");
        let streamed = streaming.finalize();

        assert_eq!(one_shot, streamed, "XXH3-128 empty input parity failed");
    }

    /// Verify XXH3-128 one-shot matches streaming across various sizes.
    #[test]
    fn simd_xxh3_128_parity_various_sizes() {
        let sizes: &[usize] = &[
            0, 1, 2, 3, 4, 7, 8, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 129, 240, 241, 255,
            256, 512, 1024, 4096, 8192, 16384, 65536,
        ];

        for &size in sizes {
            let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            for &seed in &[0u64, 1, 42, 0xDEADBEEF, u64::MAX] {
                let one_shot = Xxh3_128::digest(seed, &data);

                let mut streaming = Xxh3_128::new(seed);
                streaming.update(&data);
                let streamed = streaming.finalize();

                assert_eq!(
                    one_shot, streamed,
                    "XXH3-128 parity failed for size={size}, seed={seed}"
                );
            }
        }
    }

    /// Verify XXH3-128 at SIMD lane boundaries.
    #[test]
    fn simd_xxh3_128_parity_lane_boundaries() {
        let lane_sizes: &[usize] = &[16, 32, 64, 128, 240, 256, 512, 768, 1024, 1536, 2048, 4096];
        let seed = 54321u64;

        for &size in lane_sizes {
            let data: Vec<u8> = (0..size).map(|i| ((i * 11 + 5) % 256) as u8).collect();

            let one_shot = Xxh3_128::digest(seed, &data);

            let mut streaming = Xxh3_128::new(seed);
            streaming.update(&data);
            let streamed = streaming.finalize();

            assert_eq!(
                one_shot, streamed,
                "XXH3-128 lane boundary parity failed for size={size}"
            );
        }
    }

    /// Verify XXH3-128 one-shot matches streaming with chunked updates.
    #[test]
    fn simd_xxh3_128_parity_chunked_streaming() {
        let data: Vec<u8> = (0..10_000).map(|i| (i % 256) as u8).collect();
        let seed = 777u64;

        let one_shot = Xxh3_128::digest(seed, &data);

        for chunk_size in [1, 7, 16, 64, 128, 256, 1000, 4096] {
            let mut streaming = Xxh3_128::new(seed);
            for chunk in data.chunks(chunk_size) {
                streaming.update(chunk);
            }
            let streamed = streaming.finalize();

            assert_eq!(
                one_shot, streamed,
                "XXH3-128 chunked parity failed for chunk_size={chunk_size}"
            );
        }
    }

    // ========================================================================
    // XXH64 Parity (streaming vs one-shot)
    // ========================================================================

    /// Verify XXH64 one-shot matches streaming across various sizes.
    #[test]
    fn simd_xxh64_parity_various_sizes() {
        let sizes: &[usize] = &[0, 1, 3, 4, 7, 8, 15, 16, 31, 32, 64, 128, 256, 1024, 10_000];

        for &size in sizes {
            let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            for &seed in &[0u64, 42, 0xCAFEBABE, u64::MAX] {
                let one_shot = Xxh64::digest(seed, &data);

                let mut streaming = Xxh64::new(seed);
                streaming.update(&data);
                let streamed = streaming.finalize();

                assert_eq!(
                    one_shot, streamed,
                    "XXH64 parity failed for size={size}, seed={seed}"
                );
            }
        }
    }

    // ========================================================================
    // XXH3 Random Data Parity
    // ========================================================================

    /// Test XXH3-64 with random data at various sizes.
    #[test]
    fn simd_xxh3_64_random_data_parity() {
        use rand::Rng;
        let mut rng = rand::thread_rng();

        for _ in 0..50 {
            let size = rng.gen_range(0..20_000);
            let data: Vec<u8> = (0..size).map(|_| rng.r#gen()).collect();
            let seed: u64 = rng.r#gen();

            let one_shot = Xxh3::digest(seed, &data);

            let mut streaming = Xxh3::new(seed);
            streaming.update(&data);
            let streamed = streaming.finalize();

            assert_eq!(
                one_shot, streamed,
                "XXH3-64 random parity failed for size={size}, seed={seed}"
            );
        }
    }

    /// Test XXH3-128 with random data at various sizes.
    #[test]
    fn simd_xxh3_128_random_data_parity() {
        use rand::Rng;
        let mut rng = rand::thread_rng();

        for _ in 0..50 {
            let size = rng.gen_range(0..20_000);
            let data: Vec<u8> = (0..size).map(|_| rng.r#gen()).collect();
            let seed: u64 = rng.r#gen();

            let one_shot = Xxh3_128::digest(seed, &data);

            let mut streaming = Xxh3_128::new(seed);
            streaming.update(&data);
            let streamed = streaming.finalize();

            assert_eq!(
                one_shot, streamed,
                "XXH3-128 random parity failed for size={size}, seed={seed}"
            );
        }
    }

    // ========================================================================
    // XXH3 SIMD Availability Check
    // ========================================================================

    /// Verify SIMD availability query reports true (xxh3 crate always compiled in).
    #[test]
    fn simd_xxh3_availability_consistent() {
        assert!(
            crate::xxh3_simd_available(),
            "xxh3 crate is always compiled in, should report true"
        );
    }
}

// =============================================================================
// Cross-Implementation Consistency: strong:: digest_batch vs strong:: digest
// =============================================================================

#[cfg(test)]
mod digest_batch_parity {
    use crate::strong::{Md4, Md5, md4_digest_batch, md5_digest_batch};

    /// Verify md5_digest_batch matches sequential Md5::digest for many sizes.
    #[test]
    fn simd_parity_md5_digest_batch_matches_sequential() {
        let inputs: Vec<Vec<u8>> = (0..50)
            .map(|i| (0..(i * 37 + 1)).map(|j| ((i + j) % 256) as u8).collect())
            .collect();
        let input_refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();

        let batch = md5_digest_batch(&input_refs);
        let sequential: Vec<[u8; 16]> = inputs.iter().map(|v| Md5::digest(v)).collect();

        assert_eq!(batch.len(), sequential.len());
        for (i, (b, s)) in batch.iter().zip(sequential.iter()).enumerate() {
            assert_eq!(
                b,
                s,
                "MD5 digest_batch vs sequential mismatch at index {i} (size={})",
                inputs[i].len()
            );
        }
    }

    /// Verify md4_digest_batch matches sequential Md4::digest for many sizes.
    #[test]
    fn simd_parity_md4_digest_batch_matches_sequential() {
        let inputs: Vec<Vec<u8>> = (0..50)
            .map(|i| (0..(i * 37 + 1)).map(|j| ((i + j) % 256) as u8).collect())
            .collect();
        let input_refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();

        let batch = md4_digest_batch(&input_refs);
        let sequential: Vec<[u8; 16]> = inputs.iter().map(|v| Md4::digest(v)).collect();

        assert_eq!(batch.len(), sequential.len());
        for (i, (b, s)) in batch.iter().zip(sequential.iter()).enumerate() {
            assert_eq!(
                b,
                s,
                "MD4 digest_batch vs sequential mismatch at index {i} (size={})",
                inputs[i].len()
            );
        }
    }

    /// Test with empty, single-byte, and boundary-size inputs.
    #[test]
    fn simd_parity_md5_batch_boundary_inputs() {
        let inputs: Vec<Vec<u8>> = vec![
            vec![],                              // empty
            vec![0x42],                          // 1 byte
            (0..55).map(|i| i as u8).collect(),  // max single-block padding
            (0..56).map(|i| i as u8).collect(),  // requires 2-block padding
            (0..63).map(|i| i as u8).collect(),  // 1 byte short of block
            (0..64).map(|i| i as u8).collect(),  // exact block
            (0..65).map(|i| i as u8).collect(),  // 1 byte over block
            (0..128).map(|i| i as u8).collect(), // 2 exact blocks
        ];
        let input_refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();

        let batch = md5_digest_batch(&input_refs);

        for (i, input) in inputs.iter().enumerate() {
            let reference = Md5::digest(input);
            assert_eq!(
                batch[i],
                reference,
                "MD5 batch boundary mismatch at index {i} (size={})",
                input.len()
            );
        }
    }

    /// Test with empty, single-byte, and boundary-size inputs for MD4.
    #[test]
    fn simd_parity_md4_batch_boundary_inputs() {
        let inputs: Vec<Vec<u8>> = vec![
            vec![],
            vec![0x42],
            (0..55).map(|i| i as u8).collect(),
            (0..56).map(|i| i as u8).collect(),
            (0..63).map(|i| i as u8).collect(),
            (0..64).map(|i| i as u8).collect(),
            (0..65).map(|i| i as u8).collect(),
            (0..128).map(|i| i as u8).collect(),
        ];
        let input_refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();

        let batch = md4_digest_batch(&input_refs);

        for (i, input) in inputs.iter().enumerate() {
            let reference = Md4::digest(input);
            assert_eq!(
                batch[i],
                reference,
                "MD4 batch boundary mismatch at index {i} (size={})",
                input.len()
            );
        }
    }
}

// =============================================================================
// Proptest-Based Parity (MD5 SIMD batch vs scalar)
// =============================================================================

#[cfg(test)]
mod proptest_simd_parity {
    use crate::simd_batch;
    use crate::strong::{Md4, Md5, md4_digest_batch, md5_digest_batch};
    use proptest::prelude::*;

    proptest! {
        /// Property test: SIMD MD5 batch always matches sequential Md5::digest.
        #[test]
        fn prop_simd_md5_batch_matches_scalar(data in proptest::collection::vec(any::<u8>(), 0..2000)) {
            let inputs = vec![data.as_slice()];
            let batch_result = simd_batch::digest_batch(&inputs);
            let reference = Md5::digest(&data);
            prop_assert_eq!(batch_result[0], reference);
        }

        /// Property test: SIMD MD4 batch always matches sequential Md4::digest.
        #[test]
        fn prop_simd_md4_batch_matches_scalar(data in proptest::collection::vec(any::<u8>(), 0..2000)) {
            let inputs = vec![data.as_slice()];
            let batch_result = simd_batch::md4::digest_batch(&inputs);
            let reference = Md4::digest(&data);
            prop_assert_eq!(batch_result[0], reference);
        }

        /// Property test: md5_digest_batch matches Md5::digest for arbitrary data.
        #[test]
        fn prop_md5_digest_batch_matches_sequential(data in proptest::collection::vec(any::<u8>(), 0..2000)) {
            let inputs = vec![data.as_slice()];
            let batch = md5_digest_batch(&inputs);
            let reference = Md5::digest(&data);
            prop_assert_eq!(batch[0], reference);
        }

        /// Property test: md4_digest_batch matches Md4::digest for arbitrary data.
        #[test]
        fn prop_md4_digest_batch_matches_sequential(data in proptest::collection::vec(any::<u8>(), 0..2000)) {
            let inputs = vec![data.as_slice()];
            let batch = md4_digest_batch(&inputs);
            let reference = Md4::digest(&data);
            prop_assert_eq!(batch[0], reference);
        }

        /// Property test: Multiple inputs in a batch all match.
        #[test]
        fn prop_simd_md5_multi_input_batch(
            data1 in proptest::collection::vec(any::<u8>(), 0..500),
            data2 in proptest::collection::vec(any::<u8>(), 0..500),
            data3 in proptest::collection::vec(any::<u8>(), 0..500),
            data4 in proptest::collection::vec(any::<u8>(), 0..500),
        ) {
            let inputs: Vec<&[u8]> = vec![&data1, &data2, &data3, &data4];
            let batch = simd_batch::digest_batch(&inputs);

            prop_assert_eq!(batch[0], Md5::digest(&data1));
            prop_assert_eq!(batch[1], Md5::digest(&data2));
            prop_assert_eq!(batch[2], Md5::digest(&data3));
            prop_assert_eq!(batch[3], Md5::digest(&data4));
        }

        /// Property test: Multiple MD4 inputs in a batch all match.
        #[test]
        fn prop_simd_md4_multi_input_batch(
            data1 in proptest::collection::vec(any::<u8>(), 0..500),
            data2 in proptest::collection::vec(any::<u8>(), 0..500),
            data3 in proptest::collection::vec(any::<u8>(), 0..500),
            data4 in proptest::collection::vec(any::<u8>(), 0..500),
        ) {
            let inputs: Vec<&[u8]> = vec![&data1, &data2, &data3, &data4];
            let batch = simd_batch::md4::digest_batch(&inputs);

            prop_assert_eq!(batch[0], Md4::digest(&data1));
            prop_assert_eq!(batch[1], Md4::digest(&data2));
            prop_assert_eq!(batch[2], Md4::digest(&data3));
            prop_assert_eq!(batch[3], Md4::digest(&data4));
        }
    }
}
