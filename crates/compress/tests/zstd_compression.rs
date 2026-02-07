//! Comprehensive integration tests for Zstd compression.
//!
//! These tests verify the Zstd compression implementation for the rsync
//! protocol, including:
//! - Compress/decompress roundtrip correctness
//! - Multiple compression levels (0-22)
//! - Streaming compression (chunk by chunk)
//! - Compatibility with standard zstd format
//! - Edge cases (empty data, large data, incompressible data)
//! - Mixed content scenarios

#![cfg(feature = "zstd")]

use compress::zlib::CompressionLevel;
use compress::zstd::{
    CountingZstdDecoder, CountingZstdEncoder, compress_to_vec, decompress_to_vec,
};
use std::io::Read;
use std::num::NonZeroU8;

// ============================================================================
// Test Data
// ============================================================================

const EMPTY_DATA: &[u8] = b"";
const SINGLE_BYTE: &[u8] = b"x";
const SMALL_DATA: &[u8] = b"Hello, World!";
const MEDIUM_DATA: &[u8] = b"The quick brown fox jumps over the lazy dog. \
                              This is a medium-sized test string that should \
                              compress reasonably well with most algorithms.";

fn generate_large_data() -> Vec<u8> {
    (0..100_000).map(|i| (i % 256) as u8).collect()
}

fn generate_highly_compressible_data() -> Vec<u8> {
    let mut data = Vec::new();
    for _ in 0..1000 {
        data.extend_from_slice(b"The same text repeated over and over again. ");
    }
    data
}

fn generate_incompressible_data() -> Vec<u8> {
    // Pseudo-random data that won't compress well
    let mut data = Vec::new();
    let mut state = 0x12345678u32;
    for _ in 0..1024 {
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        data.push((state >> 24) as u8);
    }
    data
}

fn generate_already_compressed_data() -> Vec<u8> {
    // Simulate already-compressed data (e.g., JPEG, PNG, ZIP)
    compress_to_vec(&generate_highly_compressible_data(), CompressionLevel::Best).unwrap()
}

// ============================================================================
// Roundtrip Tests
// ============================================================================

#[test]
fn roundtrip_empty_data() {
    let compressed = compress_to_vec(EMPTY_DATA, CompressionLevel::Default).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, EMPTY_DATA);
}

#[test]
fn roundtrip_single_byte() {
    let compressed = compress_to_vec(SINGLE_BYTE, CompressionLevel::Default).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, SINGLE_BYTE);
}

#[test]
fn roundtrip_small_data() {
    let compressed = compress_to_vec(SMALL_DATA, CompressionLevel::Default).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, SMALL_DATA);
}

#[test]
fn roundtrip_medium_data() {
    let compressed = compress_to_vec(MEDIUM_DATA, CompressionLevel::Default).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, MEDIUM_DATA);
}

#[test]
fn roundtrip_large_data() {
    let data = generate_large_data();
    let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, data);
}

#[test]
fn roundtrip_highly_compressible_data() {
    let data = generate_highly_compressible_data();
    let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, data);

    // Verify we achieved significant compression
    assert!(
        compressed.len() < data.len() / 10,
        "Highly compressible data should compress to < 10% of original size"
    );
}

#[test]
fn roundtrip_incompressible_data() {
    let data = generate_incompressible_data();
    let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, data);
}

#[test]
fn roundtrip_already_compressed_data() {
    let data = generate_already_compressed_data();
    let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, data);
}

// ============================================================================
// Compression Level Tests (0-22)
// ============================================================================

#[test]
fn level_0_no_compression() {
    let data = MEDIUM_DATA;
    let compressed = compress_to_vec(data, CompressionLevel::None).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, data);
}

#[test]
fn level_1_fast_compression() {
    let data = MEDIUM_DATA;
    let level = CompressionLevel::Precise(NonZeroU8::new(1).unwrap());
    let compressed = compress_to_vec(data, level).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, data);
}

#[test]
fn level_3_default_compression() {
    let data = MEDIUM_DATA;
    let compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, data);
}

