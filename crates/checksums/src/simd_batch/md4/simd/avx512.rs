//! AVX-512 16-lane parallel MD4 implementation.
//!
//! Processes 16 independent MD4 computations simultaneously using 512-bit ZMM registers.
//! Uses stable inline assembly since AVX-512 intrinsics require nightly Rust.

#![allow(unsafe_op_in_unsafe_fn)]

#[cfg(target_arch = "x86_64")]
use std::arch::asm;

use super::super::super::Digest;

/// MD4 initial state constants (RFC 1320).
const INIT_A: u32 = 0x6745_2301;
const INIT_B: u32 = 0xefcd_ab89;
const INIT_C: u32 = 0x98ba_dcfe;
const INIT_D: u32 = 0x1032_5476;

/// Round constants for MD4 (RFC 1320).
///
/// Round 1 (steps 0-15) adds 0; round 2 (16-31) adds `0x5A827999` (scaled
/// sqrt(2)); round 3 (32-47) adds `0x6ED9EBA1` (scaled sqrt(3)).
const K: [u32; 3] = [
    0x0000_0000, // Round 1
    0x5A82_7999, // Round 2
    0x6ED9_EBA1, // Round 3
];

// Message schedules consumed inside the round! macros below:
// M2 = [0, 4, 8, 12, 1, 5, 9, 13, 2, 6, 10, 14, 3, 7, 11, 15]
// M3 = [0, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5, 13, 3, 11, 7, 15]

/// 64-byte aligned 16x u32 storage for ZMM register loads/stores.
///
/// AVX-512 ZMM moves perform best on 64-byte-aligned addresses, which also
/// matches the cache line on most modern CPUs.
#[repr(C, align(64))]
struct Aligned512([u32; 16]);

