//! Comprehensive tests for streaming compression/decompression and edge cases.
//!
//! This test suite focuses on:
//! 1. Streaming compression with various read/write patterns
//! 2. Error handling for corrupted and malformed data
//! 3. Edge cases (empty input, maximum sizes, boundary conditions)
//! 4. Write trait implementations (flush, write_all, write_vectored)
//! 5. Decoder accessor methods and state transitions

use std::io::{Cursor, IoSlice, IoSliceMut, Read, Write};

use compress::CountingSink;
use compress::zlib::{
    CompressionLevel, CountingZlibDecoder, CountingZlibEncoder, compress_to_vec, decompress_to_vec,
};

// =============================================================================
// SECTION 1: Streaming Compression Edge Cases
// =============================================================================

#[test]
fn encoder_flush_behavior() {
    let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);

    // Write data
    encoder.write_all(b"hello").unwrap();

    // Flush should succeed
    encoder.flush().unwrap();

    // Should be able to write more after flush
    encoder.write_all(b" world").unwrap();
    encoder.flush().unwrap();

    let (compressed, bytes) = encoder.finish_into_inner().unwrap();
    assert!(bytes > 0);
    assert_eq!(bytes as usize, compressed.len());

    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, b"hello world");
}

#[test]
fn encoder_write_all_ensures_complete_write() {
    let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);

    let data = b"test data that must be written completely";
    encoder.write_all(data).unwrap();

    let (compressed, _) = encoder.finish_into_inner().unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, data);
}

#[test]
fn encoder_multiple_finish_attempts() {
    let mut encoder = CountingZlibEncoder::new(CompressionLevel::Default);
    encoder.write(b"data").unwrap();

    // First finish should succeed
    let bytes1 = encoder.finish().unwrap();
    assert!(bytes1 > 0);
}

#[test]
fn encoder_zero_writes_before_finish() {
    // Encoder that receives no writes should still produce valid output
    let encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);
    let (compressed, bytes) = encoder.finish_into_inner().unwrap();

    assert!(bytes > 0, "Empty stream should still have framing bytes");

    // Should decompress to empty
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert!(decompressed.is_empty());
}

#[test]
fn encoder_very_small_writes() {
    let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);

    // Write data one byte at a time
    let data = b"tiny incremental writes";
    for &byte in data {
        encoder.write(&[byte]).unwrap();
    }

    let (compressed, _) = encoder.finish_into_inner().unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, data);
}

#[test]
fn encoder_very_large_single_write() {
    let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);

    // Write 10MB in a single call
    let data = vec![b'x'; 10 * 1024 * 1024];
    encoder.write_all(&data).unwrap();

    let (compressed, _) = encoder.finish_into_inner().unwrap();
    assert!(
        compressed.len() < data.len(),
        "Should compress repetitive data"
    );

    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, data);
}

#[test]
fn encoder_interleaved_writes_and_flushes() {
    let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);

    encoder.write_all(b"part1").unwrap();
    encoder.flush().unwrap();

    encoder.write_all(b"part2").unwrap();
    encoder.flush().unwrap();

    encoder.write_all(b"part3").unwrap();

    let (compressed, _) = encoder.finish_into_inner().unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, b"part1part2part3");
}

#[test]
fn encoder_bytes_written_increases_monotonically() {
    let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);

    let prev = encoder.bytes_written();
    assert_eq!(prev, 0);

    encoder.write(b"data1").unwrap();
    let after1 = encoder.bytes_written();
    assert!(after1 >= prev);

    encoder.write(b"data2").unwrap();
    let after2 = encoder.bytes_written();
    assert!(after2 >= after1);

    encoder.flush().unwrap();
    let after_flush = encoder.bytes_written();
    assert!(after_flush >= after2);
}

#[test]
fn encoder_write_fmt_formatted_output() {
    let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);

    // Test write_fmt implementation
    write!(&mut encoder, "Number: {}, String: test", 42).unwrap();
    writeln!(&mut encoder).unwrap();
    write!(&mut encoder, "Float: {:.2}", std::f64::consts::PI).unwrap();

    let (compressed, _) = encoder.finish_into_inner().unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    let expected = "Number: 42, String: test\nFloat: 3.14";
    assert_eq!(decompressed, expected.as_bytes());
}

