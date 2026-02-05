//! Comprehensive tests for compression level 0 (no compression) across all algorithms.
//!
//! This test module verifies that compression level 0 correctly:
//! 1. Stores data without compression (may include framing overhead)
//! 2. Data is fully retrievable via decompression
//! 3. Works consistently across all compression algorithms (zlib, lz4, zstd)
//! 4. Round-trip integrity is preserved for all data types
//! 5. Produces larger or equal output compared to compressed levels (for compressible data)

use std::io::{Cursor, Read};

use compress::zlib::{
    CompressionLevel, CountingZlibDecoder, CountingZlibEncoder, compress_to_vec as zlib_compress,
    decompress_to_vec as zlib_decompress,
};

#[cfg(feature = "lz4")]
use compress::lz4::frame::{
    CountingLz4Decoder, CountingLz4Encoder, compress_to_vec as lz4_compress,
    decompress_to_vec as lz4_decompress,
};

#[cfg(feature = "zstd")]
use compress::zstd::{
    CountingZstdDecoder, CountingZstdEncoder, compress_to_vec as zstd_compress,
    decompress_to_vec as zstd_decompress,
};

/// Test data generators for different content types.
mod test_data {
    /// Generates highly compressible repetitive text data.
    pub fn repetitive_text(size: usize) -> Vec<u8> {
        let pattern = b"The quick brown fox jumps over the lazy dog. ";
        pattern.iter().cycle().take(size).copied().collect()
    }

    /// Generates moderately compressible English-like text.
    pub fn english_text(size: usize) -> Vec<u8> {
        let text = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
            Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. ";
        text.iter().cycle().take(size).copied().collect()
    }

    /// Generates structured binary data (simulating file headers, records).
    pub fn structured_binary(size: usize) -> Vec<u8> {
        let mut data = Vec::with_capacity(size);
        let mut counter: u32 = 0;
        while data.len() < size {
            let record_type = (counter % 5) as u8;
            let record_len = 16 + (counter % 32) as usize;
            data.extend_from_slice(&(record_len as u32).to_le_bytes());
            data.push(record_type);
            for i in 0..record_len.min(size.saturating_sub(data.len())) {
                data.push((i as u8).wrapping_add(record_type));
            }
            counter += 1;
        }
        data.truncate(size);
        data
    }

    /// Generates pseudo-random data (low compressibility).
    pub fn random_data(size: usize, seed: u64) -> Vec<u8> {
        let mut state = seed;
        let mut data = Vec::with_capacity(size);
        for _ in 0..size {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            data.push((state >> 56) as u8);
        }
        data
    }

    /// Generates data with runs of zeros (simulating sparse data).
    pub fn sparse_data(size: usize) -> Vec<u8> {
        let mut data = Vec::with_capacity(size);
        let mut i = 0;
        while data.len() < size {
            let data_len = 10 + (i % 20);
            let zero_len = 50 + (i % 100);
            for j in 0..data_len.min(size - data.len()) {
                data.push((j + i) as u8);
            }
            data.extend(std::iter::repeat_n(
                0,
                zero_len.min(size.saturating_sub(data.len())),
            ));
            i += 1;
        }
        data.truncate(size);
        data
    }
}

// =============================================================================
// SECTION 1: Basic Level 0 Functionality Tests (Zlib)
// =============================================================================

#[test]
fn zlib_level_zero_basic_round_trip() {
    let data = test_data::english_text(10_000);
    let compressed = zlib_compress(&data, CompressionLevel::None).expect("compress level 0");

    assert!(
        !compressed.is_empty(),
        "level 0 should produce non-empty output"
    );

    let decompressed = zlib_decompress(&compressed).expect("decompress level 0");
    assert_eq!(decompressed, data, "level 0 round-trip failed");
}

#[test]
fn zlib_level_zero_from_numeric() {
    let level = CompressionLevel::from_numeric(0).expect("level 0 should be valid");
    assert_eq!(
        level,
        CompressionLevel::None,
        "level 0 should map to None variant"
    );
}

#[test]
fn zlib_level_zero_empty_data() {
    let data: &[u8] = &[];
    let compressed =
        zlib_compress(data, CompressionLevel::None).expect("compress empty with level 0");
    let decompressed = zlib_decompress(&compressed).expect("decompress empty with level 0");
    assert!(decompressed.is_empty(), "empty data should round-trip");
}

