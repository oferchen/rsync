//! SSE2 4-lane parallel MD4 implementation.
//!
//! Processes 4 independent MD4 computations simultaneously using 128-bit XMM registers.
//! SSE2 is baseline for x86_64, so this is always available on 64-bit Intel/AMD processors.

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

use super::super::super::Digest;

/// MD4 initial state constants.
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

/// Message word indices for round 2.
const M2: [usize; 16] = [0, 4, 8, 12, 1, 5, 9, 13, 2, 6, 10, 14, 3, 7, 11, 15];
/// Message word indices for round 3.
const M3: [usize; 16] = [0, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5, 13, 3, 11, 7, 15];

/// Maximum input size supported.
const MAX_INPUT_SIZE: usize = 1_024 * 1_024;

/// Rotate left macro for SSE2 (requires compile-time constant).
macro_rules! rotl {
    ($x:expr, 3) => {
        _mm_or_si128(_mm_slli_epi32($x, 3), _mm_srli_epi32($x, 29))
    };
    ($x:expr, 5) => {
        _mm_or_si128(_mm_slli_epi32($x, 5), _mm_srli_epi32($x, 27))
    };
    ($x:expr, 7) => {
        _mm_or_si128(_mm_slli_epi32($x, 7), _mm_srli_epi32($x, 25))
    };
    ($x:expr, 9) => {
        _mm_or_si128(_mm_slli_epi32($x, 9), _mm_srli_epi32($x, 23))
    };
    ($x:expr, 11) => {
        _mm_or_si128(_mm_slli_epi32($x, 11), _mm_srli_epi32($x, 21))
    };
    ($x:expr, 13) => {
        _mm_or_si128(_mm_slli_epi32($x, 13), _mm_srli_epi32($x, 19))
    };
    ($x:expr, 15) => {
        _mm_or_si128(_mm_slli_epi32($x, 15), _mm_srli_epi32($x, 17))
    };
    ($x:expr, 19) => {
        _mm_or_si128(_mm_slli_epi32($x, 19), _mm_srli_epi32($x, 13))
    };
}

