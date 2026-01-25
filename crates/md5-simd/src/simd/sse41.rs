//! SSE4.1 4-lane parallel MD5 implementation.
//!
//! Processes 4 independent MD5 computations simultaneously using 128-bit XMM registers.
//!
//! # CPU Feature Requirements
//!
//! - **SSE4.1**: Intel Penryn (2007+), AMD Bulldozer (2011+) or newer
//! - Must be verified at runtime using `is_x86_feature_detected!("sse4.1")`
//!
//! # SIMD Strategy
//!
//! SSE4.1 provides a significant improvement over SSE2/SSSE3 with the `blendv`
//! instruction family. This implementation uses `_mm_blendv_epi8` for efficient
//! lane masking when inputs have different lengths.
//!
//! **Key advantage over SSE2**: The SSE2 implementation requires three instructions
//! (AND, ANDNOT, OR) to implement conditional blending, while SSE4.1 does it in
//! a single `blendv` instruction. This reduces instruction count and improves
//! performance for inputs with varying lengths.
//!
//! # Performance Characteristics
//!
//! - **Throughput**: ~4x scalar performance
//! - **Latency**: Similar to SSE2/SSSE3
//! - **Best use case**: Processing inputs with varying block counts
//! - **Efficiency**: Better than SSE2 for mixed-length inputs
//!
//! # Availability
//!
//! SSE4.1 is available on most processors from 2008 onwards but not guaranteed
//! on all x86_64. Always use runtime detection before calling this implementation.

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

use crate::Digest;

/// MD5 initial state constants.
const INIT_A: u32 = 0x6745_2301;
const INIT_B: u32 = 0xefcd_ab89;
const INIT_C: u32 = 0x98ba_dcfe;
const INIT_D: u32 = 0x1032_5476;

/// MD5 round constants.
const K: [u32; 64] = [
    0xd76a_a478, 0xe8c7_b756, 0x2420_70db, 0xc1bd_ceee, 0xf57c_0faf, 0x4787_c62a, 0xa830_4613, 0xfd46_9501,
    0x6980_98d8, 0x8b44_f7af, 0xffff_5bb1, 0x895c_d7be, 0x6b90_1122, 0xfd98_7193, 0xa679_438e, 0x49b4_0821,
    0xf61e_2562, 0xc040_b340, 0x265e_5a51, 0xe9b6_c7aa, 0xd62f_105d, 0x0244_1453, 0xd8a1_e681, 0xe7d3_fbc8,
    0x21e1_cde6, 0xc337_07d6, 0xf4d5_0d87, 0x455a_14ed, 0xa9e3_e905, 0xfcef_a3f8, 0x676f_02d9, 0x8d2a_4c8a,
    0xfffa_3942, 0x8771_f681, 0x6d9d_6122, 0xfde5_380c, 0xa4be_ea44, 0x4bde_cfa9, 0xf6bb_4b60, 0xbebf_bc70,
    0x289b_7ec6, 0xeaa1_27fa, 0xd4ef_3085, 0x0488_1d05, 0xd9d4_d039, 0xe6db_99e5, 0x1fa2_7cf8, 0xc4ac_5665,
    0xf429_2244, 0x432a_ff97, 0xab94_23a7, 0xfc93_a039, 0x655b_59c3, 0x8f0c_cc92, 0xffef_f47d, 0x8584_5dd1,
    0x6fa8_7e4f, 0xfe2c_e6e0, 0xa301_4314, 0x4e08_11a1, 0xf753_7e82, 0xbd3a_f235, 0x2ad7_d2bb, 0xeb86_d391,
];

const MAX_INPUT_SIZE: usize = 1_024 * 1_024;

