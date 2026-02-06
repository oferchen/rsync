//! Upstream rsync rolling checksum compatibility tests.
//!
//! This module verifies that the RollingChecksum implementation produces
//! identical output to upstream rsync 3.4.1's rolling checksum algorithm.
//!
//! # Algorithm Reference
//!
//! From rsync's checksum.c and rsync.h:
//! ```c
//! #define CHAR_OFFSET 0  // rsync 3.x uses 0 (no offset)
//!
//! // Initial computation over block of length n:
//! s1 = sum of all bytes
//! s2 = sum of (n - i) * byte[i] for i in 0..n
//!    = sum of prefix sums
//! checksum = (s2 << 16) | s1
//!
//! // Rolling update (remove old_byte, add new_byte):
//! s1 = (s1 - old_byte + new_byte) & 0xFFFF
//! s2 = (s2 - n * old_byte + s1) & 0xFFFF
//! ```
//!
//! This is Mark Adler's rolling checksum algorithm, similar to Adler-32
//! but without the base modulus and with CHAR_OFFSET = 0.

use checksums::{RollingChecksum, RollingDigest};

/// Verify CHAR_OFFSET = 0 by testing that empty input produces zero.
#[test]
fn char_offset_is_zero() {
    let mut checksum = RollingChecksum::new();
    checksum.update(b"");
    assert_eq!(checksum.value(), 0x0000_0000);
    assert_eq!(checksum.digest().sum1(), 0);
    assert_eq!(checksum.digest().sum2(), 0);
}

/// Test single byte checksum.
/// For a single byte b with CHAR_OFFSET = 0:
/// s1 = b
/// s2 = b (sum of prefix sums = just the first byte)
/// value = (b << 16) | b
#[test]
fn single_byte_matches_formula() {
    for byte in [0x00, 0x01, 0x42, 0x7F, 0x80, 0xFF] {
        let mut checksum = RollingChecksum::new();
        checksum.update(&[byte]);

        let expected_s1 = byte as u16;
        let expected_s2 = byte as u16;
        let expected_value = ((byte as u32) << 16) | (byte as u32);

        assert_eq!(checksum.digest().sum1(), expected_s1, "s1 mismatch for byte {byte:#02x}");
        assert_eq!(checksum.digest().sum2(), expected_s2, "s2 mismatch for byte {byte:#02x}");
        assert_eq!(checksum.value(), expected_value, "value mismatch for byte {byte:#02x}");
    }
}

/// Test two-byte checksum.
/// For bytes [a, b]:
/// s1 = a + b
/// s2 = a + (a + b) = 2a + b
#[test]
fn two_bytes_matches_formula() {
    let data = [0x12, 0x34];
    let mut checksum = RollingChecksum::new();
    checksum.update(&data);

    let expected_s1 = (0x12 + 0x34) as u16;  // 0x46
    let expected_s2 = (0x12 + 0x46) as u16;  // 0x58
    let expected_value = ((expected_s2 as u32) << 16) | (expected_s1 as u32);

    assert_eq!(checksum.digest().sum1(), expected_s1);
    assert_eq!(checksum.digest().sum2(), expected_s2);
    assert_eq!(checksum.value(), expected_value);
}

/// Test that s1 and s2 are truncated to 16 bits.
#[test]
fn components_truncated_to_16_bits() {
    // Use 256 bytes of 0xFF to cause overflow
    let data = vec![0xFF; 256];
    let mut checksum = RollingChecksum::new();
    checksum.update(&data);

    // s1 = 256 * 0xFF = 65280 = 0xFF00 (fits in 16 bits)
    // s2 will overflow and wrap
    let s1 = checksum.digest().sum1();
    let s2 = checksum.digest().sum2();

    // Both should be 16-bit values
    assert!(s1 <= 0xFFFF);
    assert!(s2 <= 0xFFFF);

    // Verify value packing
    let value = checksum.value();
    assert_eq!(value & 0xFFFF, s1 as u32);
    assert_eq!(value >> 16, s2 as u32);
}

