//! Defense-in-depth proptests asserting the compress decoders never panic on
//! malformed input.
//!
//! Decoders are reached by attackers (the receiver decompresses bytes chosen
//! by the sender). UTS-18 (PR #5566) surfaced one bounds-check-via-panic class
//! in an adjacent parser path; these tests catch the same class in the zlib,
//! zstd, and lz4 codec decoders by sweeping arbitrary byte buffers through
//! each decoder entry point and asserting only `Ok(_)` or `Err(_)` come back.
//!
//! Output is bounded with [`std::io::Read::take`] so decompression bombs do
//! not OOM the harness. Each proptest runs `CASES` iterations; see the
//! `proptest!` config blocks for the rationale.

use std::io::Read;

use proptest::prelude::*;

/// Cap on bytes pulled out of any single decoder run. Cheap enough that
/// repeated proptest iterations stay well under CI memory budgets, large
/// enough that legitimate small payloads decode through to completion.
const MAX_OUTPUT_BYTES: u64 = 64 * 1024;

/// Cap on input length fed to each decoder. Matches the order of magnitude
/// of realistic short rsync-wire blocks while keeping the proptest shrinker
/// fast.
const MAX_INPUT_BYTES: usize = 16 * 1024;

/// Number of proptest cases per codec. 256 is the workspace default sweet
/// spot for fuzz-style tests in `crates/filters/tests/proptest_fuzz.rs`: it
/// runs in well under a second per case set in CI while giving the shrinker
/// enough samples to surface adversarial inputs.
const CASES: u32 = 256;

/// Drains a `Read` impl into a discard sink, bounded to [`MAX_OUTPUT_BYTES`].
///
/// Returns `Ok(())` on either clean EOF or a decoder error - both are valid
/// outcomes for arbitrary input. Panics propagate, which is the failure
/// signal proptest is watching for.
fn drain_bounded<R: Read>(reader: R) -> std::io::Result<u64> {
    let mut bounded = reader.take(MAX_OUTPUT_BYTES);
    let mut sink = std::io::sink();
    std::io::copy(&mut bounded, &mut sink)
}

// ---------------------------------------------------------------------------
// zlib
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: CASES, ..ProptestConfig::default() })]

    /// Arbitrary bytes fed to the raw-deflate decoder must yield `Ok` or `Err`
    /// without panicking, regardless of header validity or stream truncation.
    #[test]
    fn zlib_decoder_no_panic_on_arbitrary_input(
        buf in proptest::collection::vec(any::<u8>(), 0..MAX_INPUT_BYTES),
    ) {
        let decoder = compress::zlib::CountingZlibDecoder::new(&buf[..]);
        let _ = drain_bounded(decoder);
    }

    /// `decompress_to_vec` is the one-shot wrapper most callers use; sweep
    /// the same arbitrary inputs through it independently because its output
    /// path differs (unbounded `Vec` instead of bounded sink). Input cap
    /// keeps decompression-bomb risk bounded.
    #[test]
    fn zlib_helper_no_panic_on_arbitrary_input(
        buf in proptest::collection::vec(any::<u8>(), 0..MAX_INPUT_BYTES),
    ) {
        let _ = compress::zlib::decompress_to_vec(&buf);
    }
}

// ---------------------------------------------------------------------------
// zstd
// ---------------------------------------------------------------------------

#[cfg(feature = "zstd")]
proptest! {
    #![proptest_config(ProptestConfig { cases: CASES, ..ProptestConfig::default() })]

    /// Arbitrary bytes fed to the zstd decoder must not panic. zstd has its
    /// own magic-number and frame-header checks; we exercise both the
    /// construction path (which can fail before any byte is read) and the
    /// streaming read path.
    #[test]
    fn zstd_decoder_no_panic_on_arbitrary_input(
        buf in proptest::collection::vec(any::<u8>(), 0..MAX_INPUT_BYTES),
    ) {
        if let Ok(decoder) = compress::zstd::CountingZstdDecoder::new(&buf[..]) {
            let _ = drain_bounded(decoder);
        }
    }

    /// One-shot wrapper coverage mirrors the zlib case.
    #[test]
    fn zstd_helper_no_panic_on_arbitrary_input(
        buf in proptest::collection::vec(any::<u8>(), 0..MAX_INPUT_BYTES),
    ) {
        let _ = compress::zstd::decompress_to_vec(&buf);
    }
}

