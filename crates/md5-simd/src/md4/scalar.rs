//! Scalar (single-lane) MD4 implementation.
//!
//! Used as fallback when SIMD is unavailable or for single-hash workloads.

use crate::Digest;

/// MD4 initial state constants (same as MD5, RFC 1320).
const INIT_A: u32 = 0x6745_2301;
const INIT_B: u32 = 0xefcd_ab89;
const INIT_C: u32 = 0x98ba_dcfe;
const INIT_D: u32 = 0x1032_5476;

/// Round constants for MD4.
/// Round 1: no constant (0)
/// Round 2: sqrt(2) * 2^30
/// Round 3: sqrt(3) * 2^30
const K: [u32; 3] = [
    0x0000_0000, // Round 1
    0x5A82_7999, // Round 2: sqrt(2) * 2^30
    0x6ED9_EBA1, // Round 3: sqrt(3) * 2^30
];

/// Shift amounts for each round.
/// Each round uses 4 different shift values, cycled 4 times.
const S1: [u32; 4] = [3, 7, 11, 19];
const S2: [u32; 4] = [3, 5, 9, 13];
const S3: [u32; 4] = [3, 9, 11, 15];

/// Message word indices for each round.
const M1: [usize; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
const M2: [usize; 16] = [0, 4, 8, 12, 1, 5, 9, 13, 2, 6, 10, 14, 3, 7, 11, 15];
const M3: [usize; 16] = [0, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5, 13, 3, 11, 7, 15];

/// Compute MD4 digest for input data.
pub fn digest(input: &[u8]) -> Digest {
    let mut state = [INIT_A, INIT_B, INIT_C, INIT_D];

    // Process complete 64-byte blocks
    let mut offset = 0;
    while offset + 64 <= input.len() {
        process_block(&mut state, &input[offset..offset + 64]);
        offset += 64;
    }

    // Pad and process final block(s)
    let remaining = &input[offset..];
    let bit_len = (input.len() as u64) * 8;

    // Padding: append 1 bit, then zeros, then 64-bit length
    let mut padded = [0u8; 128]; // Max 2 blocks needed
    padded[..remaining.len()].copy_from_slice(remaining);
    padded[remaining.len()] = 0x80;

    let pad_len = if remaining.len() < 56 { 64 } else { 128 };
    padded[pad_len - 8..pad_len].copy_from_slice(&bit_len.to_le_bytes());

    process_block(&mut state, &padded[..64]);
    if pad_len == 128 {
        process_block(&mut state, &padded[64..128]);
    }

    // Convert state to bytes (little-endian)
    let mut output = [0u8; 16];
    output[0..4].copy_from_slice(&state[0].to_le_bytes());
    output[4..8].copy_from_slice(&state[1].to_le_bytes());
    output[8..12].copy_from_slice(&state[2].to_le_bytes());
    output[12..16].copy_from_slice(&state[3].to_le_bytes());
    output
}

/// MD4 round function F: (X & Y) | (~X & Z)
#[inline(always)]
fn f(x: u32, y: u32, z: u32) -> u32 {
    (x & y) | ((!x) & z)
}

/// MD4 round function G: (X & Y) | (X & Z) | (Y & Z)
/// This is the "majority" function.
#[inline(always)]
fn g(x: u32, y: u32, z: u32) -> u32 {
    (x & y) | (x & z) | (y & z)
}

/// MD4 round function H: X ^ Y ^ Z
#[inline(always)]
fn h(x: u32, y: u32, z: u32) -> u32 {
    x ^ y ^ z
}

/// Process a single 64-byte block.
fn process_block(state: &mut [u32; 4], block: &[u8]) {
    debug_assert_eq!(block.len(), 64);

    // Parse block into 16 little-endian u32 words
    let mut m = [0u32; 16];
    for (i, chunk) in block.chunks_exact(4).enumerate() {
        m[i] = u32::from_le_bytes(chunk.try_into().unwrap());
    }

    let [mut a, mut b, mut c, mut d] = *state;

    // Round 1: F function
    for i in 0..16 {
        let tmp = a
            .wrapping_add(f(b, c, d))
            .wrapping_add(m[M1[i]])
            .wrapping_add(K[0]);
        a = d;
        d = c;
        c = b;
        b = tmp.rotate_left(S1[i % 4]);
    }

    // Round 2: G function
    for i in 0..16 {
        let tmp = a
            .wrapping_add(g(b, c, d))
            .wrapping_add(m[M2[i]])
            .wrapping_add(K[1]);
        a = d;
        d = c;
        c = b;
        b = tmp.rotate_left(S2[i % 4]);
    }

    // Round 3: H function
    for i in 0..16 {
        let tmp = a
            .wrapping_add(h(b, c, d))
            .wrapping_add(m[M3[i]])
            .wrapping_add(K[2]);
        a = d;
        d = c;
        c = b;
        b = tmp.rotate_left(S3[i % 4]);
    }

    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_hex(bytes: &[u8]) -> String {
        use std::fmt::Write;
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            write!(s, "{b:02x}").unwrap();
        }
        s
    }

    #[test]
    fn rfc1320_test_vectors() {
        // RFC 1320 Appendix A.5 test vectors
        let vectors: &[(&[u8], &str)] = &[
            (b"", "31d6cfe0d16ae931b73c59d7e0c089c0"),
            (b"a", "bde52cb31de33e46245e05fbdbd6fb24"),
            (b"abc", "a448017aaf21d8525fc10ae87aa6729d"),
            (b"message digest", "d9130a8164549fe818874806e1c7014b"),
            (b"abcdefghijklmnopqrstuvwxyz", "d79e1c308aa5bbcdeea8ed63df412da9"),
            (
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
                "043f8582f241db351ce627e153e7f0e4",
            ),
            (
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                "e33b4ddc9c38f2199c3e7b164fcc0536",
            ),
        ];

        for (input, expected) in vectors {
            let result = digest(input);
            assert_eq!(
                to_hex(&result),
                *expected,
                "Failed for input: {:?}",
                String::from_utf8_lossy(input)
            );
        }
    }

    #[test]
    fn block_boundary_edge_cases() {
        // Test inputs at various block boundaries
        for len in [55, 56, 57, 63, 64, 65, 119, 120, 121] {
            let input: Vec<u8> = (0..len).map(|i| (i % 256) as u8).collect();
            let result = digest(&input);
            // Just verify it doesn't panic and returns 16 bytes
            assert_eq!(result.len(), 16);
        }
    }
}
