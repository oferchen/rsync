//! AVX2 8-lane parallel MD4 implementation.
//!
//! Processes 8 independent MD4 computations simultaneously using 256-bit YMM registers.

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

use super::super::super::Digest;

/// MD4 initial state constants broadcast to 8 lanes.
const INIT_A: u32 = 0x6745_2301;
const INIT_B: u32 = 0xefcd_ab89;
const INIT_C: u32 = 0x98ba_dcfe;
const INIT_D: u32 = 0x1032_5476;

/// Round constants for MD4.
const K: [u32; 3] = [
    0x0000_0000, // Round 1
    0x5A82_7999, // Round 2
    0x6ED9_EBA1, // Round 3
];

/// Shift amounts for each round.
const S1: [i32; 4] = [3, 7, 11, 19];
const S2: [i32; 4] = [3, 5, 9, 13];
const S3: [i32; 4] = [3, 9, 11, 15];

/// Message word indices for each round.
const M2: [usize; 16] = [0, 4, 8, 12, 1, 5, 9, 13, 2, 6, 10, 14, 3, 7, 11, 15];
const M3: [usize; 16] = [0, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5, 13, 3, 11, 7, 15];

/// Maximum input size supported.
const MAX_INPUT_SIZE: usize = 1_024 * 1_024; // 1MB per input

/// Rotate left helper - AVX2 doesn't have a rotate instruction
#[target_feature(enable = "avx2")]
unsafe fn rotl(x: __m256i, n: i32) -> __m256i {
    _mm256_or_si256(
        _mm256_sllv_epi32(x, _mm256_set1_epi32(n)),
        _mm256_srlv_epi32(x, _mm256_set1_epi32(32 - n)),
    )
}