/// Rotate left macros for compile-time constants.
macro_rules! rotl {
    ($x:expr, 4) => { _mm_or_si128(_mm_slli_epi32($x, 4), _mm_srli_epi32($x, 28)) };
    ($x:expr, 5) => { _mm_or_si128(_mm_slli_epi32($x, 5), _mm_srli_epi32($x, 27)) };
    ($x:expr, 6) => { _mm_or_si128(_mm_slli_epi32($x, 6), _mm_srli_epi32($x, 26)) };
    ($x:expr, 7) => { _mm_or_si128(_mm_slli_epi32($x, 7), _mm_srli_epi32($x, 25)) };
    ($x:expr, 9) => { _mm_or_si128(_mm_slli_epi32($x, 9), _mm_srli_epi32($x, 23)) };
    ($x:expr, 10) => { _mm_or_si128(_mm_slli_epi32($x, 10), _mm_srli_epi32($x, 22)) };
    ($x:expr, 11) => { _mm_or_si128(_mm_slli_epi32($x, 11), _mm_srli_epi32($x, 21)) };
    ($x:expr, 12) => { _mm_or_si128(_mm_slli_epi32($x, 12), _mm_srli_epi32($x, 20)) };
    ($x:expr, 14) => { _mm_or_si128(_mm_slli_epi32($x, 14), _mm_srli_epi32($x, 18)) };
    ($x:expr, 15) => { _mm_or_si128(_mm_slli_epi32($x, 15), _mm_srli_epi32($x, 17)) };
    ($x:expr, 16) => { _mm_or_si128(_mm_slli_epi32($x, 16), _mm_srli_epi32($x, 16)) };
    ($x:expr, 17) => { _mm_or_si128(_mm_slli_epi32($x, 17), _mm_srli_epi32($x, 15)) };
    ($x:expr, 20) => { _mm_or_si128(_mm_slli_epi32($x, 20), _mm_srli_epi32($x, 12)) };
    ($x:expr, 21) => { _mm_or_si128(_mm_slli_epi32($x, 21), _mm_srli_epi32($x, 11)) };
    ($x:expr, 22) => { _mm_or_si128(_mm_slli_epi32($x, 22), _mm_srli_epi32($x, 10)) };
    ($x:expr, 23) => { _mm_or_si128(_mm_slli_epi32($x, 23), _mm_srli_epi32($x, 9)) };
}

