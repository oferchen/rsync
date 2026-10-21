//! MD4 Protocol Compatibility Tests for rsync Protocol < 30
//!
//! This test suite validates that MD4 works correctly as the strong checksum
//! algorithm for rsync protocol versions < 30, ensuring backward compatibility
//! with upstream rsync 3.5.1.
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
//! 1. MD4 RFC 1320 compliance
//! 2. Streaming and one-shot consistency
//! 3. Known test vectors from upstream rsync

use checksums::strong::{Md4, StrongDigest};

/// Convert bytes to hex string for assertions
fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut out, "{byte:02x}").unwrap();
    }
    out
}

// RFC 1320 vectors pinned to guarantee upstream rsync compatibility.

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
    let digest = Md4::digest(b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789");
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

// Block-checksum scenarios below mirror how rsync uses MD4 in protocol < 30.

#[test]
fn simulate_incremental_block_generation() {
    // Reading a block in small chunks must produce the same digest as one-shot.
    let block_data = b"This is a block of data that would be checksummed by rsync";

    let oneshot = Md4::digest(block_data);

    let mut hasher = Md4::new();
    for chunk in block_data.chunks(10) {
        hasher.update(chunk);
    }
    let incremental = hasher.finalize();

    assert_eq!(
        oneshot, incremental,
        "Incremental block checksum should match one-shot"
    );
}

#[test]
fn md4_digest_length_constant() {
    assert_eq!(Md4::DIGEST_LEN, 16, "MD4::DIGEST_LEN should be 16");
}

#[test]
fn md4_empty_input() {
    // MD4("") = 31d6cfe0d16ae931b73c59d7e0c089c0
    let digest = Md4::digest(b"");
    assert_eq!(digest.len(), 16);
    assert_eq!(to_hex(&digest), "31d6cfe0d16ae931b73c59d7e0c089c0");
}

#[test]
fn md4_single_byte() {
    // 0x61 = 'a' from RFC 1320.
    let digest = Md4::digest(&[0x61]);
    assert_eq!(digest.len(), 16);
    assert_eq!(to_hex(&digest), "bde52cb31de33e46245e05fbdbd6fb24");
}

#[test]
fn md4_block_boundary_sizes() {
    // MD4 processes 64-byte blocks; sweep both sides of each boundary.
    for size in [63, 64, 65, 127, 128, 129] {
        let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16, "Size {size}: digest should be 16 bytes");

        let mut hasher = Md4::new();
        hasher.update(&data);
        assert_eq!(hasher.finalize(), digest);
    }
}

#[test]
fn md4_large_input() {
    let data = vec![0x42_u8; 1024 * 1024];
    let digest = Md4::digest(&data);
    assert_eq!(digest.len(), 16);

    assert_eq!(Md4::digest(&data), digest);
}

#[test]
fn md4_differs_from_md5() {
    use checksums::strong::Md5;

    let data = b"MD4 and MD5 should produce different digests";

    let md4_digest = Md4::digest(data);
    let md5_digest = Md5::digest(data);

    assert_eq!(md4_digest.len(), 16);
    assert_eq!(md5_digest.len(), 16);

    assert_ne!(
        md4_digest.as_ref(),
        md5_digest.as_ref(),
        "MD4 and MD5 should produce different digests for the same input"
    );
}

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