/// Compute MD4 digests for up to 8 inputs in parallel using AVX2.
///
/// # Safety
/// Caller must ensure AVX2 is available.
#[target_feature(enable = "avx2")]
pub unsafe fn digest_x8(inputs: &[&[u8]; 8]) -> [Digest; 8] {
    // Find the maximum length to determine block count
    let max_len = inputs.iter().map(|i| i.len()).max().unwrap_or(0);

    // For very large inputs, fall back to scalar to avoid huge allocations
    if max_len > MAX_INPUT_SIZE {
        return std::array::from_fn(|i| super::super::scalar::digest(inputs[i]));
    }

    // Prepare padded buffers for each input and track block counts
    let padded_storage: Vec<Vec<u8>> = inputs
        .iter()
        .map(|input| {
            let len = input.len();
            let individual_padded_len = (len + 9).div_ceil(64) * 64;
            let mut buf = vec![0u8; individual_padded_len.max(64)];
            buf[..len].copy_from_slice(input);
            buf[len] = 0x80;
            let bit_len = (len as u64) * 8;
            buf[individual_padded_len - 8..individual_padded_len]
                .copy_from_slice(&bit_len.to_le_bytes());
            buf
        })
        .collect();

    // Track block counts per lane
    let block_counts: [usize; 8] = std::array::from_fn(|i| padded_storage[i].len() / 64);
    let max_blocks = block_counts.iter().max().copied().unwrap_or(0);

    // Initialize state (8 lanes)
    let mut a = _mm256_set1_epi32(INIT_A as i32);
    let mut b = _mm256_set1_epi32(INIT_B as i32);
    let mut c = _mm256_set1_epi32(INIT_C as i32);
    let mut d = _mm256_set1_epi32(INIT_D as i32);

    // Process blocks - use masking for lanes with fewer blocks
    for block_idx in 0..max_blocks {
        let block_offset = block_idx * 64;

        // Create mask for active lanes
        let lane_active: [i32; 8] = std::array::from_fn(|lane| {
            if block_idx < block_counts[lane] {
                -1
            } else {
                0
            }
        });
        let mask = _mm256_setr_epi32(
            lane_active[0],
            lane_active[1],
            lane_active[2],
            lane_active[3],
            lane_active[4],
            lane_active[5],
            lane_active[6],
            lane_active[7],
        );

        // Load message words (transposed: word i from all 8 inputs)
        let mut m = [_mm256_setzero_si256(); 16];
        #[allow(clippy::needless_range_loop)]
        for word_idx in 0..16 {
            let word_offset = block_offset + word_idx * 4;
            let words: [i32; 8] = std::array::from_fn(|lane| {
                if word_offset + 4 <= padded_storage[lane].len() {
                    i32::from_le_bytes(
                        padded_storage[lane][word_offset..word_offset + 4]
                            .try_into()
                            .unwrap(),
                    )
                } else {
                    0
                }
            });
            m[word_idx] = _mm256_setr_epi32(
                words[0], words[1], words[2], words[3], words[4], words[5], words[6], words[7],
            );
        }

        // Save state for this block
        let aa = a;
        let bb = b;
        let cc = c;
        let dd = d;

        // Round 1: F = (B & C) | (~B & D), message indices 0-15
        let k1 = _mm256_set1_epi32(K[0] as i32);
        for i in 0..16 {
            let f_val = _mm256_or_si256(_mm256_and_si256(b, c), _mm256_andnot_si256(b, d));
            let temp = _mm256_add_epi32(_mm256_add_epi32(a, f_val), _mm256_add_epi32(k1, m[i]));
            let rotated = rotl(temp, S1[i % 4]);
            a = d;
            d = c;
            c = b;
            b = rotated;
        }

        // Round 2: G = (B & C) | (B & D) | (C & D), special message schedule
        let k2 = _mm256_set1_epi32(K[1] as i32);
        for i in 0..16 {
            // Majority function: (B & C) | (B & D) | (C & D)
            // Equivalent to: (B & C) | (D & (B | C))
            let g_val = _mm256_or_si256(
                _mm256_and_si256(b, c),
                _mm256_and_si256(d, _mm256_or_si256(b, c)),
            );
            let temp = _mm256_add_epi32(_mm256_add_epi32(a, g_val), _mm256_add_epi32(k2, m[M2[i]]));
            let rotated = rotl(temp, S2[i % 4]);
            a = d;
            d = c;
            c = b;
            b = rotated;
        }

        // Round 3: H = B ^ C ^ D, special message schedule
        let k3 = _mm256_set1_epi32(K[2] as i32);
        for i in 0..16 {
            let h_val = _mm256_xor_si256(_mm256_xor_si256(b, c), d);
            let temp = _mm256_add_epi32(_mm256_add_epi32(a, h_val), _mm256_add_epi32(k3, m[M3[i]]));
            let rotated = rotl(temp, S3[i % 4]);
            a = d;
            d = c;
            c = b;
            b = rotated;
        }

        // Compute new state
        let new_a = _mm256_add_epi32(a, aa);
        let new_b = _mm256_add_epi32(b, bb);
        let new_c = _mm256_add_epi32(c, cc);
        let new_d = _mm256_add_epi32(d, dd);

        // Blend: use new state for active lanes, preserve old state for inactive lanes
        a = _mm256_blendv_epi8(aa, new_a, mask);
        b = _mm256_blendv_epi8(bb, new_b, mask);
        c = _mm256_blendv_epi8(cc, new_c, mask);
        d = _mm256_blendv_epi8(dd, new_d, mask);
    }

    // Extract results
    let mut results = [[0u8; 16]; 8];

    #[repr(C, align(32))]
    struct Aligned([i32; 8]);

    let mut a_out = Aligned([0; 8]);
    let mut b_out = Aligned([0; 8]);
    let mut c_out = Aligned([0; 8]);
    let mut d_out = Aligned([0; 8]);

    _mm256_store_si256(a_out.0.as_mut_ptr() as *mut __m256i, a);
    _mm256_store_si256(b_out.0.as_mut_ptr() as *mut __m256i, b);
    _mm256_store_si256(c_out.0.as_mut_ptr() as *mut __m256i, c);
    _mm256_store_si256(d_out.0.as_mut_ptr() as *mut __m256i, d);

    for (lane, result) in results.iter_mut().enumerate() {
        result[0..4].copy_from_slice(&(a_out.0[lane] as u32).to_le_bytes());
        result[4..8].copy_from_slice(&(b_out.0[lane] as u32).to_le_bytes());
        result[8..12].copy_from_slice(&(c_out.0[lane] as u32).to_le_bytes());
        result[12..16].copy_from_slice(&(d_out.0[lane] as u32).to_le_bytes());
    }

    results
}

