//! SSE4.1 4-lane parallel MD5 implementation.
//!
//! SSE4.1 adds `blendvps` for efficient conditional blending, improving
//! the mask application for different-length inputs.
//! Available on Intel Penryn and later, AMD Bulldozer and later.

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

use crate::Digest;

/// MD5 initial state constants.
const INIT_A: u32 = 0x67452301;
const INIT_B: u32 = 0xefcdab89;
const INIT_C: u32 = 0x98badcfe;
const INIT_D: u32 = 0x10325476;

/// MD5 round constants.
const K: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
    0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
    0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
    0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
    0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
    0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
    0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
    0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
];

const MAX_INPUT_SIZE: usize = 1024 * 1024;

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
/// # Safety
/// Caller must ensure SSE4.1 is available.
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
        bytes.iter().map(|b| format!("{b:02x}")).collect()
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
