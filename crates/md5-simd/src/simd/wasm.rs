//! WebAssembly SIMD 4-lane parallel MD5 implementation.
//!
//! Processes 4 independent MD5 computations simultaneously using 128-bit WASM SIMD.
//! WASM SIMD is widely supported in modern browsers and runtimes.

#[cfg(target_arch = "wasm32")]
use std::arch::wasm32::*;

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

/// Maximum input size supported.
const MAX_INPUT_SIZE: usize = 1024 * 1024;

/// Rotate left for WASM SIMD (requires runtime shift amount).
#[cfg(target_arch = "wasm32")]
#[inline(always)]
fn rotl(x: v128, n: u32) -> v128 {
    v128_or(
        i32x4_shl(x, n),
        u32x4_shr(x, 32 - n),
    )
}

/// Compute MD5 digests for up to 4 inputs in parallel using WASM SIMD.
///
/// # Safety
/// Caller must ensure WASM SIMD is available.
#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
pub fn digest_x4(inputs: &[&[u8]; 4]) -> [Digest; 4] {
    let max_len = inputs.iter().map(|i| i.len()).max().unwrap_or(0);

    if max_len > MAX_INPUT_SIZE {
        return std::array::from_fn(|i| crate::scalar::digest(inputs[i]));
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

    // Initialize state
    let mut a = u32x4_splat(INIT_A);
    let mut b = u32x4_splat(INIT_B);
    let mut c = u32x4_splat(INIT_C);
    let mut d = u32x4_splat(INIT_D);

    for block_idx in 0..max_blocks {
        let block_offset = block_idx * 64;

        // Create mask for active lanes
        let lane_active: [u32; 4] = std::array::from_fn(|lane| {
            if block_idx < block_counts[lane] { 0xFFFFFFFF } else { 0 }
        });
        let mask = u32x4(lane_active[0], lane_active[1], lane_active[2], lane_active[3]);

        // Load message words
        let mut m = [u32x4_splat(0); 16];
        for (word_idx, m_word) in m.iter_mut().enumerate() {
            let word_offset = block_offset + word_idx * 4;
            let words: [u32; 4] = std::array::from_fn(|lane| {
                if word_offset + 4 <= padded_storage[lane].len() {
                    u32::from_le_bytes(
                        padded_storage[lane][word_offset..word_offset + 4]
                            .try_into()
                            .unwrap(),
                    )
                } else {
                    0
                }
            });
            *m_word = u32x4(words[0], words[1], words[2], words[3]);
        }

        let aa = a;
        let bb = b;
        let cc = c;
        let dd = d;

        // Round 1: F = (B & C) | (~B & D)
        macro_rules! round1 {
            ($a:ident, $b:ident, $c:ident, $d:ident, $mi:expr, $ki:expr, $s:expr) => {{
                let f = v128_or(v128_and($b, $c), v128_andnot($d, $b));
                let k = u32x4_splat(K[$ki]);
                let temp = i32x4_add(i32x4_add($a, f), i32x4_add(k, m[$mi]));
                $a = i32x4_add($b, rotl(temp, $s));
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

        // Round 2: G = (B & D) | (C & ~D)
        macro_rules! round2 {
            ($a:ident, $b:ident, $c:ident, $d:ident, $mi:expr, $ki:expr, $s:expr) => {{
                let g = v128_or(v128_and($b, $d), v128_andnot($c, $d));
                let k = u32x4_splat(K[$ki]);
                let temp = i32x4_add(i32x4_add($a, g), i32x4_add(k, m[$mi]));
                $a = i32x4_add($b, rotl(temp, $s));
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

        // Round 3: H = B ^ C ^ D
        macro_rules! round3 {
            ($a:ident, $b:ident, $c:ident, $d:ident, $mi:expr, $ki:expr, $s:expr) => {{
                let h = v128_xor(v128_xor($b, $c), $d);
                let k = u32x4_splat(K[$ki]);
                let temp = i32x4_add(i32x4_add($a, h), i32x4_add(k, m[$mi]));
                $a = i32x4_add($b, rotl(temp, $s));
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

        // Round 4: I = C ^ (B | ~D)
        macro_rules! round4 {
            ($a:ident, $b:ident, $c:ident, $d:ident, $mi:expr, $ki:expr, $s:expr) => {{
                let i_val = v128_xor($c, v128_or($b, v128_not($d)));
                let k = u32x4_splat(K[$ki]);
                let temp = i32x4_add(i32x4_add($a, i_val), i32x4_add(k, m[$mi]));
                $a = i32x4_add($b, rotl(temp, $s));
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
        let new_a = i32x4_add(a, aa);
        let new_b = i32x4_add(b, bb);
        let new_c = i32x4_add(c, cc);
        let new_d = i32x4_add(d, dd);

        // Blend using mask (bitselect: mask ? new : old)
        a = v128_bitselect(new_a, aa, mask);
        b = v128_bitselect(new_b, bb, mask);
        c = v128_bitselect(new_c, cc, mask);
        d = v128_bitselect(new_d, dd, mask);
    }

    // Extract results
    let mut results = [[0u8; 16]; 4];

    let a_arr = [
        u32x4_extract_lane::<0>(a),
        u32x4_extract_lane::<1>(a),
        u32x4_extract_lane::<2>(a),
        u32x4_extract_lane::<3>(a),
    ];
    let b_arr = [
        u32x4_extract_lane::<0>(b),
        u32x4_extract_lane::<1>(b),
        u32x4_extract_lane::<2>(b),
        u32x4_extract_lane::<3>(b),
    ];
    let c_arr = [
        u32x4_extract_lane::<0>(c),
        u32x4_extract_lane::<1>(c),
        u32x4_extract_lane::<2>(c),
        u32x4_extract_lane::<3>(c),
    ];
    let d_arr = [
        u32x4_extract_lane::<0>(d),
        u32x4_extract_lane::<1>(d),
        u32x4_extract_lane::<2>(d),
        u32x4_extract_lane::<3>(d),
    ];

    for (lane, result) in results.iter_mut().enumerate() {
        result[0..4].copy_from_slice(&a_arr[lane].to_le_bytes());
        result[4..8].copy_from_slice(&b_arr[lane].to_le_bytes());
        result[8..12].copy_from_slice(&c_arr[lane].to_le_bytes());
        result[12..16].copy_from_slice(&d_arr[lane].to_le_bytes());
    }

    results
}

/// Fallback for WASM without SIMD.
#[cfg(all(target_arch = "wasm32", not(target_feature = "simd128")))]
pub fn digest_x4(inputs: &[&[u8]; 4]) -> [Digest; 4] {
    std::array::from_fn(|i| crate::scalar::digest(inputs[i]))
}