#[test]
fn zlib_level_zero_single_byte() {
    let data: &[u8] = &[42];
    let compressed =
        zlib_compress(data, CompressionLevel::None).expect("compress single byte with level 0");
    let decompressed = zlib_decompress(&compressed).expect("decompress single byte with level 0");
    assert_eq!(decompressed, data, "single byte should round-trip");
}

#[test]
fn zlib_level_zero_all_byte_values() {
    let data: Vec<u8> = (0..=255).collect();
    let compressed =
        zlib_compress(&data, CompressionLevel::None).expect("compress all bytes with level 0");
    let decompressed = zlib_decompress(&compressed).expect("decompress all bytes with level 0");
    assert_eq!(decompressed, data, "all byte values should round-trip");
}

// =============================================================================
// SECTION 2: Data Type Coverage (Zlib)
// =============================================================================

#[test]
fn zlib_level_zero_various_data_types() {
    let test_cases = [
        ("repetitive_text", test_data::repetitive_text(5000)),
        ("english_text", test_data::english_text(5000)),
        ("structured_binary", test_data::structured_binary(5000)),
        ("random_data", test_data::random_data(5000, 12345)),
        ("sparse_data", test_data::sparse_data(5000)),
    ];

    for (name, data) in &test_cases {
        let compressed = zlib_compress(data, CompressionLevel::None)
            .unwrap_or_else(|e| panic!("{name}: level 0 compression failed: {e}"));

        let decompressed = zlib_decompress(&compressed)
            .unwrap_or_else(|e| panic!("{name}: level 0 decompression failed: {e}"));

        assert_eq!(
            decompressed, *data,
            "{name}: level 0 round-trip integrity failed"
        );
    }
}

#[test]
fn zlib_level_zero_boundary_sizes() {
    for size in [1, 2, 255, 256, 1023, 1024, 4095, 4096, 8192, 16384] {
        let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let compressed = zlib_compress(&data, CompressionLevel::None)
            .unwrap_or_else(|e| panic!("size {size}: level 0 compression failed: {e}"));
        let decompressed = zlib_decompress(&compressed)
            .unwrap_or_else(|e| panic!("size {size}: level 0 decompression failed: {e}"));
        assert_eq!(decompressed, data, "size {size}: level 0 round-trip failed");
    }
}

// =============================================================================
// SECTION 3: Streaming Encoder/Decoder Tests (Zlib)
// =============================================================================

#[test]
fn zlib_level_zero_streaming_encoder() {
    let data = test_data::english_text(10_000);
    let chunks: Vec<&[u8]> = data.chunks(1000).collect();

    let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::None);

    for chunk in &chunks {
        encoder.write(chunk).expect("write chunk with level 0");
    }

    let (compressed, bytes_written) = encoder.finish_into_inner().expect("finish level 0 encoder");

    assert!(bytes_written > 0, "level 0 should write bytes");
    assert_eq!(
        bytes_written as usize,
        compressed.len(),
        "level 0 byte count mismatch"
    );

    let decompressed = zlib_decompress(&compressed).expect("decompress level 0 stream");
    assert_eq!(decompressed, data, "level 0 streaming round-trip failed");
}

#[test]
fn zlib_level_zero_streaming_encoder_chunked_writes() {
    let data = test_data::repetitive_text(5000);
    let chunk_sizes = [1, 7, 13, 64, 256, 1024];

    for chunk_size in chunk_sizes {
        let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::None);

        for chunk in data.chunks(chunk_size) {
            encoder.write(chunk).expect("write chunk");
        }

        let (compressed, bytes) = encoder.finish_into_inner().expect("finish encoder");
        assert_eq!(bytes as usize, compressed.len());

        let decompressed = zlib_decompress(&compressed).expect("decompress");
        assert_eq!(
            decompressed, data,
            "chunk size {chunk_size}: level 0 round-trip failed"
        );
    }
}

