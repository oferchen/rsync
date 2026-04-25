//! Tests that verify zlib-ng backend produces valid, roundtrippable output.
//!
//! These tests only run when the `zlib-ng` feature is enabled. They confirm
//! that the zlib-ng C backend (with SIMD acceleration) produces output that
//! is decompressible and byte-identical to the original input across all
//! compression levels.

#![cfg(feature = "zlib-ng")]

use std::io::{Read, Write};

use compress::zlib::{
    CompressionLevel, CountingZlibDecoder, CountingZlibEncoder, compress_to_vec, decompress_to_vec,
};

#[test]
fn zlib_ng_roundtrip_all_levels() {
    let payload = b"zlib-ng backend validation payload with repeated words words words".repeat(50);

    for level in 0..=9 {
        let compression_level = CompressionLevel::from_numeric(level).expect("valid level");
        let compressed = compress_to_vec(&payload, compression_level)
            .unwrap_or_else(|e| panic!("zlib-ng level {level} compression failed: {e}"));

        assert!(
            !compressed.is_empty(),
            "zlib-ng level {level} produced empty output"
        );

        let decompressed = decompress_to_vec(&compressed)
            .unwrap_or_else(|e| panic!("zlib-ng level {level} decompression failed: {e}"));

        assert_eq!(
            decompressed, payload,
            "zlib-ng level {level} roundtrip integrity failed"
        );
    }
}

#[test]
fn zlib_ng_streaming_roundtrip() {
    let payload = b"streaming zlib-ng test with various chunk sizes".repeat(100);

    let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);
    for chunk in payload.chunks(137) {
        encoder.write_all(chunk).expect("write chunk");
    }
    let (compressed, bytes_written) = encoder.finish_into_inner().expect("finish encoder");

    assert_eq!(
        bytes_written as usize,
        compressed.len(),
        "byte count mismatch"
    );

    let mut decoder = CountingZlibDecoder::new(&compressed[..]);
    let mut decompressed = Vec::new();
    decoder.read_to_end(&mut decompressed).expect("read all");

    assert_eq!(decompressed, payload, "streaming roundtrip failed");
    assert_eq!(decoder.bytes_read(), payload.len() as u64);
}

#[test]
fn zlib_ng_compresses_better_than_stored() {
    let payload = b"highly compressible data with lots of repetition ".repeat(200);

    let level6 = CompressionLevel::from_numeric(6).expect("valid level");
    let compressed = compress_to_vec(&payload, level6).expect("compress");

    assert!(
        compressed.len() < payload.len() / 2,
        "zlib-ng should achieve at least 2x compression on repetitive data, \
         got {} -> {} bytes",
        payload.len(),
        compressed.len()
    );
}

#[test]
fn zlib_ng_handles_incompressible_data() {
    // Pseudo-random data using a simple LCG - low compressibility
    let mut state: u64 = 0xDEAD_BEEF;
    let payload: Vec<u8> = (0..4096)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            (state >> 56) as u8
        })
        .collect();

    let compressed =
        compress_to_vec(&payload, CompressionLevel::Best).expect("compress incompressible");

    let decompressed = decompress_to_vec(&compressed).expect("decompress incompressible");
    assert_eq!(decompressed, payload, "incompressible data roundtrip failed");
}

#[test]
fn zlib_ng_empty_input() {
    let compressed = compress_to_vec(b"", CompressionLevel::Default).expect("compress empty");
    let decompressed = decompress_to_vec(&compressed).expect("decompress empty");
    assert!(decompressed.is_empty());
}