// ---------------------------------------------------------------------------
// lz4
// ---------------------------------------------------------------------------

#[cfg(feature = "lz4")]
proptest! {
    #![proptest_config(ProptestConfig { cases: CASES, ..ProptestConfig::default() })]

    /// LZ4 frame decoder must not panic on arbitrary input. The frame format
    /// has a magic-number prefix and length-prefixed blocks; both are common
    /// bounds-check-via-panic sites in third-party decoders.
    #[test]
    fn lz4_frame_decoder_no_panic_on_arbitrary_input(
        buf in proptest::collection::vec(any::<u8>(), 0..MAX_INPUT_BYTES),
    ) {
        let decoder = compress::lz4::CountingLz4Decoder::new(&buf[..]);
        let _ = drain_bounded(decoder);
    }

    /// One-shot frame wrapper.
    #[test]
    fn lz4_frame_helper_no_panic_on_arbitrary_input(
        buf in proptest::collection::vec(any::<u8>(), 0..MAX_INPUT_BYTES),
    ) {
        let _ = compress::lz4::decompress_to_vec(&buf);
    }

    /// Raw rsync-wire LZ4 block decoder. The 2-byte header encodes the
    /// compressed length; truncated or oversized lengths must error, not
    /// panic. `max_decompressed_size` is capped at `MAX_OUTPUT_BYTES`.
    #[test]
    fn lz4_raw_decoder_no_panic_on_arbitrary_input(
        buf in proptest::collection::vec(any::<u8>(), 0..MAX_INPUT_BYTES),
    ) {
        let _ = compress::lz4::raw::decompress_block_to_vec(&buf, MAX_OUTPUT_BYTES as usize);
    }
}

// ---------------------------------------------------------------------------
// Deterministic edge cases
//
// Property tests sample a wide distribution but rarely hit the exact
// off-by-one and minimum-length boundaries that bounds-check-via-panic bugs
// live on. These explicit cases pin the contract.
// ---------------------------------------------------------------------------

#[test]
fn zlib_decoder_handles_empty_input() {
    let _ = compress::zlib::decompress_to_vec(&[]);
    let decoder = compress::zlib::CountingZlibDecoder::new(&[][..]);
    let _ = drain_bounded(decoder);
}

#[test]
fn zlib_decoder_handles_single_byte() {
    for byte in 0u8..=255 {
        let _ = compress::zlib::decompress_to_vec(&[byte]);
    }
}

#[test]
fn zlib_decoder_handles_truncated_mid_stream() {
    // Build a valid stream then chop the trailer at every offset.
    let original = b"oc-rsync zlib truncation probe oc-rsync zlib truncation probe";
    let compressed =
        compress::zlib::compress_to_vec(original, compress::zlib::CompressionLevel::Default)
            .expect("compress");
    for cut in 0..compressed.len() {
        let _ = compress::zlib::decompress_to_vec(&compressed[..cut]);
    }
}

#[test]
fn zlib_decoder_handles_decompression_bomb_shape() {
    // 1024 zero bytes compresses to a tiny stream that expands ~1000x. The
    // bounded `take` in `drain_bounded` is what keeps this test safe; the
    // assertion is purely that no panic escapes.
    let bomb_seed = vec![0u8; 1024];
    let bomb = compress::zlib::compress_to_vec(&bomb_seed, compress::zlib::CompressionLevel::Best)
        .expect("compress");
    let decoder = compress::zlib::CountingZlibDecoder::new(&bomb[..]);
    let _ = drain_bounded(decoder);
}

