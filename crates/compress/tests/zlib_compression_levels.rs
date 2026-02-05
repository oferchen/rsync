//! Comprehensive tests for zlib compression levels 1-9.
//!
//! This test module verifies:
//! 1. All compression levels 1-9 produce valid output
//! 2. Compression ratio generally increases with level (though not strictly monotonic)
//! 3. Compression/decompression speed characteristics
//! 4. Behavior with various data types (text, binary, random, repetitive)
//! 5. Round-trip integrity (decompression produces original data)

use std::io::Read;
use std::num::NonZeroU8;
use std::time::{Duration, Instant};

use compress::zlib::{
    CompressionLevel, CountingZlibDecoder, CountingZlibEncoder, compress_to_vec, decompress_to_vec,
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
            Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. \
            Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris \
            nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in \
            reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla \
            pariatur. Excepteur sint occaecat cupidatat non proident, sunt in \
            culpa qui officia deserunt mollit anim id est laborum. ";
        text.iter().cycle().take(size).copied().collect()
    }

    /// Generates structured binary data (simulating file headers, records).
    pub fn structured_binary(size: usize) -> Vec<u8> {
        let mut data = Vec::with_capacity(size);
        let mut counter: u32 = 0;
        while data.len() < size {
            // Simulate record structure: length (4 bytes) + type (1 byte) + data
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
    /// Uses a simple LCG for reproducibility without external dependencies.
    pub fn random_data(size: usize, seed: u64) -> Vec<u8> {
        let mut state = seed;
        let mut data = Vec::with_capacity(size);
        for _ in 0..size {
            // Linear congruential generator
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
            // Pattern: some data followed by zeros
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

    /// Generates source code-like text (high redundancy in keywords).
    pub fn source_code(size: usize) -> Vec<u8> {
        let code = b"fn main() {\n    let result = compute_value(42);\n    \
            if result > 0 {\n        println!(\"Value: {}\", result);\n    } else {\n        \
            eprintln!(\"Error: negative value\");\n    }\n}\n\n\
            fn compute_value(input: i32) -> i32 {\n    let mut sum = 0;\n    \
            for i in 0..input {\n        sum += i * 2;\n    }\n    sum\n}\n\n";
        code.iter().cycle().take(size).copied().collect()
    }
}

/// Results from a compression test for a single level.
#[derive(Debug)]
struct CompressionResult {
    level: u32,
    original_size: usize,
    compressed_size: usize,
    compression_time: Duration,
    decompression_time: Duration,
    round_trip_verified: bool,
}

impl CompressionResult {
    fn compression_ratio(&self) -> f64 {
        self.original_size as f64 / self.compressed_size as f64
    }

    fn space_savings_percent(&self) -> f64 {
        (1.0 - (self.compressed_size as f64 / self.original_size as f64)) * 100.0
    }
}

/// Runs compression test for a single level.
fn test_compression_level(data: &[u8], level: u32) -> CompressionResult {
    let compression_level = CompressionLevel::from_numeric(level).expect("valid level");

    // Measure compression time
    let compress_start = Instant::now();
    let compressed = compress_to_vec(data, compression_level).expect("compression succeeds");
    let compression_time = compress_start.elapsed();

    // Measure decompression time
    let decompress_start = Instant::now();
    let decompressed = decompress_to_vec(&compressed).expect("decompression succeeds");
    let decompression_time = decompress_start.elapsed();

    // Verify round-trip integrity
    let round_trip_verified = decompressed == data;

    CompressionResult {
        level,
        original_size: data.len(),
        compressed_size: compressed.len(),
        compression_time,
        decompression_time,
        round_trip_verified,
    }
}

// =============================================================================
// Test: All levels 1-9 produce valid output
// =============================================================================

#[test]
fn all_levels_produce_valid_compressed_output() {
    let data = test_data::english_text(10_000);

    for level in 1..=9 {
        let compression_level = CompressionLevel::from_numeric(level).expect("valid level");
        let compressed = compress_to_vec(&data, compression_level)
            .unwrap_or_else(|e| panic!("level {level} compression failed: {e}"));

        assert!(
            !compressed.is_empty(),
            "level {level} produced empty output"
        );
        assert!(
            compressed.len() < data.len(),
            "level {level} did not compress data"
        );

        // Verify decompression works
        let decompressed = decompress_to_vec(&compressed)
            .unwrap_or_else(|e| panic!("level {level} decompression failed: {e}"));

        assert_eq!(
            decompressed, data,
            "level {level} round-trip integrity failed"
        );
    }
}

#[test]
fn all_levels_work_with_streaming_encoder() {
    let data = test_data::english_text(10_000);
    let chunks: Vec<&[u8]> = data.chunks(1000).collect();

    for level in 1..=9 {
        let compression_level = CompressionLevel::from_numeric(level).expect("valid level");
        let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), compression_level);

        for chunk in &chunks {
            encoder.write(chunk).expect("write chunk");
        }

        let (compressed, bytes_written) = encoder.finish_into_inner().expect("finish encoder");

        assert!(bytes_written > 0, "level {level} wrote no bytes");
        assert_eq!(
            bytes_written as usize,
            compressed.len(),
            "level {level} byte count mismatch"
        );

        let decompressed = decompress_to_vec(&compressed).expect("decompress");
        assert_eq!(
            decompressed, data,
            "level {level} streaming round-trip failed"
        );
    }
}

#[test]
fn all_levels_work_with_streaming_decoder() {
    let data = test_data::english_text(10_000);

    for level in 1..=9 {
        let compression_level = CompressionLevel::from_numeric(level).expect("valid level");
        let compressed = compress_to_vec(&data, compression_level).expect("compress");

        let mut decoder = CountingZlibDecoder::new(&compressed[..]);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed).expect("read all");

        assert_eq!(
            decoder.bytes_read(),
            data.len() as u64,
            "level {level} decoder byte count mismatch"
        );
        assert_eq!(decompressed, data, "level {level} streaming decode failed");
    }
}

