//! ARM NEON 4-lane parallel MD5 implementation.
//!
//! Processes 4 independent MD5 computations simultaneously using 128-bit NEON
//! registers. NEON is mandatory on aarch64 (ARMv8-A baseline), so no runtime
//! feature detection is required. The kernel uses `vbic` (bit clear) for
//! AND-NOT, `vorn` for the I round 4 function, and `vbsl` for lane masking.
//! Rotation uses paired `vshlq_n_u32` / `vshrq_n_u32` with OR (compile-time
//! shift constants), since NEON has no native rotate.

#[cfg(target_arch = "aarch64")]
use std::arch::aarch64::*;

use super::super::Digest;

/// MD5 initial state constants.
const INIT_A: u32 = 0x6745_2301;
const INIT_B: u32 = 0xefcd_ab89;
const INIT_C: u32 = 0x98ba_dcfe;
const INIT_D: u32 = 0x1032_5476;

/// MD5 round constants (T[i] = floor(2^32 * abs(sin(i+1)))).
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

/// Macro for compile-time rotate left on NEON.
///
/// NEON lacks a rotate instruction, so rotation is implemented using
/// compile-time constant shifts combined with OR. The shift amounts
/// must be known at compile time, specified as const generic parameters.
macro_rules! rotl_const {
    ($x:expr, $n:expr) => {{
        let left = vshlq_n_u32::<$n>($x);
        let right = vshrq_n_u32::<{ 32 - $n }>($x);
        vorrq_u32(left, right)
    }};
}

/// Compute MD5 digests for 4 inputs in parallel using ARM NEON.
///
/// Returns digests in the same order as `inputs`. Lanes with shorter inputs
/// are masked off via `vbslq_u32` after their final block. Inputs larger
/// than 1 MiB fall back to the scalar path to cap padding allocations.
///
/// # Safety
///
/// Caller must ensure NEON is available. NEON is mandatory on aarch64
/// (ARMv8-A baseline), so this is always satisfied on 64-bit ARM.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
#[allow(unsafe_op_in_unsafe_fn)]
pub unsafe fn digest_x4(inputs: &[&[u8]; 4]) -> [Digest; 4] {
    let max_len = inputs.iter().map(|i| i.len()).max().unwrap_or(0);

    if max_len > MAX_INPUT_SIZE {
        return std::array::from_fn(|i| super::super::md5_scalar::digest(inputs[i]));
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

    let mut a = vdupq_n_u32(INIT_A);
    let mut b = vdupq_n_u32(INIT_B);
    let mut c = vdupq_n_u32(INIT_C);
    let mut d = vdupq_n_u32(INIT_D);

    for block_idx in 0..max_blocks {
        let block_offset = block_idx * 64;

        let lane_active: [u32; 4] = std::array::from_fn(|lane| {
            if block_idx < block_counts[lane] {
                0xFFFF_FFFF
            } else {
                0
            }
        });
        let mask = vld1q_u32(lane_active.as_ptr());

        // Transpose: word `word_idx` from all 4 lanes packed into one vector.
        let mut m = [vdupq_n_u32(0); 16];
        for (word_idx, m_word) in m.iter_mut().enumerate() {
            let word_offset = block_offset + word_idx * 4;
            let words: [u32; 4] = std::array::from_fn(|lane| {
                if word_offset + 4 <= padded_storage[lane].len() {
                    u32::from_le_bytes(
                        padded_storage[lane][word_offset..word_offset + 4]
                            .try_into()
                            .expect("4-byte word slice"),
                    )
                } else {
                    0
                }
            });
            *m_word = vld1q_u32(words.as_ptr());
        }

        let aa = a;
        let bb = b;
        let cc = c;
        let dd = d;

        // Round 1: F = (B & C) | (~B & D)
        macro_rules! round1 {
            ($a:ident, $b:ident, $c:ident, $d:ident, $mi:expr, $ki:expr, $s:expr) => {{
                let f = vorrq_u32(vandq_u32($b, $c), vbicq_u32($d, $b));
                let k = vdupq_n_u32(K[$ki]);
                let temp = vaddq_u32(vaddq_u32($a, f), vaddq_u32(k, m[$mi]));
                $a = vaddq_u32($b, rotl_const!(temp, $s));
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
                let g = vorrq_u32(vandq_u32($b, $d), vbicq_u32($c, $d));
                let k = vdupq_n_u32(K[$ki]);
                let temp = vaddq_u32(vaddq_u32($a, g), vaddq_u32(k, m[$mi]));
                $a = vaddq_u32($b, rotl_const!(temp, $s));
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
                let h = veorq_u32(veorq_u32($b, $c), $d);
                let k = vdupq_n_u32(K[$ki]);
                let temp = vaddq_u32(vaddq_u32($a, h), vaddq_u32(k, m[$mi]));
                $a = vaddq_u32($b, rotl_const!(temp, $s));
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
                let i_val = veorq_u32($c, vornq_u32($b, $d));
                let k = vdupq_n_u32(K[$ki]);
                let temp = vaddq_u32(vaddq_u32($a, i_val), vaddq_u32(k, m[$mi]));
                $a = vaddq_u32($b, rotl_const!(temp, $s));
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

        let new_a = vaddq_u32(a, aa);
        let new_b = vaddq_u32(b, bb);
        let new_c = vaddq_u32(c, cc);
        let new_d = vaddq_u32(d, dd);

        a = vbslq_u32(mask, new_a, aa);
        b = vbslq_u32(mask, new_b, bb);
        c = vbslq_u32(mask, new_c, cc);
        d = vbslq_u32(mask, new_d, dd);
    }

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
    fn neon_md5_matches_scalar() {
        let inputs: [&[u8]; 4] = [b"", b"a", b"abc", b"message digest"];

        // SAFETY: NEON is mandatory on aarch64 (ARMv8-A baseline), so
        // `digest_x4`'s "NEON available" precondition holds whenever this
        // module is compiled in (gated by `#[cfg(target_arch = "aarch64")]`).
        // `inputs` is a fixed-length array of 4 valid `&[u8]` borrows.
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
    fn neon_md5_rfc1321_vectors() {
        let inputs: [&[u8]; 4] = [b"", b"a", b"abc", b"message digest"];

        let expected = [
            "d41d8cd98f00b204e9800998ecf8427e",
            "0cc175b9c0f1b6a831c399e269772661",
            "900150983cd24fb0d6963f7d28e17f72",
            "f96b697d7cb7938d525a2f31aaf161d0",
        ];

        // SAFETY: NEON is mandatory on aarch64 (ARMv8-A baseline), so
        // `digest_x4`'s "NEON available" precondition holds whenever this
        // module is compiled in. `inputs` is a fixed-length array of 4
        // valid `&[u8]` borrows that outlive the call.
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
    fn neon_md5_various_lengths() {
        let input0: Vec<u8> = (0..55).map(|i| (i % 256) as u8).collect();
        let input1: Vec<u8> = (0..56).map(|i| (i % 256) as u8).collect();
        let input2: Vec<u8> = (0..64).map(|i| (i % 256) as u8).collect();
        let input3: Vec<u8> = (0..65).map(|i| (i % 256) as u8).collect();

        let inputs: [&[u8]; 4] = [&input0, &input1, &input2, &input3];

        // SAFETY: NEON is mandatory on aarch64 (ARMv8-A baseline), so
        // `digest_x4`'s "NEON available" precondition holds whenever this
        // module is compiled in. `inputs` borrows 4 owned `Vec<u8>`
        // buffers that outlive the call; `digest_x4` bounds-checks each
        // lane.
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
