//! AVX2 8-lane parallel MD5 implementation.
//!
//! Processes 8 independent MD5 computations simultaneously using 256-bit YMM registers.
//!
//! # CPU Feature Requirements
//!
//! - **AVX2**: Intel Haswell (2013+), AMD Excavator (2015+) or newer
//! - Must be verified at runtime using `is_x86_feature_detected!("avx2")`
//!
//! # SIMD Strategy
//!
//! Similar to SSE2 but with twice the parallelism using 256-bit YMM registers.
//! Each YMM register holds 8 lanes of 32-bit values, allowing 8 parallel MD5
//! computations to proceed in lockstep.
//!
//! AVX2 provides significant advantages over SSE2:
//! - Variable-shift instructions (`vpsllvd`/`vpsrlvd`) for efficient rotation
//! - Native blend instruction (`vpblendvb`) for cleaner lane masking
//! - 256-bit loads/stores for better memory bandwidth
//!
//! # Performance Characteristics
//!
//! - **Throughput**: ~8x scalar performance when all 8 lanes are active
//! - **Latency**: Similar to scalar for single input
//! - **Best use case**: Processing 8 or more inputs of similar lengths
//! - **Efficiency**: ~2x throughput vs SSE2 with comparable energy
//!
//! # Input Size Limits
//!
//! Falls back to scalar implementation for inputs exceeding 1 MB to avoid
//! excessive memory allocation for padding buffers.

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

use super::super::Digest;

/// MD5 initial state constants broadcast to 8 lanes.
const INIT_A: u32 = 0x6745_2301;
const INIT_B: u32 = 0xefcd_ab89;
const INIT_C: u32 = 0x98ba_dcfe;
const INIT_D: u32 = 0x1032_5476;

/// Per-round shift amounts.
const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// Pre-computed K constants.
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

/// Maximum input size supported (can be increased if needed).
const MAX_INPUT_SIZE: usize = 1_024 * 1_024; // 1MB per input

/// Rotate left helper - AVX2 doesn't have a rotate instruction.
///
/// Implements 32-bit rotate-left using AVX2's variable shift instructions.
/// Unlike SSE2, AVX2 supports variable shift amounts via `vpsllvd`/`vpsrlvd`,
/// allowing the shift amount to be a runtime value.
///
/// # Safety
///
/// Requires AVX2 support. This is enforced by the `#[target_feature]` attribute.
#[target_feature(enable = "avx2")]
unsafe fn rotl(x: __m256i, n: i32) -> __m256i {
    // Use variable shift for runtime values
    _mm256_or_si256(
        _mm256_sllv_epi32(x, _mm256_set1_epi32(n)),
        _mm256_srlv_epi32(x, _mm256_set1_epi32(32 - n)),
    )
}

/// Compute MD5 digests for up to 8 inputs in parallel using AVX2.
///
/// Processes 8 independent byte slices in parallel, computing their MD5 digests
/// simultaneously using 256-bit YMM registers.
///
/// # Arguments
///
/// * `inputs` - Array of 8 byte slices to hash
///
/// # Returns
///
/// Array of 8 MD5 digests (16 bytes each) in the same order as the inputs
///
/// # Performance
///
/// Best performance is achieved when:
/// - All 8 input slots are used
/// - Inputs have similar lengths (minimizes masked blocks)
/// - Input sizes are reasonable (< 1 MB)
/// - CPU supports AVX2 (Haswell/2013 or newer)
///
/// # Safety
///
/// Caller must ensure AVX2 is available. Use runtime detection before calling:
///
/// ```ignore
/// if is_x86_feature_detected!("avx2") {
///     let digests = unsafe { digest_x8(&inputs) };
/// }
/// ```
///
/// This function uses `unsafe` internally for:
/// - AVX2 intrinsics (`_mm256_*` functions)
/// - Aligned memory access via `_mm256_store_si256`
#[target_feature(enable = "avx2")]
pub unsafe fn digest_x8(inputs: &[&[u8]; 8]) -> [Digest; 8] {
    // Find the maximum length to determine block count
    let max_len = inputs.iter().map(|i| i.len()).max().unwrap_or(0);

    // For very large inputs, fall back to scalar to avoid huge allocations
    if max_len > MAX_INPUT_SIZE {
        return std::array::from_fn(|i| super::super::md5_scalar::digest(inputs[i]));
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

        // Create mask for active lanes (lanes that have data for this block)
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

        // 64 rounds - unrolled for performance
        macro_rules! round {
            ($i:expr, $f:expr, $g:expr) => {{
                let k_i = _mm256_set1_epi32(K[$i] as i32);
                let temp = _mm256_add_epi32(_mm256_add_epi32(a, $f), _mm256_add_epi32(k_i, m[$g]));

                // Rotate left by S[i]
                let rotated = rotl(temp, S[$i] as i32);

                a = d;
                d = c;
                c = b;
                b = _mm256_add_epi32(b, rotated);
            }};
        }

        // Rounds 0-15: F = (B & C) | (~B & D)
        for i in 0..16 {
            let f = _mm256_or_si256(_mm256_and_si256(b, c), _mm256_andnot_si256(b, d));
            round!(i, f, i);
        }

        // Rounds 16-31: G = (D & B) | (~D & C)
        for i in 16..32 {
            let f = _mm256_or_si256(_mm256_and_si256(d, b), _mm256_andnot_si256(d, c));
            let g = (5 * i + 1) % 16;
            round!(i, f, g);
        }

        // Rounds 32-47: H = B ^ C ^ D
        for i in 32..48 {
            let f = _mm256_xor_si256(_mm256_xor_si256(b, c), d);
            let g = (3 * i + 5) % 16;
            round!(i, f, g);
        }

        // Rounds 48-63: I = C ^ (B | ~D)
        for i in 48..64 {
            let not_d = _mm256_xor_si256(d, _mm256_set1_epi32(-1));
            let f = _mm256_xor_si256(c, _mm256_or_si256(b, not_d));
            let g = (7 * i) % 16;
            round!(i, f, g);
        }

        // Compute new state
        let new_a = _mm256_add_epi32(a, aa);
        let new_b = _mm256_add_epi32(b, bb);
        let new_c = _mm256_add_epi32(c, cc);
        let new_d = _mm256_add_epi32(d, dd);

        // Blend: use new state for active lanes, preserve old state for inactive lanes
        // blendv selects from second operand where mask bit is set
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
    fn avx2_matches_scalar() {
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
    fn avx2_rfc1321_vectors() {
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
            "d41d8cd98f00b204e9800998ecf8427e",
            "0cc175b9c0f1b6a831c399e269772661",
            "900150983cd24fb0d6963f7d28e17f72",
            "f96b697d7cb7938d525a2f31aaf161d0",
            "c3fcd3d76192e4007dfb496cca67e13b",
            "d174ab98d277d9f5a5611c2c9f419d9f",
            "57edf4a22be3c955ac49da2e2107b67a",
            "d41d8cd98f00b204e9800998ecf8427e",
        ];

        let results = unsafe { digest_x8(&inputs) };

        for i in 0..8 {
            assert_eq!(
                to_hex(&results[i]),
                expected[i],
                "RFC 1321 vector mismatch at lane {i}"
            );
        }
    }

    #[test]
    fn avx2_various_lengths() {
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