#[cfg(feature = "zstd")]
#[test]
fn zstd_decoder_handles_empty_input() {
    let _ = compress::zstd::decompress_to_vec(&[]);
    let _ = compress::zstd::CountingZstdDecoder::new(&[][..]);
}

#[cfg(feature = "zstd")]
#[test]
fn zstd_decoder_handles_single_byte() {
    for byte in 0u8..=255 {
        let _ = compress::zstd::decompress_to_vec(&[byte]);
    }
}

#[cfg(feature = "zstd")]
#[test]
fn zstd_decoder_handles_truncated_mid_stream() {
    let original = b"oc-rsync zstd truncation probe oc-rsync zstd truncation probe";
    let compressed =
        compress::zstd::compress_to_vec(original, compress::zlib::CompressionLevel::Default)
            .expect("compress");
    for cut in 0..compressed.len() {
        let _ = compress::zstd::decompress_to_vec(&compressed[..cut]);
    }
}

#[cfg(feature = "zstd")]
#[test]
fn zstd_decoder_handles_decompression_bomb_shape() {
    let bomb_seed = vec![0u8; 1024];
    let bomb = compress::zstd::compress_to_vec(&bomb_seed, compress::zlib::CompressionLevel::Best)
        .expect("compress");
    if let Ok(decoder) = compress::zstd::CountingZstdDecoder::new(&bomb[..]) {
        let _ = drain_bounded(decoder);
    }
}

#[cfg(feature = "lz4")]
#[test]
fn lz4_frame_decoder_handles_empty_input() {
    let _ = compress::lz4::decompress_to_vec(&[]);
    let decoder = compress::lz4::CountingLz4Decoder::new(&[][..]);
    let _ = drain_bounded(decoder);
}

#[cfg(feature = "lz4")]
#[test]
fn lz4_frame_decoder_handles_single_byte() {
    for byte in 0u8..=255 {
        let _ = compress::lz4::decompress_to_vec(&[byte]);
    }
}

#[cfg(feature = "lz4")]
#[test]
fn lz4_frame_decoder_handles_truncated_mid_stream() {
    let original = b"oc-rsync lz4 truncation probe oc-rsync lz4 truncation probe";
    let compressed =
        compress::lz4::compress_to_vec(original, compress::zlib::CompressionLevel::Default)
            .expect("compress");
    for cut in 0..compressed.len() {
        let _ = compress::lz4::decompress_to_vec(&compressed[..cut]);
    }
}

#[cfg(feature = "lz4")]
#[test]
fn lz4_raw_decoder_handles_empty_input() {
    let _ = compress::lz4::raw::decompress_block_to_vec(&[], MAX_OUTPUT_BYTES as usize);
}

#[cfg(feature = "lz4")]
#[test]
fn lz4_raw_decoder_handles_single_byte() {
    for byte in 0u8..=255 {
        let _ = compress::lz4::raw::decompress_block_to_vec(&[byte], MAX_OUTPUT_BYTES as usize);
    }
}

#[cfg(feature = "lz4")]
#[test]
fn lz4_raw_decoder_handles_header_only() {
    // 2-byte header alone (length encoded but no body) - boundary right at
    // the input buffer length used by `decompress_block`.
    for first in 0u8..=255 {
        for second in [0u8, 1, 0xFF] {
            let _ = compress::lz4::raw::decompress_block_to_vec(
                &[first, second],
                MAX_OUTPUT_BYTES as usize,
            );
        }
    }
}

#[cfg(feature = "lz4")]
#[test]
fn lz4_raw_decoder_rejects_oversize_max_output() {
    // Documented bound: `max_decompressed_size` greater than the module
    // ceiling returns `DecompressedSizeTooLarge` instead of panicking.
    let err = compress::lz4::raw::decompress_block_to_vec(
        &[0, 0],
        compress::lz4::raw::MAX_DECOMPRESSED_SIZE + 1,
    );
    assert!(err.is_err());
}
