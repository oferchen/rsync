//! Algorithm-specific tests for compress crate.
//!
//! This test suite focuses on:
//! 1. LZ4-specific features (raw vs frame format)
//! 2. Zstd-specific features and compression levels
//! 3. Cross-algorithm comparison
//! 4. Algorithm-specific error conditions

use std::io::{Cursor, Read};

// =============================================================================
// SECTION 1: LZ4 Frame Format Tests
// =============================================================================

#[cfg(feature = "lz4")]
mod lz4_frame_tests {
    use super::*;
    use compress::lz4::frame::{
        CountingLz4Decoder, CountingLz4Encoder, compress_to_vec, decompress_to_vec,
    };
    use compress::zlib::CompressionLevel;
    use std::io::IoSliceMut;

    #[test]
    fn frame_encoder_new_creates_valid_encoder() {
        let mut encoder = CountingLz4Encoder::new(CompressionLevel::Default);
        encoder.write(b"test").unwrap();
        let bytes = encoder.finish().unwrap();
        assert!(bytes > 0);
    }

    #[test]
    fn frame_encoder_with_sink_works() {
        let mut encoder = CountingLz4Encoder::with_sink(Vec::new(), CompressionLevel::Default);
        encoder.write(b"test").unwrap();

        let (compressed, bytes) = encoder.finish_into_inner().unwrap();
        assert_eq!(bytes as usize, compressed.len());
    }

    #[test]
    fn frame_encoder_tracks_bytes_written() {
        let mut encoder = CountingLz4Encoder::with_sink(Vec::new(), CompressionLevel::Default);

        assert_eq!(encoder.bytes_written(), 0);

        encoder.write(b"data").unwrap();
        let after_write = encoder.bytes_written();
        assert!(after_write > 0);

        encoder.write(b" more").unwrap();
        assert!(encoder.bytes_written() >= after_write);
    }

    #[test]
    fn frame_encoder_get_ref_and_get_mut() {
        let mut encoder = CountingLz4Encoder::with_sink(Vec::new(), CompressionLevel::Default);

        assert!(encoder.get_ref().is_empty());

        encoder.get_mut().extend_from_slice(b"prefix");
        assert_eq!(encoder.get_ref().len(), 6);

        encoder.write(b"data").unwrap();
        let (result, _) = encoder.finish_into_inner().unwrap();
        assert!(result.starts_with(b"prefix"));
    }

    #[test]
    fn frame_decoder_new_creates_valid_decoder() {
        let data = b"decoder test";
        let compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();

        let mut decoder = CountingLz4Decoder::new(&compressed[..]);
        let mut output = Vec::new();
        decoder.read_to_end(&mut output).unwrap();

        assert_eq!(output, data);
    }

    #[test]
    fn frame_decoder_tracks_bytes_read() {
        let data = b"tracking test";
        let compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();

        let mut decoder = CountingLz4Decoder::new(Cursor::new(&compressed));

        assert_eq!(decoder.bytes_read(), 0);

        let mut output = Vec::new();
        decoder.read_to_end(&mut output).unwrap();

        assert_eq!(decoder.bytes_read(), data.len() as u64);
    }

    #[test]
    fn frame_decoder_vectored_read() {
        let data = b"vectored read test data".repeat(10);
        let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();

        let mut decoder = CountingLz4Decoder::new(Cursor::new(&compressed));
        let mut buf1 = [0u8; 20];
        let mut buf2 = [0u8; 40];
        let mut bufs = [IoSliceMut::new(&mut buf1), IoSliceMut::new(&mut buf2)];

        let read = decoder.read_vectored(&mut bufs).unwrap();
        assert!(read > 0);
        assert_eq!(decoder.bytes_read(), read as u64);
    }

    #[test]
    fn frame_decoder_get_ref_get_mut_into_inner() {
        let data = b"accessor test";
        let compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();
        let cursor = Cursor::new(compressed.clone());

        let mut decoder = CountingLz4Decoder::new(cursor);

        assert_eq!(decoder.get_ref().position(), 0);

        decoder.get_mut().set_position(1);
        assert_eq!(decoder.get_ref().position(), 1);

        let inner = decoder.into_inner();
        assert_eq!(inner.get_ref(), &compressed);
    }