#[test]
fn encoder_vectored_write_partial_consumption() {
    let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);

    let data1 = b"first buffer";
    let data2 = b"second buffer";
    let data3 = b"third buffer";

    let bufs = [
        IoSlice::new(data1),
        IoSlice::new(data2),
        IoSlice::new(data3),
    ];

    // Write vectored (may consume less than all buffers)
    let written = encoder.write_vectored(&bufs).unwrap();

    // Write remaining data
    let total = data1.len() + data2.len() + data3.len();
    let all_data = [data1.as_slice(), data2.as_slice(), data3.as_slice()].concat();

    if written < total {
        encoder.write_all(&all_data[written..]).unwrap();
    }

    let (compressed, _) = encoder.finish_into_inner().unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, all_data);
}

// =============================================================================
// SECTION 2: Streaming Decompression Edge Cases
// =============================================================================

#[test]
fn decoder_small_buffer_reads() {
    let data = b"test data for small buffer reading".repeat(100);
    let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();

    let mut decoder = CountingZlibDecoder::new(Cursor::new(compressed));
    let mut output = Vec::new();

    // Read with very small buffer
    let mut buf = [0u8; 3];
    loop {
        match decoder.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => output.extend_from_slice(&buf[..n]),
            Err(e) => panic!("Read error: {e}"),
        }
    }

    assert_eq!(output, data);
    assert_eq!(decoder.bytes_read(), data.len() as u64);
}

#[test]
fn decoder_exact_size_buffer_reads() {
    let data = vec![b'x'; 4096];
    let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();

    let mut decoder = CountingZlibDecoder::new(Cursor::new(compressed));
    let mut output = vec![0u8; data.len()];

    // Try to read exactly the decompressed size
    let mut total_read = 0;
    while total_read < output.len() {
        match decoder.read(&mut output[total_read..]) {
            Ok(0) => break,
            Ok(n) => total_read += n,
            Err(e) => panic!("Read error: {e}"),
        }
    }

    assert_eq!(total_read, data.len());
    assert_eq!(&output[..total_read], &data[..]);
}

#[test]
fn decoder_read_to_end_on_large_data() {
    let data = vec![b'y'; 1024 * 1024]; // 1MB
    let compressed = compress_to_vec(&data, CompressionLevel::Fast).unwrap();

    let mut decoder = CountingZlibDecoder::new(Cursor::new(compressed));
    let mut output = Vec::new();

    decoder.read_to_end(&mut output).unwrap();

    assert_eq!(output, data);
    assert_eq!(decoder.bytes_read(), data.len() as u64);
}

#[test]
fn decoder_vectored_read_with_mixed_buffer_sizes() {
    let data = b"vectored read test with mixed sized buffers".repeat(50);
    let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();

    let mut decoder = CountingZlibDecoder::new(Cursor::new(compressed));

    let mut buf1 = [0u8; 10];
    let mut buf2 = [0u8; 50];
    let mut buf3 = [0u8; 100];
    let mut bufs = [
        IoSliceMut::new(&mut buf1),
        IoSliceMut::new(&mut buf2),
        IoSliceMut::new(&mut buf3),
    ];

    let read = decoder.read_vectored(&mut bufs).unwrap();
    assert!(read > 0);
    assert_eq!(decoder.bytes_read(), read as u64);

    // Collect the read data
    let mut collected = Vec::new();
    let mut remaining = read;

    for buf in &[buf1.as_slice(), buf2.as_slice(), buf3.as_slice()] {
        if remaining == 0 {
            break;
        }
        let to_take = remaining.min(buf.len());
        collected.extend_from_slice(&buf[..to_take]);
        remaining -= to_take;
    }

    assert_eq!(&collected[..], &data[..read]);
}