#[test]
fn zlib_level_zero_streaming_decoder() {
    let data = test_data::english_text(10_000);
    let compressed = zlib_compress(&data, CompressionLevel::None).expect("compress with level 0");

    let mut decoder = CountingZlibDecoder::new(Cursor::new(&compressed));
    let mut output = Vec::new();
    decoder
        .read_to_end(&mut output)
        .expect("read all from level 0 decoder");

    assert_eq!(
        decoder.bytes_read(),
        data.len() as u64,
        "level 0 decoder byte count mismatch"
    );
    assert_eq!(output, data, "level 0 streaming decode failed");
}

#[test]
fn zlib_level_zero_streaming_decoder_chunked_reads() {
    let data = test_data::repetitive_text(5000);
    let compressed = zlib_compress(&data, CompressionLevel::None).expect("compress with level 0");

    let mut decoder = CountingZlibDecoder::new(Cursor::new(&compressed));
    let mut output = Vec::new();
    let mut buf = [0u8; 64];

    loop {
        let n = decoder.read(&mut buf).expect("read chunk");
        if n == 0 {
            break;
        }
        output.extend_from_slice(&buf[..n]);
    }

    assert_eq!(output, data, "level 0 chunked read failed");
    assert_eq!(decoder.bytes_read(), data.len() as u64);
}

// =============================================================================
// SECTION 4: Comparison with Higher Compression Levels (Zlib)
// =============================================================================

#[test]
fn zlib_level_zero_produces_larger_output_for_compressible_data() {
    let data = test_data::repetitive_text(10_000);

    let level_0_compressed =
        zlib_compress(&data, CompressionLevel::None).expect("level 0 compress");
    let level_1_compressed =
        zlib_compress(&data, CompressionLevel::from_numeric(1).unwrap()).expect("level 1 compress");
    let level_9_compressed =
        zlib_compress(&data, CompressionLevel::from_numeric(9).unwrap()).expect("level 9 compress");

    // Level 0 should produce larger output than compressed levels for compressible data
    assert!(
        level_0_compressed.len() > level_1_compressed.len(),
        "level 0 ({}) should produce larger output than level 1 ({}) for compressible data",
        level_0_compressed.len(),
        level_1_compressed.len()
    );

    assert!(
        level_0_compressed.len() > level_9_compressed.len(),
        "level 0 ({}) should produce larger output than level 9 ({}) for compressible data",
        level_0_compressed.len(),
        level_9_compressed.len()
    );

    // Verify all round-trip correctly
    assert_eq!(
        zlib_decompress(&level_0_compressed).unwrap(),
        data,
        "level 0 round-trip failed"
    );
    assert_eq!(
        zlib_decompress(&level_1_compressed).unwrap(),
        data,
        "level 1 round-trip failed"
    );
    assert_eq!(
        zlib_decompress(&level_9_compressed).unwrap(),
        data,
        "level 9 round-trip failed"
    );
}

#[test]
fn zlib_level_zero_size_relationship_with_random_data() {
    // Random data doesn't compress well, so level 0 might have similar size to compressed
    let data = test_data::random_data(10_000, 54321);

    let level_0_compressed =
        zlib_compress(&data, CompressionLevel::None).expect("level 0 compress");
    let level_9_compressed =
        zlib_compress(&data, CompressionLevel::from_numeric(9).unwrap()).expect("level 9 compress");

    // For random data, level 0 may be larger or similar to level 9
    // We just verify both work correctly
    assert_eq!(
        zlib_decompress(&level_0_compressed).unwrap(),
        data,
        "level 0 with random data failed"
    );
    assert_eq!(
        zlib_decompress(&level_9_compressed).unwrap(),
        data,
        "level 9 with random data failed"
    );
}

#[test]
fn zlib_level_zero_may_inflate_small_data() {
    // Small data is likely to inflate due to deflate framing overhead
    let data = b"tiny";

    let compressed =
        zlib_compress(data, CompressionLevel::None).expect("level 0 compression works");

    // Level 0 adds deflate framing without compression, so small data typically inflates
    // We verify it round-trips correctly regardless of size relationship
    let decompressed = zlib_decompress(&compressed).expect("level 0 decompression works");
    assert_eq!(
        decompressed.as_slice(),
        data,
        "level 0 tiny data round-trip failed"
    );
}

// =============================================================================
// SECTION 5: LZ4 Level 0 Tests
// =============================================================================

