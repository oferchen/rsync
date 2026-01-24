//! WebAssembly SIMD 4-lane parallel MD4 implementation.
//!
//! Processes 4 independent MD4 computations simultaneously using 128-bit WASM SIMD.

#[cfg(target_arch = "wasm32")]
use std::arch::wasm32::*;

use crate::Digest;

/// MD4 initial state constants.
const INIT_A: u32 = 0x67452301;
const INIT_B: u32 = 0xefcdab89;
const INIT_C: u32 = 0x98badcfe;
const INIT_D: u32 = 0x10325476;

/// Round constants for MD4.
const K: [u32; 3] = [
    0x00000000, // Round 1
    0x5A827999, // Round 2
    0x6ED9EBA1, // Round 3
];

/// Message word indices for round 2.
const M2: [usize; 16] = [0, 4, 8, 12, 1, 5, 9, 13, 2, 6, 10, 14, 3, 7, 11, 15];
/// Message word indices for round 3.
const M3: [usize; 16] = [0, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5, 13, 3, 11, 7, 15];

const MAX_INPUT_SIZE: usize = 1024 * 1024;

/// Rotate left for WASM SIMD.
#[cfg(target_arch = "wasm32")]
#[inline(always)]
fn rotl(x: v128, n: u32) -> v128 {
    v128_or(i32x4_shl(x, n), u32x4_shr(x, 32 - n))
}

/// Compute MD4 digests for up to 4 inputs in parallel using WASM SIMD.
#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
pub fn digest_x4(inputs: &[&[u8]; 4]) -> [Digest; 4] {
    let max_len = inputs.iter().map(|i| i.len()).max().unwrap_or(0);

    if max_len > MAX_INPUT_SIZE {
        return std::array::from_fn(|i| crate::md4::scalar::digest(inputs[i]));
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

    let mut a = u32x4_splat(INIT_A);
    let mut b = u32x4_splat(INIT_B);
    let mut c = u32x4_splat(INIT_C);
    let mut d = u32x4_splat(INIT_D);

    for block_idx in 0..max_blocks {
        let block_offset = block_idx * 64;

        let lane_active: [u32; 4] = std::array::from_fn(|lane| {
            if block_idx < block_counts[lane] { 0xFFFFFFFF } else { 0 }
        });
        let mask = u32x4(lane_active[0], lane_active[1], lane_active[2], lane_active[3]);

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
        let k1 = u32x4_splat(K[0]);
        macro_rules! round1 {
            ($i:expr, $s:expr) => {{
                let f = v128_or(v128_and(b, c), v128_andnot(d, b));
                let temp = i32x4_add(i32x4_add(a, f), i32x4_add(k1, m[$i]));
                let rotated = rotl(temp, $s);
                a = d; d = c; c = b; b = rotated;
            }};
        }

        round1!(0, 3);  round1!(1, 7);  round1!(2, 11);  round1!(3, 19);
        round1!(4, 3);  round1!(5, 7);  round1!(6, 11);  round1!(7, 19);
        round1!(8, 3);  round1!(9, 7);  round1!(10, 11); round1!(11, 19);
        round1!(12, 3); round1!(13, 7); round1!(14, 11); round1!(15, 19);

        // Round 2: G = (B & C) | (B & D) | (C & D)
        let k2 = u32x4_splat(K[1]);
        macro_rules! round2 {
            ($mi:expr, $s:expr) => {{
                let g = v128_or(v128_and(b, c), v128_and(d, v128_or(b, c)));
                let temp = i32x4_add(i32x4_add(a, g), i32x4_add(k2, m[M2[$mi]]));
                let rotated = rotl(temp, $s);
                a = d; d = c; c = b; b = rotated;
            }};
        }

        round2!(0, 3);  round2!(1, 5);  round2!(2, 9);   round2!(3, 13);
        round2!(4, 3);  round2!(5, 5);  round2!(6, 9);   round2!(7, 13);
        round2!(8, 3);  round2!(9, 5);  round2!(10, 9);  round2!(11, 13);
        round2!(12, 3); round2!(13, 5); round2!(14, 9);  round2!(15, 13);

        // Round 3: H = B ^ C ^ D
        let k3 = u32x4_splat(K[2]);
        macro_rules! round3 {
            ($mi:expr, $s:expr) => {{
                let h = v128_xor(v128_xor(b, c), d);
                let temp = i32x4_add(i32x4_add(a, h), i32x4_add(k3, m[M3[$mi]]));
                let rotated = rotl(temp, $s);
                a = d; d = c; c = b; b = rotated;
            }};
        }

        round3!(0, 3);  round3!(1, 9);  round3!(2, 11);  round3!(3, 15);
        round3!(4, 3);  round3!(5, 9);  round3!(6, 11);  round3!(7, 15);
        round3!(8, 3);  round3!(9, 9);  round3!(10, 11); round3!(11, 15);
        round3!(12, 3); round3!(13, 9); round3!(14, 11); round3!(15, 15);

        let new_a = i32x4_add(a, aa);
        let new_b = i32x4_add(b, bb);
        let new_c = i32x4_add(c, cc);
        let new_d = i32x4_add(d, dd);

        a = v128_bitselect(new_a, aa, mask);
        b = v128_bitselect(new_b, bb, mask);
        c = v128_bitselect(new_c, cc, mask);
        d = v128_bitselect(new_d, dd, mask);
    }

    let mut results = [[0u8; 16]; 4];

    let a_arr = [
        u32x4_extract_lane::<0>(a), u32x4_extract_lane::<1>(a),
        u32x4_extract_lane::<2>(a), u32x4_extract_lane::<3>(a),
    ];
    let b_arr = [
        u32x4_extract_lane::<0>(b), u32x4_extract_lane::<1>(b),
        u32x4_extract_lane::<2>(b), u32x4_extract_lane::<3>(b),
    ];
    let c_arr = [
        u32x4_extract_lane::<0>(c), u32x4_extract_lane::<1>(c),
        u32x4_extract_lane::<2>(c), u32x4_extract_lane::<3>(c),
    ];
    let d_arr = [
        u32x4_extract_lane::<0>(d), u32x4_extract_lane::<1>(d),
        u32x4_extract_lane::<2>(d), u32x4_extract_lane::<3>(d),
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
    std::array::from_fn(|i| crate::md4::scalar::digest(inputs[i]))
}