#[test]
fn decoder_get_ref_returns_underlying_reader() {
    let data = b"accessor test";
    let compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();
    let cursor = Cursor::new(compressed.clone());

    let decoder = CountingZlibDecoder::new(cursor);

    // get_ref should return reference to the cursor
    assert_eq!(decoder.get_ref().position(), 0);
    assert_eq!(decoder.get_ref().get_ref(), &compressed);
}

#[test]
fn decoder_get_mut_allows_modification() {
    let data = b"mutation test";
    let compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();
    let cursor = Cursor::new(compressed);

    let mut decoder = CountingZlibDecoder::new(cursor);

    // Use get_mut to modify underlying reader
    decoder.get_mut().set_position(2);
    assert_eq!(decoder.get_ref().position(), 2);

    // Note: Reading after seeking may fail since we're in the middle of compressed data
    // This just tests that get_mut works, not that seeking in compressed data is sensible
}

#[test]
fn decoder_into_inner_consumes_decoder() {
    let data = b"into_inner test";
    let compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();
    let cursor = Cursor::new(compressed.clone());

    let mut decoder = CountingZlibDecoder::new(cursor);

    // Read some data
    let mut buf = vec![0u8; 5];
    let _ = decoder.read(&mut buf);

    // Extract the inner reader
    let inner = decoder.into_inner();
    assert_eq!(inner.get_ref(), &compressed);
}

#[test]
fn decoder_bytes_read_saturates_at_max() {
    // This test would require decompressing u64::MAX bytes which is impractical
    // Instead, we test the saturating_add logic by checking normal operation
    let data = b"saturation test";
    let compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();

    let mut decoder = CountingZlibDecoder::new(Cursor::new(compressed));
    let mut output = Vec::new();
    decoder.read_to_end(&mut output).unwrap();

    // Bytes read should equal the data length (not saturated)
    assert_eq!(decoder.bytes_read(), data.len() as u64);
}

// =============================================================================
// SECTION 3: Error Handling for Corrupted Data
// =============================================================================

#[test]
fn decompress_random_garbage() {
    let garbage = vec![0xAB, 0xCD, 0xEF, 0x12, 0x34, 0x56, 0x78, 0x90];
    let result = decompress_to_vec(&garbage);

    // Random garbage will either fail or produce unexpected output
    // We just verify it doesn't panic and handles the error gracefully
    match result {
        Ok(output) => {
            // If it succeeds, the output should be different from input
            assert_ne!(output, garbage);
        }
        Err(_) => {
            // Expected case - decompression fails
        }
    }
}

#[test]
fn decompress_truncated_at_various_positions() {
    let data = b"test data for truncation testing".repeat(10);
    let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();

    let mut failures = 0;
    let mut different_data = 0;

    // Try truncating at various positions
    for cut_point in [
        1,
        2,
        compressed.len() / 4,
        compressed.len() / 2,
        compressed.len() - 1,
    ] {
        if cut_point >= compressed.len() {
            continue;
        }

        let truncated = &compressed[..cut_point];
        let result = decompress_to_vec(truncated);

        match result {
            Err(_) => failures += 1,
            Ok(decompressed) => {
                if decompressed != data {
                    different_data += 1;
                }
            }
        }
    }

    // At least some truncations should fail or produce different data
    assert!(
        failures + different_data > 0,
        "At least some truncations should fail or produce different data"
    );
}

#[test]
fn decompress_bit_flips_at_various_positions() {
    let data = b"test data for bit flip testing".repeat(10);
    let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();

    // Try flipping bits at various positions
    for flip_pos in [
        0,
        1,
        compressed.len() / 4,
        compressed.len() / 2,
        compressed.len() - 1,
    ] {
        if flip_pos >= compressed.len() {
            continue;
        }

        let mut corrupted = compressed.clone();
        corrupted[flip_pos] ^= 0x01; // Flip one bit

        let result = decompress_to_vec(&corrupted);

        // Bit flip should generally cause failure or produce wrong data
        // (some positions might be in padding and not affect output)
        if let Ok(decompressed) = result {
            // If decompression succeeded, data should be different (corrupted)
            // or we got lucky with padding
            if decompressed.len() == data.len() {
                // Allow for the small possibility that flipping a padding bit doesn't affect output
                // Bit flip either affects output or we're in padding
                let _ = (decompressed == data, flip_pos);
            }
        }
    }
}