/// Compute MD4 digests for 16 inputs in parallel using AVX-512.
///
/// Inputs are padded to 64-byte block boundaries and processed in lockstep
/// across 16 lanes; per-lane opmask handling keeps short lanes inactive while
/// longer lanes continue. Inputs larger than 1 MiB fall back to scalar to
/// cap padding allocations. Implements the full RFC 1320 48-round MD4
/// compression function.
///
/// # Safety
///
/// Caller must verify AVX-512F and AVX-512BW are available at runtime.
/// Invoking this on a CPU without these features triggers an illegal-
/// instruction fault.
#[cfg(target_arch = "x86_64")]
pub unsafe fn digest_x16(inputs: &[&[u8]; 16]) -> [Digest; 16] {
    let max_len = inputs.iter().map(|i| i.len()).max().unwrap_or(0);

    // Fall back to scalar for inputs that would require excessive padding allocations.
    if max_len > 1024 * 1024 {
        return std::array::from_fn(|i| super::super::scalar::digest(inputs[i]));
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

    let block_counts: [usize; 16] = std::array::from_fn(|i| padded_storage[i].len() / 64);
    let max_blocks = block_counts.iter().max().copied().unwrap_or(0);

    let mut state_a = Aligned512([INIT_A; 16]);
    let mut state_b = Aligned512([INIT_B; 16]);
    let mut state_c = Aligned512([INIT_C; 16]);
    let mut state_d = Aligned512([INIT_D; 16]);

    for block_idx in 0..max_blocks {
        let block_offset = block_idx * 64;

        let mask_bits: u16 = block_counts
            .iter()
            .enumerate()
            .fold(0u16, |acc, (lane, &count)| {
                if block_idx < count {
                    acc | (1 << lane)
                } else {
                    acc
                }
            });

        // Transpose: word `word_idx` from all 16 lanes packed into one vector.
        let mut m: [Aligned512; 16] = std::array::from_fn(|_| Aligned512([0u32; 16]));

        for (word_idx, m_word) in m.iter_mut().enumerate() {
            let word_offset = block_offset + word_idx * 4;
            for (lane, padded) in padded_storage.iter().enumerate() {
                if word_offset + 4 <= padded.len() {
                    m_word.0[lane] = u32::from_le_bytes(
                        padded[word_offset..word_offset + 4]
                            .try_into()
                            .expect("4-byte word slice"),
                    );
                }
            }
        }

        process_block_avx512(
            &mut state_a,
            &mut state_b,
            &mut state_c,
            &mut state_d,
            &m,
            mask_bits,
        );
    }

    let mut results = [[0u8; 16]; 16];
    for (lane, result) in results.iter_mut().enumerate() {
        result[0..4].copy_from_slice(&state_a.0[lane].to_le_bytes());
        result[4..8].copy_from_slice(&state_b.0[lane].to_le_bytes());
        result[8..12].copy_from_slice(&state_c.0[lane].to_le_bytes());
        result[12..16].copy_from_slice(&state_d.0[lane].to_le_bytes());
    }

    results
}

/// Apply one MD4 block in parallel across 16 lanes via AVX-512 inline assembly.
///
/// Implements the RFC 1320 48-round compression function: round 1 (F, K=0),
/// round 2 (G, K=0x5A827999), round 3 (H, K=0x6ED9EBA1). `mask_bits` is a
/// 16-bit lane mask (bit i selects lane i) used through `vpblendmd` so
/// short-input lanes are not advanced past their final block.
///
/// Marked `#[inline(never)]` because the asm body is large enough that
/// inlining bloats call sites without speedup.
///
/// # Safety
///
/// Caller must guarantee AVX-512F and AVX-512BW are available; violating
/// this triggers an illegal-instruction fault.
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
    let m_ptr = m.as_ptr() as *const u32;

    // Register allocation:
    // zmm0 = a, zmm1 = b, zmm2 = c, zmm3 = d
    // zmm4 = aa (saved a), zmm5 = bb, zmm6 = cc, zmm7 = dd
    // zmm8-zmm23 = message words m[0..15]
    // zmm24-zmm27 = temporaries
    // zmm28 = k (current round constant)

    asm!(
        // Load initial state
        "vmovdqu32 zmm0, [{state_a}]",
        "vmovdqu32 zmm1, [{state_b}]",
        "vmovdqu32 zmm2, [{state_c}]",
        "vmovdqu32 zmm3, [{state_d}]",

        // Save state
        "vmovdqa32 zmm4, zmm0",
        "vmovdqa32 zmm5, zmm1",
        "vmovdqa32 zmm6, zmm2",
        "vmovdqa32 zmm7, zmm3",

        // Load message words
        "vmovdqu32 zmm8,  [{m}]",
        "vmovdqu32 zmm9,  [{m} + 64]",
        "vmovdqu32 zmm10, [{m} + 128]",
        "vmovdqu32 zmm11, [{m} + 192]",
        "vmovdqu32 zmm12, [{m} + 256]",
        "vmovdqu32 zmm13, [{m} + 320]",
        "vmovdqu32 zmm14, [{m} + 384]",
        "vmovdqu32 zmm15, [{m} + 448]",
        "vmovdqu32 zmm16, [{m} + 512]",
        "vmovdqu32 zmm17, [{m} + 576]",
        "vmovdqu32 zmm18, [{m} + 640]",
        "vmovdqu32 zmm19, [{m} + 704]",
        "vmovdqu32 zmm20, [{m} + 768]",
        "vmovdqu32 zmm21, [{m} + 832]",
        "vmovdqu32 zmm22, [{m} + 896]",
        "vmovdqu32 zmm23, [{m} + 960]",

        state_a = in(reg) state_a.0.as_ptr(),
        state_b = in(reg) state_b.0.as_ptr(),
        state_c = in(reg) state_c.0.as_ptr(),
        state_d = in(reg) state_d.0.as_ptr(),
        m = in(reg) m_ptr,
        options(nostack),
    );

    // Round 1: F = (B & C) | (~B & D), K = 0, shifts [3, 7, 11, 19].
    // vpternlogd imm8 = 0xCA implements F over (B, C, D).
    macro_rules! round1 {
        ($m_reg:literal, $s:literal) => {
            asm!(
                // F = (B & C) | (~B & D) using vpternlogd
                // zmm24 = B, C, D -> 0xCA = select C where B, else D
                "vmovdqa32 zmm24, zmm1",
                "vpternlogd zmm24, zmm2, zmm3, 0xCA",

                // Add A + F + M[i] (no K for round 1)
                "vpaddd zmm24, zmm24, zmm0",
                concat!("vpaddd zmm24, zmm24, zmm", $m_reg),

                // Rotate left by shift
                concat!("vprold zmm24, zmm24, ", $s),

                // Update state: a=d, d=c, c=b, b=rotated
                "vmovdqa32 zmm0, zmm3",
                "vmovdqa32 zmm3, zmm2",
                "vmovdqa32 zmm2, zmm1",
                "vmovdqa32 zmm1, zmm24",

                options(nostack),
            );
        };
    }

    // Round 1: steps 0-15
    round1!("8", "3"); // m[0], s=3
    round1!("9", "7"); // m[1], s=7
    round1!("10", "11"); // m[2], s=11
    round1!("11", "19"); // m[3], s=19
    round1!("12", "3"); // m[4], s=3
    round1!("13", "7"); // m[5], s=7
    round1!("14", "11"); // m[6], s=11
    round1!("15", "19"); // m[7], s=19
    round1!("16", "3"); // m[8], s=3
    round1!("17", "7"); // m[9], s=7
    round1!("18", "11"); // m[10], s=11
    round1!("19", "19"); // m[11], s=19
    round1!("20", "3"); // m[12], s=3
    round1!("21", "7"); // m[13], s=7
    round1!("22", "11"); // m[14], s=11
    round1!("23", "19"); // m[15], s=19

    // Round 2: G = (B & C) | (B & D) | (C & D), K = 0x5A827999
    // Equivalent: (B & C) | (D & (B | C))
    // Using vpternlogd with 0xE8 implements majority function
    // Shift amounts: 3, 5, 9, 13
    // Message schedule: M2 = [0, 4, 8, 12, 1, 5, 9, 13, 2, 6, 10, 14, 3, 7, 11, 15]

    // Load K2
    asm!(
        "vpbroadcastd zmm28, {k:e}",
        k = in(reg) K[1],
        options(nostack),
    );

    macro_rules! round2 {
        ($m_reg:literal, $s:literal) => {
            asm!(
                // G = majority(B, C, D) using vpternlogd 0xE8
                "vmovdqa32 zmm24, zmm1",
                "vpternlogd zmm24, zmm2, zmm3, 0xE8",

                // Add A + G + K + M[i]
                "vpaddd zmm24, zmm24, zmm0",
                "vpaddd zmm24, zmm24, zmm28",
                concat!("vpaddd zmm24, zmm24, zmm", $m_reg),

                // Rotate left
                concat!("vprold zmm24, zmm24, ", $s),

                // Update state
                "vmovdqa32 zmm0, zmm3",
                "vmovdqa32 zmm3, zmm2",
                "vmovdqa32 zmm2, zmm1",
                "vmovdqa32 zmm1, zmm24",

                options(nostack),
            );
        };
    }

    // Round 2: steps 0-15 with M2 schedule
    round2!("8", "3"); // M2[0]=0, s=3
    round2!("12", "5"); // M2[1]=4, s=5
    round2!("16", "9"); // M2[2]=8, s=9
    round2!("20", "13"); // M2[3]=12, s=13
    round2!("9", "3"); // M2[4]=1, s=3
    round2!("13", "5"); // M2[5]=5, s=5
    round2!("17", "9"); // M2[6]=9, s=9
    round2!("21", "13"); // M2[7]=13, s=13
    round2!("10", "3"); // M2[8]=2, s=3
    round2!("14", "5"); // M2[9]=6, s=5
    round2!("18", "9"); // M2[10]=10, s=9
    round2!("22", "13"); // M2[11]=14, s=13
    round2!("11", "3"); // M2[12]=3, s=3
    round2!("15", "5"); // M2[13]=7, s=5
    round2!("19", "9"); // M2[14]=11, s=9
    round2!("23", "13"); // M2[15]=15, s=13

    // Round 3: H = B ^ C ^ D, K = 0x6ED9EBA1
    // Shift amounts: 3, 9, 11, 15
    // Message schedule: M3 = [0, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5, 13, 3, 11, 7, 15]

    // Load K3
    asm!(
        "vpbroadcastd zmm28, {k:e}",
        k = in(reg) K[2],
        options(nostack),
    );

    macro_rules! round3 {
        ($m_reg:literal, $s:literal) => {
            asm!(
                // H = B ^ C ^ D
                "vmovdqa32 zmm24, zmm1",
                "vpxord zmm24, zmm24, zmm2",
                "vpxord zmm24, zmm24, zmm3",

                // Add A + H + K + M[i]
                "vpaddd zmm24, zmm24, zmm0",
                "vpaddd zmm24, zmm24, zmm28",
                concat!("vpaddd zmm24, zmm24, zmm", $m_reg),

                // Rotate left
                concat!("vprold zmm24, zmm24, ", $s),

                // Update state
                "vmovdqa32 zmm0, zmm3",
                "vmovdqa32 zmm3, zmm2",
                "vmovdqa32 zmm2, zmm1",
                "vmovdqa32 zmm1, zmm24",

                options(nostack),
            );
        };
    }

    // Round 3: steps 0-15 with M3 schedule
    round3!("8", "3"); // M3[0]=0, s=3
    round3!("16", "9"); // M3[1]=8, s=9
    round3!("12", "11"); // M3[2]=4, s=11
    round3!("20", "15"); // M3[3]=12, s=15
    round3!("10", "3"); // M3[4]=2, s=3
    round3!("18", "9"); // M3[5]=10, s=9
    round3!("14", "11"); // M3[6]=6, s=11
    round3!("22", "15"); // M3[7]=14, s=15
    round3!("9", "3"); // M3[8]=1, s=3
    round3!("17", "9"); // M3[9]=9, s=9
    round3!("13", "11"); // M3[10]=5, s=11
    round3!("21", "15"); // M3[11]=13, s=15
    round3!("11", "3"); // M3[12]=3, s=3
    round3!("19", "9"); // M3[13]=11, s=9
    round3!("15", "11"); // M3[14]=7, s=11
    round3!("23", "15"); // M3[15]=15, s=15

    // Add saved state and apply mask
    asm!(
        // Load mask into k1
        "kmovw k1, {mask:e}",

        // Add saved state to current state
        "vpaddd zmm0, zmm0, zmm4",
        "vpaddd zmm1, zmm1, zmm5",
        "vpaddd zmm2, zmm2, zmm6",
        "vpaddd zmm3, zmm3, zmm7",

        // Blend: keep old state for inactive lanes
        "vpblendmd zmm0 {{k1}}, zmm4, zmm0",
        "vpblendmd zmm1 {{k1}}, zmm5, zmm1",
        "vpblendmd zmm2 {{k1}}, zmm6, zmm2",
        "vpblendmd zmm3 {{k1}}, zmm7, zmm3",

        // Store results
        "vmovdqu32 [{state_a}], zmm0",
        "vmovdqu32 [{state_b}], zmm1",
        "vmovdqu32 [{state_c}], zmm2",
        "vmovdqu32 [{state_d}], zmm3",

        mask = in(reg) mask_bits as u32,
        state_a = in(reg) state_a.0.as_mut_ptr(),
        state_b = in(reg) state_b.0.as_mut_ptr(),
        state_c = in(reg) state_c.0.as_mut_ptr(),
        state_d = in(reg) state_d.0.as_mut_ptr(),
        options(nostack),
    );
}

