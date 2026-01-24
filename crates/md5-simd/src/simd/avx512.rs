//! AVX-512 16-lane parallel MD5 implementation.
//!
//! Processes 16 independent MD5 computations simultaneously using 512-bit ZMM registers.
//! Uses inline assembly to work on stable Rust (AVX-512 intrinsics require nightly).
//!
//! # Safety
//!
//! All AVX-512 operations are performed via inline assembly, which is stable in Rust.
//! The `digest_x16` function requires AVX-512F and AVX-512BW to be available, which
//! is verified at runtime by the dispatcher before calling.

#![allow(unsafe_code)]

#[cfg(target_arch = "x86_64")]
use std::arch::asm;

use crate::Digest;

/// MD5 initial state constants.
const INIT_A: u32 = 0x67452301;
const INIT_B: u32 = 0xefcdab89;
const INIT_C: u32 = 0x98badcfe;
const INIT_D: u32 = 0x10325476;


/// Pre-computed K constants.
const K: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee,
    0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
    0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be,
    0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
    0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa,
    0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
    0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
    0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
    0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c,
    0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
    0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05,
    0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
    0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039,
    0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
    0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1,
    0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
];

/// Maximum input size supported.
const MAX_INPUT_SIZE: usize = 1024 * 1024; // 1MB per input

/// Aligned storage for 512-bit (16 × 32-bit) values.
#[repr(C, align(64))]
struct Aligned512([u32; 16]);

/// Compute MD5 digests for up to 16 inputs in parallel using AVX-512.
///
/// # Safety
///
/// Caller must ensure AVX-512F and AVX-512BW are available.
/// This is verified at runtime by the dispatcher.
#[cfg(target_arch = "x86_64")]
pub unsafe fn digest_x16(inputs: &[&[u8]; 16]) -> [Digest; 16] {
    // Find the maximum length to determine block count
    let max_len = inputs.iter().map(|i| i.len()).max().unwrap_or(0);

    // For very large inputs, fall back to scalar to avoid huge allocations
    if max_len > MAX_INPUT_SIZE {
        return std::array::from_fn(|i| crate::scalar::digest(inputs[i]));
    }

    // Prepare padded buffers for each input
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
    let block_counts: [usize; 16] = std::array::from_fn(|i| padded_storage[i].len() / 64);
    let max_blocks = block_counts.iter().max().copied().unwrap_or(0);

    // Initialize state (16 lanes) - stored transposed
    let mut state_a = Aligned512([INIT_A; 16]);
    let mut state_b = Aligned512([INIT_B; 16]);
    let mut state_c = Aligned512([INIT_C; 16]);
    let mut state_d = Aligned512([INIT_D; 16]);

    // Message words storage (16 words × 16 lanes, transposed)
    let mut m_storage: [Aligned512; 16] = std::array::from_fn(|_| Aligned512([0; 16]));

    // Process blocks
    for block_idx in 0..max_blocks {
        let block_offset = block_idx * 64;

        // Create mask for active lanes
        let mask_bits: u16 = block_counts
            .iter()
            .enumerate()
            .fold(0u16, |acc, (lane, &count)| {
                if block_idx < count { acc | (1 << lane) } else { acc }
            });

        // Load message words (transposed: word i from all 16 inputs)
        for (word_idx, m_word) in m_storage.iter_mut().enumerate() {
            let word_offset = block_offset + word_idx * 4;
            for (lane, padded) in padded_storage.iter().enumerate() {
                m_word.0[lane] = if word_offset + 4 <= padded.len() {
                    u32::from_le_bytes(
                        padded[word_offset..word_offset + 4].try_into().unwrap(),
                    )
                } else {
                    0
                };
            }
        }

        // Process block using AVX-512 assembly
        process_block_avx512(
            &mut state_a,
            &mut state_b,
            &mut state_c,
            &mut state_d,
            &m_storage,
            mask_bits,
        );
    }

    // Extract results
    std::array::from_fn(|lane| {
        let mut digest = [0u8; 16];
        digest[0..4].copy_from_slice(&state_a.0[lane].to_le_bytes());
        digest[4..8].copy_from_slice(&state_b.0[lane].to_le_bytes());
        digest[8..12].copy_from_slice(&state_c.0[lane].to_le_bytes());
        digest[12..16].copy_from_slice(&state_d.0[lane].to_le_bytes());
        digest
    })
}

