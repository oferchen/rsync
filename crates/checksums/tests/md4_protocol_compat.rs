//! MD4 Protocol Compatibility Tests for rsync Protocol < 30
//!
//! This test suite validates that MD4 works correctly as the strong checksum
//! algorithm for rsync protocol versions < 30, ensuring backward compatibility
//! with upstream rsync 3.4.1.
//!
//! # Protocol Context
//!
//! - Protocol versions < 30: MD4 is the default strong checksum
//! - Protocol versions >= 30: MD5 is the default strong checksum
//! - MD4 produces a 16-byte (128-bit) digest
//! - MD4 is cryptographically broken but required for legacy compatibility
//!
//! # Test Coverage
//!
//! 1. Protocol version selection (< 30 uses MD4)
//! 2. MD4 RFC 1320 compliance
//! 3. Integration with ChecksumStrategy pattern
//! 4. Streaming and one-shot consistency
//! 5. Known test vectors from upstream rsync

use checksums::{
    ChecksumAlgorithmKind, ChecksumStrategySelector,
    strong::{Md4, StrongDigest},
};

/// Convert bytes to hex string for assertions
fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut out, "{byte:02x}").unwrap();
    }
    out
}

// ============================================================================
// Protocol Version Selection Tests
// ============================================================================

#[test]
fn protocol_27_selects_md4() {
    let strategy = ChecksumStrategySelector::for_protocol_version(27, 0);
    assert_eq!(
        strategy.algorithm_kind(),
        ChecksumAlgorithmKind::Md4,
        "Protocol 27 should use MD4"
    );
    assert_eq!(strategy.digest_len(), 16, "MD4 produces 16-byte digest");
}

#[test]
fn protocol_28_selects_md4() {
    let strategy = ChecksumStrategySelector::for_protocol_version(28, 0);
    assert_eq!(strategy.algorithm_kind(), ChecksumAlgorithmKind::Md4);
}

#[test]
fn protocol_29_selects_md4() {
    let strategy = ChecksumStrategySelector::for_protocol_version(29, 0);
    assert_eq!(strategy.algorithm_kind(), ChecksumAlgorithmKind::Md4);
}

#[test]
fn protocol_30_selects_md5_not_md4() {
    let strategy = ChecksumStrategySelector::for_protocol_version(30, 0);
    assert_eq!(
        strategy.algorithm_kind(),
        ChecksumAlgorithmKind::Md5,
        "Protocol 30 and above should use MD5, not MD4"
    );
}

#[test]
fn explicit_md4_selection() {
    let strategy = ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Md4, 0);
    assert_eq!(strategy.algorithm_kind(), ChecksumAlgorithmKind::Md4);
    assert_eq!(strategy.digest_len(), 16);
}

// ============================================================================
// RFC 1320 Compliance Tests
// ============================================================================
// These tests verify that our MD4 implementation matches the official
// RFC 1320 specification, ensuring compatibility with upstream rsync.

#[test]
fn rfc1320_empty_string() {
    // MD4("") = 31d6cfe0d16ae931b73c59d7e0c089c0
    let digest = Md4::digest(b"");
    assert_eq!(
        to_hex(&digest),
        "31d6cfe0d16ae931b73c59d7e0c089c0",
        "RFC 1320: MD4 of empty string"
    );

    // Verify via strategy pattern
    let strategy = ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Md4, 0);
    let strategy_digest = strategy.compute(b"");
    assert_eq!(strategy_digest.as_bytes(), &digest);
}

#[test]
fn rfc1320_single_char_a() {
    // MD4("a") = bde52cb31de33e46245e05fbdbd6fb24
    let digest = Md4::digest(b"a");
    assert_eq!(
        to_hex(&digest),
        "bde52cb31de33e46245e05fbdbd6fb24",
        "RFC 1320: MD4 of 'a'"
    );
}

#[test]
fn rfc1320_abc() {
    // MD4("abc") = a448017aaf21d8525fc10ae87aa6729d
    let digest = Md4::digest(b"abc");
    assert_eq!(
        to_hex(&digest),
        "a448017aaf21d8525fc10ae87aa6729d",
        "RFC 1320: MD4 of 'abc'"
    );
}

#[test]
fn rfc1320_message_digest() {
    // MD4("message digest") = d9130a8164549fe818874806e1c7014b
    let digest = Md4::digest(b"message digest");
    assert_eq!(
        to_hex(&digest),
        "d9130a8164549fe818874806e1c7014b",
        "RFC 1320: MD4 of 'message digest'"
    );
}

