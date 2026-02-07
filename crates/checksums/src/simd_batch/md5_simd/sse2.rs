//! SSE2 4-lane parallel MD5 implementation.
//!
//! Processes 4 independent MD5 computations simultaneously using 128-bit XMM registers.
//!
//! # CPU Feature Requirements
//!
//! - **SSE2**: Always available on x86_64 (baseline requirement)
//! - No runtime feature detection needed on 64-bit platforms
//!
//! # SIMD Strategy
//!
//! This implementation uses a transposed data layout where each XMM register holds
//! the same state variable (A, B, C, or D) for all 4 parallel computations. The MD5
//! algorithm's 64 rounds are executed in parallel across all lanes.

#![allow(unsafe_op_in_unsafe_fn)]
//!
//! Message words are loaded in transposed order: for each of the 16 message words,
//! we load word N from all 4 inputs into a single XMM register. This allows efficient
//! parallel processing of the MD5 rounds.
//!
//! # Performance Characteristics
//!
//! - **Throughput**: ~4x scalar performance when all 4 lanes are active
//! - **Latency**: Similar to scalar for single input
//! - **Best use case**: Processing 4 or more inputs of similar lengths
//!
//! # Lane Masking
//!
//! When inputs have different lengths (requiring different numbers of blocks), inactive
//! lanes are masked out using bitwise operations. The implementation uses SSE2's AND/ANDNOT/OR
//! instructions for masking since SSE2 lacks dedicated blend instructions.
//!
//! # Input Size Limits
//!
//! Falls back to scalar implementation for inputs exceeding 1 MB to avoid excessive
//! memory allocation for padding buffers.

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

use super::super::Digest;

/// MD5 initial state constants.
const INIT_A: u32 = 0x6745_2301;
const INIT_B: u32 = 0xefcd_ab89;
const INIT_C: u32 = 0x98ba_dcfe;
const INIT_D: u32 = 0x1032_5476;

/// Pre-computed K constants (RFC 1321).
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

/// Maximum input size supported.
const MAX_INPUT_SIZE: usize = 1_024 * 1_024;

/// Rotate left macro for SSE2 (requires compile-time constant).
///
/// SSE2 lacks a rotate instruction, so rotation is implemented using
/// logical left shift combined with logical right shift and OR.
/// The shift amounts must be compile-time constants.
macro_rules! rotl {
    ($x:expr, 4) => {
        _mm_or_si128(_mm_slli_epi32($x, 4), _mm_srli_epi32($x, 28))
    };
    ($x:expr, 5) => {
        _mm_or_si128(_mm_slli_epi32($x, 5), _mm_srli_epi32($x, 27))
    };
    ($x:expr, 6) => {
        _mm_or_si128(_mm_slli_epi32($x, 6), _mm_srli_epi32($x, 26))
    };
    ($x:expr, 7) => {
        _mm_or_si128(_mm_slli_epi32($x, 7), _mm_srli_epi32($x, 25))
    };
    ($x:expr, 9) => {
        _mm_or_si128(_mm_slli_epi32($x, 9), _mm_srli_epi32($x, 23))
    };
    ($x:expr, 10) => {
        _mm_or_si128(_mm_slli_epi32($x, 10), _mm_srli_epi32($x, 22))
    };
    ($x:expr, 11) => {
        _mm_or_si128(_mm_slli_epi32($x, 11), _mm_srli_epi32($x, 21))
    };
    ($x:expr, 12) => {
        _mm_or_si128(_mm_slli_epi32($x, 12), _mm_srli_epi32($x, 20))
    };
    ($x:expr, 14) => {
        _mm_or_si128(_mm_slli_epi32($x, 14), _mm_srli_epi32($x, 18))
    };
    ($x:expr, 15) => {
        _mm_or_si128(_mm_slli_epi32($x, 15), _mm_srli_epi32($x, 17))
    };
    ($x:expr, 16) => {
        _mm_or_si128(_mm_slli_epi32($x, 16), _mm_srli_epi32($x, 16))
    };
    ($x:expr, 17) => {
        _mm_or_si128(_mm_slli_epi32($x, 17), _mm_srli_epi32($x, 15))
    };
    ($x:expr, 20) => {
        _mm_or_si128(_mm_slli_epi32($x, 20), _mm_srli_epi32($x, 12))
    };
    ($x:expr, 21) => {
        _mm_or_si128(_mm_slli_epi32($x, 21), _mm_srli_epi32($x, 11))
    };
    ($x:expr, 22) => {
        _mm_or_si128(_mm_slli_epi32($x, 22), _mm_srli_epi32($x, 10))
    };
    ($x:expr, 23) => {
        _mm_or_si128(_mm_slli_epi32($x, 23), _mm_srli_epi32($x, 9))
    };
}