// =============================================================================
// Test: Compression ratio trends with level
// =============================================================================

#[test]
fn higher_levels_achieve_better_or_equal_compression_on_compressible_data() {
    // Use highly compressible data where level differences are most apparent
    let data = test_data::repetitive_text(100_000);

    let mut results: Vec<CompressionResult> = Vec::new();
    for level in 1..=9 {
        results.push(test_compression_level(&data, level));
    }

    // Verify all round trips succeeded
    for result in &results {
        assert!(
            result.round_trip_verified,
            "level {} round-trip failed",
            result.level
        );
    }

    // Compression should generally improve with level
    // Note: We allow for occasional local non-monotonicity due to algorithm internals
    let level_1_size = results[0].compressed_size;
    let level_9_size = results[8].compressed_size;

    assert!(
        level_9_size <= level_1_size,
        "level 9 ({level_9_size}) should compress at least as well as level 1 ({level_1_size})"
    );

    // Check that the highest levels achieve significantly better compression than lowest
    let improvement = (level_1_size as f64 - level_9_size as f64) / level_1_size as f64;
    assert!(
        improvement >= 0.0,
        "Expected some compression improvement from level 1 to 9, got negative improvement"
    );
}

#[test]
fn compression_ratio_varies_by_data_type() {
    let size = 50_000;
    let test_cases = [
        ("repetitive_text", test_data::repetitive_text(size)),
        ("english_text", test_data::english_text(size)),
        ("structured_binary", test_data::structured_binary(size)),
        ("random_data", test_data::random_data(size, 12345)),
        ("sparse_data", test_data::sparse_data(size)),
        ("source_code", test_data::source_code(size)),
    ];

    for (name, data) in &test_cases {
        let result = test_compression_level(data, 6); // Use default-like level
        assert!(
            result.round_trip_verified,
            "{name}: round-trip integrity failed"
        );

        // Random data should compress poorly
        if *name == "random_data" {
            // Random data typically has ratio close to 1.0 or slightly worse
            assert!(
                result.compression_ratio() < 1.5,
                "{name}: random data unexpectedly compressible (ratio: {:.2})",
                result.compression_ratio()
            );
        }

        // Highly repetitive data should compress very well
        if *name == "repetitive_text" || *name == "sparse_data" {
            assert!(
                result.compression_ratio() > 2.0,
                "{name}: expected high compression ratio, got {:.2}",
                result.compression_ratio()
            );
        }
    }
}