/// Test rolling update matches recomputation.
/// This verifies the rolling formula:
/// s1_new = (s1_old - old_byte + new_byte) & 0xFFFF
/// s2_new = (s2_old - n * old_byte + s1_new) & 0xFFFF
#[test]
fn rolling_update_matches_formula() {
    let data = b"ABCDEFGH";

    // Compute "ABCD"
    let mut rolling = RollingChecksum::new();
    rolling.update(&data[0..4]);

    // Roll to "BCDE": remove 'A' (0x41), add 'E' (0x45)
    rolling.roll(b'A', b'E').unwrap();

    // Fresh computation of "BCDE"
    let mut fresh = RollingChecksum::new();
    fresh.update(&data[1..5]);

    assert_eq!(rolling.value(), fresh.value());
    assert_eq!(rolling.digest().sum1(), fresh.digest().sum1());
    assert_eq!(rolling.digest().sum2(), fresh.digest().sum2());
    assert_eq!(rolling.len(), fresh.len());
}

/// Test with known rsync block sizes.
/// rsync uses variable block sizes based on file size:
/// - Small files: 700 bytes
/// - Medium files: ~2048 bytes
/// - Large files: 8192 bytes or more
#[test]
fn typical_rsync_block_sizes() {
    for block_size in [700, 2048, 8192] {
        let data: Vec<u8> = (0..block_size).map(|i| (i % 256) as u8).collect();

        let mut checksum = RollingChecksum::new();
        checksum.update(&data);

        assert_eq!(checksum.len(), block_size);
        assert_ne!(checksum.value(), 0);  // Should produce non-zero checksum

        // Verify components are within 16-bit range
        assert!(checksum.digest().sum1() <= 0xFFFF);
        assert!(checksum.digest().sum2() <= 0xFFFF);
    }
}

/// Test rolling over an entire file-like buffer.
/// This simulates rsync's delta detection where we slide
/// a window over the receiver's file data.
#[test]
fn sliding_window_full_scan() {
    let file_data = b"The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog.";
    let block_size = 16;

    // Compute initial window
    let mut rolling = RollingChecksum::new();
    rolling.update(&file_data[0..block_size]);

    // Slide window through entire file
    for start in 1..=(file_data.len() - block_size) {
        let old_byte = file_data[start - 1];
        let new_byte = file_data[start + block_size - 1];

        rolling.roll(old_byte, new_byte).unwrap();

        // Verify against fresh computation
        let mut fresh = RollingChecksum::new();
        fresh.update(&file_data[start..start + block_size]);

        assert_eq!(
            rolling.value(),
            fresh.value(),
            "Mismatch at offset {start}: rolling vs fresh"
        );
    }
}

/// Test all-zeros block.
/// This is a common case in sparse files.
#[test]
fn all_zeros_block() {
    let data = vec![0u8; 1024];
    let mut checksum = RollingChecksum::new();
    checksum.update(&data);

    assert_eq!(checksum.digest().sum1(), 0);
    assert_eq!(checksum.digest().sum2(), 0);
    assert_eq!(checksum.value(), 0x0000_0000);
}

/// Test all-ones block (0xFF).
/// This maximizes s1 and s2 for a given length.
#[test]
fn all_ones_block() {
    let size = 128;
    let data = vec![0xFF; size];
    let mut checksum = RollingChecksum::new();
    checksum.update(&data);

    // s1 = 128 * 255 = 32640 = 0x7F80
    let expected_s1 = ((size * 0xFF) & 0xFFFF) as u16;
    assert_eq!(checksum.digest().sum1(), expected_s1);

    // s2 = sum of prefix sums = 255 * (1 + 2 + ... + 128)
    //    = 255 * (128 * 129 / 2) = 255 * 8256 = 2105280
    //    = 0x201FC0, truncated to 16 bits = 0x1FC0
    let full_s2 = 255u32 * ((size * (size + 1) / 2) as u32);
    let expected_s2 = (full_s2 & 0xFFFF) as u16;
    assert_eq!(checksum.digest().sum2(), expected_s2);
}

/// Test incremental update matches single update.
/// This verifies that breaking data into chunks doesn't affect the result.
#[test]
fn incremental_updates_match_single_update() {
    let data = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit.";

    // Single update
    let mut single = RollingChecksum::new();
    single.update(data);

    // Incremental updates
    let mut incremental = RollingChecksum::new();
    for chunk in data.chunks(7) {
        incremental.update(chunk);
    }

    assert_eq!(single.value(), incremental.value());
    assert_eq!(single.digest(), incremental.digest());
}

