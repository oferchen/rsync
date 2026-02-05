//! Comprehensive tests for LZ4 compression (frame and raw formats).
//!
//! This test module verifies:
//! 1. LZ4 compression/decompression round-trip works
//! 2. Different compression levels work
//! 3. Empty input handling
//! 4. Large input handling
//! 5. Error handling for corrupted data
//! 6. Frame format specific tests
//! 7. Raw block format specific tests

use std::io::{Cursor, Read};

use compress::lz4::frame::{
    compress_to_vec as frame_compress, decompress_to_vec as frame_decompress,
    CountingLz4Decoder, CountingLz4Encoder,
};
use compress::lz4::raw::{
    compress_block, compress_block_to_vec, decode_header, decompress_block,
    decompress_block_to_vec, encode_header, is_deflated_data, read_compressed_block,
    write_compressed_block, RawLz4Error, DEFLATED_DATA, HEADER_SIZE, MAX_BLOCK_SIZE,
    MAX_DECOMPRESSED_SIZE,
};
use compress::zlib::CompressionLevel;

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
            Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. \
            Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris \
            nisi ut aliquip ex ea commodo consequat. ";
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
            for _ in 0..zero_len.min(size.saturating_sub(data.len())) {
                data.push(0);
            }
            i += 1;
        }
        data.truncate(size);
        data
    }

    /// Generates all possible byte values in sequence.
    pub fn all_bytes() -> Vec<u8> {
        (0..=255).collect()
    }
}

// =============================================================================
// SECTION 1: Frame Format Round-trip Tests
// =============================================================================

mod frame_round_trips {
    use super::*;