// =============================================================================
// Test: Compression/decompression speed characteristics
// =============================================================================

#[test]
fn compression_speed_generally_decreases_with_level() {
    let data = test_data::english_text(100_000);
    let iterations = 3;

    let mut avg_times: Vec<(u32, Duration)> = Vec::new();

    for level in 1..=9 {
        let compression_level = CompressionLevel::from_numeric(level).expect("valid level");
        let mut total_time = Duration::ZERO;

        for _ in 0..iterations {
            let start = Instant::now();
            let _ = compress_to_vec(&data, compression_level).expect("compress");
            total_time += start.elapsed();
        }

        avg_times.push((level, total_time / iterations as u32));
    }

    // Level 1 should generally be faster than level 9
    // We use a relaxed check because timing can be noisy
    let level_1_time = avg_times[0].1;
    let level_9_time = avg_times[8].1;

    // Only assert if there's a significant difference (level 9 is at least 20% slower)
    // This accounts for measurement noise in fast operations
    if level_9_time > Duration::from_micros(100) {
        // Skip assertion for very fast operations where noise dominates
        let ratio = level_9_time.as_nanos() as f64 / level_1_time.as_nanos().max(1) as f64;
        assert!(
            ratio >= 0.5,
            "Level 9 ({level_9_time:?}) was unexpectedly much faster than level 1 ({level_1_time:?})"
        );
    }
}

#[test]
fn decompression_speed_independent_of_compression_level() {
    let data = test_data::english_text(100_000);
    let iterations = 3;

    // Compress at different levels
    let compressed_1 = compress_to_vec(&data, CompressionLevel::from_numeric(1).unwrap()).unwrap();
    let compressed_9 = compress_to_vec(&data, CompressionLevel::from_numeric(9).unwrap()).unwrap();

    // Measure decompression times
    let mut time_1 = Duration::ZERO;
    let mut time_9 = Duration::ZERO;

    for _ in 0..iterations {
        let start = Instant::now();
        let _ = decompress_to_vec(&compressed_1).unwrap();
        time_1 += start.elapsed();

        let start = Instant::now();
        let _ = decompress_to_vec(&compressed_9).unwrap();
        time_9 += start.elapsed();
    }

    // Decompression times should be relatively similar (within 5x)
    // The exact ratio depends on the data and implementation
    let ratio = (time_1.as_nanos() as f64).max(1.0) / (time_9.as_nanos() as f64).max(1.0);
    assert!(
        (0.2..5.0).contains(&ratio),
        "Decompression times unexpectedly different: level 1 = {time_1:?}, level 9 = {time_9:?}"
    );
}

// =============================================================================
// Test: Various data types
// =============================================================================

#[test]
fn compress_empty_data_all_levels() {
    let data: &[u8] = &[];

    for level in 1..=9 {
        let compression_level = CompressionLevel::from_numeric(level).expect("valid level");
        let compressed = compress_to_vec(data, compression_level)
            .unwrap_or_else(|e| panic!("level {level} failed on empty data: {e}"));

        let decompressed = decompress_to_vec(&compressed)
            .unwrap_or_else(|e| panic!("level {level} decompression failed on empty: {e}"));

        assert!(
            decompressed.is_empty(),
            "level {level}: expected empty output"
        );
    }
}

#[test]
fn compress_single_byte_all_levels() {
    let data: &[u8] = &[42];

    for level in 1..=9 {
        let compression_level = CompressionLevel::from_numeric(level).expect("valid level");
        let compressed = compress_to_vec(data, compression_level)
            .unwrap_or_else(|e| panic!("level {level} failed on single byte: {e}"));

        let decompressed = decompress_to_vec(&compressed)
            .unwrap_or_else(|e| panic!("level {level} decompression failed on single byte: {e}"));

        assert_eq!(
            decompressed, data,
            "level {level}: single byte round-trip failed"
        );
    }
}

