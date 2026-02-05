//! WebAssembly SIMD 4-lane parallel MD5 implementation.
//!
//! Processes 4 independent MD5 computations simultaneously using 128-bit WASM SIMD.
//!
//! # CPU Feature Requirements
//!
//! - **WASM SIMD (simd128)**: WebAssembly SIMD proposal
//! - Widely supported in modern browsers and runtimes
//! - Feature must be enabled at compile time with `target_feature = "simd128"`
//!
//! # Platform Support
//!
//! WASM SIMD is supported in:
//! - Chrome/Edge 91+ (May 2021)
//! - Firefox 89+ (June 2021)
//! - Safari 16.4+ (March 2023)
//! - Node.js 16.4+ with V8 9.1+
//! - Wasmtime, Wasmer (modern versions)
//!
//! # SIMD Strategy
//!
//! WASM SIMD provides 128-bit vectors similar to SSE2, but with a portable
//! instruction set that works across all architectures (x86, ARM, RISC-V).
//!
//! The implementation uses:
//! - `v128` type for 128-bit SIMD values
//! - `i32x4` operations for 32-bit integer lanes
//! - `v128_bitselect` for efficient lane masking (similar to SSE4.1's blendv)
//! - Manual rotation using shifts and OR (no native rotate instruction)
//!
//! # Performance Characteristics
//!
//! - **Throughput**: ~4x scalar performance (similar to SSE2/NEON)
//! - **Portability**: Same code runs on x86, ARM, and other architectures
//! - **Best use case**: Web applications, serverless functions, portable libraries
//!
//! # Differences from Native SIMD
//!
//! Unlike native x86/ARM SIMD, WASM SIMD:
//! - Has no alignment requirements (but alignment can improve performance)
//! - Always uses little-endian regardless of host
//! - Provides `v128_bitselect` which is cleaner than SSE2 masking
//! - Lacks specialized instructions like `pshufb` or hardware rotate

#[cfg(target_arch = "wasm32")]
use std::arch::wasm32::*;

use super::super::Digest;

/// MD5 initial state constants.
const INIT_A: u32 = 0x6745_2301;
const INIT_B: u32 = 0xefcd_ab89;
const INIT_C: u32 = 0x98ba_dcfe;
const INIT_D: u32 = 0x1032_5476;

/// MD5 round constants.
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

/// Rotate left for WASM SIMD (requires runtime shift amount).
///
/// WASM SIMD lacks a native rotate instruction, so rotation is implemented
/// using shifts and OR. Unlike SSE2, WASM SIMD supports runtime shift amounts
/// without requiring compile-time constants.
///
/// This is a safe function (not `unsafe`) because WASM SIMD operations are
/// inherently safe - they cannot cause undefined behavior.
#[cfg(target_arch = "wasm32")]
#[inline(always)]
fn rotl(x: v128, n: u32) -> v128 {
    v128_or(i32x4_shl(x, n), u32x4_shr(x, 32 - n))
}