/// Compute MD5 digests for up to 4 inputs in parallel using SSE4.1.
///
/// Processes 4 independent byte slices in parallel, computing their MD5 digests
/// simultaneously. Uses SSE4.1's `blendv` instruction for more efficient lane
/// masking compared to SSE2.
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
/// This implementation is particularly efficient when processing inputs of
/// varying lengths, as the `blendv` instruction provides better masking
/// performance than SSE2's manual AND/ANDNOT/OR sequence.
///
/// # Safety
///
/// Caller must ensure SSE4.1 is available. Use runtime detection before calling:
///
/// ```ignore
/// if is_x86_feature_detected!("sse4.1") {
///     let digests = unsafe { digest_x4(&inputs) };
/// }
/// ```
///
/// This function uses `unsafe` internally for:
/// - SSE4.1 intrinsics (`_mm_*` functions including `_mm_blendv_epi8`)
/// - Aligned memory access via `_mm_store_si128`
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
pub unsafe fn digest_x4(inputs: &[&[u8]; 4]) -> [Digest; 4] {
    let max_len = inputs.iter().map(|i| i.len()).max().unwrap_or(0);

    if max_len > MAX_INPUT_SIZE {
        return std::array::from_fn(|i| crate::scalar::digest(inputs[i]));
    }

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

    let mut a = _mm_set1_epi32(INIT_A as i32);
    let mut b = _mm_set1_epi32(INIT_B as i32);
    let mut c = _mm_set1_epi32(INIT_C as i32);
    let mut d = _mm_set1_epi32(INIT_D as i32);

    for block_idx in 0..max_blocks {
        let block_offset = block_idx * 64;

        let lane_active: [i32; 4] = std::array::from_fn(|lane| {
            if block_idx < block_counts[lane] { -1 } else { 0 }
        });
        let mask = _mm_setr_epi32(lane_active[0], lane_active[1], lane_active[2], lane_active[3]);

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

        // Round 1
        macro_rules! round1 {
            ($a:ident, $b:ident, $c:ident, $d:ident, $mi:expr, $ki:expr, $s:tt) => {{
                let f = _mm_or_si128(_mm_and_si128($b, $c), _mm_andnot_si128($b, $d));
                let k = _mm_set1_epi32(K[$ki] as i32);
                let temp = _mm_add_epi32(_mm_add_epi32($a, f), _mm_add_epi32(k, m[$mi]));
                $a = _mm_add_epi32($b, rotl!(temp, $s));
            }};
        }

        round1!(a, b, c, d,  0,  0,  7); round1!(d, a, b, c,  1,  1, 12);
        round1!(c, d, a, b,  2,  2, 17); round1!(b, c, d, a,  3,  3, 22);
        round1!(a, b, c, d,  4,  4,  7); round1!(d, a, b, c,  5,  5, 12);
        round1!(c, d, a, b,  6,  6, 17); round1!(b, c, d, a,  7,  7, 22);
        round1!(a, b, c, d,  8,  8,  7); round1!(d, a, b, c,  9,  9, 12);
        round1!(c, d, a, b, 10, 10, 17); round1!(b, c, d, a, 11, 11, 22);
        round1!(a, b, c, d, 12, 12,  7); round1!(d, a, b, c, 13, 13, 12);
        round1!(c, d, a, b, 14, 14, 17); round1!(b, c, d, a, 15, 15, 22);

        // Round 2
        macro_rules! round2 {
            ($a:ident, $b:ident, $c:ident, $d:ident, $mi:expr, $ki:expr, $s:tt) => {{
                let g = _mm_or_si128(_mm_and_si128($b, $d), _mm_andnot_si128($d, $c));
                let k = _mm_set1_epi32(K[$ki] as i32);
                let temp = _mm_add_epi32(_mm_add_epi32($a, g), _mm_add_epi32(k, m[$mi]));
                $a = _mm_add_epi32($b, rotl!(temp, $s));
            }};
        }

        round2!(a, b, c, d,  1, 16,  5); round2!(d, a, b, c,  6, 17,  9);
        round2!(c, d, a, b, 11, 18, 14); round2!(b, c, d, a,  0, 19, 20);
        round2!(a, b, c, d,  5, 20,  5); round2!(d, a, b, c, 10, 21,  9);
        round2!(c, d, a, b, 15, 22, 14); round2!(b, c, d, a,  4, 23, 20);
        round2!(a, b, c, d,  9, 24,  5); round2!(d, a, b, c, 14, 25,  9);
        round2!(c, d, a, b,  3, 26, 14); round2!(b, c, d, a,  8, 27, 20);
        round2!(a, b, c, d, 13, 28,  5); round2!(d, a, b, c,  2, 29,  9);
        round2!(c, d, a, b,  7, 30, 14); round2!(b, c, d, a, 12, 31, 20);

        // Round 3
        macro_rules! round3 {
            ($a:ident, $b:ident, $c:ident, $d:ident, $mi:expr, $ki:expr, $s:tt) => {{
                let h = _mm_xor_si128(_mm_xor_si128($b, $c), $d);
                let k = _mm_set1_epi32(K[$ki] as i32);
                let temp = _mm_add_epi32(_mm_add_epi32($a, h), _mm_add_epi32(k, m[$mi]));
                $a = _mm_add_epi32($b, rotl!(temp, $s));
            }};
        }

        round3!(a, b, c, d,  5, 32,  4); round3!(d, a, b, c,  8, 33, 11);
        round3!(c, d, a, b, 11, 34, 16); round3!(b, c, d, a, 14, 35, 23);
        round3!(a, b, c, d,  1, 36,  4); round3!(d, a, b, c,  4, 37, 11);
        round3!(c, d, a, b,  7, 38, 16); round3!(b, c, d, a, 10, 39, 23);
        round3!(a, b, c, d, 13, 40,  4); round3!(d, a, b, c,  0, 41, 11);
        round3!(c, d, a, b,  3, 42, 16); round3!(b, c, d, a,  6, 43, 23);
        round3!(a, b, c, d,  9, 44,  4); round3!(d, a, b, c, 12, 45, 11);
        round3!(c, d, a, b, 15, 46, 16); round3!(b, c, d, a,  2, 47, 23);

        // Round 4
        macro_rules! round4 {
            ($a:ident, $b:ident, $c:ident, $d:ident, $mi:expr, $ki:expr, $s:tt) => {{
                let not_d = _mm_xor_si128($d, _mm_set1_epi32(-1));
                let i_val = _mm_xor_si128($c, _mm_or_si128($b, not_d));
                let k = _mm_set1_epi32(K[$ki] as i32);
                let temp = _mm_add_epi32(_mm_add_epi32($a, i_val), _mm_add_epi32(k, m[$mi]));
                $a = _mm_add_epi32($b, rotl!(temp, $s));
            }};
        }

        round4!(a, b, c, d,  0, 48,  6); round4!(d, a, b, c,  7, 49, 10);
        round4!(c, d, a, b, 14, 50, 15); round4!(b, c, d, a,  5, 51, 21);
        round4!(a, b, c, d, 12, 52,  6); round4!(d, a, b, c,  3, 53, 10);
        round4!(c, d, a, b, 10, 54, 15); round4!(b, c, d, a,  1, 55, 21);
        round4!(a, b, c, d,  8, 56,  6); round4!(d, a, b, c, 15, 57, 10);
        round4!(c, d, a, b,  6, 58, 15); round4!(b, c, d, a, 13, 59, 21);
        round4!(a, b, c, d,  4, 60,  6); round4!(d, a, b, c, 11, 61, 10);
        round4!(c, d, a, b,  2, 62, 15); round4!(b, c, d, a,  9, 63, 21);

        // Add saved state
        let new_a = _mm_add_epi32(a, aa);
        let new_b = _mm_add_epi32(b, bb);
        let new_c = _mm_add_epi32(c, cc);
        let new_d = _mm_add_epi32(d, dd);

        // SSE4.1 blendv for efficient conditional selection
        a = _mm_blendv_epi8(aa, new_a, mask);
        b = _mm_blendv_epi8(bb, new_b, mask);
        c = _mm_blendv_epi8(cc, new_c, mask);
        d = _mm_blendv_epi8(dd, new_d, mask);
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
    fn sse41_md5_matches_scalar() {
        if !is_x86_feature_detected!("sse4.1") {
            return;
        }

        let inputs: [&[u8]; 4] = [b"", b"a", b"abc", b"message digest"];
        let results = unsafe { digest_x4(&inputs) };

        for (i, input) in inputs.iter().enumerate() {
            let expected = crate::scalar::digest(input);
            assert_eq!(to_hex(&results[i]), to_hex(&expected), "Mismatch at lane {i}");
        }
    }

    #[test]
    fn sse41_md5_rfc1321_vectors() {
        if !is_x86_feature_detected!("sse4.1") {
            return;
        }

        let inputs: [&[u8]; 4] = [b"", b"a", b"abc", b"message digest"];
        let expected = [
            "d41d8cd98f00b204e9800998ecf8427e",
            "0cc175b9c0f1b6a831c399e269772661",
            "900150983cd24fb0d6963f7d28e17f72",
            "f96b697d7cb7938d525a2f31aaf161d0",
        ];

        let results = unsafe { digest_x4(&inputs) };

        for i in 0..4 {
            assert_eq!(to_hex(&results[i]), expected[i], "RFC 1321 mismatch at lane {i}");
        }
    }
}