/// Process a single MD5 block for 16 lanes using AVX-512 inline assembly.
#[cfg(target_arch = "x86_64")]
#[inline(never)]
unsafe fn process_block_avx512(
    state_a: &mut Aligned512,
    state_b: &mut Aligned512,
    state_c: &mut Aligned512,
    state_d: &mut Aligned512,
    m: &[Aligned512; 16],
    mask_bits: u16,
) {
    // Working registers:
    // zmm0 = a, zmm1 = b, zmm2 = c, zmm3 = d
    // zmm4 = aa (saved a), zmm5 = bb, zmm6 = cc, zmm7 = dd
    // zmm8-zmm15 = m[0..7] (first 8 message words)
    // zmm16-zmm23 = m[8..15] (second 8 message words)
    // zmm24 = temp/f, zmm25 = k constant, zmm26 = all-ones for NOT

    // Get base pointer for message array
    let m_ptr = m.as_ptr() as *const u32;

    // Load state and first 8 message words
    asm!(
        // Load current state
        "vmovdqu32 zmm0, [{a}]",
        "vmovdqu32 zmm1, [{b}]",
        "vmovdqu32 zmm2, [{c}]",
        "vmovdqu32 zmm3, [{d}]",
        // Save state for later addition
        "vmovdqa32 zmm4, zmm0",
        "vmovdqa32 zmm5, zmm1",
        "vmovdqa32 zmm6, zmm2",
        "vmovdqa32 zmm7, zmm3",
        // Load message words m[0..7] - each Aligned512 is 64 bytes
        "vmovdqu32 zmm8,  [{m}]",
        "vmovdqu32 zmm9,  [{m} + 64]",
        "vmovdqu32 zmm10, [{m} + 128]",
        "vmovdqu32 zmm11, [{m} + 192]",
        "vmovdqu32 zmm12, [{m} + 256]",
        "vmovdqu32 zmm13, [{m} + 320]",
        "vmovdqu32 zmm14, [{m} + 384]",
        "vmovdqu32 zmm15, [{m} + 448]",
        // Load message words m[8..15]
        "vmovdqu32 zmm16, [{m} + 512]",
        "vmovdqu32 zmm17, [{m} + 576]",
        "vmovdqu32 zmm18, [{m} + 640]",
        "vmovdqu32 zmm19, [{m} + 704]",
        "vmovdqu32 zmm20, [{m} + 768]",
        "vmovdqu32 zmm21, [{m} + 832]",
        "vmovdqu32 zmm22, [{m} + 896]",
        "vmovdqu32 zmm23, [{m} + 960]",
        // Create all-ones for NOT operations
        "vpternlogd zmm26, zmm26, zmm26, 0xff",
        a = in(reg) state_a.0.as_ptr(),
        b = in(reg) state_b.0.as_ptr(),
        c = in(reg) state_c.0.as_ptr(),
        d = in(reg) state_d.0.as_ptr(),
        m = in(reg) m_ptr,
        options(nostack),
    );

    // Process 64 rounds using macros for each round type
    // Shift values: F=[7,12,17,22], G=[5,9,14,20], H=[4,11,16,23], I=[6,10,15,21]

    // Round type F (0-15): F = (B & C) | (~B & D), g = i
    macro_rules! round_f {
        ($i:expr, $m_reg:literal, $s:literal) => {
            asm!(
                // F = (B & C) | (~B & D) using vpternlogd (0xCA)
                "vpternlogd zmm24, zmm1, zmm2, 0xCA",
                concat!("vpaddd zmm24, zmm24, zmm", $m_reg),
                "vpaddd zmm24, zmm24, zmm0",
                "vpbroadcastd zmm25, {k:e}",
                "vpaddd zmm24, zmm24, zmm25",
                concat!("vprold zmm24, zmm24, ", $s),
                "vmovdqa32 zmm0, zmm3",
                "vmovdqa32 zmm3, zmm2",
                "vmovdqa32 zmm2, zmm1",
                "vpaddd zmm1, zmm1, zmm24",
                k = in(reg) K[$i],
                options(nostack),
            );
        };
    }

    // Round type G (16-31): G = (D & B) | (~D & C), g = (5*i + 1) % 16
    macro_rules! round_g {
        ($i:expr, $m_reg:literal, $s:literal) => {
            asm!(
                "vpternlogd zmm24, zmm3, zmm1, 0xCA",
                concat!("vpaddd zmm24, zmm24, zmm", $m_reg),
                "vpaddd zmm24, zmm24, zmm0",
                "vpbroadcastd zmm25, {k:e}",
                "vpaddd zmm24, zmm24, zmm25",
                concat!("vprold zmm24, zmm24, ", $s),
                "vmovdqa32 zmm0, zmm3",
                "vmovdqa32 zmm3, zmm2",
                "vmovdqa32 zmm2, zmm1",
                "vpaddd zmm1, zmm1, zmm24",
                k = in(reg) K[$i],
                options(nostack),
            );
        };
    }

    // Round type H (32-47): H = B ^ C ^ D, g = (3*i + 5) % 16
    macro_rules! round_h {
        ($i:expr, $m_reg:literal, $s:literal) => {
            asm!(
                // H = B ^ C ^ D using vpternlogd (0x96 for XOR)
                "vmovdqa32 zmm24, zmm1",
                "vpxord zmm24, zmm24, zmm2",
                "vpxord zmm24, zmm24, zmm3",
                concat!("vpaddd zmm24, zmm24, zmm", $m_reg),
                "vpaddd zmm24, zmm24, zmm0",
                "vpbroadcastd zmm25, {k:e}",
                "vpaddd zmm24, zmm24, zmm25",
                concat!("vprold zmm24, zmm24, ", $s),
                "vmovdqa32 zmm0, zmm3",
                "vmovdqa32 zmm3, zmm2",
                "vmovdqa32 zmm2, zmm1",
                "vpaddd zmm1, zmm1, zmm24",
                k = in(reg) K[$i],
                options(nostack),
            );
        };
    }

    // Round type I (48-63): I = C ^ (B | ~D), g = (7*i) % 16
    macro_rules! round_i {
        ($i:expr, $m_reg:literal, $s:literal) => {
            asm!(
                "vpxord zmm24, zmm3, zmm26",
                "vpord zmm24, zmm1, zmm24",
                "vpxord zmm24, zmm2, zmm24",
                concat!("vpaddd zmm24, zmm24, zmm", $m_reg),
                "vpaddd zmm24, zmm24, zmm0",
                "vpbroadcastd zmm25, {k:e}",
                "vpaddd zmm24, zmm24, zmm25",
                concat!("vprold zmm24, zmm24, ", $s),
                "vmovdqa32 zmm0, zmm3",
                "vmovdqa32 zmm3, zmm2",
                "vmovdqa32 zmm2, zmm1",
                "vpaddd zmm1, zmm1, zmm24",
                k = in(reg) K[$i],
                options(nostack),
            );
        };
    }

    // Rounds 0-15: F function, g = i, shifts = [7, 12, 17, 22] repeating
    round_f!(0,  "8",  "7");
    round_f!(1,  "9",  "12");
    round_f!(2,  "10", "17");
    round_f!(3,  "11", "22");
    round_f!(4,  "12", "7");
    round_f!(5,  "13", "12");
    round_f!(6,  "14", "17");
    round_f!(7,  "15", "22");
    round_f!(8,  "16", "7");
    round_f!(9,  "17", "12");
    round_f!(10, "18", "17");
    round_f!(11, "19", "22");
    round_f!(12, "20", "7");
    round_f!(13, "21", "12");
    round_f!(14, "22", "17");
    round_f!(15, "23", "22");

    // Rounds 16-31: G function, g = (5*i + 1) % 16, shifts = [5, 9, 14, 20] repeating
    round_g!(16, "9",  "5");   // m[1]
    round_g!(17, "14", "9");   // m[6]
    round_g!(18, "19", "14");  // m[11]
    round_g!(19, "8",  "20");  // m[0]
    round_g!(20, "13", "5");   // m[5]
    round_g!(21, "18", "9");   // m[10]
    round_g!(22, "23", "14");  // m[15]
    round_g!(23, "12", "20");  // m[4]
    round_g!(24, "17", "5");   // m[9]
    round_g!(25, "22", "9");   // m[14]
    round_g!(26, "11", "14");  // m[3]
    round_g!(27, "16", "20");  // m[8]
    round_g!(28, "21", "5");   // m[13]
    round_g!(29, "10", "9");   // m[2]
    round_g!(30, "15", "14");  // m[7]
    round_g!(31, "20", "20");  // m[12]

    // Rounds 32-47: H function, g = (3*i + 5) % 16, shifts = [4, 11, 16, 23] repeating
    round_h!(32, "13", "4");   // m[5]
    round_h!(33, "16", "11");  // m[8]
    round_h!(34, "19", "16");  // m[11]
    round_h!(35, "22", "23");  // m[14]
    round_h!(36, "9",  "4");   // m[1]
    round_h!(37, "12", "11");  // m[4]
    round_h!(38, "15", "16");  // m[7]
    round_h!(39, "18", "23");  // m[10]
    round_h!(40, "21", "4");   // m[13]
    round_h!(41, "8",  "11");  // m[0]
    round_h!(42, "11", "16");  // m[3]
    round_h!(43, "14", "23");  // m[6]
    round_h!(44, "17", "4");   // m[9]
    round_h!(45, "20", "11");  // m[12]
    round_h!(46, "23", "16");  // m[15]
    round_h!(47, "10", "23");  // m[2]

    // Rounds 48-63: I function, g = (7*i) % 16, shifts = [6, 10, 15, 21] repeating
    round_i!(48, "8",  "6");   // m[0]
    round_i!(49, "15", "10");  // m[7]
    round_i!(50, "22", "15");  // m[14]
    round_i!(51, "13", "21");  // m[5]
    round_i!(52, "20", "6");   // m[12]
    round_i!(53, "11", "10");  // m[3]
    round_i!(54, "18", "15");  // m[10]
    round_i!(55, "9",  "21");  // m[1]
    round_i!(56, "16", "6");   // m[8]
    round_i!(57, "23", "10");  // m[15]
    round_i!(58, "14", "15");  // m[6]
    round_i!(59, "21", "21");  // m[13]
    round_i!(60, "12", "6");   // m[4]
    round_i!(61, "19", "10");  // m[11]
    round_i!(62, "10", "15");  // m[2]
    round_i!(63, "17", "21");  // m[9]

    // Add saved state and apply mask
    asm!(
        // Compute new state
        "vpaddd zmm27, zmm0, zmm4",  // new_a = a + aa
        "vpaddd zmm28, zmm1, zmm5",  // new_b = b + bb
        "vpaddd zmm29, zmm2, zmm6",  // new_c = c + cc
        "vpaddd zmm30, zmm3, zmm7",  // new_d = d + dd
        // Apply mask: blend old state for inactive lanes
        "kmovw k1, {mask:e}",
        "vpblendmd zmm27 {{k1}}, zmm4, zmm27",  // blend: k1=1 -> new, k1=0 -> old
        "vpblendmd zmm28 {{k1}}, zmm5, zmm28",
        "vpblendmd zmm29 {{k1}}, zmm6, zmm29",
        "vpblendmd zmm30 {{k1}}, zmm7, zmm30",
        // Store results
        "vmovdqu32 [{a}], zmm27",
        "vmovdqu32 [{b}], zmm28",
        "vmovdqu32 [{c}], zmm29",
        "vmovdqu32 [{d}], zmm30",
        mask = in(reg) mask_bits as u32,
        a = in(reg) state_a.0.as_mut_ptr(),
        b = in(reg) state_b.0.as_mut_ptr(),
        c = in(reg) state_c.0.as_mut_ptr(),
        d = in(reg) state_d.0.as_mut_ptr(),
        options(nostack),
    );
}