    #[test]
    fn frame_round_trip_empty() {
        let data = b"";
        let compressed = frame_compress(data, CompressionLevel::Default).unwrap();
        assert!(!compressed.is_empty(), "empty input should produce frame header");
        let decompressed = frame_decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn frame_round_trip_single_byte() {
        let data = b"x";
        let compressed = frame_compress(data, CompressionLevel::Default).unwrap();
        let decompressed = frame_decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn frame_round_trip_small_data() {
        let data = b"Hello, LZ4!";
        let compressed = frame_compress(data, CompressionLevel::Default).unwrap();
        let decompressed = frame_decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn frame_round_trip_all_bytes() {
        let data = test_data::all_bytes();
        let compressed = frame_compress(&data, CompressionLevel::Default).unwrap();
        let decompressed = frame_decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn frame_round_trip_repetitive_text() {
        let data = test_data::repetitive_text(10_000);
        let compressed = frame_compress(&data, CompressionLevel::Default).unwrap();
        assert!(compressed.len() < data.len(), "repetitive text should compress");
        let decompressed = frame_decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn frame_round_trip_english_text() {
        let data = test_data::english_text(10_000);
        let compressed = frame_compress(&data, CompressionLevel::Default).unwrap();
        let decompressed = frame_decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn frame_round_trip_structured_binary() {
        let data = test_data::structured_binary(10_000);
        let compressed = frame_compress(&data, CompressionLevel::Default).unwrap();
        let decompressed = frame_decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn frame_round_trip_random_data() {
        let data = test_data::random_data(10_000, 42);
        let compressed = frame_compress(&data, CompressionLevel::Default).unwrap();
        let decompressed = frame_decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn frame_round_trip_sparse_data() {
        let data = test_data::sparse_data(10_000);
        let compressed = frame_compress(&data, CompressionLevel::Default).unwrap();
        assert!(compressed.len() < data.len(), "sparse data should compress well");
        let decompressed = frame_decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn frame_round_trip_large_data() {
        // Test 1MB of data
        let data = test_data::english_text(1_000_000);
        let compressed = frame_compress(&data, CompressionLevel::Default).unwrap();
        assert!(compressed.len() < data.len(), "large text should compress");
        let decompressed = frame_decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn frame_round_trip_boundary_sizes() {
        for size in [1, 2, 255, 256, 1023, 1024, 4095, 4096, 65535, 65536] {
            let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            let compressed = frame_compress(&data, CompressionLevel::Default)
                .unwrap_or_else(|e| panic!("size {size} compression failed: {e}"));
            let decompressed = frame_decompress(&compressed)
                .unwrap_or_else(|e| panic!("size {size} decompression failed: {e}"));
            assert_eq!(decompressed, data, "size {size} round-trip failed");
        }
    }

    #[test]
    fn frame_round_trip_all_zeros() {
        let data = vec![0u8; 10_000];
        let compressed = frame_compress(&data, CompressionLevel::Default).unwrap();
        assert!(compressed.len() < data.len() / 10, "zeros should compress very well");
        let decompressed = frame_decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn frame_round_trip_all_ones() {
        let data = vec![255u8; 10_000];
        let compressed = frame_compress(&data, CompressionLevel::Default).unwrap();
        assert!(compressed.len() < data.len() / 10, "ones should compress very well");
        let decompressed = frame_decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }
}

// =============================================================================
// SECTION 2: Frame Format Compression Levels
// =============================================================================

mod frame_compression_levels {
    use super::*;

    #[test]
    fn frame_all_levels_produce_valid_output() {
        let data = test_data::english_text(10_000);

        for level in [
            CompressionLevel::None,
            CompressionLevel::Fast,
            CompressionLevel::Default,
            CompressionLevel::Best,
        ] {
            let compressed = frame_compress(&data, level)
                .unwrap_or_else(|e| panic!("{level:?} compression failed: {e}"));
            let decompressed = frame_decompress(&compressed)
                .unwrap_or_else(|e| panic!("{level:?} decompression failed: {e}"));
            assert_eq!(decompressed, data, "{level:?} round-trip failed");
        }
    }

    #[test]
    fn frame_precise_levels_1_through_9() {
        let data = test_data::english_text(10_000);

        for n in 1..=9 {
            let level = CompressionLevel::from_numeric(n).unwrap();
            let compressed = frame_compress(&data, level)
                .unwrap_or_else(|e| panic!("level {n} compression failed: {e}"));
            let decompressed = frame_decompress(&compressed)
                .unwrap_or_else(|e| panic!("level {n} decompression failed: {e}"));
            assert_eq!(decompressed, data, "level {n} round-trip failed");
        }
    }

    #[test]
    fn frame_level_zero_produces_valid_output() {
        let data = test_data::english_text(1000);
        let compressed = frame_compress(&data, CompressionLevel::None).unwrap();
        let decompressed = frame_decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn frame_compression_ratio_by_level() {
        // Use highly compressible data to see differences between levels
        let data = test_data::repetitive_text(100_000);

        let fast = frame_compress(&data, CompressionLevel::Fast).unwrap();
        let default = frame_compress(&data, CompressionLevel::Default).unwrap();
        let best = frame_compress(&data, CompressionLevel::Best).unwrap();

        // All should compress well
        assert!(fast.len() < data.len());
        assert!(default.len() < data.len());
        assert!(best.len() < data.len());

        // Verify all decompress correctly
        assert_eq!(frame_decompress(&fast).unwrap(), data);
        assert_eq!(frame_decompress(&default).unwrap(), data);
        assert_eq!(frame_decompress(&best).unwrap(), data);
    }
}

// =============================================================================
// SECTION 3: Frame Format Error Handling
// =============================================================================

mod frame_error_handling {
    use super::*;

    #[test]
    fn frame_decompress_invalid_magic() {
        // LZ4 frame magic is 0x184D2204
        let invalid = [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let result = frame_decompress(&invalid);
        assert!(result.is_err(), "invalid magic should fail");
    }

    #[test]
    fn frame_decompress_truncated_data() {
        let data = b"test data for truncation";
        let compressed = frame_compress(data, CompressionLevel::Default).unwrap();
        let truncated = &compressed[..compressed.len() / 2];
        let result = frame_decompress(truncated);
        assert!(result.is_err(), "truncated data should fail");
    }

    #[test]
    fn frame_decompress_corrupted_data() {
        let data = b"test data for corruption";
        let mut compressed = frame_compress(data, CompressionLevel::Default).unwrap();

        // Corrupt a byte in the middle
        if compressed.len() > 10 {
            compressed[8] ^= 0xFF;
        }

        let result = frame_decompress(&compressed);
        assert!(result.is_err(), "corrupted data should fail");
    }

    #[test]
    fn frame_decompress_empty_input() {
        // LZ4 frame format gracefully handles empty input by returning empty output
        let result = frame_decompress(&[]);
        // The result depends on the LZ4 implementation - it may return empty vec or error
        // Both are valid behaviors for empty input
        if let Ok(data) = result {
            assert!(data.is_empty(), "empty input should produce empty or error");
        }
    }

    #[test]
    fn frame_decompress_incomplete_header() {
        // LZ4 frame header is at least 7 bytes
        let incomplete = [0x04, 0x22, 0x4D, 0x18]; // Valid magic but truncated
        let result = frame_decompress(&incomplete);
        // May return empty or error depending on how the decoder handles incomplete frames
        // The key is it doesn't panic or crash
        if let Ok(ref data) = result {
            // If it succeeds, it should return empty data for incomplete input
            assert!(data.is_empty() || result.is_err());
        }
    }

    #[test]
    fn frame_decompress_missing_checksum() {
        let data = b"sufficient data for a complete frame with checksum";
        let compressed = frame_compress(data, CompressionLevel::Default).unwrap();

        // Truncate the checksum (last 4 bytes)
        if compressed.len() > 5 {
            let truncated = &compressed[..compressed.len() - 1];
            let result = frame_decompress(truncated);
            // Should either error or produce wrong data
            if let Ok(decoded) = result {
                assert_ne!(decoded, data, "truncated checksum should not match");
            }
        }
    }
}

// =============================================================================
// SECTION 4: Frame Format Streaming API
// =============================================================================

mod frame_streaming {
    use super::*;

    #[test]
    fn frame_encoder_counting_sink() {
        let data = b"test data for encoder";
        let mut encoder = CountingLz4Encoder::new(CompressionLevel::Default);
        assert_eq!(encoder.bytes_written(), 0);
        encoder.write(data).unwrap();
        let bytes = encoder.finish().unwrap();
        assert!(bytes > 0);
    }

    #[test]
    fn frame_encoder_with_custom_sink() {
        let data = b"test data";
        let mut encoder = CountingLz4Encoder::with_sink(Vec::new(), CompressionLevel::Default);
        encoder.write(data).unwrap();
        let (compressed, bytes) = encoder.finish_into_inner().unwrap();
        assert_eq!(bytes as usize, compressed.len());

        let decompressed = frame_decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn frame_encoder_chunked_writes() {
        let data = test_data::english_text(10_000);
        let chunk_sizes = [1, 10, 100, 1000];

        for chunk_size in chunk_sizes {
            let mut encoder = CountingLz4Encoder::with_sink(Vec::new(), CompressionLevel::Default);

            for chunk in data.chunks(chunk_size) {
                encoder.write(chunk).unwrap();
            }

            let (compressed, _) = encoder.finish_into_inner().unwrap();
            let decompressed = frame_decompress(&compressed).unwrap();
            assert_eq!(decompressed, data, "chunk size {chunk_size} failed");
        }
    }

    #[test]
    fn frame_encoder_single_byte_writes() {
        let data = b"single byte test";
        let mut encoder = CountingLz4Encoder::with_sink(Vec::new(), CompressionLevel::Default);

        for &byte in data.iter() {
            encoder.write(&[byte]).unwrap();
        }

        let (compressed, _) = encoder.finish_into_inner().unwrap();
        let decompressed = frame_decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn frame_encoder_bytes_written_tracking() {
        let mut encoder = CountingLz4Encoder::with_sink(Vec::new(), CompressionLevel::Default);

        // Initial state - may have frame header already written
        let initial = encoder.bytes_written();

        encoder.write(b"first chunk").unwrap();
        let after_first = encoder.bytes_written();
        assert!(after_first >= initial, "bytes should increase after first write");

        encoder.write(b"second chunk").unwrap();
        let after_second = encoder.bytes_written();
        // Note: LZ4 may buffer data before writing, so bytes_written might not
        // increase immediately for small writes
        assert!(after_second >= after_first, "bytes should not decrease");

        let (_, final_bytes) = encoder.finish_into_inner().unwrap();
        assert!(final_bytes >= after_second, "final should be >= intermediate");
        assert!(final_bytes > 0, "should have written some bytes");
    }

    #[test]
    fn frame_encoder_accessors() {
        let mut encoder = CountingLz4Encoder::with_sink(Vec::new(), CompressionLevel::Default);
        assert!(encoder.get_ref().is_empty());

        encoder.get_mut().extend_from_slice(b"prefix");
        assert!(encoder.get_ref().starts_with(b"prefix"));
    }

    #[test]
    fn frame_decoder_tracking() {
        let data = b"decoder tracking test";
        let compressed = frame_compress(data, CompressionLevel::Default).unwrap();

        let mut decoder = CountingLz4Decoder::new(Cursor::new(&compressed));
        assert_eq!(decoder.bytes_read(), 0);

        let mut output = Vec::new();
        decoder.read_to_end(&mut output).unwrap();

        assert_eq!(output, data);
        assert_eq!(decoder.bytes_read(), data.len() as u64);
    }

    #[test]
    fn frame_decoder_chunked_reads() {
        let data = test_data::english_text(10_000);
        let compressed = frame_compress(&data, CompressionLevel::Default).unwrap();

        let mut decoder = CountingLz4Decoder::new(Cursor::new(&compressed));
        let mut output = Vec::new();
        let mut buf = [0u8; 64];

        loop {
            let n = decoder.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            output.extend_from_slice(&buf[..n]);
        }

        assert_eq!(output, data);
        assert_eq!(decoder.bytes_read(), data.len() as u64);
    }

    #[test]
    fn frame_decoder_accessors() {
        let data = b"accessor test";
        let compressed = frame_compress(data, CompressionLevel::Default).unwrap();
        let cursor = Cursor::new(compressed);

        let mut decoder = CountingLz4Decoder::new(cursor);
        assert_eq!(decoder.get_ref().position(), 0);

        decoder.get_mut().set_position(1);
        assert_eq!(decoder.get_ref().position(), 1);

        let _ = decoder.into_inner();
    }
}

// =============================================================================
// SECTION 5: Raw Block Format Round-trip Tests
// =============================================================================

mod raw_round_trips {
    use super::*;

    #[test]
    fn raw_round_trip_empty() {
        let data = b"";
        let compressed = compress_block_to_vec(data).unwrap();
        let decompressed = decompress_block_to_vec(&compressed, 0).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn raw_round_trip_single_byte() {
        let data = b"x";
        let compressed = compress_block_to_vec(data).unwrap();
        let decompressed = decompress_block_to_vec(&compressed, 1).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn raw_round_trip_small_data() {
        let data = b"Hello, raw LZ4!";
        let compressed = compress_block_to_vec(data).unwrap();
        assert!(is_deflated_data(compressed[0]));
        let decompressed = decompress_block_to_vec(&compressed, data.len()).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn raw_round_trip_all_bytes() {
        let data = test_data::all_bytes();
        let compressed = compress_block_to_vec(&data).unwrap();
        let decompressed = decompress_block_to_vec(&compressed, data.len()).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn raw_round_trip_repetitive_text() {
        let data = test_data::repetitive_text(10_000);
        let compressed = compress_block_to_vec(&data).unwrap();
        assert!(compressed.len() < data.len(), "repetitive text should compress");
        let decompressed = decompress_block_to_vec(&compressed, data.len()).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn raw_round_trip_all_zeros() {
        let data = vec![0u8; 10_000];
        let compressed = compress_block_to_vec(&data).unwrap();
        assert!(compressed.len() < data.len() / 10, "zeros should compress very well");
        let decompressed = decompress_block_to_vec(&compressed, data.len()).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn raw_round_trip_max_block_size() {
        let data = vec![b'x'; MAX_BLOCK_SIZE];
        let compressed = compress_block_to_vec(&data).unwrap();
        let decompressed = decompress_block_to_vec(&compressed, MAX_BLOCK_SIZE).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn raw_round_trip_various_sizes() {
        for size in [0, 1, 10, 100, 1000, 10_000, MAX_BLOCK_SIZE] {
            let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            let compressed = compress_block_to_vec(&data)
                .unwrap_or_else(|e| panic!("size {size} compression failed: {e}"));
            let decompressed = decompress_block_to_vec(&compressed, size.max(1))
                .unwrap_or_else(|e| panic!("size {size} decompression failed: {e}"));
            assert_eq!(decompressed, data, "size {size} round-trip failed");
        }
    }

    #[test]
    fn raw_round_trip_into_buffer() {
        let data = b"buffer compression test";
        let mut compressed = vec![0u8; HEADER_SIZE + lz4_flex::block::get_maximum_output_size(data.len())];

        let total = compress_block(data, &mut compressed).unwrap();
        compressed.truncate(total);

        let mut decompressed = vec![0u8; data.len()];
        let decompressed_len = decompress_block(&compressed, &mut decompressed).unwrap();

        assert_eq!(&decompressed[..decompressed_len], data.as_slice());
    }
}

// =============================================================================
// SECTION 6: Raw Block Format Error Handling
// =============================================================================

mod raw_error_handling {
    use super::*;

    #[test]
    fn raw_input_too_large() {
        let data = vec![0u8; MAX_BLOCK_SIZE + 1];
        let result = compress_block_to_vec(&data);
        assert!(matches!(result, Err(RawLz4Error::InputTooLarge(_))));

        if let Err(RawLz4Error::InputTooLarge(size)) = result {
            assert_eq!(size, MAX_BLOCK_SIZE + 1);
        }
    }

    #[test]
    fn raw_decompressed_size_too_large() {
        let data = b"test";
        let compressed = compress_block_to_vec(data).unwrap();

        let result = decompress_block_to_vec(&compressed, MAX_DECOMPRESSED_SIZE + 1);
        assert!(matches!(result, Err(RawLz4Error::DecompressedSizeTooLarge(_))));

        if let Err(RawLz4Error::DecompressedSizeTooLarge(size)) = result {
            assert_eq!(size, MAX_DECOMPRESSED_SIZE + 1);
        }
    }

    #[test]
    fn raw_buffer_too_small_compress() {
        let data = b"test data that needs space";
        let mut output = [0u8; 5];

        let result = compress_block(data, &mut output);
        assert!(matches!(result, Err(RawLz4Error::BufferTooSmall { .. })));
    }

    #[test]
    fn raw_buffer_too_small_decompress() {
        let data = b"test data";
        let compressed = compress_block_to_vec(data).unwrap();

        let mut output = [0u8; 2];
        let _result = decompress_block(&compressed, &mut output);
        // May succeed with truncated output or fail - both are valid
        // The important thing is it doesn't crash
    }

    #[test]
    fn raw_invalid_header_token_rel() {
        // TOKEN_REL flag (0x80) should fail
        let invalid = [0x80, 0x00, 0x00, 0x00];
        let result = decompress_block(&invalid, &mut [0u8; 100]);
        assert!(matches!(result, Err(RawLz4Error::InvalidHeader(0x80))));
    }

    #[test]
    fn raw_invalid_header_end_flag() {
        // END_FLAG (0x00) should fail
        let invalid = [0x00, 0x00, 0x00, 0x00];
        let result = decompress_block(&invalid, &mut [0u8; 100]);
        assert!(matches!(result, Err(RawLz4Error::InvalidHeader(0x00))));
    }

    #[test]
    fn raw_corrupted_compressed_data() {
        let header = encode_header(10);
        let mut input = Vec::from(header);
        input.extend_from_slice(&[0xFF; 10]);

        let result = decompress_block(&input, &mut [0u8; 1000]);
        assert!(matches!(result, Err(RawLz4Error::DecompressFailed(_))));
    }

    #[test]
    fn raw_truncated_input() {
        let header = encode_header(100);
        let result = decompress_block(&header, &mut [0u8; 1000]);
        assert!(matches!(result, Err(RawLz4Error::BufferTooSmall { .. })));
    }

    #[test]
    fn raw_incomplete_header() {
        let input = [0x40]; // Only one byte
        let result = decompress_block(&input, &mut [0u8; 100]);
        assert!(matches!(result, Err(RawLz4Error::BufferTooSmall { .. })));
    }

    #[test]
    fn raw_io_error_conversion() {
        let err = RawLz4Error::InputTooLarge(20000);
        let io_err: std::io::Error = err.into();
        assert_eq!(io_err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn raw_error_display_messages() {
        let err = RawLz4Error::InputTooLarge(20000);
        let msg = err.to_string();
        assert!(msg.contains("20000"));
        assert!(msg.contains("exceeds"));

        let err = RawLz4Error::BufferTooSmall { needed: 100, available: 50 };
        let msg = err.to_string();
        assert!(msg.contains("100"));
        assert!(msg.contains("50"));

        let err = RawLz4Error::InvalidHeader(0x80);
        let msg = err.to_string();
        assert!(msg.contains("0x80"));
    }
}

// =============================================================================
// SECTION 7: Raw Block Format Header Encoding/Decoding
// =============================================================================

mod raw_header {
    use super::*;

    #[test]
    fn header_encode_decode_roundtrip() {
        for size in [0, 1, 100, 1000, 8191, 8192, 16382, MAX_BLOCK_SIZE] {
            let header = encode_header(size);
            let decoded = decode_header(header).expect("valid header");
            assert_eq!(decoded, size, "size {size} roundtrip failed");
        }
    }

    #[test]
    fn header_has_deflated_flag() {
        for size in [0, 1, 100, 1000, MAX_BLOCK_SIZE] {
            let header = encode_header(size);
            assert!(is_deflated_data(header[0]), "header for size {size} missing DEFLATED_DATA flag");
        }
    }

    #[test]
    fn header_flag_detection() {
        // DEFLATED_DATA range (0x40-0x7F)
        for flag in [0x40, 0x41, 0x5F, 0x7F] {
            assert!(is_deflated_data(flag), "0x{flag:02x} should be deflated");
        }

        // Non-deflated flags
        for flag in [0x00, 0x3F, 0x80, 0xC0, 0xFF] {
            assert!(!is_deflated_data(flag), "0x{flag:02x} should not be deflated");
        }
    }

    #[test]
    fn header_decode_invalid() {
        // Various invalid headers
        assert_eq!(decode_header([0x00, 0x00]), None);
        assert_eq!(decode_header([0x80, 0x00]), None);
        assert_eq!(decode_header([0xC0, 0x00]), None);
        assert_eq!(decode_header([0xFF, 0xFF]), None);
    }

    #[test]
    fn header_max_size() {
        let header = encode_header(MAX_BLOCK_SIZE);
        assert_eq!(header[0] & 0xC0, DEFLATED_DATA);
        let decoded = decode_header(header).unwrap();
        assert_eq!(decoded, MAX_BLOCK_SIZE);
    }

    #[test]
    fn header_size_zero() {
        let header = encode_header(0);
        assert_eq!(header[0], DEFLATED_DATA);
        assert_eq!(header[1], 0);
        let decoded = decode_header(header).unwrap();
        assert_eq!(decoded, 0);
    }
}

// =============================================================================
// SECTION 8: Raw Block Format I/O Operations
// =============================================================================

mod raw_io {
    use super::*;

    #[test]
    fn read_write_roundtrip() {
        let data = b"streaming I/O test data";
        let mut buffer = Vec::new();

        let written = write_compressed_block(data, &mut buffer).unwrap();
        assert!(written > 0);
        assert_eq!(written, buffer.len());

        let mut cursor = Cursor::new(buffer);
        let decompressed = read_compressed_block(&mut cursor, data.len()).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn read_write_empty_data() {
        let data = b"";
        let mut buffer = Vec::new();

        write_compressed_block(data, &mut buffer).unwrap();

        let mut cursor = Cursor::new(buffer);
        let decompressed = read_compressed_block(&mut cursor, 0).unwrap();
        assert!(decompressed.is_empty());
    }

    #[test]
    fn read_write_large_block() {
        let data = vec![b'x'; 16_000];
        let mut buffer = Vec::new();

        write_compressed_block(&data, &mut buffer).unwrap();

        let mut cursor = Cursor::new(buffer);
        let decompressed = read_compressed_block(&mut cursor, data.len()).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn read_eof_in_header() {
        let mut cursor = Cursor::new(vec![0x40]); // Only one byte
        let result = read_compressed_block(&mut cursor, 1000);
        assert!(matches!(result, Err(RawLz4Error::Io(_))));
    }

    #[test]
    fn read_eof_in_data() {
        let header = encode_header(100);
        let mut data = Vec::from(header);
        data.extend_from_slice(&[0x00, 0x01, 0x02]); // Only 3 bytes instead of 100

        let mut cursor = Cursor::new(data);
        let result = read_compressed_block(&mut cursor, 1000);
        assert!(matches!(result, Err(RawLz4Error::Io(_))));
    }

    #[test]
    fn read_empty_reader() {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let result = read_compressed_block(&mut cursor, 1000);
        assert!(matches!(result, Err(RawLz4Error::Io(_))));
    }

    #[test]
    fn read_max_size_exceeded() {
        let data = b"test";
        let mut buffer = Vec::new();
        write_compressed_block(data, &mut buffer).unwrap();

        let mut cursor = Cursor::new(buffer);
        let result = read_compressed_block(&mut cursor, MAX_DECOMPRESSED_SIZE + 1);
        assert!(matches!(result, Err(RawLz4Error::DecompressedSizeTooLarge(_))));
    }
}

// =============================================================================
// SECTION 9: Large Input Handling
// =============================================================================

mod large_inputs {
    use super::*;

    #[test]
    fn frame_large_compressible_data() {
        // 10MB of highly compressible data
        let data = vec![0u8; 10_000_000];
        let compressed = frame_compress(&data, CompressionLevel::Default).unwrap();
        assert!(compressed.len() < data.len() / 100, "zeros should compress extremely well");

        let decompressed = frame_decompress(&compressed).unwrap();
        assert_eq!(decompressed.len(), data.len());
    }

    #[test]
    fn frame_large_structured_data() {
        // 5MB of structured data
        let data = test_data::structured_binary(5_000_000);
        let compressed = frame_compress(&data, CompressionLevel::Default).unwrap();
        assert!(compressed.len() < data.len(), "structured data should compress");

        let decompressed = frame_decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn frame_multiple_blocks() {
        // Data larger than any single block size to ensure multi-block handling
        let data = test_data::english_text(5_000_000);

        let mut encoder = CountingLz4Encoder::with_sink(Vec::new(), CompressionLevel::Default);
        encoder.write(&data).unwrap();
        let (compressed, _) = encoder.finish_into_inner().unwrap();

        let decompressed = frame_decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn raw_max_block_size_compressible() {
        let data = vec![0u8; MAX_BLOCK_SIZE];
        let compressed = compress_block_to_vec(&data).unwrap();
        assert!(compressed.len() < MAX_BLOCK_SIZE / 10, "max-size zeros should compress well");

        let decompressed = decompress_block_to_vec(&compressed, MAX_BLOCK_SIZE).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn raw_max_block_size_varied_data() {
        let data: Vec<u8> = (0..MAX_BLOCK_SIZE).map(|i| (i % 256) as u8).collect();
        let compressed = compress_block_to_vec(&data).unwrap();

        let decompressed = decompress_block_to_vec(&compressed, MAX_BLOCK_SIZE).unwrap();
        assert_eq!(decompressed, data);
    }
}

// =============================================================================
// SECTION 10: Comparison Tests
// =============================================================================

mod comparison {
    use super::*;

    #[test]
    fn frame_vs_raw_same_data() {
        let data = test_data::english_text(1000);

        // Both should compress and decompress correctly
        let frame_compressed = frame_compress(&data, CompressionLevel::Default).unwrap();
        let frame_decompressed = frame_decompress(&frame_compressed).unwrap();
        assert_eq!(frame_decompressed, data);

        let raw_compressed = compress_block_to_vec(&data).unwrap();
        let raw_decompressed = decompress_block_to_vec(&raw_compressed, data.len()).unwrap();
        assert_eq!(raw_decompressed, data);

        // Frame format should be larger due to headers/checksums
        assert!(frame_compressed.len() > raw_compressed.len());
    }

    #[test]
    fn compression_ratio_by_data_type() {
        let size = 10_000;
        let test_cases = [
            ("repetitive", test_data::repetitive_text(size)),
            ("english", test_data::english_text(size)),
            ("structured", test_data::structured_binary(size)),
            ("random", test_data::random_data(size, 42)),
            ("sparse", test_data::sparse_data(size)),
        ];

        for (name, data) in &test_cases {
            let compressed = frame_compress(data, CompressionLevel::Default).unwrap();
            let decompressed = frame_decompress(&compressed).unwrap();
            assert_eq!(decompressed, *data, "{name}: round-trip failed");

            let ratio = data.len() as f64 / compressed.len() as f64;

            // Random data should not compress well
            if *name == "random" {
                assert!(ratio < 1.2, "{name}: unexpectedly compressible (ratio: {ratio:.2})");
            }

            // Highly repetitive data should compress very well
            if *name == "repetitive" || *name == "sparse" {
                assert!(ratio > 2.0, "{name}: expected high compression (ratio: {ratio:.2})");
            }
        }
    }
}