#[cfg(feature = "lz4")]
#[test]
fn lz4_level_zero_basic_round_trip() {
    let data = test_data::english_text(10_000);
    let compressed = lz4_compress(&data, CompressionLevel::None).expect("lz4 compress level 0");

    assert!(
        !compressed.is_empty(),
        "lz4 level 0 should produce non-empty output"
    );

    let decompressed = lz4_decompress(&compressed).expect("lz4 decompress level 0");
    assert_eq!(decompressed, data, "lz4 level 0 round-trip failed");
}

#[cfg(feature = "lz4")]
#[test]
fn lz4_level_zero_various_data_types() {
    let test_cases = [
        ("repetitive_text", test_data::repetitive_text(5000)),
        ("english_text", test_data::english_text(5000)),
        ("structured_binary", test_data::structured_binary(5000)),
        ("random_data", test_data::random_data(5000, 12345)),
        ("sparse_data", test_data::sparse_data(5000)),
    ];

    for (name, data) in &test_cases {
        let compressed = lz4_compress(data, CompressionLevel::None)
            .unwrap_or_else(|e| panic!("{name}: lz4 level 0 compression failed: {e}"));

        let decompressed = lz4_decompress(&compressed)
            .unwrap_or_else(|e| panic!("{name}: lz4 level 0 decompression failed: {e}"));

        assert_eq!(
            decompressed, *data,
            "{name}: lz4 level 0 round-trip integrity failed"
        );
    }
}

#[cfg(feature = "lz4")]
#[test]
fn lz4_level_zero_empty_data() {
    let data: &[u8] = &[];
    let compressed =
        lz4_compress(data, CompressionLevel::None).expect("lz4 compress empty with level 0");
    let decompressed = lz4_decompress(&compressed).expect("lz4 decompress empty with level 0");
    assert!(decompressed.is_empty(), "lz4 empty data should round-trip");
}

#[cfg(feature = "lz4")]
#[test]
fn lz4_level_zero_single_byte() {
    let data: &[u8] = &[42];
    let compressed =
        lz4_compress(data, CompressionLevel::None).expect("lz4 compress single byte with level 0");
    let decompressed =
        lz4_decompress(&compressed).expect("lz4 decompress single byte with level 0");
    assert_eq!(decompressed, data, "lz4 single byte should round-trip");
}

#[cfg(feature = "lz4")]
#[test]
fn lz4_level_zero_streaming_encoder() {
    let data = test_data::english_text(10_000);
    let chunks: Vec<&[u8]> = data.chunks(1000).collect();

    let mut encoder = CountingLz4Encoder::with_sink(Vec::new(), CompressionLevel::None);

    for chunk in &chunks {
        encoder.write(chunk).expect("lz4 write chunk with level 0");
    }

    let (compressed, bytes_written) = encoder
        .finish_into_inner()
        .expect("lz4 finish level 0 encoder");

    assert!(bytes_written > 0, "lz4 level 0 should write bytes");
    assert_eq!(
        bytes_written as usize,
        compressed.len(),
        "lz4 level 0 byte count mismatch"
    );

    let decompressed = lz4_decompress(&compressed).expect("lz4 decompress level 0 stream");
    assert_eq!(
        decompressed, data,
        "lz4 level 0 streaming round-trip failed"
    );
}

#[cfg(feature = "lz4")]
#[test]
fn lz4_level_zero_streaming_decoder() {
    let data = test_data::english_text(10_000);
    let compressed =
        lz4_compress(&data, CompressionLevel::None).expect("lz4 compress with level 0");

    let mut decoder = CountingLz4Decoder::new(Cursor::new(&compressed));
    let mut output = Vec::new();
    decoder
        .read_to_end(&mut output)
        .expect("lz4 read all from level 0 decoder");

    assert_eq!(
        decoder.bytes_read(),
        data.len() as u64,
        "lz4 level 0 decoder byte count mismatch"
    );
    assert_eq!(output, data, "lz4 level 0 streaming decode failed");
}