#[test]
fn rfc1320_lowercase_alphabet() {
    // MD4("abcdefghijklmnopqrstuvwxyz") = d79e1c308aa5bbcdeea8ed63df412da9
    let digest = Md4::digest(b"abcdefghijklmnopqrstuvwxyz");
    assert_eq!(
        to_hex(&digest),
        "d79e1c308aa5bbcdeea8ed63df412da9",
        "RFC 1320: MD4 of lowercase alphabet"
    );
}

#[test]
fn rfc1320_alphanumeric() {
    // MD4("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789")
    let digest =
        Md4::digest(b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789");
    assert_eq!(
        to_hex(&digest),
        "043f8582f241db351ce627e153e7f0e4",
        "RFC 1320: MD4 of alphanumeric"
    );
}

#[test]
fn rfc1320_numeric_repeated() {
    // MD4("12345678901234567890123456789012345678901234567890123456789012345678901234567890")
    let digest = Md4::digest(
        b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
    );
    assert_eq!(
        to_hex(&digest),
        "e33b4ddc9c38f2199c3e7b164fcc0536",
        "RFC 1320: MD4 of repeated digits"
    );
}

// ============================================================================
// Strategy Pattern Integration Tests
// ============================================================================

#[test]
fn strategy_produces_correct_digest_length() {
    let strategy = ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Md4, 0);
    let data = b"test data for digest length verification";
    let digest = strategy.compute(data);

    assert_eq!(digest.len(), 16, "MD4 digest should be 16 bytes");
    assert_eq!(
        digest.len(),
        strategy.digest_len(),
        "Strategy digest_len() should match actual digest length"
    );
}

#[test]
fn strategy_compute_vs_compute_into() {
    let strategy = ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Md4, 0);
    let data = b"compute vs compute_into test";

    let digest = strategy.compute(data);

    let mut buffer = [0u8; 16];
    strategy.compute_into(data, &mut buffer);

    assert_eq!(digest.as_bytes(), &buffer[..]);
}

#[test]
fn strategy_deterministic() {
    let strategy = ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Md4, 0);
    let data = b"determinism test";

    let d1 = strategy.compute(data);
    let d2 = strategy.compute(data);
    let d3 = strategy.compute(data);

    assert_eq!(d1, d2);
    assert_eq!(d2, d3);
}

#[test]
fn strategy_different_inputs_different_outputs() {
    let strategy = ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Md4, 0);

    let d1 = strategy.compute(b"input1");
    let d2 = strategy.compute(b"input2");

    assert_ne!(d1, d2, "Different inputs should produce different digests");
}

// ============================================================================
// Streaming vs One-Shot Consistency Tests
// ============================================================================

#[test]
fn streaming_matches_oneshot_empty() {
    let oneshot = Md4::digest(b"");

    let streaming = Md4::new().finalize();

    assert_eq!(oneshot, streaming);
}

#[test]
fn streaming_matches_oneshot_single_update() {
    let data = b"single update test";
    let oneshot = Md4::digest(data);

    let mut hasher = Md4::new();
    hasher.update(data);
    let streaming = hasher.finalize();

    assert_eq!(oneshot, streaming);
}

#[test]
fn streaming_matches_oneshot_multiple_updates() {
    let data = b"streaming test with multiple updates";
    let oneshot = Md4::digest(data);

    let mut hasher = Md4::new();
    hasher.update(b"streaming test");
    hasher.update(b" with");
    hasher.update(b" multiple");
    hasher.update(b" updates");
    let streaming = hasher.finalize();

    assert_eq!(oneshot, streaming);
}

#[test]
fn streaming_byte_by_byte() {
    let data = b"byte by byte streaming";
    let oneshot = Md4::digest(data);

    let mut hasher = Md4::new();
    for &byte in data.iter() {
        hasher.update(&[byte]);
    }
    let streaming = hasher.finalize();

    assert_eq!(oneshot, streaming);
}

#[test]
fn streaming_various_chunk_sizes() {
    let data: Vec<u8> = (0..1000).map(|i| (i % 256) as u8).collect();
    let expected = Md4::digest(&data);

    for chunk_size in [1, 7, 13, 64, 128, 256, 500] {
        let mut hasher = Md4::new();
        for chunk in data.chunks(chunk_size) {
            hasher.update(chunk);
        }
        let result = hasher.finalize();
        assert_eq!(
            result, expected,
            "Chunk size {chunk_size} should produce same result"
        );
    }
}