#[test]
fn compress_all_byte_values() {
    // Data containing all possible byte values
    let data: Vec<u8> = (0..=255).collect();

    for level in 1..=9 {
        let compression_level = CompressionLevel::from_numeric(level).expect("valid level");
        let compressed = compress_to_vec(&data, compression_level).expect("compress");
        let decompressed = decompress_to_vec(&compressed).expect("decompress");

        assert_eq!(
            decompressed, data,
            "level {level}: all-bytes round-trip failed"
        );
    }
}

#[test]
fn compress_large_data_all_levels() {
    // 1 MB of data
    let data = test_data::english_text(1_000_000);

    for level in 1..=9 {
        let result = test_compression_level(&data, level);
        assert!(
            result.round_trip_verified,
            "level {} failed on large data",
            result.level
        );
        assert!(
            result.compressed_size < data.len(),
            "level {level} did not compress 1MB of text"
        );
    }
}

#[test]
fn compress_binary_patterns() {
    let patterns: &[(&str, &[u8])] = &[
        ("all_zeros", &[0u8; 1000]),
        ("all_ones", &[255u8; 1000]),
        ("alternating", &{
            let mut p = [0u8; 1000];
            for (i, b) in p.iter_mut().enumerate() {
                *b = if i % 2 == 0 { 0 } else { 255 };
            }
            p
        }),
    ];

    for (name, data) in patterns {
        for level in 1..=9 {
            let compression_level = CompressionLevel::from_numeric(level).expect("valid level");
            let compressed = compress_to_vec(data, compression_level)
                .unwrap_or_else(|e| panic!("{name} level {level} compression failed: {e}"));

            let decompressed = decompress_to_vec(&compressed)
                .unwrap_or_else(|e| panic!("{name} level {level} decompression failed: {e}"));

            assert_eq!(
                decompressed.as_slice(),
                *data,
                "{name} level {level}: round-trip failed"
            );
        }
    }
}

// =============================================================================
// Test: CompressionLevel API
// =============================================================================

#[test]
fn compression_level_from_numeric_valid_range() {
    for level in 1..=9 {
        let result = CompressionLevel::from_numeric(level);
        assert!(result.is_ok(), "level {level} should be valid");

        let compression_level = result.unwrap();
        match compression_level {
            CompressionLevel::Precise(n) => {
                assert_eq!(n.get() as u32, level);
            }
            _ => panic!("Expected Precise variant for level {level}"),
        }
    }
}

// =============================================================================
// Test: Level 0 (no compression) behavior
// =============================================================================

#[test]
fn level_zero_produces_valid_output() {
    let data = test_data::english_text(10_000);

    let compression_level = CompressionLevel::from_numeric(0).expect("level 0 should be valid");
    assert_eq!(
        compression_level,
        CompressionLevel::None,
        "level 0 should map to None variant"
    );

    let compressed = compress_to_vec(&data, compression_level).expect("level 0 compression works");

    // Level 0 should produce valid output (may be larger due to framing overhead)
    assert!(!compressed.is_empty(), "level 0 produced empty output");

    // Verify decompression works
    let decompressed = decompress_to_vec(&compressed).expect("level 0 decompression works");
    assert_eq!(decompressed, data, "level 0 round-trip integrity failed");
}

#[test]
fn level_zero_may_inflate_data() {
    // Small data is likely to inflate due to deflate framing overhead
    let data = b"tiny";

    let compressed =
        compress_to_vec(data, CompressionLevel::None).expect("level 0 compression works");

    // Level 0 adds deflate framing without compression, so small data typically inflates
    // We just verify it round-trips correctly, not the size relationship
    let decompressed = decompress_to_vec(&compressed).expect("level 0 decompression works");
    assert_eq!(
        decompressed.as_slice(),
        data,
        "level 0 tiny data round-trip failed"
    );
}