#[test]
fn level_19_best_compression() {
    let data = MEDIUM_DATA;
    let compressed = compress_to_vec(data, CompressionLevel::Best).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, data);
}

#[test]
fn all_levels_1_to_22_roundtrip() {
    let data = MEDIUM_DATA;

    for level_value in 1..=22 {
        let level = CompressionLevel::Precise(NonZeroU8::new(level_value).unwrap());
        let compressed = compress_to_vec(data, level)
            .unwrap_or_else(|e| panic!("Compression failed at level {level_value}: {e}"));
        let decompressed = decompress_to_vec(&compressed)
            .unwrap_or_else(|e| panic!("Decompression failed at level {level_value}: {e}"));
        assert_eq!(
            decompressed, data,
            "Roundtrip failed at level {level_value}"
        );
    }
}

#[test]
fn preset_levels_roundtrip() {
    let data = MEDIUM_DATA;

    let levels = [
        (CompressionLevel::None, "None"),
        (CompressionLevel::Fast, "Fast"),
        (CompressionLevel::Default, "Default"),
        (CompressionLevel::Best, "Best"),
    ];

    for (level, name) in levels {
        let compressed = compress_to_vec(data, level)
            .unwrap_or_else(|e| panic!("Compression failed at level {name}: {e}"));
        let decompressed = decompress_to_vec(&compressed)
            .unwrap_or_else(|e| panic!("Decompression failed at level {name}: {e}"));
        assert_eq!(decompressed, data, "Roundtrip failed at level {name}");
    }
}

#[test]
fn higher_levels_produce_smaller_output() {
    let data = generate_highly_compressible_data();

    let level_1 = CompressionLevel::Precise(NonZeroU8::new(1).unwrap());
    let level_5 = CompressionLevel::Precise(NonZeroU8::new(5).unwrap());
    let level_10 = CompressionLevel::Precise(NonZeroU8::new(10).unwrap());
    let level_15 = CompressionLevel::Precise(NonZeroU8::new(15).unwrap());
    let level_19 = CompressionLevel::Best;

    let compressed_1 = compress_to_vec(&data, level_1).unwrap();
    let compressed_5 = compress_to_vec(&data, level_5).unwrap();
    let compressed_10 = compress_to_vec(&data, level_10).unwrap();
    let compressed_15 = compress_to_vec(&data, level_15).unwrap();
    let compressed_19 = compress_to_vec(&data, level_19).unwrap();

    // Verify compression improves with level for highly compressible data
    assert!(
        compressed_5.len() <= compressed_1.len(),
        "Level 5 should compress better than level 1"
    );
    assert!(
        compressed_10.len() <= compressed_5.len(),
        "Level 10 should compress better than level 5"
    );
    assert!(
        compressed_15.len() <= compressed_10.len(),
        "Level 15 should compress better than level 10"
    );
    assert!(
        compressed_19.len() <= compressed_15.len(),
        "Level 19 should compress better than level 15"
    );

    // Verify all decompress correctly
    assert_eq!(decompress_to_vec(&compressed_1).unwrap(), data);
    assert_eq!(decompress_to_vec(&compressed_5).unwrap(), data);
    assert_eq!(decompress_to_vec(&compressed_10).unwrap(), data);
    assert_eq!(decompress_to_vec(&compressed_15).unwrap(), data);
    assert_eq!(decompress_to_vec(&compressed_19).unwrap(), data);
}

#[test]
fn compression_ratio_at_different_levels() {
    let data = generate_highly_compressible_data();

    let fast = compress_to_vec(&data, CompressionLevel::Fast).unwrap();
    let default = compress_to_vec(&data, CompressionLevel::Default).unwrap();
    let best = compress_to_vec(&data, CompressionLevel::Best).unwrap();

    // All should achieve significant compression
    assert!(fast.len() < data.len() / 5, "Fast should compress to < 20%");
    assert!(
        default.len() < data.len() / 5,
        "Default should compress to < 20%"
    );
    assert!(best.len() < data.len() / 5, "Best should compress to < 20%");

    // Best should be smallest or equal
    assert!(best.len() <= default.len());
    assert!(best.len() <= fast.len());
}