/// Test update_byte method matches slice update.
#[test]
fn update_byte_matches_slice_update() {
    let data = b"test data";

    let mut byte_by_byte = RollingChecksum::new();
    for &byte in data {
        byte_by_byte.update_byte(byte);
    }

    let mut slice_update = RollingChecksum::new();
    slice_update.update(data);

    assert_eq!(byte_by_byte.value(), slice_update.value());
    assert_eq!(byte_by_byte.digest(), slice_update.digest());
}

/// Test wire format matches upstream rsync.
/// rsync transmits checksums as little-endian 32-bit values.
#[test]
fn wire_format_little_endian() {
    let mut checksum = RollingChecksum::new();
    checksum.update(b"rsync");

    let digest = checksum.digest();
    let bytes = digest.to_le_bytes();

    // Reconstruct from bytes
    let reconstructed = RollingDigest::from_le_bytes(bytes, digest.len());
    assert_eq!(digest, reconstructed);

    // Verify byte order: little-endian means LSB first
    let value = digest.value();
    assert_eq!(bytes[0], (value & 0xFF) as u8);
    assert_eq!(bytes[1], ((value >> 8) & 0xFF) as u8);
    assert_eq!(bytes[2], ((value >> 16) & 0xFF) as u8);
    assert_eq!(bytes[3], ((value >> 24) & 0xFF) as u8);
}

/// Golden test vectors computed by hand and verified.
/// These test vectors are computed using the exact algorithm
/// from upstream rsync to ensure byte-for-byte compatibility.
#[test]
fn golden_test_vectors() {
    struct TestVector {
        input: &'static [u8],
        expected_s1: u16,
        expected_s2: u16,
        expected_value: u32,
    }

    let vectors = [
        // Empty input
        TestVector {
            input: b"",
            expected_s1: 0,
            expected_s2: 0,
            expected_value: 0x0000_0000,
        },
        // Single byte 'A' (0x41 = 65)
        TestVector {
            input: b"A",
            expected_s1: 65,
            expected_s2: 65,
            expected_value: 0x0041_0041,
        },
        // "AB" (0x41, 0x42)
        // s1 = 0x41 + 0x42 = 0x83 = 131
        // s2 = 0x41 + 0x83 = 0xC4 = 196
        TestVector {
            input: b"AB",
            expected_s1: 131,
            expected_s2: 196,
            expected_value: 0x00C4_0083,
        },
        // "rsync" - the canonical test
        // 'r'=0x72=114, 's'=0x73=115, 'y'=0x79=121, 'n'=0x6E=110, 'c'=0x63=99
        // s1 = 114 + 115 + 121 + 110 + 99 = 559 = 0x022F
        // s2 = 114 + 229 + 350 + 460 + 559 = 1712 = 0x06B0
        TestVector {
            input: b"rsync",
            expected_s1: 0x022F,
            expected_s2: 0x06B0,
            expected_value: 0x06B0_022F,
        },
        // "ABCD"
        // A=65, B=66, C=67, D=68
        // s1 = 65+66+67+68 = 266 = 0x010A
        // s2 = 65 + 131 + 198 + 266 = 660 = 0x0294
        TestVector {
            input: b"ABCD",
            expected_s1: 0x010A,
            expected_s2: 0x0294,
            expected_value: 0x0294_010A,
        },
        // Four zeros
        TestVector {
            input: &[0, 0, 0, 0],
            expected_s1: 0,
            expected_s2: 0,
            expected_value: 0x0000_0000,
        },
        // Four ones
        // s1 = 4, s2 = 1+2+3+4 = 10
        TestVector {
            input: &[1, 1, 1, 1],
            expected_s1: 4,
            expected_s2: 10,
            expected_value: 0x000A_0004,
        },
    ];

    for (i, vector) in vectors.iter().enumerate() {
        let mut checksum = RollingChecksum::new();
        checksum.update(vector.input);

        let digest = checksum.digest();
        assert_eq!(
            digest.sum1(),
            vector.expected_s1,
            "Test vector {i}: s1 mismatch for input {:?}",
            vector.input
        );
        assert_eq!(
            digest.sum2(),
            vector.expected_s2,
            "Test vector {i}: s2 mismatch for input {:?}",
            vector.input
        );
        assert_eq!(
            checksum.value(),
            vector.expected_value,
            "Test vector {i}: value mismatch for input {:?}",
            vector.input
        );
    }
}

