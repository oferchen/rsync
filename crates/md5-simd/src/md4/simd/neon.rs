//! ARM NEON 4-lane parallel MD4 implementation.
//!
//! Processes 4 independent MD4 computations simultaneously using 128-bit NEON registers.

#[cfg(target_arch = "aarch64")]
use std::arch::aarch64::*;

use crate::Digest;

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

// Shift amounts for each round (used in macros as compile-time constants):
// S1: [3, 7, 11, 19]
// S2: [3, 5, 9, 13]
// S3: [3, 9, 11, 15]

/// Message word indices for round 2.
const M2: [usize; 16] = [0, 4, 8, 12, 1, 5, 9, 13, 2, 6, 10, 14, 3, 7, 11, 15];
/// Message word indices for round 3.
const M3: [usize; 16] = [0, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5, 13, 3, 11, 7, 15];

/// Maximum input size supported.
const MAX_INPUT_SIZE: usize = 1_024 * 1_024; // 1MB per input

/// Macro for compile-time rotate left.
macro_rules! rotl_const {
    ($x:expr, $n:expr) => {{
        let left = vshlq_n_u32::<$n>($x);
        let right = vshrq_n_u32::<{ 32 - $n }>($x);
        vorrq_u32(left, right)
    }};
}

/// Compute MD4 digests for up to 4 inputs in parallel using NEON.
///
/// # Safety
/// Caller must ensure NEON is available (mandatory on aarch64).
#[cfg(target_arch = "aarch64")]
pub unsafe fn digest_x4(inputs: &[&[u8]; 4]) -> [Digest; 4] {
    // Find the maximum length to determine block count
    let max_len = inputs.iter().map(|i| i.len()).max().unwrap_or(0);

    // For very large inputs, fall back to scalar to avoid huge allocations
    if max_len > MAX_INPUT_SIZE {
        return std::array::from_fn(|i| crate::md4::scalar::digest(inputs[i]));
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
    let block_counts: [usize; 4] = std::array::from_fn(|i| padded_storage[i].len() / 64);
    let max_blocks = block_counts.iter().max().copied().unwrap_or(0);

    // Initialize state (4 lanes)
    let mut a = vdupq_n_u32(INIT_A);
    let mut b = vdupq_n_u32(INIT_B);
    let mut c = vdupq_n_u32(INIT_C);
    let mut d = vdupq_n_u32(INIT_D);

    // Process blocks
    for block_idx in 0..max_blocks {
        let block_offset = block_idx * 64;

        // Create mask for active lanes
        let lane_active: [u32; 4] = std::array::from_fn(|lane| {
            if block_idx < block_counts[lane] { 0xFFFF_FFFF } else { 0 }
        });
        let mask = vld1q_u32(lane_active.as_ptr());

        // Load message words (transposed: word i from all 4 inputs)
        let mut m = [vdupq_n_u32(0); 16];
        for word_idx in 0..16 {
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
            m[word_idx] = vld1q_u32(words.as_ptr());
        }

        // Save state for this block
        let aa = a;
        let bb = b;
        let cc = c;
        let dd = d;

        // Round 1: F = (B & C) | (~B & D)
        let k1 = vdupq_n_u32(K[0]);
        macro_rules! round1_step {
            ($i:expr, $shift:expr) => {{
                // F = (B & C) | (~B & D)
                let f_val = vorrq_u32(
                    vandq_u32(b, c),
                    vbicq_u32(d, b), // ~B & D = D & ~B = vbic(D, B)
                );
                let temp = vaddq_u32(vaddq_u32(a, f_val), vaddq_u32(k1, m[$i]));
                let rotated = rotl_const!(temp, $shift);
                a = d;
                d = c;
                c = b;
                b = rotated;
            }};
        }

        round1_step!(0, 3);
        round1_step!(1, 7);
        round1_step!(2, 11);
        round1_step!(3, 19);
        round1_step!(4, 3);
        round1_step!(5, 7);
        round1_step!(6, 11);
        round1_step!(7, 19);
        round1_step!(8, 3);
        round1_step!(9, 7);
        round1_step!(10, 11);
        round1_step!(11, 19);
        round1_step!(12, 3);
        round1_step!(13, 7);
        round1_step!(14, 11);
        round1_step!(15, 19);

        // Round 2: G = (B & C) | (B & D) | (C & D) = majority
        let k2 = vdupq_n_u32(K[1]);
        macro_rules! round2_step {
            ($mi:expr, $shift:expr) => {{
                // G = (B & C) | (B & D) | (C & D)
                // Equivalent to: (B & C) | (D & (B | C))
                let g_val = vorrq_u32(
                    vandq_u32(b, c),
                    vandq_u32(d, vorrq_u32(b, c)),
                );
                let temp = vaddq_u32(vaddq_u32(a, g_val), vaddq_u32(k2, m[M2[$mi]]));
                let rotated = rotl_const!(temp, $shift);
                a = d;
                d = c;
                c = b;
                b = rotated;
            }};
        }

        round2_step!(0, 3);
        round2_step!(1, 5);
        round2_step!(2, 9);
        round2_step!(3, 13);
        round2_step!(4, 3);
        round2_step!(5, 5);
        round2_step!(6, 9);
        round2_step!(7, 13);
        round2_step!(8, 3);
        round2_step!(9, 5);
        round2_step!(10, 9);
        round2_step!(11, 13);
        round2_step!(12, 3);
        round2_step!(13, 5);
        round2_step!(14, 9);
        round2_step!(15, 13);

        // Round 3: H = B ^ C ^ D
        let k3 = vdupq_n_u32(K[2]);
        macro_rules! round3_step {
            ($mi:expr, $shift:expr) => {{
                // H = B ^ C ^ D
                let h_val = veorq_u32(veorq_u32(b, c), d);
                let temp = vaddq_u32(vaddq_u32(a, h_val), vaddq_u32(k3, m[M3[$mi]]));
                let rotated = rotl_const!(temp, $shift);
                a = d;
                d = c;
                c = b;
                b = rotated;
            }};
        }

        round3_step!(0, 3);
        round3_step!(1, 9);
        round3_step!(2, 11);
        round3_step!(3, 15);
        round3_step!(4, 3);
        round3_step!(5, 9);
        round3_step!(6, 11);
        round3_step!(7, 15);
        round3_step!(8, 3);
        round3_step!(9, 9);
        round3_step!(10, 11);
        round3_step!(11, 15);
        round3_step!(12, 3);
        round3_step!(13, 9);
        round3_step!(14, 11);
        round3_step!(15, 15);

        // Compute new state
        let new_a = vaddq_u32(a, aa);
        let new_b = vaddq_u32(b, bb);
        let new_c = vaddq_u32(c, cc);
        let new_d = vaddq_u32(d, dd);

        // Blend: use new state for active lanes, preserve old state for inactive lanes
        a = vbslq_u32(mask, new_a, aa);
        b = vbslq_u32(mask, new_b, bb);
        c = vbslq_u32(mask, new_c, cc);
        d = vbslq_u32(mask, new_d, dd);
    }

    // Extract results
    let mut results = [[0u8; 16]; 4];

    #[repr(C, align(16))]
    struct Aligned([u32; 4]);

    let mut a_out = Aligned([0; 4]);
    let mut b_out = Aligned([0; 4]);
    let mut c_out = Aligned([0; 4]);
    let mut d_out = Aligned([0; 4]);

    vst1q_u32(a_out.0.as_mut_ptr(), a);
    vst1q_u32(b_out.0.as_mut_ptr(), b);
    vst1q_u32(c_out.0.as_mut_ptr(), c);
    vst1q_u32(d_out.0.as_mut_ptr(), d);

    for (lane, result) in results.iter_mut().enumerate() {
        result[0..4].copy_from_slice(&a_out.0[lane].to_le_bytes());
        result[4..8].copy_from_slice(&b_out.0[lane].to_le_bytes());
        result[8..12].copy_from_slice(&c_out.0[lane].to_le_bytes());
        result[12..16].copy_from_slice(&d_out.0[lane].to_le_bytes());
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
    fn neon_md4_matches_scalar() {
        let inputs: [&[u8]; 4] = [
            b"",
            b"a",
            b"abc",
            b"message digest",
        ];

        let results = unsafe { digest_x4(&inputs) };

        for (i, input) in inputs.iter().enumerate() {
            let expected = crate::md4::scalar::digest(input);
            assert_eq!(
                to_hex(&results[i]),
                to_hex(&expected),
                "Mismatch at lane {i} for input {:?}",
                String::from_utf8_lossy(input)
            );
        }
    }

    #[test]
    fn neon_md4_rfc1320_vectors() {
        let inputs: [&[u8]; 4] = [
            b"",
            b"a",
            b"abc",
            b"message digest",
        ];

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
    fn neon_md4_various_lengths() {
        // Test with varying lengths including block boundaries
        let input0: Vec<u8> = (0..55).map(|i| (i % 256) as u8).collect();
        let input1: Vec<u8> = (0..56).map(|i| (i % 256) as u8).collect();
        let input2: Vec<u8> = (0..64).map(|i| (i % 256) as u8).collect();
        let input3: Vec<u8> = (0..65).map(|i| (i % 256) as u8).collect();

        let inputs: [&[u8]; 4] = [&input0, &input1, &input2, &input3];

        let results = unsafe { digest_x4(&inputs) };

        for (i, input) in inputs.iter().enumerate() {
            let expected = crate::md4::scalar::digest(input);
            assert_eq!(
                to_hex(&results[i]),
                to_hex(&expected),
                "Mismatch at lane {i} for input length {}",
                input.len()
            );
        }
    }
}