// ============================================================================
// Streaming Compression Tests
// ============================================================================

#[test]
fn streaming_encoder_single_write() {
    let data = MEDIUM_DATA;
    let mut encoder = CountingZstdEncoder::new(CompressionLevel::Default).unwrap();
    encoder.write(data).unwrap();
    let compressed_bytes = encoder.finish().unwrap();
    assert!(compressed_bytes > 0);
}

#[test]
fn streaming_encoder_multiple_writes() {
    let data1 = b"First chunk of data. ";
    let data2 = b"Second chunk of data. ";
    let data3 = b"Third chunk of data.";

    let mut encoder = CountingZstdEncoder::new(CompressionLevel::Default).unwrap();
    encoder.write(data1).unwrap();
    encoder.write(data2).unwrap();
    encoder.write(data3).unwrap();
    let compressed_bytes = encoder.finish().unwrap();
    assert!(compressed_bytes > 0);
}

#[test]
fn streaming_encoder_with_sink_roundtrip() {
    let data = MEDIUM_DATA;
    let mut output = Vec::new();

    let mut encoder =
        CountingZstdEncoder::with_sink(&mut output, CompressionLevel::Default).unwrap();
    encoder.write(data).unwrap();
    let (returned_output, bytes_written) = encoder.finish_into_inner().unwrap();

    assert_eq!(bytes_written as usize, returned_output.len());

    let decompressed = decompress_to_vec(returned_output).unwrap();
    assert_eq!(decompressed, data);
}

#[test]
fn streaming_encoder_chunk_by_chunk() {
    let data = generate_large_data();
    let chunk_size = 4096;
    let mut output = Vec::new();

    let mut encoder =
        CountingZstdEncoder::with_sink(&mut output, CompressionLevel::Default).unwrap();

    for chunk in data.chunks(chunk_size) {
        encoder.write(chunk).unwrap();
    }

    let (compressed, _) = encoder.finish_into_inner().unwrap();
    let decompressed = decompress_to_vec(compressed).unwrap();
    assert_eq!(decompressed, data);
}

#[test]
fn streaming_decoder_read() {
    let data = MEDIUM_DATA;
    let compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();

    let mut decoder = CountingZstdDecoder::new(&compressed[..]).unwrap();
    let mut decompressed = Vec::new();
    decoder.read_to_end(&mut decompressed).unwrap();

    assert_eq!(decompressed, data);
    assert_eq!(decoder.bytes_read(), data.len() as u64);
}

#[test]
fn streaming_decoder_chunked_read() {
    let data = generate_large_data();
    let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();

    let mut decoder = CountingZstdDecoder::new(&compressed[..]).unwrap();
    let mut decompressed = Vec::new();
    let mut buffer = [0u8; 1024];

    loop {
        let bytes_read = decoder.read(&mut buffer).unwrap();
        if bytes_read == 0 {
            break;
        }
        decompressed.extend_from_slice(&buffer[..bytes_read]);
    }

    assert_eq!(decompressed, data);
    assert_eq!(decoder.bytes_read(), data.len() as u64);
}

#[test]
fn streaming_encoder_bytes_written_tracking() {
    let data = MEDIUM_DATA;
    let mut output = Vec::new();
    let mut encoder =
        CountingZstdEncoder::with_sink(&mut output, CompressionLevel::Default).unwrap();

    assert_eq!(encoder.bytes_written(), 0);
    encoder.write(&data[..10]).unwrap();
    encoder.write(&data[10..]).unwrap();

    let (_, bytes_written) = encoder.finish_into_inner().unwrap();
    // After finish, we should have written compressed bytes
    assert!(bytes_written > 0);
    assert_eq!(bytes_written as usize, output.len());
}

// ============================================================================
// Standard Format Compatibility Tests
// ============================================================================