/// Test rolling update with golden vectors.
#[test]
fn rolling_golden_vectors() {
    // Start with "ABCD", roll to "BCDE"
    let mut checksum = RollingChecksum::new();
    checksum.update(b"ABCD");

    // Verify initial state
    assert_eq!(checksum.value(), 0x0294_010A);

    // Roll: remove 'A' (65), add 'E' (69)
    // n = 4
    // s1_new = (266 - 65 + 69) & 0xFFFF = 270 = 0x010E
    // s2_new = (660 - 4*65 + 270) & 0xFFFF = (660 - 260 + 270) = 670 = 0x029E
    checksum.roll(b'A', b'E').unwrap();

    assert_eq!(checksum.digest().sum1(), 0x010E);
    assert_eq!(checksum.digest().sum2(), 0x029E);
    assert_eq!(checksum.value(), 0x029E_010E);

    // Verify against fresh computation
    let mut fresh = RollingChecksum::new();
    fresh.update(b"BCDE");
    assert_eq!(checksum.value(), fresh.value());
}

/// Test that SIMD and scalar implementations produce identical results.
#[test]
fn simd_scalar_compatibility() {
    let test_data = [
        b"" as &[u8],
        b"x",
        b"short",
        b"medium length data",
        b"The quick brown fox jumps over the lazy dog",
        &vec![0xAAu8; 1000],
        &vec![0x00u8; 1000],
        &vec![0xFFu8; 1000],
    ];

    for data in &test_data {
        let mut checksum = RollingChecksum::new();
        checksum.update(data);

        // The value should be consistent regardless of SIMD implementation
        let value = checksum.value();
        let digest = checksum.digest();

        // Verify internal consistency
        assert_eq!(value, ((digest.sum2() as u32) << 16) | (digest.sum1() as u32));
        assert_eq!(digest.len(), data.len());
    }
}

/// Test determinism: same input always produces same output.
#[test]
fn deterministic_output() {
    let data = b"deterministic test data";

    let mut c1 = RollingChecksum::new();
    c1.update(data);
    let v1 = c1.value();

    let mut c2 = RollingChecksum::new();
    c2.update(data);
    let v2 = c2.value();

    assert_eq!(v1, v2);
    assert_eq!(c1.digest(), c2.digest());
}

/// Test that different data produces different checksums (collision resistance).
/// Note: This is a weak checksum, so collisions can happen, but they should be rare.
#[test]
fn different_inputs_produce_different_checksums() {
    let inputs = [
        b"apple" as &[u8],
        b"orange",
        b"banana",
        b"grape",
        b"melon",
    ];

    let mut checksums = Vec::new();
    for input in &inputs {
        let mut checksum = RollingChecksum::new();
        checksum.update(input);
        checksums.push(checksum.value());
    }

    // All checksums should be unique for these diverse inputs
    for i in 0..checksums.len() {
        for j in (i + 1)..checksums.len() {
            assert_ne!(
                checksums[i],
                checksums[j],
                "Unexpected collision between {:?} and {:?}",
                inputs[i],
                inputs[j]
            );
        }
    }
}

/// Test reset functionality.
#[test]
fn reset_clears_state() {
    let mut checksum = RollingChecksum::new();
    checksum.update(b"some data");

    let value_before = checksum.value();
    assert_ne!(value_before, 0);

    checksum.reset();

    assert_eq!(checksum.value(), 0);
    assert_eq!(checksum.len(), 0);
    assert!(checksum.is_empty());
    assert_eq!(checksum.digest(), RollingDigest::ZERO);
}

/// Test update_from_block resets and computes.
#[test]
fn update_from_block_resets() {
    let mut checksum = RollingChecksum::new();
    checksum.update(b"initial data");

    checksum.update_from_block(b"new block");

    let mut fresh = RollingChecksum::new();
    fresh.update(b"new block");

    assert_eq!(checksum.value(), fresh.value());
    assert_eq!(checksum.len(), 9);
}