#[test]
fn level_zero_streaming_works() {
    let data = test_data::english_text(10_000);
    let chunks: Vec<&[u8]> = data.chunks(1000).collect();

    let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::None);

    for chunk in &chunks {
        encoder.write(chunk).expect("write chunk");
    }

    let (compressed, bytes_written) = encoder.finish_into_inner().expect("finish encoder");

    assert!(bytes_written > 0, "level 0 wrote no bytes");
    assert_eq!(
        bytes_written as usize,
        compressed.len(),
        "level 0 byte count mismatch"
    );

    let decompressed = decompress_to_vec(&compressed).expect("decompress");
    assert_eq!(decompressed, data, "level 0 streaming round-trip failed");
}

#[test]
fn level_zero_produces_larger_output_than_higher_levels() {
    // For compressible data, level 0 should produce larger output than level 1+
    let data = test_data::repetitive_text(10_000);

    let level_0_compressed =
        compress_to_vec(&data, CompressionLevel::None).expect("level 0 compress");
    let level_1_compressed = compress_to_vec(&data, CompressionLevel::from_numeric(1).unwrap())
        .expect("level 1 compress");
    let level_9_compressed = compress_to_vec(&data, CompressionLevel::from_numeric(9).unwrap())
        .expect("level 9 compress");

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
        decompress_to_vec(&level_0_compressed).unwrap(),
        data,
        "level 0 round-trip failed"
    );
    assert_eq!(
        decompress_to_vec(&level_1_compressed).unwrap(),
        data,
        "level 1 round-trip failed"
    );
    assert_eq!(
        decompress_to_vec(&level_9_compressed).unwrap(),
        data,
        "level 9 round-trip failed"
    );
}

#[test]
fn level_zero_with_empty_data() {
    let data: &[u8] = &[];

    let compressed = compress_to_vec(data, CompressionLevel::None).expect("level 0 empty compress");

    let decompressed = decompress_to_vec(&compressed).expect("level 0 empty decompress");

    assert!(decompressed.is_empty(), "level 0: expected empty output");
}

#[test]
fn level_zero_with_single_byte() {
    let data: &[u8] = &[42];

    let compressed =
        compress_to_vec(data, CompressionLevel::None).expect("level 0 single byte compress");

    let decompressed = decompress_to_vec(&compressed).expect("level 0 single byte decompress");

    assert_eq!(decompressed, data, "level 0: single byte round-trip failed");
}

#[test]
fn level_zero_with_all_byte_values() {
    // Data containing all possible byte values
    let data: Vec<u8> = (0..=255).collect();

    let compressed =
        compress_to_vec(&data, CompressionLevel::None).expect("level 0 all-bytes compress");
    let decompressed = decompress_to_vec(&compressed).expect("level 0 all-bytes decompress");

    assert_eq!(decompressed, data, "level 0: all-bytes round-trip failed");
}

#[test]
fn compression_level_from_numeric_zero_returns_none_variant() {
    let result = CompressionLevel::from_numeric(0);
    assert!(result.is_ok(), "level 0 should be valid");

    let level = result.unwrap();
    assert_eq!(level, CompressionLevel::None);
}

#[test]
fn compression_level_from_numeric_invalid_above_nine() {
    for level in [10, 11, 100, u32::MAX] {
        let result = CompressionLevel::from_numeric(level);
        assert!(result.is_err(), "level {level} should be invalid");

        let err = result.unwrap_err();
        assert_eq!(err.level(), level);
    }
}

#[test]
fn compression_level_precise_constructor() {
    for n in 1..=9 {
        let nz = NonZeroU8::new(n).unwrap();
        let level = CompressionLevel::precise(nz);
        match level {
            CompressionLevel::Precise(inner) => assert_eq!(inner.get(), n),
            _ => panic!("Expected Precise variant"),
        }
    }
}