#[test]
fn zstd_format_magic_bytes() {
    let data = MEDIUM_DATA;
    let compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();

    // Zstd format magic number: 0x28, 0xB5, 0x2F, 0xFD
    assert!(compressed.len() >= 4);
    assert_eq!(compressed[0], 0x28);
    assert_eq!(compressed[1], 0xB5);
    assert_eq!(compressed[2], 0x2F);
    assert_eq!(compressed[3], 0xFD);
}

#[test]
fn deterministic_compression() {
    let data = MEDIUM_DATA;
    let compressed1 = compress_to_vec(data, CompressionLevel::Default).unwrap();
    let compressed2 = compress_to_vec(data, CompressionLevel::Default).unwrap();

    // Zstd compression should be deterministic
    assert_eq!(compressed1, compressed2);
}

#[test]
fn different_data_produces_different_output() {
    let data1 = b"First set of data";
    let data2 = b"Second set of data";

    let compressed1 = compress_to_vec(data1, CompressionLevel::Default).unwrap();
    let compressed2 = compress_to_vec(data2, CompressionLevel::Default).unwrap();

    assert_ne!(compressed1, compressed2);
}

// ============================================================================
// Edge Cases
// ============================================================================

#[test]
fn empty_data_multiple_levels() {
    for level_value in [0, 1, 5, 10, 19, 22] {
        let level = if level_value == 0 {
            CompressionLevel::None
        } else {
            CompressionLevel::Precise(NonZeroU8::new(level_value).unwrap())
        };

        let compressed = compress_to_vec(EMPTY_DATA, level)
            .unwrap_or_else(|e| panic!("Compression failed at level {level_value}: {e}"));
        let decompressed = decompress_to_vec(&compressed)
            .unwrap_or_else(|e| panic!("Decompression failed at level {level_value}: {e}"));
        assert_eq!(
            decompressed, EMPTY_DATA,
            "Empty data roundtrip failed at level {level_value}"
        );
    }
}

#[test]
fn single_byte_all_values() {
    // Test all possible byte values
    for byte_value in 0u8..=255 {
        let data = [byte_value];
        let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();
        let decompressed = decompress_to_vec(&compressed).unwrap();
        assert_eq!(decompressed, data, "Failed for byte value {byte_value}");
    }
}

#[test]
fn very_large_data() {
    let size = 10_000_000; // 10 MB
    let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();

    let compressed = compress_to_vec(&data, CompressionLevel::Fast).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();

    assert_eq!(decompressed.len(), data.len());
    assert_eq!(decompressed, data);
}

#[test]
fn repeated_single_byte() {
    let data = vec![b'A'; 100_000];
    let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();

    assert_eq!(decompressed, data);
    // Should achieve excellent compression ratio
    assert!(
        compressed.len() < 1000,
        "Repeated byte should compress extremely well"
    );
}

#[test]
fn alternating_pattern() {
    let data: Vec<u8> = (0..10_000)
        .map(|i| if i % 2 == 0 { 0xAA } else { 0x55 })
        .collect();
    let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, data);
}

#[test]
fn null_bytes() {
    let data = vec![0u8; 10_000];
    let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();

    assert_eq!(decompressed, data);
    // Null bytes should compress extremely well
    assert!(
        compressed.len() < 100,
        "Null bytes should compress to < 100 bytes"
    );
}

#[test]
fn unicode_text() {
    let data = "Hello, 世界! Привет мир! مرحبا بالعالم! 🌍🌎🌏"
        .repeat(100)
        .into_bytes();
    let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, data);
}

// ============================================================================
// Mixed Content Scenarios
// ============================================================================

#[test]
fn mixed_text_and_binary() {
    let mut data = Vec::new();
    data.extend_from_slice(b"Text header: ");
    data.extend_from_slice(&[0xFF, 0xFE, 0xFD, 0xFC]);
    data.extend_from_slice(b" More text ");
    data.extend_from_slice(&generate_incompressible_data()[..100]);

    let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, data);
}