#[cfg(feature = "lz4")]
#[test]
fn lz4_level_zero_produces_larger_output_for_compressible_data() {
    let data = test_data::repetitive_text(10_000);

    let level_0_compressed =
        lz4_compress(&data, CompressionLevel::None).expect("lz4 level 0 compress");
    let level_default_compressed =
        lz4_compress(&data, CompressionLevel::Default).expect("lz4 default compress");

    // LZ4 level 0 may still apply some compression, so we just verify it works
    // The actual compression ratio depends on the LZ4 implementation
    assert!(
        level_0_compressed.len() >= level_default_compressed.len(),
        "lz4 level 0 ({}) should produce equal or larger output than default ({}) for compressible data",
        level_0_compressed.len(),
        level_default_compressed.len()
    );

    // Verify both round-trip correctly
    assert_eq!(
        lz4_decompress(&level_0_compressed).unwrap(),
        data,
        "lz4 level 0 round-trip failed"
    );
    assert_eq!(
        lz4_decompress(&level_default_compressed).unwrap(),
        data,
        "lz4 default round-trip failed"
    );
}

// =============================================================================
// SECTION 6: Zstd Level 0 Tests
// =============================================================================

#[cfg(feature = "zstd")]
#[test]
fn zstd_level_zero_basic_round_trip() {
    let data = test_data::english_text(10_000);
    let compressed = zstd_compress(&data, CompressionLevel::None).expect("zstd compress level 0");

    assert!(
        !compressed.is_empty(),
        "zstd level 0 should produce non-empty output"
    );

    let decompressed = zstd_decompress(&compressed).expect("zstd decompress level 0");
    assert_eq!(decompressed, data, "zstd level 0 round-trip failed");
}

#[cfg(feature = "zstd")]
#[test]
fn zstd_level_zero_various_data_types() {
    let test_cases = [
        ("repetitive_text", test_data::repetitive_text(5000)),
        ("english_text", test_data::english_text(5000)),
        ("structured_binary", test_data::structured_binary(5000)),
        ("random_data", test_data::random_data(5000, 12345)),
        ("sparse_data", test_data::sparse_data(5000)),
    ];

    for (name, data) in &test_cases {
        let compressed = zstd_compress(data, CompressionLevel::None)
            .unwrap_or_else(|e| panic!("{name}: zstd level 0 compression failed: {e}"));

        let decompressed = zstd_decompress(&compressed)
            .unwrap_or_else(|e| panic!("{name}: zstd level 0 decompression failed: {e}"));

        assert_eq!(
            decompressed, *data,
            "{name}: zstd level 0 round-trip integrity failed"
        );
    }
}

#[cfg(feature = "zstd")]
#[test]
fn zstd_level_zero_empty_data() {
    let data: &[u8] = &[];
    let compressed =
        zstd_compress(data, CompressionLevel::None).expect("zstd compress empty with level 0");
    let decompressed = zstd_decompress(&compressed).expect("zstd decompress empty with level 0");
    assert!(decompressed.is_empty(), "zstd empty data should round-trip");
}

#[cfg(feature = "zstd")]
#[test]
fn zstd_level_zero_single_byte() {
    let data: &[u8] = &[42];
    let compressed = zstd_compress(data, CompressionLevel::None)
        .expect("zstd compress single byte with level 0");
    let decompressed =
        zstd_decompress(&compressed).expect("zstd decompress single byte with level 0");
    assert_eq!(decompressed, data, "zstd single byte should round-trip");
}

#[cfg(feature = "zstd")]
#[test]
fn zstd_level_zero_streaming_encoder() {
    let data = test_data::english_text(10_000);
    let chunks: Vec<&[u8]> = data.chunks(1000).collect();

    let mut encoder = CountingZstdEncoder::with_sink(Vec::new(), CompressionLevel::None)
        .expect("zstd encoder with level 0");

    for chunk in &chunks {
        encoder.write(chunk).expect("zstd write chunk with level 0");
    }

    let (compressed, bytes_written) = encoder
        .finish_into_inner()
        .expect("zstd finish level 0 encoder");

    assert!(bytes_written > 0, "zstd level 0 should write bytes");
    assert_eq!(
        bytes_written as usize,
        compressed.len(),
        "zstd level 0 byte count mismatch"
    );

    let decompressed = zstd_decompress(&compressed).expect("zstd decompress level 0 stream");
    assert_eq!(
        decompressed, data,
        "zstd level 0 streaming round-trip failed"
    );
}