#[cfg(test)]
mod tests {
    use super::super::super::scalar;
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
    fn avx512_md4_matches_scalar() {
        if !is_x86_feature_detected!("avx512f") || !is_x86_feature_detected!("avx512bw") {
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

        // SAFETY: AVX-512F and AVX-512BW availability were verified above
        // via `is_x86_feature_detected!`, satisfying the
        // `target_feature = "avx512f,avx512bw"` precondition of
        // `digest_x16`. `inputs` is a fixed-length array of 16 valid
        // `&[u8]` borrows; `digest_x16` bounds-checks each lane.
        let results = unsafe { digest_x16(&inputs) };

        for (i, input) in inputs.iter().enumerate() {
            let expected = scalar::digest(input);
            assert_eq!(
                to_hex(&results[i]),
                to_hex(&expected),
                "Mismatch at lane {i} for input {:?}",
                String::from_utf8_lossy(input)
            );
        }
    }

    #[test]
    fn avx512_md4_rfc1320_vectors() {
        if !is_x86_feature_detected!("avx512f") || !is_x86_feature_detected!("avx512bw") {
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
            "31d6cfe0d16ae931b73c59d7e0c089c0",
            "bde52cb31de33e46245e05fbdbd6fb24",
            "a448017aaf21d8525fc10ae87aa6729d",
            "d9130a8164549fe818874806e1c7014b",
            "d79e1c308aa5bbcdeea8ed63df412da9",
            "043f8582f241db351ce627e153e7f0e4",
            "e33b4ddc9c38f2199c3e7b164fcc0536",
            "31d6cfe0d16ae931b73c59d7e0c089c0",
            "bde52cb31de33e46245e05fbdbd6fb24",
            "a448017aaf21d8525fc10ae87aa6729d",
            "d9130a8164549fe818874806e1c7014b",
            "d79e1c308aa5bbcdeea8ed63df412da9",
            "043f8582f241db351ce627e153e7f0e4",
            "e33b4ddc9c38f2199c3e7b164fcc0536",
            "31d6cfe0d16ae931b73c59d7e0c089c0",
            "bde52cb31de33e46245e05fbdbd6fb24",
        ];

        // SAFETY: AVX-512F and AVX-512BW availability were verified above
        // via `is_x86_feature_detected!`, satisfying the
        // `target_feature = "avx512f,avx512bw"` precondition of
        // `digest_x16`. `inputs` is a fixed-length array of 16 valid
        // `&[u8]` borrows that outlive the call.
        let results = unsafe { digest_x16(&inputs) };

        for i in 0..16 {
            assert_eq!(
                to_hex(&results[i]),
                expected[i],
                "RFC 1320 vector mismatch at lane {i}"
            );
        }
    }

    #[test]
    fn avx512_md4_various_lengths() {
        if !is_x86_feature_detected!("avx512f") || !is_x86_feature_detected!("avx512bw") {
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
        let input6: Vec<u8> = (0..1_000).map(|i| (i % 256) as u8).collect();
        let input7: Vec<u8> = vec![];
        let input8: Vec<u8> = (0..63).map(|i| (i % 256) as u8).collect();
        let input9: Vec<u8> = (0..57).map(|i| (i % 256) as u8).collect();
        let input10: Vec<u8> = (0..119).map(|i| (i % 256) as u8).collect();
        let input11: Vec<u8> = (0..120).map(|i| (i % 256) as u8).collect();
        let input12: Vec<u8> = (0..121).map(|i| (i % 256) as u8).collect();
        let input13: Vec<u8> = vec![0xAB; 500];
        let input14: Vec<u8> = vec![0xCD; 700];
        let input15: Vec<u8> = vec![0xEF; 900];

        let inputs: [&[u8]; 16] = [
            &input0, &input1, &input2, &input3, &input4, &input5, &input6, &input7, &input8,
            &input9, &input10, &input11, &input12, &input13, &input14, &input15,
        ];

        // SAFETY: AVX-512F and AVX-512BW availability were verified above
        // via `is_x86_feature_detected!`, satisfying the
        // `target_feature = "avx512f,avx512bw"` precondition of
        // `digest_x16`. `inputs` borrows 16 owned `Vec<u8>` buffers that
        // outlive the call; `digest_x16` bounds-checks each lane.
        let results = unsafe { digest_x16(&inputs) };

        for (i, input) in inputs.iter().enumerate() {
            let expected = scalar::digest(input);
            assert_eq!(
                to_hex(&results[i]),
                to_hex(&expected),
                "Mismatch at lane {i} for input length {}",
                input.len()
            );
        }
    }
}