/// Compute MD5 digests for up to 4 inputs in parallel using WASM SIMD.
///
/// Processes 4 independent byte slices in parallel, computing their MD5 digests
/// simultaneously using WebAssembly SIMD instructions.
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
/// WASM SIMD performance varies by runtime:
/// - Browser engines: Near-native performance on modern V8/SpiderMonkey/JavaScriptCore
/// - Standalone runtimes: Performance depends on JIT quality and host architecture
/// - Generally achieves 3-4x speedup over scalar WASM code
///
/// # Platform Requirements
///
/// This function is only available when:
/// - Compiling for `wasm32` target
/// - The `simd128` target feature is enabled
///
/// Unlike native SIMD implementations, this is a **safe** function (not `unsafe`)
/// because WASM SIMD operations cannot cause undefined behavior - they're
/// sandboxed and validated by the WASM runtime.
///
/// # Availability
///
/// The function has a fallback for WASM without SIMD support that processes
/// inputs sequentially using the scalar implementation.
#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
pub fn digest_x4(inputs: &[&[u8]; 4]) -> [Digest; 4] {
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

    // Initialize state
    let mut a = u32x4_splat(INIT_A);
    let mut b = u32x4_splat(INIT_B);
    let mut c = u32x4_splat(INIT_C);
    let mut d = u32x4_splat(INIT_D);

    for block_idx in 0..max_blocks {
        let block_offset = block_idx * 64;

        // Create mask for active lanes
        let lane_active: [u32; 4] = std::array::from_fn(|lane| {
            if block_idx < block_counts[lane] {
                0xFFFF_FFFF
            } else {
                0
            }
        });
        let mask = u32x4(
            lane_active[0],
            lane_active[1],
            lane_active[2],
            lane_active[3],
        );

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

        round1!(a, b, c, d, 0, 0, 7);
        round1!(d, a, b, c, 1, 1, 12);
        round1!(c, d, a, b, 2, 2, 17);
        round1!(b, c, d, a, 3, 3, 22);
        round1!(a, b, c, d, 4, 4, 7);
        round1!(d, a, b, c, 5, 5, 12);
        round1!(c, d, a, b, 6, 6, 17);
        round1!(b, c, d, a, 7, 7, 22);
        round1!(a, b, c, d, 8, 8, 7);
        round1!(d, a, b, c, 9, 9, 12);
        round1!(c, d, a, b, 10, 10, 17);
        round1!(b, c, d, a, 11, 11, 22);
        round1!(a, b, c, d, 12, 12, 7);
        round1!(d, a, b, c, 13, 13, 12);
        round1!(c, d, a, b, 14, 14, 17);
        round1!(b, c, d, a, 15, 15, 22);

        // Round 2: G = (B & D) | (C & ~D)
        macro_rules! round2 {
            ($a:ident, $b:ident, $c:ident, $d:ident, $mi:expr, $ki:expr, $s:expr) => {{
                let g = v128_or(v128_and($b, $d), v128_andnot($c, $d));
                let k = u32x4_splat(K[$ki]);
                let temp = i32x4_add(i32x4_add($a, g), i32x4_add(k, m[$mi]));
                $a = i32x4_add($b, rotl(temp, $s));
            }};
        }

        round2!(a, b, c, d, 1, 16, 5);
        round2!(d, a, b, c, 6, 17, 9);
        round2!(c, d, a, b, 11, 18, 14);
        round2!(b, c, d, a, 0, 19, 20);
        round2!(a, b, c, d, 5, 20, 5);
        round2!(d, a, b, c, 10, 21, 9);
        round2!(c, d, a, b, 15, 22, 14);
        round2!(b, c, d, a, 4, 23, 20);
        round2!(a, b, c, d, 9, 24, 5);
        round2!(d, a, b, c, 14, 25, 9);
        round2!(c, d, a, b, 3, 26, 14);
        round2!(b, c, d, a, 8, 27, 20);
        round2!(a, b, c, d, 13, 28, 5);
        round2!(d, a, b, c, 2, 29, 9);
        round2!(c, d, a, b, 7, 30, 14);
        round2!(b, c, d, a, 12, 31, 20);

        // Round 3: H = B ^ C ^ D
        macro_rules! round3 {
            ($a:ident, $b:ident, $c:ident, $d:ident, $mi:expr, $ki:expr, $s:expr) => {{
                let h = v128_xor(v128_xor($b, $c), $d);
                let k = u32x4_splat(K[$ki]);
                let temp = i32x4_add(i32x4_add($a, h), i32x4_add(k, m[$mi]));
                $a = i32x4_add($b, rotl(temp, $s));
            }};
        }

        round3!(a, b, c, d, 5, 32, 4);
        round3!(d, a, b, c, 8, 33, 11);
        round3!(c, d, a, b, 11, 34, 16);
        round3!(b, c, d, a, 14, 35, 23);
        round3!(a, b, c, d, 1, 36, 4);
        round3!(d, a, b, c, 4, 37, 11);
        round3!(c, d, a, b, 7, 38, 16);
        round3!(b, c, d, a, 10, 39, 23);
        round3!(a, b, c, d, 13, 40, 4);
        round3!(d, a, b, c, 0, 41, 11);
        round3!(c, d, a, b, 3, 42, 16);
        round3!(b, c, d, a, 6, 43, 23);
        round3!(a, b, c, d, 9, 44, 4);
        round3!(d, a, b, c, 12, 45, 11);
        round3!(c, d, a, b, 15, 46, 16);
        round3!(b, c, d, a, 2, 47, 23);

        // Round 4: I = C ^ (B | ~D)
        macro_rules! round4 {
            ($a:ident, $b:ident, $c:ident, $d:ident, $mi:expr, $ki:expr, $s:expr) => {{
                let i_val = v128_xor($c, v128_or($b, v128_not($d)));
                let k = u32x4_splat(K[$ki]);
                let temp = i32x4_add(i32x4_add($a, i_val), i32x4_add(k, m[$mi]));
                $a = i32x4_add($b, rotl(temp, $s));
            }};
        }

        round4!(a, b, c, d, 0, 48, 6);
        round4!(d, a, b, c, 7, 49, 10);
        round4!(c, d, a, b, 14, 50, 15);
        round4!(b, c, d, a, 5, 51, 21);
        round4!(a, b, c, d, 12, 52, 6);
        round4!(d, a, b, c, 3, 53, 10);
        round4!(c, d, a, b, 10, 54, 15);
        round4!(b, c, d, a, 1, 55, 21);
        round4!(a, b, c, d, 8, 56, 6);
        round4!(d, a, b, c, 15, 57, 10);
        round4!(c, d, a, b, 6, 58, 15);
        round4!(b, c, d, a, 13, 59, 21);
        round4!(a, b, c, d, 4, 60, 6);
        round4!(d, a, b, c, 11, 61, 10);
        round4!(c, d, a, b, 2, 62, 15);
        round4!(b, c, d, a, 9, 63, 21);

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
    std::array::from_fn(|i| super::super::md5_scalar::digest(inputs[i]))
}