#[test]
fn nested_compression() {
    // Compress data, then compress the compressed data again
    let original = MEDIUM_DATA;

    let compressed_once = compress_to_vec(original, CompressionLevel::Default).unwrap();
    let compressed_twice = compress_to_vec(&compressed_once, CompressionLevel::Default).unwrap();

    // Decompress in reverse order
    let decompressed_once = decompress_to_vec(&compressed_twice).unwrap();
    let decompressed_twice = decompress_to_vec(&decompressed_once).unwrap();

    assert_eq!(decompressed_twice, original);
}

#[test]
fn multiple_independent_compressions() {
    let data1 = b"First independent data stream";
    let data2 = b"Second independent data stream";
    let data3 = b"Third independent data stream";

    let compressed1 = compress_to_vec(data1, CompressionLevel::Default).unwrap();
    let compressed2 = compress_to_vec(data2, CompressionLevel::Default).unwrap();
    let compressed3 = compress_to_vec(data3, CompressionLevel::Default).unwrap();

    // Decompress in different order
    assert_eq!(decompress_to_vec(&compressed3).unwrap(), data3);
    assert_eq!(decompress_to_vec(&compressed1).unwrap(), data1);
    assert_eq!(decompress_to_vec(&compressed2).unwrap(), data2);
}

// ============================================================================
// Error Handling Tests
// ============================================================================

#[test]
fn decompress_invalid_data_returns_error() {
    let invalid_data = b"This is not compressed data";
    let result = decompress_to_vec(invalid_data);
    assert!(result.is_err());
}

#[test]
fn decompress_truncated_data_returns_error() {
    let data = MEDIUM_DATA;
    let compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();

    // Truncate the compressed data
    let truncated = &compressed[..compressed.len() - 10];
    let result = decompress_to_vec(truncated);
    assert!(result.is_err());
}

#[test]
fn decompress_corrupted_magic_bytes_returns_error() {
    let data = MEDIUM_DATA;
    let mut compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();

    // Corrupt the magic bytes
    compressed[0] = 0x00;
    let result = decompress_to_vec(&compressed);
    assert!(result.is_err());
}

// ============================================================================
// Performance Characteristics Tests
// ============================================================================

#[test]
fn fast_level_is_faster_than_best() {
    let data = generate_large_data();

    // This test just verifies both levels work - actual performance testing
    // would require benchmarks, but we can verify the compression succeeds
    let fast = compress_to_vec(&data, CompressionLevel::Fast).unwrap();
    let best = compress_to_vec(&data, CompressionLevel::Best).unwrap();

    // Both should produce valid output
    assert_eq!(decompress_to_vec(&fast).unwrap(), data);
    assert_eq!(decompress_to_vec(&best).unwrap(), data);
}

#[test]
fn compression_overhead_for_small_data() {
    let data = b"tiny";
    let compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();

    // Small data will expand due to compression overhead
    // Zstd header and frame overhead is relatively small
    assert!(
        compressed.len() < 50,
        "Compression overhead should be reasonable"
    );
}

// ============================================================================
// Compatibility with rsync protocol
// ============================================================================

#[test]
fn rsync_protocol_36_default_level() {
    // rsync protocol 36 uses zstd with default compression
    let data = MEDIUM_DATA;
    let compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, data);
}

#[test]
fn rsync_typical_file_chunk_size() {
    // rsync typically transfers files in chunks
    let chunk_size = 8192; // Common rsync chunk size
    let data: Vec<u8> = (0..chunk_size).map(|i| (i % 256) as u8).collect();

    let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, data);
}

#[test]
fn rsync_multiple_chunks_independent_compression() {
    // Simulate rsync compressing multiple file chunks independently
    let chunk_size = 4096;
    let num_chunks = 10;

    for chunk_num in 0..num_chunks {
        let data: Vec<u8> = (0..chunk_size)
            .map(|i| ((i + chunk_num * chunk_size) % 256) as u8)
            .collect();

        let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();
        let decompressed = decompress_to_vec(&compressed).unwrap();
        assert_eq!(decompressed, data, "Failed for chunk {chunk_num}");
    }
}
