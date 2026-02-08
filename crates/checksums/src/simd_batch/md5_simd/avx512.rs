//! AVX-512 16-lane parallel MD5 implementation.
//!
//! Processes 16 independent MD5 computations simultaneously using 512-bit ZMM registers.
//!
//! # CPU Feature Requirements
//!
//! - **AVX-512F**: Foundation instructions (Intel Skylake-X/2017+, AMD Zen 4/2022+)
//! - **AVX-512BW**: Byte/word instructions (same CPU generations)
//! - Must be verified at runtime using `is_x86_feature_detected!`
//!
//! # Implementation Strategy
//!
//! This implementation uses **inline assembly** rather than intrinsics because AVX-512
//! intrinsics require nightly Rust. Inline assembly is stable as of Rust 1.59 and provides
//! full access to AVX-512 instructions.

#![allow(unsafe_op_in_unsafe_fn)]
//!
//! The assembly implementation:
//! - Uses ZMM registers (zmm0-zmm30) for 512-bit operations
//! - Leverages `vprold` for efficient rotation (AVX-512F native rotate)
//! - Uses `vpternlogd` for computing MD5 round functions efficiently
//! - Employs opmask registers (k1) for lane masking
//!
//! # Performance Characteristics
//!
//! - **Throughput**: ~16x scalar performance when all 16 lanes are active
//! - **Latency**: Similar to scalar for single input
//! - **Best use case**: High-throughput scenarios with 16+ inputs
//! - **Efficiency**: Best on Ice Lake and newer (improved AVX-512 execution)
//!
//! # Power Considerations
//!
//! AVX-512 can cause CPU frequency throttling on some processors (Skylake-X).
//! Modern CPUs (Ice Lake, Zen 4) have improved this significantly. Consider
//! using AVX2 for workloads that don't benefit from 16-wide parallelism.
//!
//! # Safety
//!
//! All AVX-512 operations are performed via inline assembly, which is stable in Rust.
//! The `digest_x16` function requires AVX-512F and AVX-512BW to be available, which
//! must be verified at runtime by the caller before invoking this function.

#![allow(unsafe_code)]

#[cfg(target_arch = "x86_64")]
use std::arch::asm;

use super::super::Digest;

/// MD5 initial state constants (RFC 1321).
///
/// These magic constants initialize the MD5 hash state. They represent
/// the first 32 bits of the fractional parts of the cube roots of the
/// first four prime numbers (2, 3, 5, 7).
const INIT_A: u32 = 0x6745_2301;
const INIT_B: u32 = 0xefcd_ab89;
const INIT_C: u32 = 0x98ba_dcfe;
const INIT_D: u32 = 0x1032_5476;

/// Pre-computed K constants for MD5 rounds (RFC 1321).
///
/// These 64 constants are derived from the sine function and are used as
/// additive constants in the MD5 compression function. Specifically,
/// K[i] = floor(2^32 × abs(sin(i + 1))).
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

/// Maximum input size supported for parallel processing.
///
/// Inputs larger than this threshold automatically fall back to scalar processing
/// to avoid excessive memory allocation for padded buffers. This limit balances
/// memory usage with the benefits of parallel processing.
const MAX_INPUT_SIZE: usize = 1_024 * 1_024; // 1MB per input

/// Aligned storage for 512-bit (16 × 32-bit) values.
///
/// This type ensures proper 64-byte alignment required for efficient AVX-512 operations.
/// Each instance holds 16 32-bit values that are accessed by a single ZMM register load/store.
///
/// The 64-byte alignment matches cache line boundaries on most modern CPUs, reducing
/// the risk of cache line splits and improving memory access performance.
#[repr(C, align(64))]
struct Aligned512([u32; 16]);