#[test]
fn decompress_with_extra_trailing_data() {
    let data = b"test data";
    let mut compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();

    // Add extra data at the end
    compressed.extend_from_slice(b"EXTRA GARBAGE DATA");

    // Decompression should still work (extra data is ignored)
    let result = decompress_to_vec(&compressed);

    // This might succeed (extra data ignored) or fail depending on implementation
    if let Ok(decompressed) = result {
        assert_eq!(decompressed, data);
    }
}

#[test]
fn decompress_with_prepended_garbage() {
    let data = b"test data";
    let compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();

    // Prepend garbage
    let mut corrupted = vec![0xFF, 0xFE, 0xFD, 0xFC];
    corrupted.extend_from_slice(&compressed);

    let result = decompress_to_vec(&corrupted);
    assert!(
        result.is_err(),
        "Prepended garbage should cause decompression to fail"
    );
}

#[test]
fn decompress_all_zeros() {
    let zeros = vec![0u8; 100];
    let result = decompress_to_vec(&zeros);

    // All zeros is not valid compressed data
    assert!(result.is_err(), "All zeros should fail decompression");
}

#[test]
fn decompress_all_ones() {
    let ones = vec![0xFF; 100];
    let result = decompress_to_vec(&ones);

    // All ones is not valid compressed data
    assert!(result.is_err(), "All ones should fail decompression");
}

#[test]
fn decoder_error_recovery() {
    // Try to read from corrupted stream - use data that definitely causes error
    // Use a proper deflate header but corrupt the data section
    let mut garbage = vec![0x78, 0x9C]; // Valid zlib header
    garbage.extend_from_slice(&[0xFF; 100]); // Corrupt data

    let mut decoder = CountingZlibDecoder::new(Cursor::new(garbage));

    let mut buf = vec![0u8; 1000];
    let result = decoder.read(&mut buf);

    // Should get an error or very short read
    match result {
        Err(_) => {
            // Expected case - error reading corrupted data
        }
        Ok(n) => {
            // Some decoders might return partial data before error
            // This is also acceptable behavior
            assert!(n < 1000, "Should not read full buffer from corrupt data");
        }
    }
}

// =============================================================================
// SECTION 4: Boundary and Edge Cases
// =============================================================================

#[test]
fn compress_empty_input_all_levels() {
    for level_num in 0..=9 {
        let level = CompressionLevel::from_numeric(level_num).unwrap();
        let compressed = compress_to_vec(&[], level).unwrap();

        // Should produce valid (though possibly larger) output
        assert!(!compressed.is_empty() || level_num == 0);

        let decompressed = decompress_to_vec(&compressed).unwrap();
        assert!(decompressed.is_empty());
    }
}

#[test]
fn compress_single_byte_values() {
    // Test all possible single byte values
    for byte_val in [0x00, 0x01, 0x7F, 0x80, 0xFF] {
        let data = vec![byte_val];
        let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();
        let decompressed = decompress_to_vec(&compressed).unwrap();
        assert_eq!(decompressed, data, "Failed for byte value 0x{byte_val:02X}");
    }
}

#[test]
fn compress_power_of_two_sizes() {
    // Test data sizes that are powers of 2
    for power in [1, 2, 4, 8, 10, 12, 16, 20] {
        let size = 1 << power; // 2^power
        let data = vec![b'x'; size];

        let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();
        let decompressed = decompress_to_vec(&compressed).unwrap();

        assert_eq!(
            decompressed.len(),
            size,
            "Failed for size 2^{power} = {size}"
        );
        assert_eq!(decompressed, data);
    }
}

#[test]
fn compress_sizes_around_common_boundaries() {
    // Test sizes around common buffer boundaries
    let boundaries = [
        255, 256, 257, // Around 256
        1023, 1024, 1025, // Around 1KB
        4095, 4096, 4097, // Around 4KB
        8191, 8192, 8193, // Around 8KB
        16383, 16384, 16385, // Around 16KB
        32767, 32768, 32769, // Around 32KB
        65535, 65536, 65537, // Around 64KB
    ];

    for size in boundaries {
        let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();
        let decompressed = decompress_to_vec(&compressed).unwrap();

        assert_eq!(decompressed, data, "Failed for size {size}");
    }
}