#[cfg(feature = "zstd")]
#[test]
fn zstd_level_zero_streaming_decoder() {
    let data = test_data::english_text(10_000);
    let compressed =
        zstd_compress(&data, CompressionLevel::None).expect("zstd compress with level 0");

    let mut decoder = CountingZstdDecoder::new(Cursor::new(&compressed)).expect("zstd decoder");
    let mut output = Vec::new();
    decoder
        .read_to_end(&mut output)
        .expect("zstd read all from level 0 decoder");

    assert_eq!(
        decoder.bytes_read(),
        data.len() as u64,
        "zstd level 0 decoder byte count mismatch"
    );
    assert_eq!(output, data, "zstd level 0 streaming decode failed");
}

#[cfg(feature = "zstd")]
#[test]
fn zstd_level_zero_produces_larger_output_for_compressible_data() {
    let data = test_data::repetitive_text(10_000);

    let level_0_compressed =
        zstd_compress(&data, CompressionLevel::None).expect("zstd level 0 compress");
    let level_default_compressed =
        zstd_compress(&data, CompressionLevel::Default).expect("zstd default compress");

    // Zstd level 0 may still apply some compression, so we just verify it works
    // The actual compression ratio depends on the zstd implementation
    assert!(
        level_0_compressed.len() >= level_default_compressed.len(),
        "zstd level 0 ({}) should produce equal or larger output than default ({}) for compressible data",
        level_0_compressed.len(),
        level_default_compressed.len()
    );

    // Verify both round-trip correctly
    assert_eq!(
        zstd_decompress(&level_0_compressed).unwrap(),
        data,
        "zstd level 0 round-trip failed"
    );
    assert_eq!(
        zstd_decompress(&level_default_compressed).unwrap(),
        data,
        "zstd default round-trip failed"
    );
}

#[cfg(feature = "zstd")]
#[test]
fn zstd_level_zero_boundary_sizes() {
    for size in [1, 2, 255, 256, 1023, 1024, 4095, 4096, 8192] {
        let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let compressed = zstd_compress(&data, CompressionLevel::None)
            .unwrap_or_else(|e| panic!("size {size}: zstd level 0 compression failed: {e}"));
        let decompressed = zstd_decompress(&compressed)
            .unwrap_or_else(|e| panic!("size {size}: zstd level 0 decompression failed: {e}"));
        assert_eq!(
            decompressed, data,
            "size {size}: zstd level 0 round-trip failed"
        );
    }
}

// =============================================================================
// SECTION 7: Cross-Algorithm Consistency Tests
// =============================================================================

#[test]
fn all_algorithms_handle_level_zero_consistently() {
    let data = test_data::english_text(5000);

    // Zlib
    let zlib_compressed = zlib_compress(&data, CompressionLevel::None).expect("zlib level 0");
    let zlib_decompressed = zlib_decompress(&zlib_compressed).expect("zlib decompress");
    assert_eq!(zlib_decompressed, data, "zlib level 0 failed");

    // LZ4
    #[cfg(feature = "lz4")]
    {
        let lz4_compressed = lz4_compress(&data, CompressionLevel::None).expect("lz4 level 0");
        let lz4_decompressed = lz4_decompress(&lz4_compressed).expect("lz4 decompress");
        assert_eq!(lz4_decompressed, data, "lz4 level 0 failed");
    }

    // Zstd
    #[cfg(feature = "zstd")]
    {
        let zstd_compressed = zstd_compress(&data, CompressionLevel::None).expect("zstd level 0");
        let zstd_decompressed = zstd_decompress(&zstd_compressed).expect("zstd decompress");
        assert_eq!(zstd_decompressed, data, "zstd level 0 failed");
    }
}

#[test]
fn all_algorithms_level_zero_empty_data() {
    let data: &[u8] = &[];

    // Zlib
    let zlib_compressed = zlib_compress(data, CompressionLevel::None).expect("zlib level 0 empty");
    let zlib_decompressed = zlib_decompress(&zlib_compressed).expect("zlib decompress empty");
    assert!(zlib_decompressed.is_empty(), "zlib level 0 empty failed");

    // LZ4
    #[cfg(feature = "lz4")]
    {
        let lz4_compressed = lz4_compress(data, CompressionLevel::None).expect("lz4 level 0 empty");
        let lz4_decompressed = lz4_decompress(&lz4_compressed).expect("lz4 decompress empty");
        assert!(lz4_decompressed.is_empty(), "lz4 level 0 empty failed");
    }

    // Zstd
    #[cfg(feature = "zstd")]
    {
        let zstd_compressed =
            zstd_compress(data, CompressionLevel::None).expect("zstd level 0 empty");
        let zstd_decompressed = zstd_decompress(&zstd_compressed).expect("zstd decompress empty");
        assert!(zstd_decompressed.is_empty(), "zstd level 0 empty failed");
    }
}