// ============================================================================
// Protocol Version Boundary Tests
// ============================================================================

#[test]
fn protocol_version_boundary_at_30() {
    let v29 = ChecksumStrategySelector::for_protocol_version(29, 0);
    let v30 = ChecksumStrategySelector::for_protocol_version(30, 0);

    assert_eq!(v29.algorithm_kind(), ChecksumAlgorithmKind::Md4);
    assert_eq!(v30.algorithm_kind(), ChecksumAlgorithmKind::Md5);

    let test_data = b"boundary test";
    let d29 = v29.compute(test_data);
    let d30 = v30.compute(test_data);

    // Both should produce 16-byte digests (MD4 and MD5)
    assert_eq!(d29.len(), 16);
    assert_eq!(d30.len(), 16);

    // But the digests should be different (MD4 vs MD5)
    assert_ne!(d29, d30, "MD4 and MD5 should produce different digests");
}

#[test]
fn all_protocols_below_30_use_md4() {
    for version in 20..30 {
        let strategy = ChecksumStrategySelector::for_protocol_version(version, 0);
        assert_eq!(
            strategy.algorithm_kind(),
            ChecksumAlgorithmKind::Md4,
            "Protocol {version} should use MD4"
        );
    }
}

#[test]
fn all_protocols_30_and_above_use_md5() {
    for version in 30..35 {
        let strategy = ChecksumStrategySelector::for_protocol_version(version, 0);
        assert_eq!(
            strategy.algorithm_kind(),
            ChecksumAlgorithmKind::Md5,
            "Protocol {version} should use MD5"
        );
    }
}

// ============================================================================
// rsync Block Checksum Simulation Tests
// ============================================================================
// These tests simulate how rsync uses MD4 for block checksums in protocol < 30

#[test]
fn simulate_block_checksum_small_file() {
    // Simulate checksumming a small file in blocks (e.g., 700-byte blocks)
    let file_data: Vec<u8> = (0..2100).map(|i| (i % 256) as u8).collect();
    let block_size = 700;

    let strategy = ChecksumStrategySelector::for_protocol_version(29, 0);

    let mut block_checksums = Vec::new();
    for block in file_data.chunks(block_size) {
        let digest = strategy.compute(block);
        assert_eq!(digest.len(), 16, "Each block checksum should be 16 bytes");
        block_checksums.push(digest);
    }

    // Should have 3 blocks
    assert_eq!(block_checksums.len(), 3);

    // All checksums should be unique (different blocks)
    assert_ne!(block_checksums[0], block_checksums[1]);
    assert_ne!(block_checksums[1], block_checksums[2]);
    assert_ne!(block_checksums[0], block_checksums[2]);
}

#[test]
fn simulate_block_checksum_large_file() {
    // Simulate checksumming a larger file
    let file_data: Vec<u8> = (0..100_000).map(|i| (i % 256) as u8).collect();
    let block_size = 4096;

    let strategy = ChecksumStrategySelector::for_protocol_version(29, 0);

    let block_count = (file_data.len() + block_size - 1) / block_size;
    let mut block_checksums = Vec::with_capacity(block_count);

    for block in file_data.chunks(block_size) {
        let digest = strategy.compute(block);
        assert_eq!(digest.len(), 16);
        block_checksums.push(digest);
    }

    // Verify we got the expected number of blocks
    assert_eq!(block_checksums.len(), block_count);

    // Verify determinism: recompute and compare
    for (i, block) in file_data.chunks(block_size).enumerate() {
        let recomputed = strategy.compute(block);
        assert_eq!(
            recomputed, block_checksums[i],
            "Block {i} checksum should be deterministic"
        );
    }
}

#[test]
fn simulate_incremental_block_generation() {
    // Simulate generating block checksums incrementally (streaming)
    let block_data = b"This is a block of data that would be checksummed by rsync";

    let oneshot = Md4::digest(block_data);

    let mut hasher = Md4::new();
    // Simulate reading the block in smaller chunks
    for chunk in block_data.chunks(10) {
        hasher.update(chunk);
    }
    let incremental = hasher.finalize();

    assert_eq!(
        oneshot, incremental,
        "Incremental block checksum should match one-shot"
    );
}

// ============================================================================
// Edge Cases and Correctness Tests
// ============================================================================

#[test]
fn md4_digest_length_constant() {
    assert_eq!(Md4::DIGEST_LEN, 16, "MD4::DIGEST_LEN should be 16");
}