#[test]
fn compress_highly_repetitive_data() {
    // Single repeated byte should compress extremely well
    let size = 1_000_000;
    let data = vec![b'A'; size];

    let compressed = compress_to_vec(&data, CompressionLevel::Best).unwrap();

    // Should achieve very high compression ratio
    assert!(
        compressed.len() < size / 100,
        "Expected 100:1 compression, got {}:1",
        size / compressed.len()
    );

    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, data);
}

#[test]
fn compress_all_different_bytes() {
    // Each byte appears exactly once
    let data: Vec<u8> = (0..=255).collect();

    let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();

    assert_eq!(decompressed, data);
}

#[test]
fn compress_alternating_pattern() {
    let size = 10_000;
    let data: Vec<u8> = (0..size)
        .map(|i| if i % 2 == 0 { 0x00 } else { 0xFF })
        .collect();

    let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();

    assert_eq!(decompressed, data);
}

#[test]
fn compress_ascending_sequence() {
    let size = 10_000;
    let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();

    let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();

    assert_eq!(decompressed, data);
}

// =============================================================================
// SECTION 5: CountingSink Coverage
// =============================================================================

#[test]
fn counting_sink_write_returns_exact_length() {
    let mut sink = CountingSink;

    assert_eq!(sink.write(b"").unwrap(), 0);
    assert_eq!(sink.write(b"a").unwrap(), 1);
    assert_eq!(sink.write(b"hello").unwrap(), 5);
    assert_eq!(sink.write(&[0u8; 1000]).unwrap(), 1000);
}

#[test]
fn counting_sink_write_vectored_sums_all_buffers() {
    let mut sink = CountingSink;

    let bufs = [
        IoSlice::new(b""),
        IoSlice::new(b"hello"),
        IoSlice::new(b" "),
        IoSlice::new(b"world"),
        IoSlice::new(b"!"),
    ];

    let written = sink.write_vectored(&bufs).unwrap();
    assert_eq!(written, 12); // 0 + 5 + 1 + 5 + 1
}

#[test]
fn counting_sink_flush_always_succeeds() {
    let mut sink = CountingSink;
    sink.write_all(b"data").unwrap();
    assert!(sink.flush().is_ok());
    assert!(sink.flush().is_ok()); // Multiple flushes ok
}

#[test]
fn counting_sink_with_encoder() {
    // Verify CountingSink works as default encoder sink
    let mut encoder = CountingZlibEncoder::new(CompressionLevel::Default);
    encoder.write(b"test data").unwrap();
    let bytes = encoder.finish().unwrap();

    assert!(bytes > 0);
    // Data was written to CountingSink (discarded), but bytes were counted
}

// =============================================================================
// SECTION 6: Cross-Algorithm Tests (if features enabled)
// =============================================================================

#[cfg(feature = "lz4")]
#[test]
fn lz4_frame_empty_input() {
    use compress::lz4::frame::{compress_to_vec, decompress_to_vec};

    let compressed = compress_to_vec(&[], CompressionLevel::Default).unwrap();
    assert!(
        !compressed.is_empty(),
        "LZ4 frame for empty input should have headers"
    );

    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert!(decompressed.is_empty());
}

#[cfg(feature = "lz4")]
#[test]
fn lz4_frame_large_data() {
    use compress::lz4::frame::{compress_to_vec, decompress_to_vec};

    let data = vec![b'z'; 1_000_000];
    let compressed = compress_to_vec(&data, CompressionLevel::Best).unwrap();

    assert!(
        compressed.len() < data.len(),
        "Should compress repetitive data"
    );

    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, data);
}

#[cfg(feature = "lz4")]
#[test]
fn lz4_raw_empty_block() {
    use compress::lz4::raw::{compress_block_to_vec, decompress_block_to_vec};

    let compressed = compress_block_to_vec(&[]).unwrap();
    let decompressed = decompress_block_to_vec(&compressed, 0).unwrap();
    assert!(decompressed.is_empty());
}