#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn digest_x16(inputs: &[&[u8]; 16]) -> [Digest; 16] {
    // Fallback for non-x86_64 platforms
    std::array::from_fn(|i| crate::scalar::digest(inputs[i]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn avx512_matches_scalar() {
        if !std::arch::is_x86_feature_detected!("avx512f")
            || !std::arch::is_x86_feature_detected!("avx512bw")
        {
            eprintln!("AVX-512 not available, skipping test");
            return;
        }

        let inputs: [&[u8]; 16] = [
            b"",
            b"a",
            b"abc",
            b"message digest",
            b"abcdefghijklmnopqrstuvwxyz",
            b"test input 5",
            b"test input 6",
            b"test input 7",
            b"test input 8",
            b"test input 9",
            b"test input 10",
            b"test input 11",
            b"test input 12",
            b"test input 13",
            b"test input 14",
            b"test input 15",
        ];

        let results = unsafe { digest_x16(&inputs) };

        for (i, input) in inputs.iter().enumerate() {
            let expected = crate::scalar::digest(input);
            assert_eq!(
                to_hex(&results[i]),
                to_hex(&expected),
                "Mismatch at lane {i} for input {:?}",
                String::from_utf8_lossy(input)
            );
        }
    }

    #[test]
    fn avx512_rfc1321_vectors() {
        if !std::arch::is_x86_feature_detected!("avx512f")
            || !std::arch::is_x86_feature_detected!("avx512bw")
        {
            eprintln!("AVX-512 not available, skipping test");
            return;
        }

        let inputs: [&[u8]; 16] = [
            b"",
            b"a",
            b"abc",
            b"message digest",
            b"abcdefghijklmnopqrstuvwxyz",
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
            b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
            b"",
            b"a",
            b"abc",
            b"message digest",
            b"abcdefghijklmnopqrstuvwxyz",
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
            b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
            b"",
            b"a",
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
            "0cc175b9c0f1b6a831c399e269772661",
            "900150983cd24fb0d6963f7d28e17f72",
            "f96b697d7cb7938d525a2f31aaf161d0",
            "c3fcd3d76192e4007dfb496cca67e13b",
            "d174ab98d277d9f5a5611c2c9f419d9f",
            "57edf4a22be3c955ac49da2e2107b67a",
            "d41d8cd98f00b204e9800998ecf8427e",
            "0cc175b9c0f1b6a831c399e269772661",
        ];

        let results = unsafe { digest_x16(&inputs) };

        for i in 0..16 {
            assert_eq!(
                to_hex(&results[i]),
                expected[i],
                "RFC 1321 vector mismatch at lane {i}"
            );
        }
    }

    #[test]
    fn avx512_various_lengths() {
        if !std::arch::is_x86_feature_detected!("avx512f")
            || !std::arch::is_x86_feature_detected!("avx512bw")
        {
            eprintln!("AVX-512 not available, skipping test");
            return;
        }

        // Test with varying lengths including block boundaries
        let input0: Vec<u8> = (0..55).map(|i| (i % 256) as u8).collect();
        let input1: Vec<u8> = (0..56).map(|i| (i % 256) as u8).collect();
        let input2: Vec<u8> = (0..64).map(|i| (i % 256) as u8).collect();
        let input3: Vec<u8> = (0..65).map(|i| (i % 256) as u8).collect();
        let input4: Vec<u8> = (0..128).map(|i| (i % 256) as u8).collect();
        let input5: Vec<u8> = (0..200).map(|i| (i % 256) as u8).collect();
        let input6: Vec<u8> = (0..1000).map(|i| (i % 256) as u8).collect();
        let input7: Vec<u8> = vec![];
        let input8: Vec<u8> = (0..63).map(|i| (i % 256) as u8).collect();
        let input9: Vec<u8> = (0..127).map(|i| (i % 256) as u8).collect();
        let input10: Vec<u8> = (0..129).map(|i| (i % 256) as u8).collect();
        let input11: Vec<u8> = (0..256).map(|i| (i % 256) as u8).collect();
        let input12: Vec<u8> = (0..512).map(|i| (i % 256) as u8).collect();
        let input13: Vec<u8> = (0..1024).map(|i| (i % 256) as u8).collect();
        let input14: Vec<u8> = (0..2048).map(|i| (i % 256) as u8).collect();
        let input15: Vec<u8> = (0..4096).map(|i| (i % 256) as u8).collect();

        let inputs: [&[u8]; 16] = [
            &input0, &input1, &input2, &input3,
            &input4, &input5, &input6, &input7,
            &input8, &input9, &input10, &input11,
            &input12, &input13, &input14, &input15,
        ];

        let results = unsafe { digest_x16(&inputs) };

        for (i, input) in inputs.iter().enumerate() {
            let expected = crate::scalar::digest(input);
            assert_eq!(
                to_hex(&results[i]),
                to_hex(&expected),
                "Mismatch at lane {i} for input length {}",
                input.len()
            );
        }
    }
}