#[test]
fn md4_empty_input() {
    let digest = Md4::digest(b"");
    assert_eq!(digest.len(), 16);
    // Known empty string digest
    assert_eq!(to_hex(&digest), "31d6cfe0d16ae931b73c59d7e0c089c0");
}

#[test]
fn md4_single_byte() {
    let digest = Md4::digest(&[0x61]); // 'a'
    assert_eq!(digest.len(), 16);
    assert_eq!(to_hex(&digest), "bde52cb31de33e46245e05fbdbd6fb24");
}

#[test]
fn md4_block_boundary_sizes() {
    // MD4 processes data in 64-byte blocks
    for size in [63, 64, 65, 127, 128, 129] {
        let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16, "Size {size}: digest should be 16 bytes");

        // Verify streaming matches
        let mut hasher = Md4::new();
        hasher.update(&data);
        assert_eq!(hasher.finalize(), digest);
    }
}

#[test]
fn md4_large_input() {
    let data = vec![0x42_u8; 1024 * 1024]; // 1 MB
    let digest = Md4::digest(&data);
    assert_eq!(digest.len(), 16);

    // Verify determinism
    assert_eq!(Md4::digest(&data), digest);
}

// ============================================================================
// Comparison Tests (MD4 vs MD5)
// ============================================================================

#[test]
fn md4_differs_from_md5() {
    use checksums::strong::Md5;

    let data = b"MD4 and MD5 should produce different digests";

    let md4_digest = Md4::digest(data);
    let md5_digest = Md5::digest(data);

    // Both are 16 bytes
    assert_eq!(md4_digest.len(), 16);
    assert_eq!(md5_digest.len(), 16);

    // But the digests should be different
    assert_ne!(
        md4_digest.as_ref(),
        md5_digest.as_ref(),
        "MD4 and MD5 should produce different digests for the same input"
    );
}

// ============================================================================
// StrongDigest Trait Compliance Tests
// ============================================================================

#[test]
fn md4_implements_strong_digest_trait() {
    // Test that Md4 properly implements StrongDigest

    // Test new()
    let mut hasher: Md4 = StrongDigest::new();
    hasher.update(b"trait test");
    let digest = hasher.finalize();
    assert_eq!(digest.len(), 16);

    // Test with_seed (MD4 has () as seed type)
    let mut seeded: Md4 = StrongDigest::with_seed(());
    seeded.update(b"trait test");
    assert_eq!(seeded.finalize(), digest);

    // Test digest() convenience method
    let quick = <Md4 as StrongDigest>::digest(b"trait test");
    assert_eq!(quick, digest);
}

#[test]
fn md4_digest_len_constant_matches_trait() {
    assert_eq!(Md4::DIGEST_LEN, 16);
    let digest = Md4::digest(b"test");
    assert_eq!(digest.as_ref().len(), Md4::DIGEST_LEN);
}

// ============================================================================
// Protocol Compatibility Summary Test
// ============================================================================

#[test]
fn protocol_compatibility_summary() {
    // This test serves as documentation for the protocol behavior

    // Protocol < 30: MD4
    for protocol in [27, 28, 29] {
        let strategy = ChecksumStrategySelector::for_protocol_version(protocol, 0);
        assert_eq!(
            strategy.algorithm_kind(),
            ChecksumAlgorithmKind::Md4,
            "Protocol {protocol} uses MD4"
        );
        assert_eq!(strategy.digest_len(), 16, "MD4 produces 16-byte digest");
    }

    // Protocol >= 30: MD5
    for protocol in [30, 31, 32] {
        let strategy = ChecksumStrategySelector::for_protocol_version(protocol, 0);
        assert_eq!(
            strategy.algorithm_kind(),
            ChecksumAlgorithmKind::Md5,
            "Protocol {protocol} uses MD5"
        );
        assert_eq!(strategy.digest_len(), 16, "MD5 produces 16-byte digest");
    }

    // Explicit algorithm selection works regardless of protocol
    let md4 = ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Md4, 0);
    assert_eq!(md4.algorithm_kind(), ChecksumAlgorithmKind::Md4);

    let md5 = ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Md5, 0);
    assert_eq!(md5.algorithm_kind(), ChecksumAlgorithmKind::Md5);

    // Both work correctly
    let test_data = b"protocol compatibility test";
    let md4_digest = md4.compute(test_data);
    let md5_digest = md5.compute(test_data);

    assert_eq!(md4_digest.len(), 16);
    assert_eq!(md5_digest.len(), 16);
    assert_ne!(md4_digest, md5_digest, "MD4 and MD5 should differ");
}