#[cfg(test)]
mod tests {
    use super::super::super::scalar;
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
    fn avx2_md4_matches_scalar() {
        if !is_x86_feature_detected!("avx2") {
            eprintln!("AVX2 not available, skipping test");
            return;
        }

        let inputs: [&[u8]; 8] = [
            b"",
            b"a",
            b"abc",
            b"message digest",
            b"abcdefghijklmnopqrstuvwxyz",
            b"test input 5",
            b"test input 6",
            b"test input 7",
        ];

        let results = unsafe { digest_x8(&inputs) };

        for (i, input) in inputs.iter().enumerate() {
            let expected = scalar::digest(input);
            assert_eq!(
                to_hex(&results[i]),
                to_hex(&expected),
                "Mismatch at lane {i} for input {:?}",
                String::from_utf8_lossy(input)
            );
        }
    }

    #[test]
    fn avx2_md4_rfc1320_vectors() {
        if !is_x86_feature_detected!("avx2") {
            eprintln!("AVX2 not available, skipping test");
            return;
        }

        let inputs: [&[u8]; 8] = [
            b"",
            b"a",
            b"abc",
            b"message digest",
            b"abcdefghijklmnopqrstuvwxyz",
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
            b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
            b"",
        ];

        let expected = [
            "31d6cfe0d16ae931b73c59d7e0c089c0",
            "bde52cb31de33e46245e05fbdbd6fb24",
            "a448017aaf21d8525fc10ae87aa6729d",
            "d9130a8164549fe818874806e1c7014b",
            "d79e1c308aa5bbcdeea8ed63df412da9",
            "043f8582f241db351ce627e153e7f0e4",
            "e33b4ddc9c38f2199c3e7b164fcc0536",
            "31d6cfe0d16ae931b73c59d7e0c089c0",
        ];

        let results = unsafe { digest_x8(&inputs) };

        for i in 0..8 {
            assert_eq!(
                to_hex(&results[i]),
                expected[i],
                "RFC 1320 vector mismatch at lane {i}"
            );
        }
    }

    #[test]
    fn avx2_md4_various_lengths() {
        if !is_x86_feature_detected!("avx2") {
            eprintln!("AVX2 not available, skipping test");
            return;
        }

        // Test with varying lengths including block boundaries
        let input0: Vec<u8> = (0..55).map(|i| (i % 256) as u8).collect();
        let input1: Vec<u8> = (0..56).map(|i| (i % 256) as u8).collect();
        let input2: Vec<u8> = (0..64).map(|i| (i % 256) as u8).collect();
        let input3: Vec<u8> = (0..65).map(|i| (i % 256) as u8).collect();
        let input4: Vec<u8> = (0..128).map(|i| (i % 256) as u8).collect();
        let input5: Vec<u8> = (0..200).map(|i| (i % 256) as u8).collect();
        let input6: Vec<u8> = (0..1_000).map(|i| (i % 256) as u8).collect();
        let input7: Vec<u8> = vec![];

        let inputs: [&[u8]; 8] = [
            &input0, &input1, &input2, &input3, &input4, &input5, &input6, &input7,
        ];

        let results = unsafe { digest_x8(&inputs) };

        for (i, input) in inputs.iter().enumerate() {
            let expected = scalar::digest(input);
            assert_eq!(
                to_hex(&results[i]),
                to_hex(&expected),
                "Mismatch at lane {i} for input length {}",
                input.len()
            );
        }
    }
}