#[test]
fn all_algorithms_level_zero_single_byte() {
    let data: &[u8] = &[123];

    // Zlib
    let zlib_compressed = zlib_compress(data, CompressionLevel::None).expect("zlib level 0 single");
    let zlib_decompressed = zlib_decompress(&zlib_compressed).expect("zlib decompress single");
    assert_eq!(zlib_decompressed, data, "zlib level 0 single failed");

    // LZ4
    #[cfg(feature = "lz4")]
    {
        let lz4_compressed =
            lz4_compress(data, CompressionLevel::None).expect("lz4 level 0 single");
        let lz4_decompressed = lz4_decompress(&lz4_compressed).expect("lz4 decompress single");
        assert_eq!(lz4_decompressed, data, "lz4 level 0 single failed");
    }

    // Zstd
    #[cfg(feature = "zstd")]
    {
        let zstd_compressed =
            zstd_compress(data, CompressionLevel::None).expect("zstd level 0 single");
        let zstd_decompressed = zstd_decompress(&zstd_compressed).expect("zstd decompress single");
        assert_eq!(zstd_decompressed, data, "zstd level 0 single failed");
    }
}

#[test]
fn all_algorithms_level_zero_all_byte_values() {
    let data: Vec<u8> = (0..=255).collect();

    // Zlib
    let zlib_compressed =
        zlib_compress(&data, CompressionLevel::None).expect("zlib level 0 all bytes");
    let zlib_decompressed = zlib_decompress(&zlib_compressed).expect("zlib decompress all bytes");
    assert_eq!(zlib_decompressed, data, "zlib level 0 all bytes failed");

    // LZ4
    #[cfg(feature = "lz4")]
    {
        let lz4_compressed =
            lz4_compress(&data, CompressionLevel::None).expect("lz4 level 0 all bytes");
        let lz4_decompressed = lz4_decompress(&lz4_compressed).expect("lz4 decompress all bytes");
        assert_eq!(lz4_decompressed, data, "lz4 level 0 all bytes failed");
    }

    // Zstd
    #[cfg(feature = "zstd")]
    {
        let zstd_compressed =
            zstd_compress(&data, CompressionLevel::None).expect("zstd level 0 all bytes");
        let zstd_decompressed =
            zstd_decompress(&zstd_compressed).expect("zstd decompress all bytes");
        assert_eq!(zstd_decompressed, data, "zstd level 0 all bytes failed");
    }
}

// =============================================================================
// SECTION 8: Large Data Tests
// =============================================================================

#[test]
fn zlib_level_zero_large_data() {
    // Test with 1 MB of data
    let data = test_data::english_text(1_000_000);
    let compressed = zlib_compress(&data, CompressionLevel::None).expect("zlib level 0 large data");
    let decompressed = zlib_decompress(&compressed).expect("zlib decompress large data");
    assert_eq!(
        decompressed, data,
        "zlib level 0 large data round-trip failed"
    );
}

#[cfg(feature = "lz4")]
#[test]
fn lz4_level_zero_large_data() {
    // Test with 1 MB of data
    let data = test_data::english_text(1_000_000);
    let compressed = lz4_compress(&data, CompressionLevel::None).expect("lz4 level 0 large data");
    let decompressed = lz4_decompress(&compressed).expect("lz4 decompress large data");
    assert_eq!(
        decompressed, data,
        "lz4 level 0 large data round-trip failed"
    );
}

#[cfg(feature = "zstd")]
#[test]
fn zstd_level_zero_large_data() {
    // Test with 1 MB of data
    let data = test_data::english_text(1_000_000);
    let compressed = zstd_compress(&data, CompressionLevel::None).expect("zstd level 0 large data");
    let decompressed = zstd_decompress(&compressed).expect("zstd decompress large data");
    assert_eq!(
        decompressed, data,
        "zstd level 0 large data round-trip failed"
    );
}