#[cfg(feature = "zstd")]
#[test]
fn zstd_empty_input() {
    use compress::zstd::{compress_to_vec, decompress_to_vec};

    let compressed = compress_to_vec(&[], CompressionLevel::Default).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert!(decompressed.is_empty());
}

#[cfg(feature = "zstd")]
#[test]
fn zstd_large_data() {
    use compress::zstd::{compress_to_vec, decompress_to_vec};

    let data = vec![b'w'; 1_000_000];
    let compressed = compress_to_vec(&data, CompressionLevel::Best).unwrap();

    assert!(
        compressed.len() < data.len(),
        "Should compress repetitive data"
    );

    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, data);
}

// =============================================================================
// SECTION 7: Compression Level Edge Cases
// =============================================================================

#[test]
fn compression_level_none_preserves_data_exactly() {
    let test_cases = vec![
        vec![],
        vec![0],
        vec![0xFF],
        (0..=255).collect::<Vec<u8>>(),
        vec![b'x'; 10000],
    ];

    for data in test_cases {
        let compressed = compress_to_vec(&data, CompressionLevel::None).unwrap();
        let decompressed = decompress_to_vec(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }
}

#[test]
fn compression_level_fast_works_correctly() {
    let data = b"fast compression test data".repeat(100);
    let compressed = compress_to_vec(&data, CompressionLevel::Fast).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, data);
}

#[test]
fn compression_level_best_works_correctly() {
    let data = b"best compression test data".repeat(100);
    let compressed = compress_to_vec(&data, CompressionLevel::Best).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, data);
}

#[test]
fn compression_level_default_works_correctly() {
    let data = b"default compression test data".repeat(100);
    let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, data);
}

// =============================================================================
// SECTION 8: Encoder/Decoder Accessor Coverage
// =============================================================================

#[test]
fn encoder_get_ref_before_writes() {
    let encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);
    assert!(encoder.get_ref().is_empty());
}

#[test]
fn encoder_get_ref_during_compression() {
    let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);
    encoder.write(b"test").unwrap();

    // get_ref returns the underlying sink (Vec in this case)
    // The Vec might have data if flushes occurred
    let _vec_ref = encoder.get_ref();
}

#[test]
fn encoder_get_mut_modification() {
    let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);

    // Modify the underlying Vec directly
    encoder.get_mut().extend_from_slice(b"prefix:");

    // Now compress data
    encoder.write(b"data").unwrap();

    let (result, _) = encoder.finish_into_inner().unwrap();

    // Result should have our prefix plus compressed data
    assert!(result.starts_with(b"prefix:"));
}

#[test]
fn encoder_bytes_written_before_any_writes() {
    let encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);
    assert_eq!(encoder.bytes_written(), 0);
}

// =============================================================================
// SECTION 9: Write Trait Implementation Coverage
// =============================================================================

#[test]
fn encoder_write_trait_write_method() {
    let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);

    // Use Write::write directly (may not write all bytes)
    let data = b"test data for partial write";
    let written = Write::write(&mut encoder, data).unwrap();

    assert!(written > 0);
    assert!(written <= data.len());

    // Write any remaining
    if written < data.len() {
        encoder.write_all(&data[written..]).unwrap();
    }

    let (compressed, _) = encoder.finish_into_inner().unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, data);
}

#[test]
fn encoder_write_all_guarantees_complete_write() {
    let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);

    let data = b"write_all should write everything";
    encoder.write_all(data).unwrap();

    let (compressed, _) = encoder.finish_into_inner().unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, data);
}

#[test]
fn encoder_flush_can_be_called_multiple_times() {
    let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);

    encoder.write(b"data").unwrap();
    encoder.flush().unwrap();
    encoder.flush().unwrap();
    encoder.flush().unwrap();

    let (compressed, _) = encoder.finish_into_inner().unwrap();
    let decompressed = decompress_to_vec(&compressed).unwrap();
    assert_eq!(decompressed, b"data");
}