/// Compute MD5 digests for 16 inputs in parallel using AVX-512.
///
/// This function processes 16 independent MD5 hash computations simultaneously using
/// AVX-512 SIMD instructions, providing significant performance improvements over
/// sequential hashing when multiple inputs need to be processed.
///
/// # Algorithm
///
/// Uses 512-bit ZMM registers to compute 16 MD5 hashes in parallel through data-level
/// parallelism. The implementation uses inline assembly to access AVX-512F and AVX-512BW
/// instructions on stable Rust. Data is organized in a "transposed" layout where each
/// ZMM register holds the same field (e.g., message word 0) from all 16 inputs.
///
/// # Performance
///
/// - **Throughput**: Processes 16 hashes with only ~16x the latency of a single hash
/// - **Best for**: Batches of similarly-sized inputs (e.g., file checksums, password hashing)
/// - **Fallback**: Inputs larger than 1MB automatically fall back to scalar implementation
///   to avoid excessive memory allocation
/// - **Requirements**: Requires AVX-512F and AVX-512BW CPU features (Intel Skylake-X or later,
///   AMD Zen 4 or later)
///
/// # Parameters
///
/// * `inputs` - Array of exactly 16 byte slices to hash. Each slice can be any length,
///   though performance is optimal when all inputs are similar in size.
///
/// # Returns
///
/// Array of 16 MD5 digests (16-byte arrays) corresponding to each input in the same order.
///
/// # Safety
///
/// Caller must ensure AVX-512F and AVX-512BW CPU features are available at runtime.
/// The dispatcher module verifies this before calling this function. Calling this
/// function on a CPU without these features will result in an illegal instruction fault.
///
/// # Examples
///
/// ```ignore
/// // Prepare 16 inputs to hash in parallel
/// let inputs: [&[u8]; 16] = [
///     b"input 0", b"input 1", b"input 2", b"input 3",
///     b"input 4", b"input 5", b"input 6", b"input 7",
///     b"input 8", b"input 9", b"input 10", b"input 11",
///     b"input 12", b"input 13", b"input 14", b"input 15",
/// ];
///
/// // Safety: Caller must ensure AVX-512F and AVX-512BW are available.
/// let digests = unsafe { digest_x16(&inputs) };
/// ```
///
/// # Implementation Notes
///
/// - Handles variable-length inputs by padding each to the nearest 64-byte block boundary
/// - Processes blocks in lockstep, using masking to handle inputs of different lengths
/// - Uses transposed data layout for efficient SIMD processing
/// - Implements full MD5 specification (RFC 1321) including all 64 rounds
#[cfg(target_arch = "x86_64")]
pub unsafe fn digest_x16(inputs: &[&[u8]; 16]) -> [Digest; 16] {
    // Find the maximum length to determine block count
    let max_len = inputs.iter().map(|i| i.len()).max().unwrap_or(0);

    // For very large inputs, fall back to scalar to avoid huge allocations
    if max_len > MAX_INPUT_SIZE {
        return std::array::from_fn(|i| super::super::md5_scalar::digest(inputs[i]));
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
                if block_idx < count {
                    acc | (1 << lane)
                } else {
                    acc
                }
            });

        // Load message words (transposed: word i from all 16 inputs)
        for (word_idx, m_word) in m_storage.iter_mut().enumerate() {
            let word_offset = block_offset + word_idx * 4;
            for (lane, padded) in padded_storage.iter().enumerate() {
                m_word.0[lane] = if word_offset + 4 <= padded.len() {
                    u32::from_le_bytes(padded[word_offset..word_offset + 4].try_into().unwrap())
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
///
/// This is the core computation kernel that implements the MD5 compression function
/// for 16 independent hash states in parallel. It processes one 64-byte block for
/// each of the 16 lanes simultaneously.
///
/// # Algorithm
///
/// Implements the 64-round MD5 compression function (RFC 1321):
/// - Rounds 0-15: F function with message schedule [0..15]
/// - Rounds 16-31: G function with permuted message schedule
/// - Rounds 32-47: H function with permuted message schedule
/// - Rounds 48-63: I function with permuted message schedule
///
/// # Parameters
///
/// * `state_a`, `state_b`, `state_c`, `state_d` - MD5 state registers for all 16 lanes
/// * `m` - Transposed message words (16 arrays of 16 u32 values each)
/// * `mask_bits` - Bitmask indicating which lanes are active (bit i = lane i)
///
/// # Implementation
///
/// Uses inline assembly to efficiently utilize AVX-512 instructions including:
/// - `vpternlogd` for computing MD5 auxiliary functions
/// - `vprold` for rotation operations
/// - `vpblendmd` for conditional updates based on lane mask
/// - `vmovdqa32/vmovdqu32` for aligned/unaligned loads and stores
///
/// The function is marked `#[inline(never)]` to reduce code size, as it contains
/// substantial inline assembly that doesn't benefit from inlining.
///
/// # Safety
///
/// Requires AVX-512F and AVX-512BW to be available. Violating this precondition
/// results in undefined behavior (illegal instruction fault).
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
    //
    // IMPORTANT: All assembly is in a single asm! block to prevent the compiler
    // from inserting code between rounds that could clobber ZMM registers.
    // K constants are loaded from memory via pointer instead of per-round operands.

    let k_ptr = K.as_ptr();

    asm!(
        // ============================================================
        // Load state
        // ============================================================
        "vmovdqu32 zmm0, [{a}]",
        "vmovdqu32 zmm1, [{b}]",
        "vmovdqu32 zmm2, [{c}]",
        "vmovdqu32 zmm3, [{d}]",
        // Save state for later addition
        "vmovdqa32 zmm4, zmm0",
        "vmovdqa32 zmm5, zmm1",
        "vmovdqa32 zmm6, zmm2",
        "vmovdqa32 zmm7, zmm3",
        // Load message words m[0..15] - each Aligned512 is 64 bytes
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
        // Create all-ones for NOT operations
        "vpternlogd zmm26, zmm26, zmm26, 0xff",

        // ============================================================
        // Rounds 0-15: F = (B & C) | (~B & D), g = i
        // ============================================================
        // Round 0: m[0]=zmm8, s=7
        "vmovdqa32 zmm24, zmm1",
        "vpternlogd zmm24, zmm2, zmm3, 0xCA",
        "vpaddd zmm24, zmm24, zmm8",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k}]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 7",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 1: m[1]=zmm9, s=12
        "vmovdqa32 zmm24, zmm1",
        "vpternlogd zmm24, zmm2, zmm3, 0xCA",
        "vpaddd zmm24, zmm24, zmm9",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 4]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 12",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 2: m[2]=zmm10, s=17
        "vmovdqa32 zmm24, zmm1",
        "vpternlogd zmm24, zmm2, zmm3, 0xCA",
        "vpaddd zmm24, zmm24, zmm10",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 8]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 17",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 3: m[3]=zmm11, s=22
        "vmovdqa32 zmm24, zmm1",
        "vpternlogd zmm24, zmm2, zmm3, 0xCA",
        "vpaddd zmm24, zmm24, zmm11",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 12]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 22",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 4: m[4]=zmm12, s=7
        "vmovdqa32 zmm24, zmm1",
        "vpternlogd zmm24, zmm2, zmm3, 0xCA",
        "vpaddd zmm24, zmm24, zmm12",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 16]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 7",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 5: m[5]=zmm13, s=12
        "vmovdqa32 zmm24, zmm1",
        "vpternlogd zmm24, zmm2, zmm3, 0xCA",
        "vpaddd zmm24, zmm24, zmm13",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 20]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 12",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 6: m[6]=zmm14, s=17
        "vmovdqa32 zmm24, zmm1",
        "vpternlogd zmm24, zmm2, zmm3, 0xCA",
        "vpaddd zmm24, zmm24, zmm14",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 24]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 17",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 7: m[7]=zmm15, s=22
        "vmovdqa32 zmm24, zmm1",
        "vpternlogd zmm24, zmm2, zmm3, 0xCA",
        "vpaddd zmm24, zmm24, zmm15",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 28]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 22",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 8: m[8]=zmm16, s=7
        "vmovdqa32 zmm24, zmm1",
        "vpternlogd zmm24, zmm2, zmm3, 0xCA",
        "vpaddd zmm24, zmm24, zmm16",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 32]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 7",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 9: m[9]=zmm17, s=12
        "vmovdqa32 zmm24, zmm1",
        "vpternlogd zmm24, zmm2, zmm3, 0xCA",
        "vpaddd zmm24, zmm24, zmm17",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 36]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 12",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 10: m[10]=zmm18, s=17
        "vmovdqa32 zmm24, zmm1",
        "vpternlogd zmm24, zmm2, zmm3, 0xCA",
        "vpaddd zmm24, zmm24, zmm18",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 40]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 17",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 11: m[11]=zmm19, s=22
        "vmovdqa32 zmm24, zmm1",
        "vpternlogd zmm24, zmm2, zmm3, 0xCA",
        "vpaddd zmm24, zmm24, zmm19",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 44]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 22",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 12: m[12]=zmm20, s=7
        "vmovdqa32 zmm24, zmm1",
        "vpternlogd zmm24, zmm2, zmm3, 0xCA",
        "vpaddd zmm24, zmm24, zmm20",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 48]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 7",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 13: m[13]=zmm21, s=12
        "vmovdqa32 zmm24, zmm1",
        "vpternlogd zmm24, zmm2, zmm3, 0xCA",
        "vpaddd zmm24, zmm24, zmm21",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 52]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 12",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 14: m[14]=zmm22, s=17
        "vmovdqa32 zmm24, zmm1",
        "vpternlogd zmm24, zmm2, zmm3, 0xCA",
        "vpaddd zmm24, zmm24, zmm22",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 56]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 17",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 15: m[15]=zmm23, s=22
        "vmovdqa32 zmm24, zmm1",
        "vpternlogd zmm24, zmm2, zmm3, 0xCA",
        "vpaddd zmm24, zmm24, zmm23",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 60]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 22",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",

        // ============================================================
        // Rounds 16-31: G = (D & B) | (~D & C), g = (5*i+1)%16
        // ============================================================
        // Round 16: m[1]=zmm9, s=5
        "vmovdqa32 zmm24, zmm3",
        "vpternlogd zmm24, zmm1, zmm2, 0xCA",
        "vpaddd zmm24, zmm24, zmm9",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 64]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 5",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 17: m[6]=zmm14, s=9
        "vmovdqa32 zmm24, zmm3",
        "vpternlogd zmm24, zmm1, zmm2, 0xCA",
        "vpaddd zmm24, zmm24, zmm14",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 68]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 9",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 18: m[11]=zmm19, s=14
        "vmovdqa32 zmm24, zmm3",
        "vpternlogd zmm24, zmm1, zmm2, 0xCA",
        "vpaddd zmm24, zmm24, zmm19",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 72]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 14",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 19: m[0]=zmm8, s=20
        "vmovdqa32 zmm24, zmm3",
        "vpternlogd zmm24, zmm1, zmm2, 0xCA",
        "vpaddd zmm24, zmm24, zmm8",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 76]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 20",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 20: m[5]=zmm13, s=5
        "vmovdqa32 zmm24, zmm3",
        "vpternlogd zmm24, zmm1, zmm2, 0xCA",
        "vpaddd zmm24, zmm24, zmm13",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 80]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 5",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 21: m[10]=zmm18, s=9
        "vmovdqa32 zmm24, zmm3",
        "vpternlogd zmm24, zmm1, zmm2, 0xCA",
        "vpaddd zmm24, zmm24, zmm18",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 84]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 9",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 22: m[15]=zmm23, s=14
        "vmovdqa32 zmm24, zmm3",
        "vpternlogd zmm24, zmm1, zmm2, 0xCA",
        "vpaddd zmm24, zmm24, zmm23",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 88]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 14",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 23: m[4]=zmm12, s=20
        "vmovdqa32 zmm24, zmm3",
        "vpternlogd zmm24, zmm1, zmm2, 0xCA",
        "vpaddd zmm24, zmm24, zmm12",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 92]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 20",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 24: m[9]=zmm17, s=5
        "vmovdqa32 zmm24, zmm3",
        "vpternlogd zmm24, zmm1, zmm2, 0xCA",
        "vpaddd zmm24, zmm24, zmm17",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 96]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 5",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 25: m[14]=zmm22, s=9
        "vmovdqa32 zmm24, zmm3",
        "vpternlogd zmm24, zmm1, zmm2, 0xCA",
        "vpaddd zmm24, zmm24, zmm22",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 100]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 9",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 26: m[3]=zmm11, s=14
        "vmovdqa32 zmm24, zmm3",
        "vpternlogd zmm24, zmm1, zmm2, 0xCA",
        "vpaddd zmm24, zmm24, zmm11",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 104]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 14",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 27: m[8]=zmm16, s=20
        "vmovdqa32 zmm24, zmm3",
        "vpternlogd zmm24, zmm1, zmm2, 0xCA",
        "vpaddd zmm24, zmm24, zmm16",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 108]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 20",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 28: m[13]=zmm21, s=5
        "vmovdqa32 zmm24, zmm3",
        "vpternlogd zmm24, zmm1, zmm2, 0xCA",
        "vpaddd zmm24, zmm24, zmm21",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 112]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 5",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 29: m[2]=zmm10, s=9
        "vmovdqa32 zmm24, zmm3",
        "vpternlogd zmm24, zmm1, zmm2, 0xCA",
        "vpaddd zmm24, zmm24, zmm10",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 116]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 9",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 30: m[7]=zmm15, s=14
        "vmovdqa32 zmm24, zmm3",
        "vpternlogd zmm24, zmm1, zmm2, 0xCA",
        "vpaddd zmm24, zmm24, zmm15",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 120]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 14",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 31: m[12]=zmm20, s=20
        "vmovdqa32 zmm24, zmm3",
        "vpternlogd zmm24, zmm1, zmm2, 0xCA",
        "vpaddd zmm24, zmm24, zmm20",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 124]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 20",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",

        // ============================================================
        // Rounds 32-47: H = B ^ C ^ D, g = (3*i+5)%16
        // ============================================================
        // Round 32: m[5]=zmm13, s=4
        "vmovdqa32 zmm24, zmm1",
        "vpxord zmm24, zmm24, zmm2",
        "vpxord zmm24, zmm24, zmm3",
        "vpaddd zmm24, zmm24, zmm13",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 128]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 4",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 33: m[8]=zmm16, s=11
        "vmovdqa32 zmm24, zmm1",
        "vpxord zmm24, zmm24, zmm2",
        "vpxord zmm24, zmm24, zmm3",
        "vpaddd zmm24, zmm24, zmm16",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 132]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 11",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 34: m[11]=zmm19, s=16
        "vmovdqa32 zmm24, zmm1",
        "vpxord zmm24, zmm24, zmm2",
        "vpxord zmm24, zmm24, zmm3",
        "vpaddd zmm24, zmm24, zmm19",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 136]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 16",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 35: m[14]=zmm22, s=23
        "vmovdqa32 zmm24, zmm1",
        "vpxord zmm24, zmm24, zmm2",
        "vpxord zmm24, zmm24, zmm3",
        "vpaddd zmm24, zmm24, zmm22",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 140]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 23",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 36: m[1]=zmm9, s=4
        "vmovdqa32 zmm24, zmm1",
        "vpxord zmm24, zmm24, zmm2",
        "vpxord zmm24, zmm24, zmm3",
        "vpaddd zmm24, zmm24, zmm9",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 144]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 4",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 37: m[4]=zmm12, s=11
        "vmovdqa32 zmm24, zmm1",
        "vpxord zmm24, zmm24, zmm2",
        "vpxord zmm24, zmm24, zmm3",
        "vpaddd zmm24, zmm24, zmm12",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 148]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 11",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 38: m[7]=zmm15, s=16
        "vmovdqa32 zmm24, zmm1",
        "vpxord zmm24, zmm24, zmm2",
        "vpxord zmm24, zmm24, zmm3",
        "vpaddd zmm24, zmm24, zmm15",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 152]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 16",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 39: m[10]=zmm18, s=23
        "vmovdqa32 zmm24, zmm1",
        "vpxord zmm24, zmm24, zmm2",
        "vpxord zmm24, zmm24, zmm3",
        "vpaddd zmm24, zmm24, zmm18",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 156]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 23",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 40: m[13]=zmm21, s=4
        "vmovdqa32 zmm24, zmm1",
        "vpxord zmm24, zmm24, zmm2",
        "vpxord zmm24, zmm24, zmm3",
        "vpaddd zmm24, zmm24, zmm21",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 160]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 4",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 41: m[0]=zmm8, s=11
        "vmovdqa32 zmm24, zmm1",
        "vpxord zmm24, zmm24, zmm2",
        "vpxord zmm24, zmm24, zmm3",
        "vpaddd zmm24, zmm24, zmm8",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 164]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 11",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 42: m[3]=zmm11, s=16
        "vmovdqa32 zmm24, zmm1",
        "vpxord zmm24, zmm24, zmm2",
        "vpxord zmm24, zmm24, zmm3",
        "vpaddd zmm24, zmm24, zmm11",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 168]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 16",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 43: m[6]=zmm14, s=23
        "vmovdqa32 zmm24, zmm1",
        "vpxord zmm24, zmm24, zmm2",
        "vpxord zmm24, zmm24, zmm3",
        "vpaddd zmm24, zmm24, zmm14",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 172]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 23",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 44: m[9]=zmm17, s=4
        "vmovdqa32 zmm24, zmm1",
        "vpxord zmm24, zmm24, zmm2",
        "vpxord zmm24, zmm24, zmm3",
        "vpaddd zmm24, zmm24, zmm17",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 176]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 4",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 45: m[12]=zmm20, s=11
        "vmovdqa32 zmm24, zmm1",
        "vpxord zmm24, zmm24, zmm2",
        "vpxord zmm24, zmm24, zmm3",
        "vpaddd zmm24, zmm24, zmm20",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 180]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 11",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 46: m[15]=zmm23, s=16
        "vmovdqa32 zmm24, zmm1",
        "vpxord zmm24, zmm24, zmm2",
        "vpxord zmm24, zmm24, zmm3",
        "vpaddd zmm24, zmm24, zmm23",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 184]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 16",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 47: m[2]=zmm10, s=23
        "vmovdqa32 zmm24, zmm1",
        "vpxord zmm24, zmm24, zmm2",
        "vpxord zmm24, zmm24, zmm3",
        "vpaddd zmm24, zmm24, zmm10",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 188]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 23",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",

        // ============================================================
        // Rounds 48-63: I = C ^ (B | ~D), g = (7*i)%16
        // ============================================================
        // Round 48: m[0]=zmm8, s=6
        "vpxord zmm24, zmm3, zmm26",
        "vpord zmm24, zmm1, zmm24",
        "vpxord zmm24, zmm2, zmm24",
        "vpaddd zmm24, zmm24, zmm8",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 192]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 6",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 49: m[7]=zmm15, s=10
        "vpxord zmm24, zmm3, zmm26",
        "vpord zmm24, zmm1, zmm24",
        "vpxord zmm24, zmm2, zmm24",
        "vpaddd zmm24, zmm24, zmm15",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 196]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 10",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 50: m[14]=zmm22, s=15
        "vpxord zmm24, zmm3, zmm26",
        "vpord zmm24, zmm1, zmm24",
        "vpxord zmm24, zmm2, zmm24",
        "vpaddd zmm24, zmm24, zmm22",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 200]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 15",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 51: m[5]=zmm13, s=21
        "vpxord zmm24, zmm3, zmm26",
        "vpord zmm24, zmm1, zmm24",
        "vpxord zmm24, zmm2, zmm24",
        "vpaddd zmm24, zmm24, zmm13",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 204]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 21",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 52: m[12]=zmm20, s=6
        "vpxord zmm24, zmm3, zmm26",
        "vpord zmm24, zmm1, zmm24",
        "vpxord zmm24, zmm2, zmm24",
        "vpaddd zmm24, zmm24, zmm20",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 208]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 6",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 53: m[3]=zmm11, s=10
        "vpxord zmm24, zmm3, zmm26",
        "vpord zmm24, zmm1, zmm24",
        "vpxord zmm24, zmm2, zmm24",
        "vpaddd zmm24, zmm24, zmm11",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 212]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 10",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 54: m[10]=zmm18, s=15
        "vpxord zmm24, zmm3, zmm26",
        "vpord zmm24, zmm1, zmm24",
        "vpxord zmm24, zmm2, zmm24",
        "vpaddd zmm24, zmm24, zmm18",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 216]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 15",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 55: m[1]=zmm9, s=21
        "vpxord zmm24, zmm3, zmm26",
        "vpord zmm24, zmm1, zmm24",
        "vpxord zmm24, zmm2, zmm24",
        "vpaddd zmm24, zmm24, zmm9",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 220]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 21",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 56: m[8]=zmm16, s=6
        "vpxord zmm24, zmm3, zmm26",
        "vpord zmm24, zmm1, zmm24",
        "vpxord zmm24, zmm2, zmm24",
        "vpaddd zmm24, zmm24, zmm16",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 224]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 6",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 57: m[15]=zmm23, s=10
        "vpxord zmm24, zmm3, zmm26",
        "vpord zmm24, zmm1, zmm24",
        "vpxord zmm24, zmm2, zmm24",
        "vpaddd zmm24, zmm24, zmm23",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 228]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 10",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 58: m[6]=zmm14, s=15
        "vpxord zmm24, zmm3, zmm26",
        "vpord zmm24, zmm1, zmm24",
        "vpxord zmm24, zmm2, zmm24",
        "vpaddd zmm24, zmm24, zmm14",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 232]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 15",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 59: m[13]=zmm21, s=21
        "vpxord zmm24, zmm3, zmm26",
        "vpord zmm24, zmm1, zmm24",
        "vpxord zmm24, zmm2, zmm24",
        "vpaddd zmm24, zmm24, zmm21",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 236]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 21",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 60: m[4]=zmm12, s=6
        "vpxord zmm24, zmm3, zmm26",
        "vpord zmm24, zmm1, zmm24",
        "vpxord zmm24, zmm2, zmm24",
        "vpaddd zmm24, zmm24, zmm12",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 240]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 6",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 61: m[11]=zmm19, s=10
        "vpxord zmm24, zmm3, zmm26",
        "vpord zmm24, zmm1, zmm24",
        "vpxord zmm24, zmm2, zmm24",
        "vpaddd zmm24, zmm24, zmm19",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 244]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 10",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 62: m[2]=zmm10, s=15
        "vpxord zmm24, zmm3, zmm26",
        "vpord zmm24, zmm1, zmm24",
        "vpxord zmm24, zmm2, zmm24",
        "vpaddd zmm24, zmm24, zmm10",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 248]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 15",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",
        // Round 63: m[9]=zmm17, s=21
        "vpxord zmm24, zmm3, zmm26",
        "vpord zmm24, zmm1, zmm24",
        "vpxord zmm24, zmm2, zmm24",
        "vpaddd zmm24, zmm24, zmm17",
        "vpaddd zmm24, zmm24, zmm0",
        "vpbroadcastd zmm25, dword ptr [{k} + 252]",
        "vpaddd zmm24, zmm24, zmm25",
        "vprold zmm24, zmm24, 21",
        "vmovdqa32 zmm0, zmm3",
        "vmovdqa32 zmm3, zmm2",
        "vmovdqa32 zmm2, zmm1",
        "vpaddd zmm1, zmm1, zmm24",

        // ============================================================
        // Add saved state and apply mask
        // ============================================================
        "vpaddd zmm27, zmm0, zmm4",
        "vpaddd zmm28, zmm1, zmm5",
        "vpaddd zmm29, zmm2, zmm6",
        "vpaddd zmm30, zmm3, zmm7",
        // Apply mask: blend old state for inactive lanes
        "kmovw k1, {mask:e}",
        "vpblendmd zmm27 {{k1}}, zmm4, zmm27",
        "vpblendmd zmm28 {{k1}}, zmm5, zmm28",
        "vpblendmd zmm29 {{k1}}, zmm6, zmm29",
        "vpblendmd zmm30 {{k1}}, zmm7, zmm30",
        // Store results
        "vmovdqu32 [{a}], zmm27",
        "vmovdqu32 [{b}], zmm28",
        "vmovdqu32 [{c}], zmm29",
        "vmovdqu32 [{d}], zmm30",

        a = in(reg) state_a.0.as_mut_ptr(),
        b = in(reg) state_b.0.as_mut_ptr(),
        c = in(reg) state_c.0.as_mut_ptr(),
        d = in(reg) state_d.0.as_mut_ptr(),
        m = in(reg) m.as_ptr() as *const u32,
        k = in(reg) k_ptr,
        mask = in(reg) mask_bits as u32,
        // Clobber all ZMM registers used
        out("zmm0") _, out("zmm1") _, out("zmm2") _, out("zmm3") _,
        out("zmm4") _, out("zmm5") _, out("zmm6") _, out("zmm7") _,
        out("zmm8") _, out("zmm9") _, out("zmm10") _, out("zmm11") _,
        out("zmm12") _, out("zmm13") _, out("zmm14") _, out("zmm15") _,
        out("zmm16") _, out("zmm17") _, out("zmm18") _, out("zmm19") _,
        out("zmm20") _, out("zmm21") _, out("zmm22") _, out("zmm23") _,
        out("zmm24") _, out("zmm25") _, out("zmm26") _, out("zmm27") _,
        out("zmm28") _, out("zmm29") _, out("zmm30") _,
        out("k1") _,
        options(nostack),
    );
}