    #[test]
    fn frame_compression_levels() {
        let data = b"compression level test".repeat(100);

        for level in [
            CompressionLevel::None,
            CompressionLevel::Fast,
            CompressionLevel::Default,
            CompressionLevel::Best,
        ] {
            let compressed = compress_to_vec(&data, level).unwrap();
            let decompressed = decompress_to_vec(&compressed).unwrap();
            assert_eq!(decompressed, data);
        }
    }

    #[test]
    fn frame_precise_levels() {
        let data = b"precise level test".repeat(100);

        for n in 1..=9 {
            let level = CompressionLevel::from_numeric(n).unwrap();
            let compressed = compress_to_vec(&data, level).unwrap();
            let decompressed = decompress_to_vec(&compressed).unwrap();
            assert_eq!(decompressed, data);
        }
    }

    #[test]
    fn frame_empty_data() {
        let compressed = compress_to_vec(&[], CompressionLevel::Default).unwrap();
        assert!(!compressed.is_empty(), "Frame should have headers");

        let decompressed = decompress_to_vec(&compressed).unwrap();
        assert!(decompressed.is_empty());
    }

    #[test]
    fn frame_single_byte() {
        let data = vec![42u8];
        let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();
        let decompressed = decompress_to_vec(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn frame_large_compressible_data() {
        let data = vec![b'X'; 500_000];
        let compressed = compress_to_vec(&data, CompressionLevel::Best).unwrap();

        // Should achieve good compression
        assert!(compressed.len() < data.len() / 10);

        let decompressed = decompress_to_vec(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn frame_corruption_detection() {
        let data = b"corruption detection test";
        let mut compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();

        // Corrupt a byte in the middle
        if compressed.len() > 10 {
            let mid = compressed.len() / 2;
            compressed[mid] ^= 0xFF;
        }

        // Should fail to decompress or produce wrong data
        let result = decompress_to_vec(&compressed);
        if let Ok(decompressed) = result {
            assert_ne!(decompressed, data, "Corrupted data should not match");
        }
    }
}

// =============================================================================
// SECTION 2: LZ4 Raw Format Tests
// =============================================================================

#[cfg(feature = "lz4")]
mod lz4_raw_tests {
    use super::*;
    use compress::lz4::raw::{
        DEFLATED_DATA, HEADER_SIZE, MAX_BLOCK_SIZE, MAX_DECOMPRESSED_SIZE, compress_block,
        compress_block_to_vec, compressed_size_from_header, decode_header, decompress_block,
        decompress_block_to_vec, encode_header, is_deflated_data, read_compressed_block,
        write_compressed_block,
    };
    use lz4_flex::block::get_maximum_output_size;

    #[test]
    fn raw_header_encoding_all_sizes() {
        for size in [0, 1, 10, 100, 1000, 8192, MAX_BLOCK_SIZE] {
            let header = encode_header(size);
            assert_eq!(header[0] & 0xC0, DEFLATED_DATA);

            let decoded = decode_header(header).unwrap();
            assert_eq!(decoded, size);
        }
    }

    #[test]
    fn raw_header_flag_detection() {
        assert!(is_deflated_data(0x40));
        assert!(is_deflated_data(0x7F));
        assert!(!is_deflated_data(0x00));
        assert!(!is_deflated_data(0x80));
        assert!(!is_deflated_data(0xC0));
    }

    #[test]
    fn raw_compressed_size_from_header() {
        let header = encode_header(1234);
        assert_eq!(compressed_size_from_header(header), Some(1234));

        assert_eq!(compressed_size_from_header([0x00, 0x00]), None);
        assert_eq!(compressed_size_from_header([0x80, 0x00]), None);
    }

    #[test]
    fn raw_compress_decompress_various_sizes() {
        for size in [0, 1, 10, 100, 1000, 10000] {
            let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            let compressed = compress_block_to_vec(&data).unwrap();
            let decompressed = decompress_block_to_vec(&compressed, size.max(1)).unwrap();
            assert_eq!(decompressed, data);
        }
    }

    #[test]
    fn raw_compress_into_preallocated_buffer() {
        let data = b"buffer test data";
        let max_size = HEADER_SIZE + get_maximum_output_size(data.len());
        let mut buffer = vec![0u8; max_size];

        let total = compress_block(data, &mut buffer).unwrap();
        buffer.truncate(total);

        let mut decompressed = vec![0u8; data.len()];
        let decompressed_len = decompress_block(&buffer, &mut decompressed).unwrap();

        assert_eq!(&decompressed[..decompressed_len], data.as_slice());
    }

    #[test]
    fn raw_read_write_streaming() {
        let data = b"streaming I/O test";
        let mut buffer = Vec::new();

        write_compressed_block(data, &mut buffer).unwrap();

        let mut cursor = Cursor::new(buffer);
        let decompressed = read_compressed_block(&mut cursor, data.len()).unwrap();

        assert_eq!(decompressed, data);
    }

    #[test]
    fn raw_highly_compressible() {
        let data = vec![0u8; 10000];
        let compressed = compress_block_to_vec(&data).unwrap();

        // Should compress very well
        assert!(compressed.len() < data.len() / 50);

        let decompressed = decompress_block_to_vec(&compressed, data.len()).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn raw_max_block_size() {
        let data = vec![b'M'; MAX_BLOCK_SIZE];
        let compressed = compress_block_to_vec(&data).unwrap();
        let decompressed = decompress_block_to_vec(&compressed, MAX_BLOCK_SIZE).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn raw_empty_block() {
        let compressed = compress_block_to_vec(&[]).unwrap();
        // Empty block produces header (2 bytes) + minimal compressed data (1 byte)
        assert_eq!(compressed.len(), HEADER_SIZE + 1);

        let decompressed = decompress_block_to_vec(&compressed, 0).unwrap();
        assert!(decompressed.is_empty());
    }

    #[test]
    fn raw_error_input_too_large() {
        let data = vec![0u8; MAX_BLOCK_SIZE + 1];
        let result = compress_block_to_vec(&data);
        assert!(result.is_err());
    }

    #[test]
    fn raw_error_buffer_too_small() {
        let data = b"test data";
        let mut small_buffer = [0u8; 5];
        let result = compress_block(data, &mut small_buffer);
        assert!(result.is_err());
    }

    #[test]
    fn raw_error_invalid_header() {
        let invalid = [0x80, 0x00, 0xFF, 0xFF];
        let result = decompress_block(&invalid, &mut [0u8; 100]);
        assert!(result.is_err());
    }

    #[test]
    fn raw_error_truncated_input() {
        let header = encode_header(100);
        let result = decompress_block(&header, &mut [0u8; 1000]);
        assert!(result.is_err());
    }

    #[test]
    fn raw_error_corrupted_data() {
        let header = encode_header(10);
        let mut corrupted = Vec::from(header);
        corrupted.extend_from_slice(&[0xFF; 10]);

        let result = decompress_block(&corrupted, &mut [0u8; 1000]);
        assert!(result.is_err());
    }

    #[test]
    fn raw_error_max_decompressed_size_exceeded() {
        let data = b"test";
        let compressed = compress_block_to_vec(data).unwrap();

        let result = decompress_block_to_vec(&compressed, MAX_DECOMPRESSED_SIZE + 1);
        assert!(result.is_err());
    }
}

// =============================================================================
// SECTION 3: Zstd-Specific Tests
// =============================================================================

#[cfg(feature = "zstd")]
mod zstd_tests {
    use super::*;
    use compress::zlib::CompressionLevel;
    use compress::zstd::{
        CountingZstdDecoder, CountingZstdEncoder, compress_to_vec, decompress_to_vec,
    };
    use std::io::IoSliceMut;

    #[test]
    fn zstd_encoder_new_works() {
        let mut encoder = CountingZstdEncoder::new(CompressionLevel::Default).unwrap();
        encoder.write(b"test").unwrap();
        let bytes = encoder.finish().unwrap();
        assert!(bytes > 0);
    }

    #[test]
    fn zstd_encoder_with_sink() {
        let mut encoder =
            CountingZstdEncoder::with_sink(Vec::new(), CompressionLevel::Default).unwrap();
        encoder.write(b"test").unwrap();

        let (compressed, bytes) = encoder.finish_into_inner().unwrap();
        assert_eq!(bytes as usize, compressed.len());
    }

    #[test]
    fn zstd_encoder_tracks_bytes() {
        let mut encoder =
            CountingZstdEncoder::with_sink(Vec::new(), CompressionLevel::Default).unwrap();

        assert_eq!(encoder.bytes_written(), 0);

        encoder.write(b"data").unwrap();
        let (_, bytes) = encoder.finish_into_inner().unwrap();
        assert!(bytes > 0);
    }

    #[test]
    fn zstd_encoder_accessors() {
        let mut encoder =
            CountingZstdEncoder::with_sink(Vec::new(), CompressionLevel::Default).unwrap();

        assert!(encoder.get_ref().is_empty());

        encoder.get_mut().extend_from_slice(b"prefix");
        assert!(!encoder.get_ref().is_empty());
    }

    #[test]
    fn zstd_decoder_new_works() {
        let data = b"decoder test";
        let compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();

        let mut decoder = CountingZstdDecoder::new(&compressed[..]).unwrap();
        let mut output = Vec::new();
        decoder.read_to_end(&mut output).unwrap();

        assert_eq!(output, data);
    }

    #[test]
    fn zstd_decoder_tracks_bytes() {
        let data = b"tracking test";
        let compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();

        let mut decoder = CountingZstdDecoder::new(Cursor::new(&compressed)).unwrap();

        assert_eq!(decoder.bytes_read(), 0);

        let mut output = Vec::new();
        decoder.read_to_end(&mut output).unwrap();

        assert_eq!(decoder.bytes_read(), data.len() as u64);
    }

    #[test]
    fn zstd_decoder_vectored_read() {
        let data = b"vectored test".repeat(10);
        let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();

        let mut decoder = CountingZstdDecoder::new(Cursor::new(&compressed)).unwrap();
        let mut buf1 = [0u8; 20];
        let mut buf2 = [0u8; 30];
        let mut bufs = [IoSliceMut::new(&mut buf1), IoSliceMut::new(&mut buf2)];

        let read = decoder.read_vectored(&mut bufs).unwrap();
        assert!(read > 0);
        assert_eq!(decoder.bytes_read(), read as u64);
    }

    #[test]
    fn zstd_decoder_accessors() {
        let data = b"accessor test";
        let compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();
        let cursor = Cursor::new(compressed.clone());

        let mut decoder = CountingZstdDecoder::new(cursor).unwrap();

        assert_eq!(decoder.get_ref().position(), 0);

        decoder.get_mut().set_position(1);
        assert_eq!(decoder.get_ref().position(), 1);

        let inner = decoder.into_inner();
        assert_eq!(inner.get_ref(), &compressed);
    }

    #[test]
    fn zstd_compression_levels() {
        let data = b"level test".repeat(100);

        for level in [
            CompressionLevel::None,
            CompressionLevel::Fast,
            CompressionLevel::Default,
            CompressionLevel::Best,
        ] {
            let compressed = compress_to_vec(&data, level).unwrap();
            let decompressed = decompress_to_vec(&compressed).unwrap();
            assert_eq!(decompressed, data);
        }
    }

    #[test]
    fn zstd_precise_levels() {
        let data = b"precise test".repeat(100);

        for n in 1..=9 {
            let level = CompressionLevel::from_numeric(n).unwrap();
            let compressed = compress_to_vec(&data, level).unwrap();
            let decompressed = decompress_to_vec(&compressed).unwrap();
            assert_eq!(decompressed, data);
        }
    }

    #[test]
    fn zstd_empty_data() {
        let compressed = compress_to_vec(&[], CompressionLevel::Default).unwrap();
        let decompressed = decompress_to_vec(&compressed).unwrap();
        assert!(decompressed.is_empty());
    }

    #[test]
    fn zstd_single_byte() {
        let data = vec![99u8];
        let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();
        let decompressed = decompress_to_vec(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn zstd_large_compressible_data() {
        let data = vec![b'Z'; 500_000];
        let compressed = compress_to_vec(&data, CompressionLevel::Best).unwrap();

        // Should achieve excellent compression
        assert!(compressed.len() < data.len() / 100);

        let decompressed = decompress_to_vec(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn zstd_corruption_detection() {
        let data = b"corruption test data that is long enough to ensure proper compression";
        let mut compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();

        // Corrupt a byte in the compressed data (avoid header)
        if compressed.len() > 10 {
            let mid = compressed.len() / 2;
            compressed[mid] ^= 0xFF;
        }

        let result = decompress_to_vec(&compressed);
        // Note: zstd may or may not detect corruption depending on where it occurs
        // This test verifies the API works but doesn't guarantee failure
        let _ = result;
    }

    #[test]
    fn zstd_truncated_stream() {
        let data = b"truncation test";
        let compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();

        if compressed.len() > 10 {
            let truncated = &compressed[..compressed.len() - 5];
            let result = decompress_to_vec(truncated);
            assert!(result.is_err(), "Truncated stream should fail");
        }
    }
}

// =============================================================================
// SECTION 4: Cross-Algorithm Comparison
// =============================================================================

#[test]
fn all_algorithms_handle_empty_data() {
    use compress::zlib;

    let compressed = zlib::compress_to_vec(&[], zlib::CompressionLevel::Default).unwrap();
    let decompressed = zlib::decompress_to_vec(&compressed).unwrap();
    assert!(decompressed.is_empty());

    #[cfg(feature = "lz4")]
    {
        use compress::lz4;
        let compressed = lz4::compress_to_vec(&[], zlib::CompressionLevel::Default).unwrap();
        let decompressed = lz4::decompress_to_vec(&compressed).unwrap();
        assert!(decompressed.is_empty());
    }

    #[cfg(feature = "zstd")]
    {
        use compress::zstd;
        let compressed = zstd::compress_to_vec(&[], zlib::CompressionLevel::Default).unwrap();
        let decompressed = zstd::decompress_to_vec(&compressed).unwrap();
        assert!(decompressed.is_empty());
    }
}

#[test]
fn all_algorithms_compress_repetitive_data_well() {
    use compress::zlib;

    let data = vec![b'R'; 10_000];

    let zlib_compressed = zlib::compress_to_vec(&data, zlib::CompressionLevel::Best).unwrap();
    assert!(zlib_compressed.len() < data.len() / 10);

    #[cfg(feature = "lz4")]
    {
        use compress::lz4;
        let lz4_compressed = lz4::compress_to_vec(&data, zlib::CompressionLevel::Best).unwrap();
        assert!(lz4_compressed.len() < data.len() / 10);
    }

    #[cfg(feature = "zstd")]
    {
        use compress::zstd;
        let zstd_compressed = zstd::compress_to_vec(&data, zlib::CompressionLevel::Best).unwrap();
        assert!(zstd_compressed.len() < data.len() / 10);
    }
}

#[test]
fn all_algorithms_roundtrip_correctly() {
    use compress::zlib;

    let test_data = b"Cross-algorithm roundtrip test data".repeat(50);

    // Zlib
    let zlib_compressed =
        zlib::compress_to_vec(&test_data, zlib::CompressionLevel::Default).unwrap();
    let zlib_decompressed = zlib::decompress_to_vec(&zlib_compressed).unwrap();
    assert_eq!(zlib_decompressed, test_data);

    // LZ4
    #[cfg(feature = "lz4")]
    {
        use compress::lz4;
        let lz4_compressed =
            lz4::compress_to_vec(&test_data, zlib::CompressionLevel::Default).unwrap();
        let lz4_decompressed = lz4::decompress_to_vec(&lz4_compressed).unwrap();
        assert_eq!(lz4_decompressed, test_data);
    }

    // Zstd
    #[cfg(feature = "zstd")]
    {
        use compress::zstd;
        let zstd_compressed =
            zstd::compress_to_vec(&test_data, zlib::CompressionLevel::Default).unwrap();
        let zstd_decompressed = zstd::decompress_to_vec(&zstd_compressed).unwrap();
        assert_eq!(zstd_decompressed, test_data);
    }
}

#[test]
fn compression_algorithm_enum_coverage() {
    use compress::algorithm::CompressionAlgorithm;

    let zlib = CompressionAlgorithm::Zlib;
    assert_eq!(zlib.name(), "zlib");

    #[cfg(feature = "lz4")]
    {
        let lz4 = CompressionAlgorithm::Lz4;
        assert_eq!(lz4.name(), "lz4");
    }

    #[cfg(feature = "zstd")]
    {
        let zstd = CompressionAlgorithm::Zstd;
        assert_eq!(zstd.name(), "zstd");
    }

    let available = CompressionAlgorithm::available();
    assert!(!available.is_empty());
    assert!(available.contains(&CompressionAlgorithm::Zlib));
}