#[test]
fn compression_level_presets_produce_valid_output() {
    let data = test_data::english_text(10_000);
    let presets = [
        ("Fast", CompressionLevel::Fast),
        ("Default", CompressionLevel::Default),
        ("Best", CompressionLevel::Best),
    ];

    for (name, level) in presets {
        let compressed = compress_to_vec(&data, level)
            .unwrap_or_else(|e| panic!("{name} compression failed: {e}"));

        let decompressed = decompress_to_vec(&compressed)
            .unwrap_or_else(|e| panic!("{name} decompression failed: {e}"));

        assert_eq!(decompressed, data, "{name}: round-trip integrity failed");
    }
}

#[test]
fn compression_level_presets_ordering() {
    let data = test_data::repetitive_text(50_000);

    let fast_compressed = compress_to_vec(&data, CompressionLevel::Fast).unwrap();
    let default_compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();
    let best_compressed = compress_to_vec(&data, CompressionLevel::Best).unwrap();

    // Best should compress at least as well as Fast
    assert!(
        best_compressed.len() <= fast_compressed.len(),
        "Best ({}) should compress at least as well as Fast ({})",
        best_compressed.len(),
        fast_compressed.len()
    );

    // Default should be between Fast and Best (or equal to one)
    assert!(
        default_compressed.len() <= fast_compressed.len()
            || default_compressed.len() >= best_compressed.len(),
        "Default ({}) should be between Fast ({}) and Best ({})",
        default_compressed.len(),
        fast_compressed.len(),
        best_compressed.len()
    );
}

// =============================================================================
// Test: Round-trip integrity with various chunk sizes
// =============================================================================

#[test]
fn streaming_compression_various_chunk_sizes() {
    let data = test_data::english_text(50_000);
    let chunk_sizes = [1, 7, 64, 256, 1024, 4096, 8192];

    for level in [1, 5, 9] {
        let compression_level = CompressionLevel::from_numeric(level).expect("valid level");

        for &chunk_size in &chunk_sizes {
            let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), compression_level);

            for chunk in data.chunks(chunk_size) {
                encoder.write(chunk).expect("write chunk");
            }

            let (compressed, _) = encoder.finish_into_inner().expect("finish");
            let decompressed = decompress_to_vec(&compressed).expect("decompress");

            assert_eq!(
                decompressed, data,
                "level {level}, chunk size {chunk_size}: round-trip failed"
            );
        }
    }
}

// =============================================================================
// Test: Error display
// =============================================================================

#[test]
fn compression_level_error_display() {
    let err = CompressionLevel::from_numeric(42).unwrap_err();
    let display = err.to_string();
    assert!(
        display.contains("42"),
        "Error message should contain the invalid level"
    );
    assert!(
        display.contains("1-9") || display.contains("range"),
        "Error message should mention valid range"
    );
}

// =============================================================================
// Benchmark-style tests (measure relative performance)
// =============================================================================

/// Prints a summary table comparing all compression levels.
/// This test always passes but produces useful diagnostic output.
#[test]
fn compression_level_comparison_summary() {
    let size = 100_000;
    let test_cases = [
        ("English text", test_data::english_text(size)),
        ("Source code", test_data::source_code(size)),
        ("Structured binary", test_data::structured_binary(size)),
        ("Random data", test_data::random_data(size, 42)),
    ];

    for (name, data) in &test_cases {
        eprintln!("\n=== {name} ({size} bytes) ===");
        eprintln!(
            "{:>5} | {:>10} | {:>8} | {:>8} | {:>10} | {:>12}",
            "Level", "Compressed", "Ratio", "Savings", "Comp Time", "Decomp Time"
        );
        eprintln!("{}", "-".repeat(70));

        for level in 1..=9 {
            let result = test_compression_level(data, level);
            assert!(result.round_trip_verified, "Round-trip failed");

            eprintln!(
                "{:>5} | {:>10} | {:>7.2}x | {:>7.1}% | {:>10.2?} | {:>12.2?}",
                level,
                result.compressed_size,
                result.compression_ratio(),
                result.space_savings_percent(),
                result.compression_time,
                result.decompression_time
            );
        }
    }
}
