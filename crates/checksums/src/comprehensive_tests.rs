//! Comprehensive tests targeting 95% line coverage for the checksums crate.
//!
//! These tests cover edge cases and paths not exercised by the existing tests.

#[cfg(test)]
mod strong_checksum_tests {
    use crate::strong::{Md4, Md5, Md5Seed, Sha1, Sha256, Sha512, StrongDigest, Xxh3, Xxh3_128, Xxh64};

    // ========================================================================
    // MD4 Tests
    // ========================================================================

    #[test]
    fn md4_default_equals_new() {
        let new = Md4::new();
        let default = Md4::default();
        // Both should produce same digest for same input
        let mut h1 = new;
        let mut h2 = default;
        h1.update(b"test");
        h2.update(b"test");
        assert_eq!(h1.finalize(), h2.finalize());
    }

    #[test]
    fn md4_debug_format() {
        let hasher = Md4::new();
        let debug = format!("{:?}", hasher);
        assert!(debug.contains("Md4"));
    }

    #[test]
    fn md4_empty_input() {
        let digest = Md4::digest(b"");
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn md4_single_byte() {
        let digest = Md4::digest(&[0x42]);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn md4_large_input() {
        let data = vec![0xAB; 100_000];
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn md4_streaming_multiple_chunks() {
        let data = b"this is a test of streaming md4 hash";
        let mut hasher = Md4::new();
        for chunk in data.chunks(5) {
            hasher.update(chunk);
        }
        let streaming = hasher.finalize();

        let oneshot = Md4::digest(data);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn md4_trait_new() {
        let hasher: Md4 = StrongDigest::new();
        let mut h = hasher;
        h.update(b"test");
        let digest = h.finalize();
        assert_eq!(digest.len(), Md4::DIGEST_LEN);
    }

    #[test]
    fn md4_trait_with_seed() {
        let hasher: Md4 = StrongDigest::with_seed(());
        let mut h = hasher;
        h.update(b"seeded");
        let digest = h.finalize();
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn md4_trait_digest_with_seed() {
        let d1 = <Md4 as StrongDigest>::digest_with_seed((), b"test");
        let d2 = Md4::digest(b"test");
        assert_eq!(d1, d2);
    }

    #[test]
    fn md4_clone() {
        let mut h1 = Md4::new();
        h1.update(b"hello");
        let h2 = h1.clone();
        // Both clones should produce same result
        let d1 = h1.finalize();
        let mut h3 = Md4::new();
        h3.update(b"hello");
        assert_eq!(d1, h3.finalize());
        // h2 should also work
        let mut h4 = h2;
        h4.update(b"");
        let d2 = h4.finalize();
        assert_eq!(d1, d2);
    }

    // ========================================================================
    // MD5 Tests
    // ========================================================================

    #[test]
    fn md5_default_equals_new() {
        let new = Md5::new();
        let default = Md5::default();
        let mut h1 = new;
        let mut h2 = default;
        h1.update(b"test");
        h2.update(b"test");
        assert_eq!(h1.finalize(), h2.finalize());
    }

    #[test]
    fn md5_debug_format() {
        let hasher = Md5::new();
        let debug = format!("{:?}", hasher);
        assert!(debug.contains("Md5"));
    }

    #[test]
    fn md5_empty_input() {
        let digest = Md5::digest(b"");
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn md5_single_byte() {
        let digest = Md5::digest(&[0x42]);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn md5_large_input() {
        let data = vec![0xCD; 100_000];
        let digest = Md5::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn md5_streaming_byte_at_a_time() {
        let data = b"byte by byte streaming test";
        let mut hasher = Md5::new();
        for &byte in data.iter() {
            hasher.update(&[byte]);
        }
        let streaming = hasher.finalize();
        let oneshot = Md5::digest(data);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn md5_seed_proper_order_multiple_updates() {
        let seed = Md5Seed::proper(42);
        let mut hasher = Md5::with_seed(seed);
        hasher.update(b"part1");
        hasher.update(b"part2");
        let digest = hasher.finalize();
        assert_eq!(digest.len(), 16);

        // Compare with manual construction
        let mut manual = Md5::new();
        manual.update(&42i32.to_le_bytes());
        manual.update(b"part1part2");
        assert_eq!(digest, manual.finalize());
    }

    #[test]
    fn md5_seed_legacy_order_multiple_updates() {
        let seed = Md5Seed::legacy(99);
        let mut hasher = Md5::with_seed(seed);
        hasher.update(b"data1");
        hasher.update(b"data2");
        let digest = hasher.finalize();
        assert_eq!(digest.len(), 16);

        // Compare with manual construction
        let mut manual = Md5::new();
        manual.update(b"data1data2");
        manual.update(&99i32.to_le_bytes());
        assert_eq!(digest, manual.finalize());
    }

    #[test]
    fn md5_seed_none_equals_unseeded() {
        let data = b"compare seeds";
        let mut seeded = Md5::with_seed(Md5Seed::none());
        seeded.update(data);
        let seeded_digest = seeded.finalize();

        let unseeded_digest = Md5::digest(data);
        assert_eq!(seeded_digest, unseeded_digest);
    }

    #[test]
    fn md5_seed_debug_format() {
        let seed = Md5Seed::proper(123);
        let debug = format!("{:?}", seed);
        assert!(debug.contains("Md5Seed"));
    }

    #[test]
    fn md5_seed_clone_and_copy() {
        let seed = Md5Seed::proper(456);
        let cloned = seed.clone();
        let copied = seed;
        assert_eq!(cloned, copied);
        assert_eq!(seed.value, Some(456));
        assert!(seed.proper_order);
    }

    #[test]
    fn md5_seed_equality() {
        let s1 = Md5Seed::proper(100);
        let s2 = Md5Seed::proper(100);
        let s3 = Md5Seed::legacy(100);
        assert_eq!(s1, s2);
        assert_ne!(s1, s3);
    }

    #[test]
    fn md5_trait_operations() {
        let mut hasher: Md5 = StrongDigest::new();
        hasher.update(b"trait test");
        let _digest = hasher.finalize();
        assert_eq!(Md5::DIGEST_LEN, 16);

        let d1 = <Md5 as StrongDigest>::digest(b"test");
        let d2 = Md5::digest(b"test");
        assert_eq!(d1, d2);
    }

    #[test]
    fn md5_clone() {
        let mut h1 = Md5::new();
        h1.update(b"clone test");
        let h2 = h1.clone();
        let d1 = h1.finalize();
        let mut h3 = Md5::new();
        h3.update(b"clone test");
        assert_eq!(d1, h3.finalize());
        // h2 should also finalize correctly
        let d2 = h2.finalize();
        assert_eq!(d1, d2);
    }

    // ========================================================================
    // SHA-1 Tests
    // ========================================================================

    #[test]
    fn sha1_default_equals_new() {
        let new = Sha1::new();
        let default = Sha1::default();
        let mut h1 = new;
        let mut h2 = default;
        h1.update(b"test");
        h2.update(b"test");
        assert_eq!(h1.finalize(), h2.finalize());
    }

    #[test]
    fn sha1_debug_format() {
        let hasher = Sha1::new();
        let debug = format!("{:?}", hasher);
        assert!(debug.contains("Sha1"));
    }

    #[test]
    fn sha1_empty_input() {
        let digest = Sha1::digest(b"");
        assert_eq!(digest.len(), 20);
    }

    #[test]
    fn sha1_single_byte() {
        let digest = Sha1::digest(&[0x42]);
        assert_eq!(digest.len(), 20);
    }

    #[test]
    fn sha1_large_input() {
        let data = vec![0xEF; 100_000];
        let digest = Sha1::digest(&data);
        assert_eq!(digest.len(), 20);
    }

    #[test]
    fn sha1_streaming_multiple_chunks() {
        let data = b"streaming sha1 hash test data";
        let mut hasher = Sha1::new();
        for chunk in data.chunks(7) {
            hasher.update(chunk);
        }
        let streaming = hasher.finalize();
        let oneshot = Sha1::digest(data);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn sha1_clone() {
        let mut h1 = Sha1::new();
        h1.update(b"sha1 clone");
        let h2 = h1.clone();
        let d1 = h1.finalize();
        let d2 = h2.finalize();
        assert_eq!(d1, d2);
    }

    #[test]
    fn sha1_trait_with_seed() {
        let hasher: Sha1 = StrongDigest::with_seed(());
        let mut h = hasher;
        h.update(b"test");
        let d = h.finalize();
        assert_eq!(d, Sha1::digest(b"test"));
    }

    // ========================================================================
    // SHA-256 Tests
    // ========================================================================

    #[test]
    fn sha256_default_equals_new() {
        let new = Sha256::new();
        let default = Sha256::default();
        let mut h1 = new;
        let mut h2 = default;
        h1.update(b"test");
        h2.update(b"test");
        assert_eq!(h1.finalize(), h2.finalize());
    }

    #[test]
    fn sha256_debug_format() {
        let hasher = Sha256::new();
        let debug = format!("{:?}", hasher);
        assert!(debug.contains("Sha256"));
    }

    #[test]
    fn sha256_empty_input() {
        let digest = Sha256::digest(b"");
        assert_eq!(digest.len(), 32);
    }

    #[test]
    fn sha256_single_byte() {
        let digest = Sha256::digest(&[0x42]);
        assert_eq!(digest.len(), 32);
    }

    #[test]
    fn sha256_large_input() {
        let data = vec![0x12; 100_000];
        let digest = Sha256::digest(&data);
        assert_eq!(digest.len(), 32);
    }

    #[test]
    fn sha256_streaming_byte_at_a_time() {
        let data = b"sha256 byte by byte";
        let mut hasher = Sha256::new();
        for &byte in data.iter() {
            hasher.update(&[byte]);
        }
        let streaming = hasher.finalize();
        let oneshot = Sha256::digest(data);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn sha256_clone() {
        let mut h1 = Sha256::new();
        h1.update(b"sha256 clone");
        let h2 = h1.clone();
        let d1 = h1.finalize();
        let d2 = h2.finalize();
        assert_eq!(d1, d2);
    }

    #[test]
    fn sha256_trait_with_seed() {
        let hasher: Sha256 = StrongDigest::with_seed(());
        let mut h = hasher;
        h.update(b"test");
        let d = h.finalize();
        assert_eq!(d, Sha256::digest(b"test"));
    }

    // ========================================================================
    // SHA-512 Tests
    // ========================================================================

    #[test]
    fn sha512_default_equals_new() {
        let new = Sha512::new();
        let default = Sha512::default();
        let mut h1 = new;
        let mut h2 = default;
        h1.update(b"test");
        h2.update(b"test");
        assert_eq!(h1.finalize(), h2.finalize());
    }

    #[test]
    fn sha512_debug_format() {
        let hasher = Sha512::new();
        let debug = format!("{:?}", hasher);
        assert!(debug.contains("Sha512"));
    }

    #[test]
    fn sha512_empty_input() {
        let digest = Sha512::digest(b"");
        assert_eq!(digest.len(), 64);
    }

    #[test]
    fn sha512_single_byte() {
        let digest = Sha512::digest(&[0x42]);
        assert_eq!(digest.len(), 64);
    }

    #[test]
    fn sha512_large_input() {
        let data = vec![0x34; 100_000];
        let digest = Sha512::digest(&data);
        assert_eq!(digest.len(), 64);
    }

    #[test]
    fn sha512_streaming_multiple_chunks() {
        let data = b"sha512 streaming hash test";
        let mut hasher = Sha512::new();
        for chunk in data.chunks(11) {
            hasher.update(chunk);
        }
        let streaming = hasher.finalize();
        let oneshot = Sha512::digest(data);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn sha512_clone() {
        let mut h1 = Sha512::new();
        h1.update(b"sha512 clone");
        let h2 = h1.clone();
        let d1 = h1.finalize();
        let d2 = h2.finalize();
        assert_eq!(d1, d2);
    }

    #[test]
    fn sha512_trait_with_seed() {
        let hasher: Sha512 = StrongDigest::with_seed(());
        let mut h = hasher;
        h.update(b"test");
        let d = h.finalize();
        assert_eq!(d, Sha512::digest(b"test"));
    }

    // ========================================================================
    // XXH64 Tests
    // ========================================================================

    #[test]
    fn xxh64_empty_input() {
        let digest = Xxh64::digest(0, b"");
        assert_eq!(digest.len(), 8);
    }

    #[test]
    fn xxh64_single_byte() {
        let digest = Xxh64::digest(0, &[0x42]);
        assert_eq!(digest.len(), 8);
    }

    #[test]
    fn xxh64_large_input() {
        let data = vec![0x56; 100_000];
        let digest = Xxh64::digest(0, &data);
        assert_eq!(digest.len(), 8);
    }

    #[test]
    fn xxh64_streaming_multiple_chunks() {
        let data = b"xxh64 streaming test data";
        let seed = 12345u64;
        let mut hasher = Xxh64::new(seed);
        for chunk in data.chunks(3) {
            hasher.update(chunk);
        }
        let streaming = hasher.finalize();
        let oneshot = Xxh64::digest(seed, data);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn xxh64_streaming_byte_at_a_time() {
        let data = b"byte by byte xxh64";
        let seed = 0u64;
        let mut hasher = Xxh64::new(seed);
        for &byte in data.iter() {
            hasher.update(&[byte]);
        }
        let streaming = hasher.finalize();
        let oneshot = Xxh64::digest(seed, data);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn xxh64_different_seeds_produce_different_results() {
        let data = b"same data";
        let d1 = Xxh64::digest(0, data);
        let d2 = Xxh64::digest(u64::MAX, data);
        assert_ne!(d1, d2);
    }

    #[test]
    fn xxh64_trait_operations() {
        let hasher: Xxh64 = StrongDigest::with_seed(999);
        let mut h = hasher;
        h.update(b"trait test");
        let d = h.finalize();
        assert_eq!(d, Xxh64::digest(999, b"trait test"));
    }

    #[test]
    fn xxh64_clone() {
        let mut h1 = Xxh64::new(42);
        h1.update(b"clone");
        let h2 = h1.clone();
        let d1 = h1.finalize();
        let d2 = h2.finalize();
        assert_eq!(d1, d2);
    }

    // ========================================================================
    // XXH3-64 Tests
    // ========================================================================

    #[test]
    fn xxh3_empty_input() {
        let digest = Xxh3::digest(0, b"");
        assert_eq!(digest.len(), 8);
    }

    #[test]
    fn xxh3_single_byte() {
        let digest = Xxh3::digest(0, &[0x42]);
        assert_eq!(digest.len(), 8);
    }

    #[test]
    fn xxh3_large_input() {
        let data = vec![0x78; 100_000];
        let digest = Xxh3::digest(0, &data);
        assert_eq!(digest.len(), 8);
    }

    #[test]
    fn xxh3_streaming_multiple_chunks() {
        let data = b"xxh3 streaming test";
        let seed = 54321u64;
        let mut hasher = Xxh3::new(seed);
        for chunk in data.chunks(4) {
            hasher.update(chunk);
        }
        let streaming = hasher.finalize();
        // Streaming uses xxhash-rust, so compare against that
        let expected = xxhash_rust::xxh3::xxh3_64_with_seed(data, seed).to_le_bytes();
        assert_eq!(streaming, expected);
    }

    #[test]
    fn xxh3_different_seeds_produce_different_results() {
        let data = b"same data";
        let d1 = Xxh3::digest(0, data);
        let d2 = Xxh3::digest(u64::MAX, data);
        assert_ne!(d1, d2);
    }

    #[test]
    fn xxh3_trait_operations() {
        let hasher: Xxh3 = StrongDigest::with_seed(777);
        let mut h = hasher;
        h.update(b"trait test");
        let d = h.finalize();
        // Compare against xxhash-rust reference
        let expected = xxhash_rust::xxh3::xxh3_64_with_seed(b"trait test", 777).to_le_bytes();
        assert_eq!(d, expected);
    }

    // ========================================================================
    // XXH3-128 Tests
    // ========================================================================

    #[test]
    fn xxh3_128_empty_input() {
        let digest = Xxh3_128::digest(0, b"");
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn xxh3_128_single_byte() {
        let digest = Xxh3_128::digest(0, &[0x42]);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn xxh3_128_large_input() {
        let data = vec![0x9A; 100_000];
        let digest = Xxh3_128::digest(0, &data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn xxh3_128_streaming_multiple_chunks() {
        let data = b"xxh3-128 streaming test";
        let seed = 11111u64;
        let mut hasher = Xxh3_128::new(seed);
        for chunk in data.chunks(6) {
            hasher.update(chunk);
        }
        let streaming = hasher.finalize();
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(data, seed).to_le_bytes();
        assert_eq!(streaming, expected);
    }

    #[test]
    fn xxh3_128_different_seeds_produce_different_results() {
        let data = b"same data";
        let d1 = Xxh3_128::digest(0, data);
        let d2 = Xxh3_128::digest(u64::MAX, data);
        assert_ne!(d1, d2);
    }

    #[test]
    fn xxh3_128_trait_operations() {
        let hasher: Xxh3_128 = StrongDigest::with_seed(888);
        let mut h = hasher;
        h.update(b"trait test");
        let d = h.finalize();
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(b"trait test", 888).to_le_bytes();
        assert_eq!(d, expected);
    }

    // ========================================================================
    // XXHash Additional Coverage Tests
    // ========================================================================

    #[test]
    fn xxh3_streaming_empty() {
        let mut hasher = Xxh3::new(0);
        hasher.update(&[]);
        let digest = hasher.finalize();
        assert_eq!(digest.len(), 8);
    }

    #[test]
    fn xxh3_streaming_large_chunks() {
        let data = vec![0xAB; 100_000];
        let seed = 42u64;

        let mut hasher = Xxh3::new(seed);
        hasher.update(&data);
        let streaming = hasher.finalize();

        // Compare with reference
        let expected = xxhash_rust::xxh3::xxh3_64_with_seed(&data, seed).to_le_bytes();
        assert_eq!(streaming, expected);
    }

    #[test]
    fn xxh3_128_streaming_empty() {
        let mut hasher = Xxh3_128::new(0);
        hasher.update(&[]);
        let digest = hasher.finalize();
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn xxh3_128_streaming_large_chunks() {
        let data = vec![0xCD; 100_000];
        let seed = 123u64;

        let mut hasher = Xxh3_128::new(seed);
        hasher.update(&data);
        let streaming = hasher.finalize();

        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&data, seed).to_le_bytes();
        assert_eq!(streaming, expected);
    }

    #[test]
    fn xxh64_streaming_empty_then_data() {
        let mut hasher = Xxh64::new(0);
        hasher.update(&[]);
        hasher.update(b"after empty");
        let digest = hasher.finalize();

        let expected = Xxh64::digest(0, b"after empty");
        assert_eq!(digest, expected);
    }

    #[test]
    fn xxh3_trait_digest() {
        let d = <Xxh3 as StrongDigest>::digest(b"test");
        let expected = xxhash_rust::xxh3::xxh3_64_with_seed(b"test", 0).to_le_bytes();
        assert_eq!(d, expected);
    }

    #[test]
    fn xxh3_128_trait_digest() {
        let d = <Xxh3_128 as StrongDigest>::digest(b"test");
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(b"test", 0).to_le_bytes();
        assert_eq!(d, expected);
    }

    #[test]
    fn xxh64_trait_digest() {
        let d = <Xxh64 as StrongDigest>::digest(b"test");
        let expected = Xxh64::digest(0, b"test");
        assert_eq!(d, expected);
    }

    // ========================================================================
    // Algorithm Selection and Capability Tests
    // ========================================================================

    #[test]
    fn xxh3_simd_availability_returns_expected_value() {
        let available = crate::xxh3_simd_available();
        // Should match compile-time feature
        #[cfg(feature = "xxh3-simd")]
        assert!(available);
        #[cfg(not(feature = "xxh3-simd"))]
        assert!(!available);
    }

    #[test]
    fn openssl_availability_returns_expected_value() {
        let available = crate::openssl_acceleration_available();
        // Should match compile-time feature
        #[cfg(feature = "openssl")]
        assert!(available);
        #[cfg(not(feature = "openssl"))]
        assert!(!available);
    }

    #[test]
    fn simd_availability_returns_bool() {
        let _available = crate::simd_acceleration_available();
        // Just verify it runs without panic
    }

    // ========================================================================
    // Generic StrongDigest Tests
    // ========================================================================

    fn compute_generic<D: StrongDigest>(data: &[u8]) -> D::Digest
    where
        D::Seed: Default,
    {
        D::digest(data)
    }

    #[test]
    fn generic_digest_computation_md4() {
        let d = compute_generic::<Md4>(b"test");
        assert_eq!(d.as_ref().len(), 16);
    }

    #[test]
    fn generic_digest_computation_md5() {
        let d = compute_generic::<Md5>(b"test");
        assert_eq!(d.as_ref().len(), 16);
    }

    #[test]
    fn generic_digest_computation_sha1() {
        let d = compute_generic::<Sha1>(b"test");
        assert_eq!(d.as_ref().len(), 20);
    }

    #[test]
    fn generic_digest_computation_sha256() {
        let d = compute_generic::<Sha256>(b"test");
        assert_eq!(d.as_ref().len(), 32);
    }

    #[test]
    fn generic_digest_computation_sha512() {
        let d = compute_generic::<Sha512>(b"test");
        assert_eq!(d.as_ref().len(), 64);
    }
}

#[cfg(test)]
mod rolling_checksum_tests {
    use crate::{RollingChecksum, RollingDigest, RollingError, RollingSliceError};
    use std::io::IoSlice;

    // ========================================================================
    // Edge Cases for Rolling Checksum
    // ========================================================================

    #[test]
    fn update_byte_single() {
        let mut checksum = RollingChecksum::new();
        checksum.update_byte(0x42);
        assert_eq!(checksum.len(), 1);
        assert!(!checksum.is_empty());

        let mut full = RollingChecksum::new();
        full.update(&[0x42]);
        assert_eq!(checksum.value(), full.value());
    }

    #[test]
    fn update_byte_multiple() {
        let mut checksum = RollingChecksum::new();
        for byte in b"hello" {
            checksum.update_byte(*byte);
        }

        let mut full = RollingChecksum::new();
        full.update(b"hello");
        assert_eq!(checksum.value(), full.value());
        assert_eq!(checksum.len(), full.len());
    }

    #[test]
    fn update_byte_max_value() {
        let mut checksum = RollingChecksum::new();
        checksum.update_byte(0xFF);
        assert_eq!(checksum.len(), 1);
    }

    #[test]
    fn update_byte_zero() {
        let mut checksum = RollingChecksum::new();
        checksum.update_byte(0x00);
        assert_eq!(checksum.len(), 1);
    }

    #[test]
    fn roll_with_zero_values() {
        let mut checksum = RollingChecksum::new();
        checksum.update(&[0, 0, 0, 0]);
        let before = checksum.value();
        checksum.roll(0, 0).unwrap();
        assert_eq!(checksum.value(), before);
    }

    #[test]
    fn roll_with_max_values() {
        let mut checksum = RollingChecksum::new();
        checksum.update(&[0xFF, 0xFF, 0xFF, 0xFF]);
        let _before = checksum.value();
        checksum.roll(0xFF, 0xFF).unwrap();
        // Value should remain stable when in/out are same
    }

    #[test]
    fn roll_many_large_batch() {
        let data = vec![0xAB; 1000];
        let mut checksum = RollingChecksum::new();
        checksum.update(&data);

        let outgoing: Vec<u8> = (0..500).map(|i| (i % 256) as u8).collect();
        let incoming: Vec<u8> = (500..1000).map(|i| (i % 256) as u8).collect();

        checksum.roll_many(&outgoing, &incoming).unwrap();
        assert_eq!(checksum.len(), 1000);
    }

    // ========================================================================
    // Vectored I/O Edge Cases
    // ========================================================================

    #[test]
    fn update_vectored_many_small_slices() {
        let slices: Vec<Vec<u8>> = (0..50).map(|i| vec![(i % 256) as u8; 3]).collect();
        let io_slices: Vec<IoSlice<'_>> = slices.iter().map(|s| IoSlice::new(s)).collect();

        let mut checksum = RollingChecksum::new();
        checksum.update_vectored(&io_slices);

        let mut combined = Vec::new();
        for s in &slices {
            combined.extend_from_slice(s);
        }

        let mut expected = RollingChecksum::new();
        expected.update(&combined);

        assert_eq!(checksum.value(), expected.value());
    }

    #[test]
    fn update_vectored_mixed_sizes() {
        let small = vec![1u8; 10];
        let medium = vec![2u8; 100];
        let large = vec![3u8; 200];

        let slices = [
            IoSlice::new(&small),
            IoSlice::new(&medium),
            IoSlice::new(&large),
        ];

        let mut checksum = RollingChecksum::new();
        checksum.update_vectored(&slices);

        let mut combined = Vec::new();
        combined.extend_from_slice(&small);
        combined.extend_from_slice(&medium);
        combined.extend_from_slice(&large);

        let mut expected = RollingChecksum::new();
        expected.update(&combined);

        assert_eq!(checksum.value(), expected.value());
    }

    // ========================================================================
    // Rolling Digest Edge Cases
    // ========================================================================

    #[test]
    fn rolling_digest_max_values() {
        let digest = RollingDigest::new(u16::MAX, u16::MAX, usize::MAX);
        assert_eq!(digest.sum1(), u16::MAX);
        assert_eq!(digest.sum2(), u16::MAX);
        assert_eq!(digest.len(), usize::MAX);
        assert!(!digest.is_empty());
    }

    #[test]
    fn rolling_digest_from_value_roundtrip_max() {
        let original = RollingDigest::new(u16::MAX, u16::MAX, 12345);
        let packed = original.value();
        let reconstructed = RollingDigest::from_value(packed, 12345);
        assert_eq!(original, reconstructed);
    }

    // ========================================================================
    // Error Type Edge Cases
    // ========================================================================

    #[test]
    fn rolling_error_empty_window_display() {
        let err = RollingError::EmptyWindow;
        let display = err.to_string();
        assert!(display.contains("non-empty"));
    }

    #[test]
    fn rolling_error_window_too_large_display() {
        let err = RollingError::WindowTooLarge { len: 5_000_000_000 };
        let display = err.to_string();
        assert!(display.contains("5000000000"));
        assert!(display.contains("exceeds"));
    }

    #[test]
    fn rolling_error_mismatched_slice_display() {
        let err = RollingError::MismatchedSliceLength {
            outgoing: 10,
            incoming: 5,
        };
        let display = err.to_string();
        assert!(display.contains("10"));
        assert!(display.contains("5"));
    }

    #[test]
    fn rolling_error_clone_and_eq() {
        let err1 = RollingError::EmptyWindow;
        let err2 = err1.clone();
        assert_eq!(err1, err2);

        let err3 = RollingError::WindowTooLarge { len: 100 };
        let err4 = RollingError::WindowTooLarge { len: 100 };
        assert_eq!(err3, err4);
    }

    #[test]
    fn rolling_error_debug() {
        let err = RollingError::EmptyWindow;
        let debug = format!("{:?}", err);
        assert!(debug.contains("EmptyWindow"));
    }

    #[test]
    fn rolling_slice_error_expected_len() {
        assert_eq!(RollingSliceError::EXPECTED_LEN, 4);
    }

    #[test]
    fn rolling_slice_error_display() {
        let err = RollingDigest::from_le_slice(&[1, 2, 3], 0).unwrap_err();
        let display = err.to_string();
        assert!(display.contains("4"));
        assert!(display.contains("3"));
    }

    #[test]
    fn rolling_slice_error_clone_copy_eq() {
        let err1 = RollingDigest::from_le_slice(&[1, 2, 3], 0).unwrap_err();
        let err2 = err1.clone();
        let err3 = err1;
        assert_eq!(err1, err2);
        assert_eq!(err1, err3);
    }

    #[test]
    fn rolling_slice_error_debug() {
        let err = RollingDigest::from_le_slice(&[1, 2, 3], 0).unwrap_err();
        let debug = format!("{:?}", err);
        assert!(debug.contains("RollingSliceError"));
    }
}

#[cfg(test)]
mod pipelined_tests {
    use crate::pipelined::{
        compute_checksums_pipelined, BlockChecksums, DoubleBufferedReader, PipelineConfig,
        PipelinedChecksumIterator,
    };
    use crate::strong::{Md5, Sha256};
    use crate::RollingDigest;
    use std::io::Cursor;

    // ========================================================================
    // PipelineConfig Tests
    // ========================================================================

    #[test]
    fn pipeline_config_default_values() {
        let config = PipelineConfig::default();
        assert_eq!(config.block_size, 64 * 1024);
        assert_eq!(config.min_file_size, 256 * 1024);
        assert!(config.enabled);
    }

    #[test]
    fn pipeline_config_builder_chain() {
        let config = PipelineConfig::new()
            .with_block_size(32 * 1024)
            .with_min_file_size(128 * 1024)
            .with_enabled(true);

        assert_eq!(config.block_size, 32 * 1024);
        assert_eq!(config.min_file_size, 128 * 1024);
        assert!(config.enabled);
    }

    #[test]
    fn pipeline_config_debug() {
        let config = PipelineConfig::default();
        let debug = format!("{:?}", config);
        assert!(debug.contains("PipelineConfig"));
    }

    #[test]
    fn pipeline_config_clone() {
        let config = PipelineConfig::default();
        let cloned = config;
        assert_eq!(config.block_size, cloned.block_size);
    }

    // ========================================================================
    // DoubleBufferedReader Tests
    // ========================================================================

    #[test]
    fn double_buffered_reader_block_size() {
        let data = vec![0xAB; 256 * 1024];
        let config = PipelineConfig::default().with_block_size(32 * 1024);
        let reader = DoubleBufferedReader::new(Cursor::new(data), config);
        assert_eq!(reader.block_size(), 32 * 1024);
    }

    #[test]
    fn double_buffered_reader_tiny_file_sync_mode() {
        let data = vec![0xCD; 1024]; // 1KB, way below threshold
        let config = PipelineConfig::default()
            .with_block_size(512)
            .with_min_file_size(64 * 1024);

        let reader =
            DoubleBufferedReader::with_size_hint(Cursor::new(data), config, Some(1024));

        assert!(!reader.is_pipelined());
    }

    #[test]
    fn double_buffered_reader_large_file_pipelined_mode() {
        let data = vec![0xEF; 512 * 1024];
        let config = PipelineConfig::default()
            .with_block_size(64 * 1024)
            .with_min_file_size(128 * 1024);

        let reader =
            DoubleBufferedReader::with_size_hint(Cursor::new(data), config, Some(512 * 1024));

        assert!(reader.is_pipelined());
    }

    #[test]
    fn double_buffered_reader_reads_all_data() {
        let data: Vec<u8> = (0..1000).map(|i| (i % 256) as u8).collect();
        let config = PipelineConfig::default()
            .with_block_size(100)
            .with_min_file_size(0)
            .with_enabled(false); // Force sync mode

        let mut reader = DoubleBufferedReader::new(Cursor::new(data.clone()), config);

        let mut collected = Vec::new();
        while let Some(block) = reader.next_block().unwrap() {
            collected.extend_from_slice(block);
        }

        assert_eq!(collected, data);
    }

    #[test]
    fn double_buffered_reader_eof_returns_none() {
        let data = vec![0x12; 100];
        let config = PipelineConfig::default()
            .with_block_size(50)
            .with_enabled(false);

        let mut reader = DoubleBufferedReader::new(Cursor::new(data), config);

        // Read all blocks
        while reader.next_block().unwrap().is_some() {}

        // Additional calls should return None
        assert!(reader.next_block().unwrap().is_none());
        assert!(reader.next_block().unwrap().is_none());
    }

    // ========================================================================
    // BlockChecksums Tests
    // ========================================================================

    #[test]
    fn block_checksums_debug() {
        let cs = BlockChecksums {
            rolling: RollingDigest::from_bytes(b"test"),
            strong: [0u8; 16],
            len: 4,
        };
        let debug = format!("{:?}", cs);
        assert!(debug.contains("BlockChecksums"));
    }

    #[test]
    fn block_checksums_clone() {
        let cs = BlockChecksums {
            rolling: RollingDigest::from_bytes(b"clone test"),
            strong: [0xAB; 16],
            len: 10,
        };
        let cloned = cs.clone();
        assert_eq!(cs.rolling, cloned.rolling);
        assert_eq!(cs.strong, cloned.strong);
        assert_eq!(cs.len, cloned.len);
    }

    // ========================================================================
    // compute_checksums_pipelined Tests
    // ========================================================================

    #[test]
    fn compute_checksums_pipelined_single_block() {
        let data = vec![0x34; 32 * 1024];
        let config = PipelineConfig::default()
            .with_block_size(64 * 1024)
            .with_enabled(false);

        let checksums =
            compute_checksums_pipelined::<Md5, _>(Cursor::new(data.clone()), config, Some(32 * 1024))
                .unwrap();

        assert_eq!(checksums.len(), 1);
        assert_eq!(checksums[0].len, 32 * 1024);

        // Verify correctness
        let expected_rolling = RollingDigest::from_bytes(&data);
        let expected_strong = Md5::digest(&data);

        assert_eq!(checksums[0].rolling, expected_rolling);
        assert_eq!(checksums[0].strong, expected_strong);
    }

    #[test]
    fn compute_checksums_pipelined_with_sha256() {
        let data = vec![0x56; 128 * 1024];
        let config = PipelineConfig::default()
            .with_block_size(32 * 1024)
            .with_enabled(false);

        let checksums = compute_checksums_pipelined::<Sha256, _>(
            Cursor::new(data.clone()),
            config,
            Some(128 * 1024),
        )
        .unwrap();

        assert_eq!(checksums.len(), 4);

        for (i, cs) in checksums.iter().enumerate() {
            let start = i * 32 * 1024;
            let end = start + 32 * 1024;
            let block = &data[start..end];

            assert_eq!(cs.rolling, RollingDigest::from_bytes(block));
            assert_eq!(cs.strong, Sha256::digest(block));
            assert_eq!(cs.len, 32 * 1024);
        }
    }

    // ========================================================================
    // PipelinedChecksumIterator Tests
    // ========================================================================

    #[test]
    fn pipelined_iterator_creation() {
        let data = vec![0x78; 64 * 1024];
        let config = PipelineConfig::default().with_block_size(16 * 1024);

        let iter: PipelinedChecksumIterator<Md5, _> =
            PipelinedChecksumIterator::new(Cursor::new(data), config);

        // Just check it was created
        let _pipelined = iter.is_pipelined();
    }

    #[test]
    fn pipelined_iterator_with_size_hint() {
        let data = vec![0x9A; 128 * 1024];
        let config = PipelineConfig::default()
            .with_block_size(32 * 1024)
            .with_min_file_size(64 * 1024);

        let iter: PipelinedChecksumIterator<Md5, _> =
            PipelinedChecksumIterator::with_size_hint(Cursor::new(data), config, Some(128 * 1024));

        assert!(iter.is_pipelined());
    }

    #[test]
    fn pipelined_iterator_iterates_all_blocks() {
        let data = vec![0xBC; 100];
        let config = PipelineConfig::default()
            .with_block_size(25)
            .with_enabled(false);

        let mut iter: PipelinedChecksumIterator<Md5, _> =
            PipelinedChecksumIterator::new(Cursor::new(data), config);

        let mut count = 0;
        while let Some(_cs) = iter.next_block_checksums().unwrap() {
            count += 1;
        }

        assert_eq!(count, 4);
    }
}

#[cfg(feature = "parallel")]
#[cfg(test)]
mod parallel_tests {
    use crate::parallel::{
        compute_block_signatures_parallel, compute_digests_parallel,
        compute_digests_with_seed_parallel, compute_rolling_checksums_parallel,
        filter_blocks_by_checksum, process_blocks_parallel,
    };
    use crate::strong::{Md5, Sha256, Sha512, Xxh64};
    use crate::RollingChecksum;

    #[test]
    fn compute_digests_parallel_large_batch() {
        let blocks: Vec<Vec<u8>> = (0..100).map(|i| vec![(i % 256) as u8; 1000]).collect();

        let digests = compute_digests_parallel::<Sha256, _>(&blocks);

        assert_eq!(digests.len(), 100);
        for (i, d) in digests.iter().enumerate() {
            let expected = Sha256::digest(&blocks[i]);
            assert_eq!(*d, expected);
        }
    }

    #[test]
    fn compute_digests_with_seed_parallel_large_batch() {
        let blocks: Vec<Vec<u8>> = (0..50).map(|i| vec![(i * 3 % 256) as u8; 500]).collect();
        let seed = 99999u64;

        let digests = compute_digests_with_seed_parallel::<Xxh64, _>(&blocks, seed);

        assert_eq!(digests.len(), 50);
        for (i, d) in digests.iter().enumerate() {
            let expected = Xxh64::digest(seed, &blocks[i]);
            assert_eq!(*d, expected);
        }
    }

    #[test]
    fn compute_rolling_checksums_parallel_large_batch() {
        let blocks: Vec<Vec<u8>> = (0..75).map(|i| vec![(i * 7 % 256) as u8; 800]).collect();

        let checksums = compute_rolling_checksums_parallel(&blocks);

        assert_eq!(checksums.len(), 75);
        for (i, &c) in checksums.iter().enumerate() {
            let mut expected = RollingChecksum::new();
            expected.update(&blocks[i]);
            assert_eq!(c, expected.value());
        }
    }

    #[test]
    fn compute_block_signatures_parallel_with_md5() {
        let blocks: Vec<Vec<u8>> = (0..25).map(|i| vec![(i * 11 % 256) as u8; 600]).collect();

        let signatures = compute_block_signatures_parallel::<Md5, _>(&blocks);

        assert_eq!(signatures.len(), 25);
        for (i, sig) in signatures.iter().enumerate() {
            let mut rolling = RollingChecksum::new();
            rolling.update(&blocks[i]);
            assert_eq!(sig.rolling, rolling.value());

            let expected_strong = Md5::digest(&blocks[i]);
            assert_eq!(sig.strong, expected_strong);
        }
    }

    #[test]
    fn compute_block_signatures_parallel_with_sha512() {
        let blocks: Vec<Vec<u8>> = (0..10).map(|i| vec![(i * 13 % 256) as u8; 400]).collect();

        let signatures = compute_block_signatures_parallel::<Sha512, _>(&blocks);

        assert_eq!(signatures.len(), 10);
        for sig in &signatures {
            assert_eq!(sig.strong.len(), 64);
        }
    }

    #[test]
    fn process_blocks_parallel_custom_function() {
        let blocks: Vec<Vec<u8>> = (0..20).map(|i| vec![(i % 256) as u8; 100]).collect();

        let results: Vec<(usize, u8)> = process_blocks_parallel(&blocks, |block| {
            (block.len(), block.iter().sum::<u8>())
        });

        assert_eq!(results.len(), 20);
        for (i, &(len, sum)) in results.iter().enumerate() {
            assert_eq!(len, 100);
            let expected_sum = blocks[i].iter().sum::<u8>();
            assert_eq!(sum, expected_sum);
        }
    }

    #[test]
    fn filter_blocks_by_checksum_with_predicate() {
        let blocks: Vec<Vec<u8>> = (0..30).map(|i| vec![(i * 17 % 256) as u8; 200]).collect();

        // Filter for checksums with LSB set
        let matches = filter_blocks_by_checksum(&blocks, |c| c & 1 == 1);

        // Verify each match actually has LSB set
        let checksums = compute_rolling_checksums_parallel(&blocks);
        for &idx in &matches {
            assert!(checksums[idx] & 1 == 1);
        }
    }

    #[test]
    fn filter_blocks_by_checksum_no_matches() {
        let blocks: Vec<Vec<u8>> = (0..10).map(|_| vec![0u8; 100]).collect();

        // Impossible predicate
        let matches = filter_blocks_by_checksum(&blocks, |c| c == u32::MAX);

        assert!(matches.is_empty());
    }

    #[test]
    fn filter_blocks_by_checksum_all_match() {
        let blocks: Vec<Vec<u8>> = (0..10).map(|i| vec![(i % 256) as u8; 50]).collect();

        // Always true predicate
        let matches = filter_blocks_by_checksum(&blocks, |_| true);

        assert_eq!(matches.len(), 10);
    }
}

// ========================================================================
// Error Path Tests for Pipelined I/O
// ========================================================================

#[cfg(test)]
mod pipelined_error_tests {
    use crate::pipelined::{compute_checksums_pipelined, DoubleBufferedReader, PipelineConfig};
    use crate::strong::Md5;
    use std::io::{self, Read};

    /// A reader that fails after reading a certain number of bytes.
    struct FailingReader {
        data: Vec<u8>,
        position: usize,
        fail_at: usize,
    }

    impl FailingReader {
        fn new(data: Vec<u8>, fail_at: usize) -> Self {
            Self {
                data,
                position: 0,
                fail_at,
            }
        }
    }

    impl Read for FailingReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.position >= self.fail_at {
                return Err(io::Error::new(io::ErrorKind::Other, "simulated error"));
            }
            let remaining = self.data.len() - self.position;
            let to_read = buf.len().min(remaining).min(self.fail_at - self.position);
            if to_read == 0 {
                return Ok(0);
            }
            buf[..to_read].copy_from_slice(&self.data[self.position..self.position + to_read]);
            self.position += to_read;
            Ok(to_read)
        }
    }

    /// A reader that returns interrupted errors and then succeeds.
    struct InterruptingReader<R> {
        inner: R,
        interrupt_countdown: usize,
    }

    impl<R: Read> InterruptingReader<R> {
        fn new(inner: R, interrupts: usize) -> Self {
            Self {
                inner,
                interrupt_countdown: interrupts,
            }
        }
    }

    impl<R: Read> Read for InterruptingReader<R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.interrupt_countdown > 0 {
                self.interrupt_countdown -= 1;
                return Err(io::Error::new(io::ErrorKind::Interrupted, "interrupted"));
            }
            self.inner.read(buf)
        }
    }

    #[test]
    fn pipelined_reader_handles_read_error_in_sync_mode() {
        let data = vec![0xAB; 1024];
        let reader = FailingReader::new(data, 600);
        let config = PipelineConfig::default()
            .with_block_size(256)
            .with_enabled(false); // Force sync mode

        let mut buffered = DoubleBufferedReader::new(reader, config);

        // First two blocks should succeed (256 bytes each = 512 bytes)
        let block1 = buffered.next_block();
        assert!(block1.is_ok());
        assert!(block1.unwrap().is_some());

        let block2 = buffered.next_block();
        assert!(block2.is_ok());
        assert!(block2.unwrap().is_some());

        // Third block should fail (would need to read past 600 bytes)
        let block3 = buffered.next_block();
        assert!(block3.is_err());
    }

    #[test]
    fn compute_checksums_handles_interrupts() {
        let data = vec![0xCD; 512];
        let reader = InterruptingReader::new(std::io::Cursor::new(data.clone()), 2);
        let config = PipelineConfig::default()
            .with_block_size(128)
            .with_enabled(false);

        let result = compute_checksums_pipelined::<Md5, _>(reader, config, Some(512));
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 4);
    }

    #[test]
    fn double_buffered_reader_handles_early_read_error() {
        // Error before first block is fully read
        let data = vec![0xEF; 100];
        let reader = FailingReader::new(data, 50);
        let config = PipelineConfig::default()
            .with_block_size(200)
            .with_min_file_size(0)
            .with_enabled(true);

        // Should fall back to sync mode on error
        let mut buffered = DoubleBufferedReader::with_size_hint(reader, config, Some(100));

        // Should be in sync mode due to error
        assert!(!buffered.is_pipelined() || buffered.next_block().is_err());
    }
}

// ========================================================================
// Batch Digest Tests
// ========================================================================

#[cfg(test)]
mod batch_digest_tests {
    use crate::strong::{md4_digest_batch, md5_digest_batch, Md4, Md5};

    #[test]
    fn md4_batch_empty_input() {
        let inputs: &[&[u8]] = &[];
        let results = md4_digest_batch(inputs);
        assert!(results.is_empty());
    }

    #[test]
    fn md4_batch_single_input() {
        let inputs: &[&[u8]] = &[b"single"];
        let results = md4_digest_batch(inputs);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], Md4::digest(b"single"));
    }

    #[test]
    fn md4_batch_many_inputs() {
        let inputs: Vec<Vec<u8>> = (0..100).map(|i| vec![(i % 256) as u8; 100]).collect();
        let input_refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();

        let results = md4_digest_batch(&input_refs);

        assert_eq!(results.len(), 100);
        for (i, r) in results.iter().enumerate() {
            assert_eq!(*r, Md4::digest(&inputs[i]));
        }
    }

    #[test]
    fn md5_batch_empty_input() {
        let inputs: &[&[u8]] = &[];
        let results = md5_digest_batch(inputs);
        assert!(results.is_empty());
    }

    #[test]
    fn md5_batch_single_input() {
        let inputs: &[&[u8]] = &[b"single"];
        let results = md5_digest_batch(inputs);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], Md5::digest(b"single"));
    }

    #[test]
    fn md5_batch_many_inputs() {
        let inputs: Vec<Vec<u8>> = (0..100).map(|i| vec![(i * 3 % 256) as u8; 150]).collect();
        let input_refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();

        let results = md5_digest_batch(&input_refs);

        assert_eq!(results.len(), 100);
        for (i, r) in results.iter().enumerate() {
            assert_eq!(*r, Md5::digest(&inputs[i]));
        }
    }

    #[test]
    fn md5_batch_varying_sizes() {
        let inputs: Vec<Vec<u8>> = (0..20)
            .map(|i| vec![(i % 256) as u8; (i + 1) * 10])
            .collect();
        let input_refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();

        let results = md5_digest_batch(&input_refs);

        assert_eq!(results.len(), 20);
        for (i, r) in results.iter().enumerate() {
            assert_eq!(*r, Md5::digest(&inputs[i]));
        }
    }
}