/// Fallback implementation of parallel MD5 hashing for non-x86_64 platforms.
///
/// On platforms without x86_64 architecture, this function falls back to computing
/// each hash sequentially using the scalar implementation. This provides API compatibility
/// across platforms while sacrificing the performance benefits of parallel SIMD processing.
///
/// # Safety
///
/// This function is safe to call on any platform, though it is marked unsafe to match
/// the signature of the x86_64 implementation.
#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn digest_x16(inputs: &[&[u8]; 16]) -> [Digest; 16] {
    // Fallback for non-x86_64 platforms
    std::array::from_fn(|i| super::super::md5_scalar::digest(inputs[i]))
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
        let input6: Vec<u8> = (0..1_000).map(|i| (i % 256) as u8).collect();
        let input7: Vec<u8> = vec![];
        let input8: Vec<u8> = (0..63).map(|i| (i % 256) as u8).collect();
        let input9: Vec<u8> = (0..127).map(|i| (i % 256) as u8).collect();
        let input10: Vec<u8> = (0..129).map(|i| (i % 256) as u8).collect();
        let input11: Vec<u8> = (0..256).map(|i| (i % 256) as u8).collect();
        let input12: Vec<u8> = (0..512).map(|i| (i % 256) as u8).collect();
        let input13: Vec<u8> = (0..1_024).map(|i| (i % 256) as u8).collect();
        let input14: Vec<u8> = (0..2_048).map(|i| (i % 256) as u8).collect();
        let input15: Vec<u8> = (0..4_096).map(|i| (i % 256) as u8).collect();

        let inputs: [&[u8]; 16] = [
            &input0, &input1, &input2, &input3, &input4, &input5, &input6, &input7, &input8,
            &input9, &input10, &input11, &input12, &input13, &input14, &input15,
        ];

        let results = unsafe { digest_x16(&inputs) };

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