/// Compute MD4 digests for up to 4 inputs in parallel using SSE2.
///
/// # Safety
/// Caller must ensure SSE2 is available (always true on x86_64).
#[target_feature(enable = "sse2")]
pub unsafe fn digest_x4(inputs: &[&[u8]; 4]) -> [Digest; 4] {
    let max_len = inputs.iter().map(|i| i.len()).max().unwrap_or(0);

    if max_len > MAX_INPUT_SIZE {
        return std::array::from_fn(|i| super::super::scalar::digest(inputs[i]));
    }

    // Prepare padded buffers
    let padded_storage: Vec<Vec<u8>> = inputs
        .iter()
        .map(|input| {
            let len = input.len();
            let padded_len = (len + 9).div_ceil(64) * 64;
            let mut buf = vec![0u8; padded_len.max(64)];
            buf[..len].copy_from_slice(input);
            buf[len] = 0x80;
            let bit_len = (len as u64) * 8;
            buf[padded_len - 8..padded_len].copy_from_slice(&bit_len.to_le_bytes());
            buf
        })
        .collect();

    let block_counts: [usize; 4] = std::array::from_fn(|i| padded_storage[i].len() / 64);
    let max_blocks = block_counts.iter().max().copied().unwrap_or(0);

    // Initialize state (4 lanes)
    let mut a = _mm_set1_epi32(INIT_A as i32);
    let mut b = _mm_set1_epi32(INIT_B as i32);
    let mut c = _mm_set1_epi32(INIT_C as i32);
    let mut d = _mm_set1_epi32(INIT_D as i32);

    for block_idx in 0..max_blocks {
        let block_offset = block_idx * 64;

        // Create mask for active lanes
        let lane_active: [i32; 4] = std::array::from_fn(|lane| {
            if block_idx < block_counts[lane] {
                -1
            } else {
                0
            }
        });
        let mask = _mm_setr_epi32(
            lane_active[0],
            lane_active[1],
            lane_active[2],
            lane_active[3],
        );

        // Load message words (transposed)
        let mut m = [_mm_setzero_si128(); 16];
        for (word_idx, m_word) in m.iter_mut().enumerate() {
            let word_offset = block_offset + word_idx * 4;
            let words: [i32; 4] = std::array::from_fn(|lane| {
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
            *m_word = _mm_setr_epi32(words[0], words[1], words[2], words[3]);
        }

        let aa = a;
        let bb = b;
        let cc = c;
        let dd = d;

        // Round 1: F = (B & C) | (~B & D), shifts: 3,7,11,19
        let k1 = _mm_set1_epi32(K[0] as i32);
        macro_rules! round1 {
            ($i:expr, $s:tt) => {{
                let f_val = _mm_or_si128(_mm_and_si128(b, c), _mm_andnot_si128(b, d));
                let temp = _mm_add_epi32(_mm_add_epi32(a, f_val), _mm_add_epi32(k1, m[$i]));
                let rotated = rotl!(temp, $s);
                a = d;
                d = c;
                c = b;
                b = rotated;
            }};
        }

        round1!(0, 3);
        round1!(1, 7);
        round1!(2, 11);
        round1!(3, 19);
        round1!(4, 3);
        round1!(5, 7);
        round1!(6, 11);
        round1!(7, 19);
        round1!(8, 3);
        round1!(9, 7);
        round1!(10, 11);
        round1!(11, 19);
        round1!(12, 3);
        round1!(13, 7);
        round1!(14, 11);
        round1!(15, 19);

        // Round 2: G = (B & C) | (B & D) | (C & D), shifts: 3,5,9,13
        let k2 = _mm_set1_epi32(K[1] as i32);
        macro_rules! round2 {
            ($mi:expr, $s:tt) => {{
                let g_val = _mm_or_si128(_mm_and_si128(b, c), _mm_and_si128(d, _mm_or_si128(b, c)));
                let temp = _mm_add_epi32(_mm_add_epi32(a, g_val), _mm_add_epi32(k2, m[M2[$mi]]));
                let rotated = rotl!(temp, $s);
                a = d;
                d = c;
                c = b;
                b = rotated;
            }};
        }

        round2!(0, 3);
        round2!(1, 5);
        round2!(2, 9);
        round2!(3, 13);
        round2!(4, 3);
        round2!(5, 5);
        round2!(6, 9);
        round2!(7, 13);
        round2!(8, 3);
        round2!(9, 5);
        round2!(10, 9);
        round2!(11, 13);
        round2!(12, 3);
        round2!(13, 5);
        round2!(14, 9);
        round2!(15, 13);

        // Round 3: H = B ^ C ^ D, shifts: 3,9,11,15
        let k3 = _mm_set1_epi32(K[2] as i32);
        macro_rules! round3 {
            ($mi:expr, $s:tt) => {{
                let h_val = _mm_xor_si128(_mm_xor_si128(b, c), d);
                let temp = _mm_add_epi32(_mm_add_epi32(a, h_val), _mm_add_epi32(k3, m[M3[$mi]]));
                let rotated = rotl!(temp, $s);
                a = d;
                d = c;
                c = b;
                b = rotated;
            }};
        }

        round3!(0, 3);
        round3!(1, 9);
        round3!(2, 11);
        round3!(3, 15);
        round3!(4, 3);
        round3!(5, 9);
        round3!(6, 11);
        round3!(7, 15);
        round3!(8, 3);
        round3!(9, 9);
        round3!(10, 11);
        round3!(11, 15);
        round3!(12, 3);
        round3!(13, 9);
        round3!(14, 11);
        round3!(15, 15);

        // Add saved state
        let new_a = _mm_add_epi32(a, aa);
        let new_b = _mm_add_epi32(b, bb);
        let new_c = _mm_add_epi32(c, cc);
        let new_d = _mm_add_epi32(d, dd);

        // Blend using mask (SSE2 doesn't have blendv)
        let not_mask = _mm_xor_si128(mask, _mm_set1_epi32(-1));
        a = _mm_or_si128(_mm_and_si128(mask, new_a), _mm_and_si128(not_mask, aa));
        b = _mm_or_si128(_mm_and_si128(mask, new_b), _mm_and_si128(not_mask, bb));
        c = _mm_or_si128(_mm_and_si128(mask, new_c), _mm_and_si128(not_mask, cc));
        d = _mm_or_si128(_mm_and_si128(mask, new_d), _mm_and_si128(not_mask, dd));
    }

    // Extract results
    let mut results = [[0u8; 16]; 4];

    #[repr(C, align(16))]
    struct Aligned([i32; 4]);

    let mut a_out = Aligned([0; 4]);
    let mut b_out = Aligned([0; 4]);
    let mut c_out = Aligned([0; 4]);
    let mut d_out = Aligned([0; 4]);

    _mm_store_si128(a_out.0.as_mut_ptr() as *mut __m128i, a);
    _mm_store_si128(b_out.0.as_mut_ptr() as *mut __m128i, b);
    _mm_store_si128(c_out.0.as_mut_ptr() as *mut __m128i, c);
    _mm_store_si128(d_out.0.as_mut_ptr() as *mut __m128i, d);

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
    fn sse2_md4_matches_scalar() {
        let inputs: [&[u8]; 4] = [b"", b"a", b"abc", b"message digest"];

        let results = unsafe { digest_x4(&inputs) };

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
    fn sse2_md4_rfc1320_vectors() {
        let inputs: [&[u8]; 4] = [b"", b"a", b"abc", b"message digest"];

        let expected = [
            "31d6cfe0d16ae931b73c59d7e0c089c0",
            "bde52cb31de33e46245e05fbdbd6fb24",
            "a448017aaf21d8525fc10ae87aa6729d",
            "d9130a8164549fe818874806e1c7014b",
        ];

        let results = unsafe { digest_x4(&inputs) };

        for i in 0..4 {
            assert_eq!(
                to_hex(&results[i]),
                expected[i],
                "RFC 1320 vector mismatch at lane {i}"
            );
        }
    }

    #[test]
    fn sse2_md4_various_lengths() {
        let input0: Vec<u8> = (0..55).map(|i| (i % 256) as u8).collect();
        let input1: Vec<u8> = (0..56).map(|i| (i % 256) as u8).collect();
        let input2: Vec<u8> = (0..64).map(|i| (i % 256) as u8).collect();
        let input3: Vec<u8> = (0..65).map(|i| (i % 256) as u8).collect();

        let inputs: [&[u8]; 4] = [&input0, &input1, &input2, &input3];

        let results = unsafe { digest_x4(&inputs) };

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