/// Compute MD5 digests for up to 4 inputs in parallel using SSE2.
///
/// Processes 4 independent byte slices in parallel, computing their MD5 digests
/// simultaneously. The implementation handles inputs of varying lengths by using
/// lane masking for blocks that don't exist in shorter inputs.
///
/// # Arguments
///
/// * `inputs` - Array of 4 byte slices to hash
///
/// # Returns
///
/// Array of 4 MD5 digests (16 bytes each) in the same order as the inputs
///
/// # Performance
///
/// Best performance is achieved when:
/// - All 4 input slots are used
/// - Inputs have similar lengths (minimizes masked blocks)
/// - Input sizes are reasonable (< 1 MB)
///
/// # Safety
///
/// Caller must ensure SSE2 is available. On x86_64, SSE2 is always available
/// as it's part of the baseline ISA, so this function is always safe to call
/// on 64-bit platforms.
///
/// This function uses `unsafe` internally for:
/// - SSE2 intrinsics (`_mm_*` functions)
/// - Aligned memory access via `_mm_store_si128`
///
/// # Examples
///
/// ```ignore
/// let inputs = [b"hello", b"world", b"foo", b"bar"];
/// let digests = unsafe { digest_x4(&inputs) };
/// ```
#[target_feature(enable = "sse2")]
pub unsafe fn digest_x4(inputs: &[&[u8]; 4]) -> [Digest; 4] {
    let max_len = inputs.iter().map(|i| i.len()).max().unwrap_or(0);

    if max_len > MAX_INPUT_SIZE {
        return std::array::from_fn(|i| super::super::md5_scalar::digest(inputs[i]));
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

        // Round 1: F = (B & C) | (~B & D), shifts: 7,12,17,22
        macro_rules! round1 {
            ($i:expr, $g:expr, $s:tt) => {{
                let f = _mm_or_si128(_mm_and_si128(b, c), _mm_andnot_si128(b, d));
                let k_i = _mm_set1_epi32(K[$i] as i32);
                let temp = _mm_add_epi32(_mm_add_epi32(a, f), _mm_add_epi32(k_i, m[$g]));
                let rotated = rotl!(temp, $s);
                a = d;
                d = c;
                c = b;
                b = _mm_add_epi32(b, rotated);
            }};
        }

        round1!(0, 0, 7);
        round1!(1, 1, 12);
        round1!(2, 2, 17);
        round1!(3, 3, 22);
        round1!(4, 4, 7);
        round1!(5, 5, 12);
        round1!(6, 6, 17);
        round1!(7, 7, 22);
        round1!(8, 8, 7);
        round1!(9, 9, 12);
        round1!(10, 10, 17);
        round1!(11, 11, 22);
        round1!(12, 12, 7);
        round1!(13, 13, 12);
        round1!(14, 14, 17);
        round1!(15, 15, 22);

        // Round 2: G = (D & B) | (~D & C), shifts: 5,9,14,20
        macro_rules! round2 {
            ($i:expr, $g:expr, $s:tt) => {{
                let f = _mm_or_si128(_mm_and_si128(d, b), _mm_andnot_si128(d, c));
                let k_i = _mm_set1_epi32(K[$i] as i32);
                let temp = _mm_add_epi32(_mm_add_epi32(a, f), _mm_add_epi32(k_i, m[$g]));
                let rotated = rotl!(temp, $s);
                a = d;
                d = c;
                c = b;
                b = _mm_add_epi32(b, rotated);
            }};
        }

        round2!(16, 1, 5);
        round2!(17, 6, 9);
        round2!(18, 11, 14);
        round2!(19, 0, 20);
        round2!(20, 5, 5);
        round2!(21, 10, 9);
        round2!(22, 15, 14);
        round2!(23, 4, 20);
        round2!(24, 9, 5);
        round2!(25, 14, 9);
        round2!(26, 3, 14);
        round2!(27, 8, 20);
        round2!(28, 13, 5);
        round2!(29, 2, 9);
        round2!(30, 7, 14);
        round2!(31, 12, 20);

        // Round 3: H = B ^ C ^ D, shifts: 4,11,16,23
        macro_rules! round3 {
            ($i:expr, $g:expr, $s:tt) => {{
                let f = _mm_xor_si128(_mm_xor_si128(b, c), d);
                let k_i = _mm_set1_epi32(K[$i] as i32);
                let temp = _mm_add_epi32(_mm_add_epi32(a, f), _mm_add_epi32(k_i, m[$g]));
                let rotated = rotl!(temp, $s);
                a = d;
                d = c;
                c = b;
                b = _mm_add_epi32(b, rotated);
            }};
        }

        round3!(32, 5, 4);
        round3!(33, 8, 11);
        round3!(34, 11, 16);
        round3!(35, 14, 23);
        round3!(36, 1, 4);
        round3!(37, 4, 11);
        round3!(38, 7, 16);
        round3!(39, 10, 23);
        round3!(40, 13, 4);
        round3!(41, 0, 11);
        round3!(42, 3, 16);
        round3!(43, 6, 23);
        round3!(44, 9, 4);
        round3!(45, 12, 11);
        round3!(46, 15, 16);
        round3!(47, 2, 23);

        // Round 4: I = C ^ (B | ~D), shifts: 6,10,15,21
        macro_rules! round4 {
            ($i:expr, $g:expr, $s:tt) => {{
                let not_d = _mm_xor_si128(d, _mm_set1_epi32(-1));
                let f = _mm_xor_si128(c, _mm_or_si128(b, not_d));
                let k_i = _mm_set1_epi32(K[$i] as i32);
                let temp = _mm_add_epi32(_mm_add_epi32(a, f), _mm_add_epi32(k_i, m[$g]));
                let rotated = rotl!(temp, $s);
                a = d;
                d = c;
                c = b;
                b = _mm_add_epi32(b, rotated);
            }};
        }

        round4!(48, 0, 6);
        round4!(49, 7, 10);
        round4!(50, 14, 15);
        round4!(51, 5, 21);
        round4!(52, 12, 6);
        round4!(53, 3, 10);
        round4!(54, 10, 15);
        round4!(55, 1, 21);
        round4!(56, 8, 6);
        round4!(57, 15, 10);
        round4!(58, 6, 15);
        round4!(59, 13, 21);
        round4!(60, 4, 6);
        round4!(61, 11, 10);
        round4!(62, 2, 15);
        round4!(63, 9, 21);

        // Add saved state
        let new_a = _mm_add_epi32(a, aa);
        let new_b = _mm_add_epi32(b, bb);
        let new_c = _mm_add_epi32(c, cc);
        let new_d = _mm_add_epi32(d, dd);

        // Blend using mask (SSE2 doesn't have blendv, use AND/ANDNOT/OR)
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
    use super::super::super::md5_scalar;
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
    fn sse2_matches_scalar() {
        let inputs: [&[u8]; 4] = [b"", b"a", b"abc", b"message digest"];

        let results = unsafe { digest_x4(&inputs) };

        for (i, input) in inputs.iter().enumerate() {
            let expected = md5_scalar::digest(input);
            assert_eq!(
                to_hex(&results[i]),
                to_hex(&expected),
                "Mismatch at lane {i} for input {:?}",
                String::from_utf8_lossy(input)
            );
        }
    }

    #[test]
    fn sse2_rfc1321_vectors() {
        let inputs: [&[u8]; 4] = [b"", b"a", b"abc", b"message digest"];

        let expected = [
            "d41d8cd98f00b204e9800998ecf8427e",
            "0cc175b9c0f1b6a831c399e269772661",
            "900150983cd24fb0d6963f7d28e17f72",
            "f96b697d7cb7938d525a2f31aaf161d0",
        ];

        let results = unsafe { digest_x4(&inputs) };

        for i in 0..4 {
            assert_eq!(
                to_hex(&results[i]),
                expected[i],
                "RFC 1321 vector mismatch at lane {i}"
            );
        }
    }

    #[test]
    fn sse2_various_lengths() {
        let input0: Vec<u8> = (0..55).map(|i| (i % 256) as u8).collect();
        let input1: Vec<u8> = (0..56).map(|i| (i % 256) as u8).collect();
        let input2: Vec<u8> = (0..64).map(|i| (i % 256) as u8).collect();
        let input3: Vec<u8> = (0..65).map(|i| (i % 256) as u8).collect();

        let inputs: [&[u8]; 4] = [&input0, &input1, &input2, &input3];

        let results = unsafe { digest_x4(&inputs) };

        for (i, input) in inputs.iter().enumerate() {
            let expected = md5_scalar::digest(input);
            assert_eq!(
                to_hex(&results[i]),
                to_hex(&expected),
                "Mismatch at lane {i} for input length {}",
                input.len()
            );
        }
    }
}
