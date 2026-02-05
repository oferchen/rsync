//! Scalar (single-lane) MD5 implementation.
//!
//! Used as fallback when SIMD is unavailable or for single-hash workloads.

use super::Digest;

/// MD5 initial state constants (RFC 1321).
const INIT_A: u32 = 0x6745_2301;
const INIT_B: u32 = 0xefcd_ab89;
const INIT_C: u32 = 0x98ba_dcfe;
const INIT_D: u32 = 0x1032_5476;

/// Per-round shift amounts (RFC 1321).
const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// Pre-computed T[i] = floor(2^32 * |sin(i + 1)|) constants.
const K: [u32; 64] = [
    0xd76a_a478,
    0xe8c7_b756,
    0x2420_70db,
    0xc1bd_ceee,
    0xf57c_0faf,
    0x4787_c62a,
    0xa830_4613,
    0xfd46_9501,
    0x6980_98d8,
    0x8b44_f7af,
    0xffff_5bb1,
    0x895c_d7be,
    0x6b90_1122,
    0xfd98_7193,
    0xa679_438e,
    0x49b4_0821,
    0xf61e_2562,
    0xc040_b340,
    0x265e_5a51,
    0xe9b6_c7aa,
    0xd62f_105d,
    0x0244_1453,
    0xd8a1_e681,
    0xe7d3_fbc8,
    0x21e1_cde6,
    0xc337_07d6,
    0xf4d5_0d87,
    0x455a_14ed,
    0xa9e3_e905,
    0xfcef_a3f8,
    0x676f_02d9,
    0x8d2a_4c8a,
    0xfffa_3942,
    0x8771_f681,
    0x6d9d_6122,
    0xfde5_380c,
    0xa4be_ea44,
    0x4bde_cfa9,
    0xf6bb_4b60,
    0xbebf_bc70,
    0x289b_7ec6,
    0xeaa1_27fa,
    0xd4ef_3085,
    0x0488_1d05,
    0xd9d4_d039,
    0xe6db_99e5,
    0x1fa2_7cf8,
    0xc4ac_5665,
    0xf429_2244,
    0x432a_ff97,
    0xab94_23a7,
    0xfc93_a039,
    0x655b_59c3,
    0x8f0c_cc92,
    0xffef_f47d,
    0x8584_5dd1,
    0x6fa8_7e4f,
    0xfe2c_e6e0,
    0xa301_4314,
    0x4e08_11a1,
    0xf753_7e82,
    0xbd3a_f235,
    0x2ad7_d2bb,
    0xeb86_d391,
];

/// Compute MD5 digest for input data.
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

/// Process a single 64-byte block.
fn process_block(state: &mut [u32; 4], block: &[u8]) {
    debug_assert_eq!(block.len(), 64);

    // Parse block into 16 little-endian u32 words
    let mut m = [0u32; 16];
    for (i, chunk) in block.chunks_exact(4).enumerate() {
        m[i] = u32::from_le_bytes(chunk.try_into().unwrap());
    }

    let [mut a, mut b, mut c, mut d] = *state;

    for i in 0..64 {
        let (f, g) = match i {
            0..=15 => ((b & c) | ((!b) & d), i),
            16..=31 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
            32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
            _ => (c ^ (b | (!d)), (7 * i) % 16),
        };

        let temp = d;
        d = c;
        c = b;
        b = b.wrapping_add(
            (a.wrapping_add(f).wrapping_add(K[i]).wrapping_add(m[g])).rotate_left(S[i]),
        );
        a = temp;
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
    fn rfc1321_test_vectors() {
        // RFC 1321 Appendix A.5 test vectors
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
